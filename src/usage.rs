//! Inferring when a package was last actually used, from file access times.
//!
//! # Why atime works, and where it does not
//!
//! Arch mounts filesystems `relatime` by default: a file's access time is
//! updated on read only if the stored atime is older than its mtime/ctime or
//! more than 24 hours old. That is useless for "was this read in the last
//! minute" but exactly right for "has this been touched in the last six
//! months", which is the question worth asking before deleting something.
//!
//! Two failure modes are handled explicitly rather than papered over:
//!
//! * A `noatime` mount freezes access times entirely. Reporting a stale
//!   timestamp as "last used" would be actively misleading, so the mount
//!   options are read and the whole column is disabled instead.
//! * Package extraction sets an atime at install time. A package that has
//!   never been run therefore looks "used" on its install date. Comparing
//!   against the recorded install date turns that into the more useful
//!   verdict: *never used since it was installed*.
//!
//! Pure: [`crate::scan`] supplies the stat results.

use crate::model::{AtimeSupport, UsageEvidence};

/// Path prefixes grouped into tiers of evidence, strongest first.
///
/// A package is judged by the strongest tier it actually ships. If it has
/// executables, whether those ran is the question worth asking; if it ships
/// only data — a font family, an icon theme, a TeX distribution — then whether
/// anything read that data is the question instead.
///
/// Judging by the *best available* tier rather than by everything at once
/// matters: taking the newest access time across a mixed set would let a
/// stray read of one data file vouch for a binary that has never run.
const WITNESS_TIERS: [&[&str]; 4] = [
    // Executables: running one is the strongest possible evidence.
    &[
        "usr/bin/",
        "usr/local/bin/",
        "usr/libexec/",
        "usr/lib/systemd/",
        "opt/",
    ],
    // Shared libraries, read when something links against them at run time.
    &["usr/lib/", "usr/lib32/", "usr/local/lib/"],
    // Data that software reads while running: fonts, icons, themes, texmf,
    // application resources. This is where the largest packages on a desktop
    // live, so excluding it wholesale left them with no verdict at all.
    &["usr/share/", "usr/local/share/", "var/lib/"],
    // Anything else the package owns. Weak, but a weak verdict beats none.
    &[],
];

/// Path prefixes that are read by indexers, documentation viewers and backup
/// tools rather than by using the software, so their access times mean nothing.
const NOISE_PREFIXES: [&str; 8] = [
    "usr/share/man/",
    "usr/share/doc/",
    "usr/share/info/",
    "usr/share/licenses/",
    "usr/share/help/",
    // Translations for languages the user does not run in are never read even
    // when the package is in daily use.
    "usr/share/locale/",
    "usr/include/",
    "usr/src/",
];

/// Files whose access time is set by tooling rather than by use.
const NOISE_SUFFIXES: [&str; 6] = [".pc", ".h", ".hpp", ".a", ".pyc", ".mo"];

/// Which evidence tier a path belongs to, or `None` if it is noise.
pub fn witness_tier(path: &str) -> Option<usize> {
    if NOISE_PREFIXES.iter().any(|prefix| path.starts_with(prefix)) {
        return None;
    }
    if NOISE_SUFFIXES.iter().any(|suffix| path.ends_with(suffix)) {
        return None;
    }

    for (tier, prefixes) in WITNESS_TIERS.iter().enumerate() {
        if prefixes.iter().any(|prefix| path.starts_with(prefix)) {
            return Some(tier);
        }
    }

    // The final tier has no prefixes and catches everything left over.
    Some(WITNESS_TIERS.len().saturating_sub(1))
}

/// Whether a path is worth stat-ing as evidence of use.
pub fn is_witness(path: &str) -> bool {
    witness_tier(path).is_some()
}

/// Choose up to `budget` files to stat, all from the strongest tier present.
///
/// Within a tier the database order is kept, so the choice is deterministic.
pub fn witnesses(files: &[String], budget: usize) -> Vec<&str> {
    if budget == 0 {
        return Vec::new();
    }

    let ranked: Vec<(usize, &str)> = files
        .iter()
        .map(String::as_str)
        .filter_map(|path| witness_tier(path).map(|tier| (tier, path)))
        .collect();

    let Some(best) = ranked.iter().map(|(tier, _path)| *tier).min() else {
        return Vec::new();
    };

    ranked
        .into_iter()
        .filter(|(tier, _path)| *tier == best)
        .map(|(_tier, path)| path)
        .take(budget)
        .collect()
}

/// An access time observed for one witness file.
pub struct Observation<'a> {
    /// Root-relative path that was stat-ed.
    pub path: &'a str,
    /// Its access time, in Unix seconds.
    pub atime: i64,
}

/// Grace period, in seconds, around the recorded install date.
///
/// Extraction stamps files over the course of the transaction and pacman
/// records a single install date for the whole package, so an atime a few
/// minutes either side of that date still means "untouched since install".
const INSTALL_GRACE: i64 = 900;

/// Turn observed access times into a verdict.
pub fn evaluate(
    observations: &[Observation<'_>],
    install_date: Option<i64>,
    support: AtimeSupport,
) -> UsageEvidence {
    if !support.is_meaningful() {
        return UsageEvidence::AtimeDisabled;
    }

    let Some(best) = observations
        .iter()
        .max_by_key(|observation| observation.atime)
    else {
        return UsageEvidence::NoWitness;
    };

    let threshold = install_date.map(|date| date.saturating_add(INSTALL_GRACE));
    match threshold {
        Some(threshold) if best.atime <= threshold => {
            UsageEvidence::NeverSinceInstall { at: best.atime }
        }
        Some(_) | None => UsageEvidence::Used {
            at: best.atime,
            witness: best.path.to_owned(),
        },
    }
}

/// Parse `/proc/mounts` and decide how much access times can be trusted.
///
/// The strictest option wins across the mounts that matter: if `/usr` — or
/// whichever mount covers it — is `noatime`, the whole feature is off, because
/// that is where package files live.
pub fn atime_support(mounts: &str, path_of_interest: &str) -> AtimeSupport {
    let mut best: Option<(usize, AtimeSupport)> = None;

    for line in mounts.lines() {
        let mut fields = line.split_whitespace();
        let _device = fields.next();
        let Some(mount_point) = fields.next() else {
            continue;
        };
        let _filesystem = fields.next();
        let Some(options) = fields.next() else {
            continue;
        };

        if !covers(mount_point, path_of_interest) {
            continue;
        }

        let support = if has_option(options, "noatime") {
            AtimeSupport::Disabled
        } else if has_option(options, "strictatime") {
            AtimeSupport::Strict
        } else {
            // `relatime` is the kernel default even when the option is absent.
            AtimeSupport::Relatime
        };

        let specificity = mount_point.len();
        let replace = match best {
            Some((current, _support)) => specificity > current,
            None => true,
        };
        if replace {
            best = Some((specificity, support));
        }
    }

    match best {
        Some((_specificity, support)) => support,
        None => AtimeSupport::Unknown,
    }
}

/// Whether `mount_point` is an ancestor of (or equal to) `path`.
fn covers(mount_point: &str, path: &str) -> bool {
    if mount_point == "/" {
        return path.starts_with('/');
    }
    if path == mount_point {
        return true;
    }
    path.starts_with(mount_point) && path.get(mount_point.len()..).unwrap_or("").starts_with('/')
}

/// Whether a comma-separated mount option list contains `needle` exactly.
fn has_option(options: &str, needle: &str) -> bool {
    options.split(',').any(|option| option == needle)
}

#[cfg(test)]
mod tests {
    use super::{atime_support, evaluate, is_witness, witness_tier, witnesses, Observation};
    use crate::model::{AtimeSupport, UsageEvidence};

    #[test]
    fn binaries_and_libraries_are_witnesses() {
        assert!(is_witness("usr/bin/rg"));
        assert!(is_witness("usr/lib/libfoo.so.1"));
        assert!(is_witness("opt/vendor/tool/run"));
    }

    #[test]
    fn documentation_is_not_a_witness() {
        assert!(!is_witness("usr/share/man/man1/rg.1.gz"));
        assert!(!is_witness("usr/share/doc/rg/README"));
        assert!(!is_witness("usr/share/locale/de/LC_MESSAGES/rg.mo"));
        assert!(!is_witness("usr/include/foo.h"));
        assert!(!is_witness("usr/lib/pkgconfig/foo.pc"));
    }

    #[test]
    fn package_data_is_a_witness() {
        // The largest packages on a desktop ship nothing but data — fonts,
        // icon themes, a TeX distribution. Excluding all of usr/share left
        // exactly those packages with no verdict.
        assert!(is_witness("usr/share/fonts/noto/NotoSans-Regular.ttf"));
        assert!(is_witness("usr/share/icons/Adwaita/index.theme"));
        assert!(is_witness(
            "usr/share/texmf-dist/tex/latex/base/article.cls"
        ));
    }

    #[test]
    fn tiers_run_from_executables_down_to_leftovers() {
        assert_eq!(witness_tier("usr/bin/rg"), Some(0));
        assert_eq!(witness_tier("usr/lib/libfoo.so.1"), Some(1));
        assert_eq!(witness_tier("usr/share/fonts/x.ttf"), Some(2));
        assert_eq!(witness_tier("etc/rg.conf"), Some(3));
        assert_eq!(witness_tier("usr/share/man/man1/rg.1"), None);
    }

    #[test]
    fn only_the_strongest_tier_present_is_used() {
        // A stray read of a data file must not vouch for a binary that has
        // never run, so the tiers are not mixed.
        let files = vec![
            "usr/lib/libfoo.so".to_owned(),
            "usr/share/man/man1/x.1".to_owned(),
            "usr/share/foo/data.bin".to_owned(),
            "usr/bin/foo".to_owned(),
        ];
        assert_eq!(witnesses(&files, 5), vec!["usr/bin/foo"]);
        assert_eq!(witnesses(&files, 1), vec!["usr/bin/foo"]);
        assert!(witnesses(&files, 0).is_empty());
    }

    #[test]
    fn a_package_falls_back_to_the_best_tier_it_actually_ships() {
        let libraries = vec![
            "usr/lib/libfoo.so".to_owned(),
            "usr/share/foo/data.bin".to_owned(),
        ];
        assert_eq!(witnesses(&libraries, 5), vec!["usr/lib/libfoo.so"]);

        let data_only = vec![
            "usr/share/man/man1/x.1".to_owned(),
            "usr/share/fonts/x.ttf".to_owned(),
        ];
        assert_eq!(witnesses(&data_only, 5), vec!["usr/share/fonts/x.ttf"]);

        let leftovers = vec!["etc/foo.conf".to_owned()];
        assert_eq!(witnesses(&leftovers, 5), vec!["etc/foo.conf"]);
    }

    #[test]
    fn only_a_package_with_no_usable_file_has_no_witness() {
        let nothing = vec![
            "usr/share/man/man1/x.1".to_owned(),
            "usr/share/doc/foo/README".to_owned(),
            "usr/include/foo.h".to_owned(),
        ];
        assert!(witnesses(&nothing, 5).is_empty());
        assert!(witnesses(&[], 5).is_empty());
    }

    #[test]
    fn a_recent_read_counts_as_use() {
        let observations = vec![Observation {
            path: "usr/bin/rg",
            atime: 2_000,
        }];
        assert_eq!(
            evaluate(&observations, Some(1_000), AtimeSupport::Relatime),
            UsageEvidence::Used {
                at: 2_000,
                witness: "usr/bin/rg".to_owned()
            }
        );
    }

    #[test]
    fn an_atime_at_install_time_means_never_used() {
        let observations = vec![Observation {
            path: "usr/bin/rg",
            atime: 1_010,
        }];
        assert_eq!(
            evaluate(&observations, Some(1_000), AtimeSupport::Relatime),
            UsageEvidence::NeverSinceInstall { at: 1_010 }
        );
    }

    #[test]
    fn the_newest_witness_wins() {
        let observations = vec![
            Observation {
                path: "usr/lib/libfoo.so",
                atime: 5_000,
            },
            Observation {
                path: "usr/bin/foo",
                atime: 9_000,
            },
        ];
        assert_eq!(
            evaluate(&observations, Some(1_000), AtimeSupport::Relatime),
            UsageEvidence::Used {
                at: 9_000,
                witness: "usr/bin/foo".to_owned()
            }
        );
    }

    #[test]
    fn no_witnesses_is_reported_as_such() {
        assert_eq!(
            evaluate(&[], Some(1_000), AtimeSupport::Relatime),
            UsageEvidence::NoWitness
        );
    }

    #[test]
    fn noatime_suppresses_every_verdict() {
        let observations = vec![Observation {
            path: "usr/bin/rg",
            atime: 9_999,
        }];
        assert_eq!(
            evaluate(&observations, Some(1_000), AtimeSupport::Disabled),
            UsageEvidence::AtimeDisabled
        );
    }

    #[test]
    fn the_most_specific_mount_decides() {
        let mounts = "/dev/root / ext4 rw,relatime 0 0\n/dev/usr /usr ext4 rw,noatime 0 0\n";
        assert_eq!(atime_support(mounts, "/usr"), AtimeSupport::Disabled);
        assert_eq!(atime_support(mounts, "/var"), AtimeSupport::Relatime);
    }

    #[test]
    fn a_missing_mount_table_is_reported_as_unknown() {
        assert_eq!(atime_support("", "/usr"), AtimeSupport::Unknown);
    }

    #[test]
    fn a_prefix_that_is_not_a_path_component_does_not_match() {
        let mounts = "/dev/a /usrlocal ext4 rw,noatime 0 0\n/dev/b / ext4 rw,relatime 0 0\n";
        assert_eq!(atime_support(mounts, "/usr"), AtimeSupport::Relatime);
    }

    #[test]
    fn strictatime_is_recognised() {
        let mounts = "/dev/root / ext4 rw,strictatime 0 0\n";
        assert_eq!(atime_support(mounts, "/usr"), AtimeSupport::Strict);
    }
}
