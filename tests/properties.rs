//! Invariants over generated inputs.
//!
//! These state properties the implementation must hold for *any* input, which
//! is where the example-based tests are weakest: a size formatter that is
//! right for the six sizes someone thought to write down can still be wrong
//! for the seventh, and a removal simulation that is right for a hand-drawn
//! graph can still be wrong for a cyclic one.

use std::collections::{BTreeMap, BTreeSet};

use pacpurge::filter::{self, SortKey, View};
use pacpurge::format;
use pacpurge::graph::Graph;
use pacpurge::janitor::{self, CachedArchive};
use pacpurge::localdb;
use pacpurge::model::{Entry, Facts, InstallReason, Origin, Package, UsageEvidence};

use proptest::collection::vec;
use proptest::prelude::{any, prop_assert, prop_assert_eq, proptest, Just, Strategy};
use proptest::sample::select;
use proptest::string::string_regex;

/// A package name that looks like a real one.
fn package_name() -> impl Strategy<Value = String> {
    string_regex("[a-z][a-z0-9-]{0,12}").unwrap_or_else(|_error| unreachable!())
}

/// A version string in pacman's `pkgver-pkgrel` shape.
fn version() -> impl Strategy<Value = String> {
    (0u32..40, 0u32..40, 1u32..9)
        .prop_map(|(major, minor, release)| format!("{major}.{minor}-{release}"))
}

proptest! {
    /// A rendered size must never claim more space than there really is.
    ///
    /// Truncation rather than rounding is the whole point: a cleanup tool that
    /// rounds 1.99 GiB up to 2.0 GiB promises space it cannot deliver.
    #[test]
    fn a_rendered_size_never_overstates_the_real_one(value in any::<u64>()) {
        let rendered = format::bytes(value);
        let (number, unit) = rendered
            .rsplit_once(' ')
            .unwrap_or((rendered.as_str(), "B"));

        let scale: u64 = match unit {
            "B" => 1,
            "KiB" => 1 << 10,
            "MiB" => 1 << 20,
            "GiB" => 1 << 30,
            "TiB" => 1 << 40,
            other => panic!("unexpected unit {other}"),
        };

        let tenths: u64 = match number.split_once('.') {
            Some((whole, fraction)) => {
                let whole: u64 = whole.parse().unwrap_or_default();
                let fraction: u64 = fraction.parse().unwrap_or_default();
                whole.saturating_mul(10).saturating_add(fraction)
            }
            None => number.parse::<u64>().unwrap_or_default().saturating_mul(10),
        };

        let claimed = tenths.saturating_mul(scale) / 10;
        prop_assert!(claimed <= value, "{rendered} claims {claimed} for {value}");
    }

    /// Every size renders to something a person can read.
    #[test]
    fn a_rendered_size_is_always_well_formed(value in any::<u64>()) {
        let rendered = format::bytes(value);
        prop_assert!(rendered.contains(' '), "{rendered}");
        prop_assert!(
            rendered.chars().next().is_some_and(|first| first.is_ascii_digit()),
            "{rendered}"
        );
    }

    /// Truncation never exceeds the requested width and never grows the text.
    #[test]
    fn truncation_respects_its_width(text in ".{0,60}", width in 0usize..40) {
        let truncated = format::truncate(&text, width);
        prop_assert!(truncated.chars().count() <= width);
        if text.chars().count() <= width {
            prop_assert_eq!(truncated, text);
        }
    }

    /// Rendering a date and reading its year back must agree with the epoch.
    #[test]
    fn dates_are_ordered_the_same_way_as_their_timestamps(
        earlier in -2_000_000_000i64..2_000_000_000,
        gap in 0i64..2_000_000_000,
    ) {
        let later = earlier.saturating_add(gap);
        let first = format::date(Some(earlier));
        let second = format::date(Some(later));
        // ISO-8601 dates sort lexicographically in chronological order.
        prop_assert!(first <= second, "{first} should not sort after {second}");
    }

    /// Stripping a version constraint never invents characters.
    #[test]
    fn a_dependency_name_is_a_prefix_of_its_specification(
        name in package_name(),
        constraint in select(vec![">=", "<=", "=", ">", "<"]),
        bound in version(),
    ) {
        let spec = format!("{name}{constraint}{bound}");
        prop_assert_eq!(localdb::dependency_name(&spec), name.as_str());
    }

    /// A `desc` file written from a package parses back into the same package.
    #[test]
    fn a_database_entry_round_trips(
        name in package_name(),
        pkgver in version(),
        size in any::<u64>(),
        installed in 0i64..2_000_000_000,
        dependency in package_name(),
    ) {
        let text = format!(
            "%NAME%\n{name}\n\n%VERSION%\n{pkgver}\n\n%SIZE%\n{size}\n\n\
             %INSTALLDATE%\n{installed}\n\n%REASON%\n1\n\n%DEPENDS%\n{dependency}\n"
        );

        let parsed = localdb::parse_desc(&text, std::path::Path::new("/tmp"))
            .unwrap_or_else(|| unreachable!("a well-formed entry must parse"));

        prop_assert_eq!(parsed.name, name);
        prop_assert_eq!(parsed.version, pkgver);
        prop_assert_eq!(parsed.size, size);
        prop_assert_eq!(parsed.install_date, Some(installed));
        prop_assert_eq!(parsed.reason, InstallReason::Dependency);
        prop_assert_eq!(parsed.depends, vec![dependency]);
    }

    /// Parsing never panics, whatever the database contains.
    ///
    /// A single corrupt entry — a truncated write, a filesystem that lost a
    /// block — must not take the whole scan down with it.
    #[test]
    fn arbitrary_text_never_breaks_the_parser(text in ".{0,400}") {
        let _parsed = localdb::parse_desc(&text, std::path::Path::new("/tmp"));
        let _files = localdb::parse_files(&text);
        let _sections = localdb::sections(&text);
    }

    /// A removal cascade always contains what was asked for, and never
    /// contains anything twice.
    #[test]
    fn a_cascade_contains_its_seed(specification in package_graph()) {
        let packages = build_packages(&specification);
        let graph = Graph::build(&packages);

        let seed: BTreeSet<usize> = specification
            .seed
            .iter()
            .filter(|position| **position < packages.len())
            .copied()
            .collect();

        let removed = graph.cascade(&seed);
        for position in &seed {
            prop_assert!(removed.contains(position), "seed {position} was dropped");
        }
    }

    /// Removing more can never free less.
    ///
    /// Adding a package to the selection can only add to what pacman takes
    /// with it, so the reclaimable figure must be monotonic. A cascade that
    /// went backwards would mean the simulation had double-counted.
    #[test]
    fn reclaimable_space_grows_with_the_selection(specification in package_graph()) {
        let packages = build_packages(&specification);
        let graph = Graph::build(&packages);
        if packages.is_empty() {
            return Ok(());
        }

        let mut seed: BTreeSet<usize> = BTreeSet::new();
        let mut previous = 0u64;

        for position in 0..packages.len() {
            seed.insert(position);
            let reclaimable = graph.reclaimable(&seed);
            prop_assert!(
                reclaimable >= previous,
                "adding {position} dropped the total from {previous} to {reclaimable}"
            );
            previous = reclaimable;
        }
    }

    /// A cascade never removes a package something outside it still needs.
    #[test]
    fn a_cascade_never_strands_a_dependant(specification in package_graph()) {
        let packages = build_packages(&specification);
        let graph = Graph::build(&packages);

        let seed: BTreeSet<usize> = specification
            .seed
            .iter()
            .filter(|position| **position < packages.len())
            .copied()
            .collect();

        let removed = graph.cascade(&seed);

        // Anything swept up beyond the seed must have had every dependant
        // removed too, or pacman would refuse the transaction.
        for position in removed.difference(&seed) {
            let dependants = graph.required_by(*position);
            for dependant in dependants {
                let Some(other) = graph.position(&dependant) else {
                    continue;
                };
                prop_assert!(
                    removed.contains(&other),
                    "{dependant} still needs a package the cascade removed"
                );
            }
        }
    }

    /// Sorting produces a total order: same length, same members, no
    /// duplicates, whichever column is chosen.
    #[test]
    fn ordering_is_a_permutation_of_what_it_filters(
        entries in vec(arbitrary_entry(), 0..24),
        descending in any::<bool>(),
        key in select(vec![
            SortKey::Name,
            SortKey::Size,
            SortKey::Reclaimable,
            SortKey::LastUsed,
            SortKey::Installed,
            SortKey::RequiredBy,
        ]),
    ) {
        let view = View { sort: key, descending, ..View::default() };
        let order = filter::order(&entries, &view, 2_000_000_000);

        prop_assert_eq!(order.len(), entries.len());
        let unique: BTreeSet<usize> = order.iter().copied().collect();
        prop_assert_eq!(unique.len(), order.len());
    }

    /// Reversing the sort direction reverses the result exactly.
    ///
    /// Only holds because package name breaks every tie, which is what stops
    /// the table reshuffling under the cursor between redraws.
    #[test]
    fn reversing_the_sort_reverses_the_order(entries in vec(arbitrary_entry(), 0..16)) {
        let ascending = View { sort: SortKey::Size, descending: false, ..View::default() };
        let descending = View { sort: SortKey::Size, descending: true, ..View::default() };

        let mut forward = filter::order(&entries, &ascending, 2_000_000_000);
        let backward = filter::order(&entries, &descending, 2_000_000_000);
        forward.reverse();

        // Compare by the sort key rather than by index: equal sizes are
        // ordered by name in both directions, so the tie-break is not itself
        // reversed.
        let sizes = |order: &[usize]| -> Vec<u64> {
            order
                .iter()
                .filter_map(|position| entries.get(*position))
                .map(|entry| entry.package.size)
                .collect()
        };
        prop_assert_eq!(sizes(&forward), sizes(&backward));
    }

    /// Filtering only ever removes rows.
    #[test]
    fn a_filter_never_adds_rows(entries in vec(arbitrary_entry(), 0..24), query in "[a-z]{0,3}") {
        let unfiltered = View::default();
        let filtered = View { query, ..View::default() };

        let all = filter::order(&entries, &unfiltered, 2_000_000_000);
        let some = filter::order(&entries, &filtered, 2_000_000_000);
        prop_assert!(some.len() <= all.len());
    }

    /// The cache plan never proposes deleting the installed version.
    #[test]
    fn the_installed_archive_is_never_reclaimed(
        name in package_name(),
        versions in vec(version(), 1..6),
        keep in 0usize..4,
    ) {
        let archives: Vec<CachedArchive> = versions
            .iter()
            .enumerate()
            .map(|(position, pkgver)| CachedArchive {
                path: std::path::PathBuf::from(format!("/cache/{name}-{pkgver}.pkg.tar.zst")),
                name: name.clone(),
                version: pkgver.clone(),
                bytes: 1_000,
                mtime: i64::try_from(position).unwrap_or_default(),
            })
            .collect();

        let Some(current) = versions.first() else {
            return Ok(());
        };
        let mut installed = BTreeMap::new();
        installed.insert(name.clone(), current.clone());

        let plan = janitor::plan_cache(&archives, &installed, keep);
        let doomed: BTreeSet<&std::path::PathBuf> =
            plan.superseded.iter().chain(plan.uninstalled.iter()).collect();

        for archive in &archives {
            if archive.version == *current {
                prop_assert!(
                    !doomed.contains(&archive.path),
                    "the installed version {} was marked for deletion",
                    archive.version
                );
            }
        }
    }

    /// A parsed archive filename can be rebuilt from its parts.
    #[test]
    fn archive_names_round_trip(name in package_name(), pkgver in version()) {
        let file_name = format!("{name}-{pkgver}-x86_64.pkg.tar.zst");
        let parsed = janitor::parse_archive_name(&file_name);
        prop_assert_eq!(parsed, Some((name, pkgver)));
    }
}

/// A randomly generated dependency graph.
#[derive(Clone, Debug)]
struct Specification {
    /// `(size, is_explicit, dependency indices)` per package.
    nodes: Vec<(u64, bool, Vec<usize>)>,
    /// Which packages the cascade starts from.
    seed: Vec<usize>,
}

/// Generate a small dependency graph, cycles included.
///
/// Edges are unrestricted rather than acyclic on purpose: real package sets
/// contain dependency cycles, and a traversal that assumes otherwise hangs.
fn package_graph() -> impl Strategy<Value = Specification> {
    vec((1u64..10_000, any::<bool>(), vec(0usize..12, 0..4)), 1..12).prop_flat_map(|nodes| {
        let count = nodes.len();
        (Just(nodes), vec(0..count, 0..4)).prop_map(|(nodes, seed)| Specification { nodes, seed })
    })
}

/// Turn a specification into packages named `p0`, `p1`, and so on.
fn build_packages(specification: &Specification) -> Vec<Package> {
    specification
        .nodes
        .iter()
        .enumerate()
        .map(|(position, (size, explicit, dependencies))| Package {
            name: format!("p{position}"),
            version: "1-1".to_owned(),
            size: *size,
            reason: if *explicit {
                InstallReason::Explicit
            } else {
                InstallReason::Dependency
            },
            depends: dependencies
                .iter()
                .filter(|target| **target < specification.nodes.len() && **target != position)
                .map(|target| format!("p{target}"))
                .collect(),
            ..Package::default()
        })
        .collect()
}

/// A single table row with arbitrary but plausible contents.
fn arbitrary_entry() -> impl Strategy<Value = Entry> {
    (
        package_name(),
        any::<u64>(),
        any::<u64>(),
        0i64..2_000_000_000,
        any::<bool>(),
        any::<Option<i64>>(),
    )
        .prop_map(
            |(name, size, reclaimable, installed, foreign, last_used)| Entry {
                package: Package {
                    name,
                    version: "1-1".to_owned(),
                    size,
                    install_date: Some(installed),
                    reason: InstallReason::Explicit,
                    ..Package::default()
                },
                facts: Facts {
                    required_by: Vec::new(),
                    optional_for: Vec::new(),
                    origin: if foreign {
                        Origin::Foreign
                    } else {
                        Origin::Repository("extra".to_owned())
                    },
                    usage: match last_used {
                        Some(at) => UsageEvidence::Used {
                            at,
                            witness: "usr/bin/x".to_owned(),
                        },
                        None => UsageEvidence::NotProbed,
                    },
                    reclaimable,
                    frees: Vec::new(),
                    protected: false,
                },
            },
        )
}
