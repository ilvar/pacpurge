//! Reclaimable space that is not a package.
//!
//! Removing packages is rarely the biggest win on an Arch system. The package
//! cache alone routinely holds tens of gigabytes of superseded `.pkg.tar.zst`
//! files, and an AUR helper's build cache holds a full git clone plus build
//! tree for every package it ever touched. This module models those targets
//! and the arithmetic behind them; [`crate::scan`] fills them in.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

/// Which cleanup a target represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// Superseded package archives under `/var/cache/pacman/pkg`.
    PacmanCacheSuperseded,
    /// Cached archives for packages that are no longer installed.
    PacmanCacheUninstalled,
    /// An AUR helper's clone and build tree cache.
    AurHelperCache,
    /// Packages installed as dependencies that nothing requires any more.
    Orphans,
    /// AUR packages whose files have not been read since they were installed.
    UnusedForeign,
    /// Debug symbol packages.
    DebugPackages,
    /// `.pacnew` and `.pacsave` configuration leftovers.
    ConfigLeftovers,
    /// Kernel module trees left behind by removed kernels.
    StaleKernelModules,
    /// Systemd coredumps.
    Coredumps,
    /// The systemd journal.
    Journal,
    /// Per-user application caches.
    UserCache,
    /// Language toolchain caches: cargo, npm, go, gradle, pip.
    DeveloperCache,
    /// The desktop trash directory.
    Trash,
    /// Unused flatpak runtimes.
    FlatpakUnused,
}

impl Kind {
    /// Stable identifier used in the JSON report.
    pub fn slug(self) -> &'static str {
        match self {
            Kind::PacmanCacheSuperseded => "pacman-cache-superseded",
            Kind::PacmanCacheUninstalled => "pacman-cache-uninstalled",
            Kind::AurHelperCache => "aur-helper-cache",
            Kind::Orphans => "orphans",
            Kind::UnusedForeign => "unused-foreign",
            Kind::DebugPackages => "debug-packages",
            Kind::ConfigLeftovers => "config-leftovers",
            Kind::StaleKernelModules => "stale-kernel-modules",
            Kind::Coredumps => "coredumps",
            Kind::Journal => "journal",
            Kind::UserCache => "user-cache",
            Kind::DeveloperCache => "developer-cache",
            Kind::Trash => "trash",
            Kind::FlatpakUnused => "flatpak-unused",
        }
    }
}

/// How much care a target deserves before acting on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Safety {
    /// Pure cache. Deleting it costs a re-download at worst.
    Safe,
    /// Recoverable, but look at the list first.
    Review,
    /// Removes something that cannot be regenerated.
    Careful,
}

impl Safety {
    /// Short label for the UI.
    pub fn label(self) -> &'static str {
        match self {
            Safety::Safe => "safe",
            Safety::Review => "review",
            Safety::Careful => "careful",
        }
    }
}

/// An external command that performs a cleanup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Command {
    /// Executable name, resolved through `PATH`.
    pub program: String,
    /// Arguments, already split.
    pub args: Vec<String>,
    /// Whether the command must run as root.
    pub needs_root: bool,
}

impl Command {
    /// Build a command.
    pub fn new(program: &str, args: &[&str], needs_root: bool) -> Command {
        Command {
            program: program.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            needs_root,
        }
    }

    /// Render the command as a copy-pasteable shell line.
    pub fn to_shell(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.needs_root {
            parts.push("sudo".to_owned());
        }
        parts.push(self.program.clone());
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }
}

/// How a target's space is reclaimed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "how", rename_all = "kebab-case")]
pub enum Reclaim {
    /// Delete these paths. Held explicitly so the UI can show them first.
    Paths {
        /// Absolute paths to remove.
        paths: Vec<PathBuf>,
        /// Whether removal needs root.
        needs_root: bool,
    },
    /// Run this command.
    Run {
        /// The command to run.
        command: Command,
    },
    /// Handled elsewhere in the UI, e.g. on the packages tab.
    Handoff {
        /// Where the user should go.
        hint: String,
    },
    /// Nothing automatic. Here is the suggestion.
    Advice {
        /// What to do by hand.
        text: String,
    },
}

/// One reclaimable thing found on the system.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Target {
    /// Which cleanup this is.
    pub kind: Kind,
    /// One-line title for the table.
    pub title: String,
    /// Where the space is.
    pub location: String,
    /// Why it is safe, or what to watch out for.
    pub detail: String,
    /// Bytes recoverable, or `None` when the size cannot be known cheaply.
    pub bytes: Option<u64>,
    /// Number of files, directories or packages involved.
    pub items: usize,
    /// How much care it deserves.
    pub safety: Safety,
    /// How to reclaim it.
    pub reclaim: Reclaim,
}

impl Target {
    /// Bytes, treating an unknown size as zero for totalling purposes.
    pub fn known_bytes(&self) -> u64 {
        self.bytes.unwrap_or(0)
    }
}

/// One archive sitting in the package cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedArchive {
    /// Absolute path of the archive.
    pub path: PathBuf,
    /// Package name parsed out of the filename.
    pub name: String,
    /// `pkgver-pkgrel` parsed out of the filename.
    pub version: String,
    /// Size on disk, signature file included.
    pub bytes: u64,
    /// Modification time, used to order versions newest first.
    pub mtime: i64,
}

/// Split a cached archive filename into `(name, version)`.
///
/// Filenames are `<pkgname>-<pkgver>-<pkgrel>-<arch>.pkg.tar.<ext>`. Neither
/// `pkgname` nor `pkgver` may contain a hyphen, so peeling the last three
/// hyphen-separated fields recovers the name. Anything that does not fit the
/// shape is left alone rather than guessed at.
pub fn parse_archive_name(file_name: &str) -> Option<(String, String)> {
    let stem = file_name
        .strip_suffix(".sig")
        .unwrap_or(file_name)
        .split(".pkg.tar")
        .next()
        .filter(|stem| *stem != file_name)?;

    let (rest, _arch) = stem.rsplit_once('-')?;
    let (rest, pkgrel) = rest.rsplit_once('-')?;
    let (name, pkgver) = rest.rsplit_once('-')?;
    if name.is_empty() {
        return None;
    }

    Some((name.to_owned(), format!("{pkgver}-{pkgrel}")))
}

/// The split of a package cache into what is worth keeping and what is not.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CachePlan {
    /// Archives for packages that are not installed at all.
    pub uninstalled: Vec<PathBuf>,
    /// Bytes held by those archives.
    pub uninstalled_bytes: u64,
    /// Archives of installed packages beyond the versions being kept.
    pub superseded: Vec<PathBuf>,
    /// Bytes held by those archives.
    pub superseded_bytes: u64,
}

/// Decide which cached archives are no longer worth keeping.
///
/// `keep` is how many versions of an *installed* package to retain, matching
/// `paccache -rk<keep>`. The currently installed version is always retained
/// regardless of `keep`, because deleting it removes the only way to roll a
/// bad upgrade back offline.
pub fn plan_cache(
    archives: &[CachedArchive],
    installed: &BTreeMap<String, String>,
    keep: usize,
) -> CachePlan {
    let mut by_package: BTreeMap<&str, Vec<&CachedArchive>> = BTreeMap::new();
    for archive in archives {
        by_package
            .entry(archive.name.as_str())
            .or_default()
            .push(archive);
    }

    let mut plan = CachePlan::default();

    for (name, mut versions) in by_package {
        // Newest first; ties broken by version string so the order is stable.
        versions.sort_by(|left, right| {
            right
                .mtime
                .cmp(&left.mtime)
                .then_with(|| right.version.cmp(&left.version))
        });

        let Some(current) = installed.get(name) else {
            for archive in versions {
                plan.uninstalled.push(archive.path.clone());
                plan.uninstalled_bytes = plan.uninstalled_bytes.saturating_add(archive.bytes);
            }
            continue;
        };

        let mut kept = 0usize;
        for archive in versions {
            let is_current = archive.version == *current;
            if is_current || kept < keep {
                if !is_current {
                    kept = kept.saturating_add(1);
                }
                continue;
            }
            plan.superseded.push(archive.path.clone());
            plan.superseded_bytes = plan.superseded_bytes.saturating_add(archive.bytes);
        }
    }

    plan.uninstalled.sort();
    plan.superseded.sort();
    plan
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::{parse_archive_name, plan_cache, CachedArchive, Command};

    fn archive(name: &str, version: &str, bytes: u64, mtime: i64) -> CachedArchive {
        CachedArchive {
            path: PathBuf::from(format!(
                "/var/cache/pacman/pkg/{name}-{version}-x86_64.pkg.tar.zst"
            )),
            name: name.to_owned(),
            version: version.to_owned(),
            bytes,
            mtime,
        }
    }

    #[test]
    fn archive_names_split_into_name_and_version() {
        assert_eq!(
            parse_archive_name("ripgrep-14.1.1-1-x86_64.pkg.tar.zst"),
            Some(("ripgrep".to_owned(), "14.1.1-1".to_owned()))
        );
        assert_eq!(
            parse_archive_name("gcc-libs-13.2.1-3-x86_64.pkg.tar.zst"),
            Some(("gcc-libs".to_owned(), "13.2.1-3".to_owned()))
        );
        assert_eq!(
            parse_archive_name("ripgrep-14.1.1-1-x86_64.pkg.tar.zst.sig"),
            Some(("ripgrep".to_owned(), "14.1.1-1".to_owned()))
        );
        assert_eq!(
            parse_archive_name("linux-firmware-20240115.2b0c4b6-1-any.pkg.tar.zst"),
            Some(("linux-firmware".to_owned(), "20240115.2b0c4b6-1".to_owned()))
        );
    }

    #[test]
    fn non_archives_are_left_alone() {
        assert_eq!(parse_archive_name("README"), None);
        assert_eq!(parse_archive_name("something.tar.zst"), None);
    }

    #[test]
    fn archives_of_removed_packages_are_all_reclaimable() {
        let archives = vec![
            archive("gone", "1-1", 500, 10),
            archive("gone", "2-1", 700, 20),
        ];
        let plan = plan_cache(&archives, &BTreeMap::new(), 3);
        assert_eq!(plan.uninstalled_bytes, 1_200);
        assert_eq!(plan.uninstalled.len(), 2);
        assert!(plan.superseded.is_empty());
    }

    #[test]
    fn the_installed_version_is_always_kept() {
        let archives = vec![
            archive("app", "1-1", 100, 10),
            archive("app", "2-1", 200, 20),
            archive("app", "3-1", 400, 30),
        ];
        let mut installed = BTreeMap::new();
        installed.insert("app".to_owned(), "1-1".to_owned());

        // keep = 0 means "only the installed version", so 2-1 and 3-1 go even
        // though they are newer on disk.
        let plan = plan_cache(&archives, &installed, 0);
        assert_eq!(plan.superseded_bytes, 600);
        assert!(plan.uninstalled.is_empty());
    }

    #[test]
    fn keep_retains_that_many_extra_versions_newest_first() {
        let archives = vec![
            archive("app", "1-1", 100, 10),
            archive("app", "2-1", 200, 20),
            archive("app", "3-1", 400, 30),
        ];
        let mut installed = BTreeMap::new();
        installed.insert("app".to_owned(), "3-1".to_owned());

        let plan = plan_cache(&archives, &installed, 1);
        // 3-1 is installed and 2-1 is the one retained version, so only 1-1 goes.
        assert_eq!(plan.superseded_bytes, 100);
    }

    #[test]
    fn commands_render_with_sudo_when_root_is_needed() {
        let command = Command::new("paccache", &["-r", "-k1"], true);
        assert_eq!(command.to_shell(), "sudo paccache -r -k1");
        let user = Command::new("paru", &["-Sc"], false);
        assert_eq!(user.to_shell(), "paru -Sc");
    }
}
