//! End-to-end tests against a synthetic pacman root.
//!
//! The point of these is that the analysis is exercised the way a user
//! exercises it — through a real filesystem, a real `desc` parse and a real
//! `stat` — without needing an Arch system to run the suite on. The fixture
//! writes actual files and sets actual access times, so the last-use logic is
//! tested against the kernel rather than against a mock.

use std::collections::BTreeMap;
use std::fs::{self, File, FileTimes};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use pacpurge::filter::{SortKey, Toggle, View};
use pacpurge::model::{Origin, UsageEvidence};
use pacpurge::report;
use pacpurge::scan::{self, Config};

/// Seconds in a day.
const DAY: u64 = 86_400;

/// Build a throwaway root under `target/` and return its path.
///
/// Deliberately not a temporary directory: when an assertion fails, the tree
/// is still there to look at.
fn fixture_root(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-roots")
        .join(name);
    if root.exists() {
        fs::remove_dir_all(&root).expect("could not clear the previous fixture root");
    }
    fs::create_dir_all(&root).expect("could not create the fixture root");
    root
}

/// Timestamp `days` in the past.
fn days_ago(days: u64) -> SystemTime {
    SystemTime::now()
        .checked_sub(Duration::from_secs(days.saturating_mul(DAY)))
        .expect("clock is before the epoch")
}

/// Unix seconds for a timestamp.
fn unix(time: SystemTime) -> i64 {
    let seconds = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("time is before the epoch")
        .as_secs();
    i64::try_from(seconds).expect("timestamp does not fit in i64")
}

/// One package to write into the fixture database.
struct Fixture {
    name: &'static str,
    version: &'static str,
    size: u64,
    /// Days ago the package was installed.
    installed: u64,
    /// `1` for a dependency, `0` for an explicit install.
    reason: &'static str,
    depends: &'static [&'static str],
    /// Files the package owns, relative to the root.
    files: &'static [&'static str],
    /// Days ago the package's files were last read, when it was read at all.
    last_read: Option<u64>,
}

/// Write a package into the fixture's local database and create its files.
fn install(root: &Path, fixture: &Fixture) {
    let entry = root
        .join("var/lib/pacman/local")
        .join(format!("{}-{}", fixture.name, fixture.version));
    fs::create_dir_all(&entry).expect("could not create the database entry");

    let installed_at = unix(days_ago(fixture.installed));
    let depends = fixture.depends.join("\n");

    let desc = format!(
        "%NAME%\n{}\n\n%VERSION%\n{}\n\n%DESC%\nfixture package {}\n\n%SIZE%\n{}\n\n\
         %INSTALLDATE%\n{}\n\n%REASON%\n{}\n\n%DEPENDS%\n{}\n",
        fixture.name,
        fixture.version,
        fixture.name,
        fixture.size,
        installed_at,
        fixture.reason,
        depends
    );
    fs::write(entry.join("desc"), desc).expect("could not write desc");

    let listing: String = fixture
        .files
        .iter()
        .map(|path| format!("{path}\n"))
        .collect();
    fs::write(entry.join("files"), format!("%FILES%\n{listing}")).expect("could not write files");

    // The access time a package's files carry is the whole basis of the
    // last-use column, so the fixture sets it explicitly: unread packages get
    // an atime matching their install date, which is what extraction leaves
    // behind on a real system.
    let read_at = days_ago(fixture.last_read.unwrap_or(fixture.installed));

    for relative in fixture.files {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("could not create a package directory");
        }
        fs::write(&path, vec![0u8; 64]).expect("could not write a package file");

        let handle = File::options()
            .write(true)
            .open(&path)
            .expect("could not reopen a package file");
        handle
            .set_times(
                FileTimes::new()
                    .set_accessed(read_at)
                    .set_modified(days_ago(fixture.installed)),
            )
            .expect("could not set the access time");
    }
}

/// The packages every test shares.
const PACKAGES: [Fixture; 5] = [
    Fixture {
        name: "daily-editor",
        version: "3.1-1",
        size: 40_000_000,
        installed: 400,
        reason: "0",
        depends: &["shared-lib"],
        files: &["usr/bin/daily-editor"],
        last_read: Some(1),
    },
    Fixture {
        name: "abandoned-toy",
        version: "0.2-1",
        size: 300_000_000,
        installed: 500,
        reason: "0",
        depends: &["lonely-lib"],
        files: &["usr/bin/abandoned-toy", "usr/share/man/man1/toy.1"],
        last_read: None,
    },
    Fixture {
        name: "shared-lib",
        version: "1.0-1",
        size: 5_000_000,
        installed: 400,
        reason: "1",
        depends: &[],
        files: &["usr/lib/libshared.so.1"],
        last_read: Some(2),
    },
    Fixture {
        name: "lonely-lib",
        version: "2.0-1",
        size: 90_000_000,
        installed: 500,
        reason: "1",
        depends: &[],
        files: &["usr/lib/liblonely.so.2"],
        last_read: None,
    },
    Fixture {
        name: "true-orphan",
        version: "1.4-2",
        size: 12_000_000,
        installed: 300,
        reason: "1",
        depends: &[],
        files: &["usr/lib/liborphan.so"],
        last_read: None,
    },
];

/// Build the shared fixture root.
fn build_root(name: &str) -> PathBuf {
    let root = fixture_root(name);
    for fixture in &PACKAGES {
        install(&root, fixture);
    }
    root
}

/// A scan configuration pointed at a fixture root.
fn config(root: &Path) -> Config {
    Config {
        root: root.to_path_buf(),
        db_path: root.join("var/lib/pacman"),
        cache_dirs: vec![root.join("var/cache/pacman/pkg")],
        home: None,
        ..Config::default()
    }
}

#[test]
fn a_scan_reads_every_package_and_its_relationships() {
    let root = build_root("relationships");
    let inventory = scan::scan(&config(&root)).expect("the scan should succeed");

    assert_eq!(inventory.entries.len(), 5);

    let shared = inventory
        .get("shared-lib")
        .expect("shared-lib is installed");
    assert_eq!(shared.facts.required_by, vec!["daily-editor".to_owned()]);
    assert!(!shared.facts.is_orphan());

    let orphan = inventory
        .get("true-orphan")
        .expect("true-orphan is installed");
    assert!(orphan.facts.is_orphan());
    assert_eq!(orphan.package.size, 12_000_000);
}

#[test]
fn removing_a_package_reports_the_dependency_it_drags_along() {
    let root = build_root("cascade");
    let inventory = scan::scan(&config(&root)).expect("the scan should succeed");

    let toy = inventory
        .get("abandoned-toy")
        .expect("abandoned-toy is installed");
    // Its own 300 MB plus lonely-lib's 90 MB, which nothing else needs.
    assert_eq!(toy.facts.reclaimable, 390_000_000);
    assert_eq!(toy.facts.frees, vec!["lonely-lib".to_owned()]);

    let editor = inventory
        .get("daily-editor")
        .expect("daily-editor is installed");
    // shared-lib is only needed by daily-editor, so it comes too.
    assert_eq!(editor.facts.reclaimable, 45_000_000);
}

#[test]
fn access_times_separate_used_packages_from_forgotten_ones() {
    let root = build_root("usage");
    let inventory = scan::scan(&config(&root)).expect("the scan should succeed");

    if !inventory.atime_support.is_meaningful() {
        // The build machine mounts noatime; the verdict is correctly withheld.
        return;
    }

    let editor = inventory
        .get("daily-editor")
        .expect("daily-editor is installed");
    match &editor.facts.usage {
        UsageEvidence::Used { at: _, witness } => {
            assert_eq!(witness, "usr/bin/daily-editor");
        }
        other => panic!("expected daily-editor to look used, got {other:?}"),
    }

    let toy = inventory
        .get("abandoned-toy")
        .expect("abandoned-toy is installed");
    assert!(
        toy.facts.usage.is_unused(),
        "expected abandoned-toy to look unused, got {:?}",
        toy.facts.usage
    );
}

#[test]
fn a_manual_page_is_never_taken_as_evidence_of_use() {
    let root = fixture_root("witness-choice");
    install(
        &root,
        &Fixture {
            name: "docs-only",
            version: "1-1",
            size: 1_000,
            installed: 100,
            reason: "0",
            depends: &[],
            // The manual page was read yesterday; the binary never was.
            files: &["usr/share/man/man1/docs-only.1"],
            last_read: Some(1),
        },
    );

    let inventory = scan::scan(&config(&root)).expect("the scan should succeed");
    let entry = inventory.get("docs-only").expect("docs-only is installed");
    assert_eq!(entry.facts.usage, UsageEvidence::NoWitness);
}

#[test]
fn the_package_cache_is_split_into_superseded_and_removed() {
    let root = build_root("cache");
    let cache = root.join("var/cache/pacman/pkg");
    fs::create_dir_all(&cache).expect("could not create the cache directory");

    for name in [
        // The installed version, and one older build of it.
        "daily-editor-3.1-1-x86_64.pkg.tar.zst",
        "daily-editor-3.0-1-x86_64.pkg.tar.zst",
        "daily-editor-2.9-1-x86_64.pkg.tar.zst",
        // A package that is not installed at all.
        "long-gone-1.0-1-x86_64.pkg.tar.zst",
    ] {
        fs::write(cache.join(name), vec![0u8; 200_000]).expect("could not write a cache archive");
    }

    let inventory = scan::scan(&config(&root)).expect("the scan should succeed");

    let uninstalled = inventory
        .targets
        .iter()
        .find(|target| target.kind == pacpurge::janitor::Kind::PacmanCacheUninstalled)
        .expect("the uninstalled-archive target should be present");
    assert_eq!(uninstalled.items, 1);

    let superseded = inventory
        .targets
        .iter()
        .find(|target| target.kind == pacpurge::janitor::Kind::PacmanCacheSuperseded)
        .expect("the superseded-archive target should be present");
    // Three builds are cached: the installed one is kept, one rollback is
    // kept, and the oldest is reclaimable.
    assert_eq!(superseded.items, 1);
}

#[test]
fn a_kernel_module_tree_without_its_package_is_reported() {
    let root = build_root("kernels");
    let stale = root.join("usr/lib/modules/6.1.0-old");
    fs::create_dir_all(&stale).expect("could not create the module directory");
    fs::write(stale.join("pkgbase"), "linux-old\n").expect("could not write pkgbase");
    fs::write(stale.join("vmlinuz"), vec![0u8; 100_000]).expect("could not write a module file");

    let inventory = scan::scan(&config(&root)).expect("the scan should succeed");
    let target = inventory
        .targets
        .iter()
        .find(|target| target.kind == pacpurge::janitor::Kind::StaleKernelModules)
        .expect("the stale-module target should be present");
    assert_eq!(target.items, 1);
}

#[test]
fn pacnew_files_are_found_and_flagged_as_needing_care() {
    let root = build_root("pacnew");
    let etc = root.join("etc");
    fs::create_dir_all(etc.join("ssh")).expect("could not create /etc/ssh");
    fs::write(etc.join("ssh/sshd_config.pacnew"), b"# new config\n")
        .expect("could not write the pacnew file");

    let inventory = scan::scan(&config(&root)).expect("the scan should succeed");
    let target = inventory
        .targets
        .iter()
        .find(|target| target.kind == pacpurge::janitor::Kind::ConfigLeftovers)
        .expect("the config-leftover target should be present");
    assert_eq!(target.items, 1);
    assert_eq!(target.safety, pacpurge::janitor::Safety::Careful);
}

#[test]
fn a_missing_database_fails_with_an_actionable_message() {
    let root = fixture_root("no-database");
    let error = scan::scan(&config(&root)).expect_err("an empty root has no database");
    let message = error.to_string();
    assert!(message.contains("--root"), "message was: {message}");
}

#[test]
fn without_pacman_the_repository_is_left_unknown_and_said_so() {
    let root = build_root("origins");
    let inventory = scan::scan(&config(&root)).expect("the scan should succeed");

    // This suite does not run on Arch, so `pacman -Sl` is unavailable and the
    // honest answer is that repository membership is unknown.
    if pacpurge::capability::has_program("pacman") {
        return;
    }

    let entry = inventory
        .get("daily-editor")
        .expect("daily-editor is installed");
    assert_eq!(entry.facts.origin, Origin::Unknown);
    assert!(
        inventory
            .warnings
            .iter()
            .any(|warning| warning.contains("pacman is not on PATH")),
        "warnings were: {:?}",
        inventory.warnings
    );
}

#[test]
fn the_json_report_round_trips_through_serde() {
    let root = build_root("json");
    let inventory = scan::scan(&config(&root)).expect("the scan should succeed");
    let rendered = report::json(&inventory).expect("the report should encode");
    let parsed: serde_json::Value =
        serde_json::from_str(&rendered).expect("the report should be valid JSON");

    assert_eq!(parsed["summary"]["packages"], 5);
    assert_eq!(parsed["summary"]["orphans"], 1);
    assert!(parsed["inventory"]["entries"].is_array());
}

#[test]
fn the_text_listing_orders_by_what_removal_actually_frees() {
    let root = build_root("listing");
    let inventory = scan::scan(&config(&root)).expect("the scan should succeed");

    let view = View {
        sort: SortKey::Reclaimable,
        descending: true,
        ..View::default()
    };
    let rendered = report::list(&inventory, &view, 10);
    let first_row = rendered.lines().nth(1).unwrap_or_default();

    // abandoned-toy is not the largest package by installed size once
    // lonely-lib is counted, and sorting by `frees` should surface it first.
    assert!(
        first_row.starts_with("abandoned-toy"),
        "first row was: {first_row}"
    );
}

#[test]
fn the_orphan_filter_finds_exactly_the_orphaned_dependency() {
    let root = build_root("filter");
    let inventory = scan::scan(&config(&root)).expect("the scan should succeed");

    let mut view = View::default();
    view.toggle(Toggle::Orphans);
    let names: Vec<String> =
        pacpurge::filter::order(&inventory.entries, &view, inventory.scanned_at)
            .into_iter()
            .filter_map(|position| inventory.entries.get(position))
            .map(|entry| entry.package.name.clone())
            .collect();

    assert_eq!(names, vec!["true-orphan".to_owned()]);
}

#[test]
fn every_package_name_resolves_through_the_index() {
    let root = build_root("index");
    let inventory = scan::scan(&config(&root)).expect("the scan should succeed");

    let by_name: BTreeMap<&str, u64> = inventory
        .entries
        .iter()
        .map(|entry| (entry.package.name.as_str(), entry.package.size))
        .collect();

    for (name, size) in by_name {
        let entry = inventory.get(name).expect("the index should resolve");
        assert_eq!(entry.package.size, size);
    }
}
