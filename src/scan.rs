//! Collecting the system's state into an [`Inventory`].
//!
//! This is the only module that both performs effects (through
//! [`crate::capability`]) and knows about the analysis passes. Everything it
//! learns is handed downstream as plain data, so the UI never touches a disk.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::capability;
use crate::graph::Graph;
use crate::janitor::{CachedArchive, Command, Kind, Reclaim, Safety, Target};
use crate::localdb;
use crate::model::{AtimeSupport, Entry, Facts, Inventory, Origin, Package, UsageEvidence};
use crate::usage;

/// Why a scan could not be completed.
#[derive(Debug)]
pub enum Error {
    /// The local package database is missing or unreadable.
    NoDatabase {
        /// Path that was tried.
        path: PathBuf,
        /// Underlying reason.
        reason: String,
    },
    /// The database exists but contains no usable entries.
    EmptyDatabase {
        /// Path that was read.
        path: PathBuf,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoDatabase { path, reason } => write!(
                formatter,
                "cannot read the pacman database at {}: {reason}\n\
                 pacpurge needs an Arch-style local database; pass --root to point at one",
                path.display()
            ),
            Error::EmptyDatabase { path } => write!(
                formatter,
                "no packages found in {}; the database looks empty",
                path.display()
            ),
        }
    }
}

/// What to scan and how hard to look.
#[derive(Clone, Debug)]
pub struct Config {
    /// Filesystem root, normally `/`. Overridable so the scan can be tested.
    pub root: PathBuf,
    /// Pacman database directory, normally `<root>/var/lib/pacman`.
    pub db_path: PathBuf,
    /// Package cache directories.
    pub cache_dirs: Vec<PathBuf>,
    /// Home directory whose caches should be measured.
    pub home: Option<PathBuf>,
    /// How many of the largest packages to probe for access times.
    ///
    /// Zero means every package, which is the default: probing a two thousand
    /// package system costs a third of a second, and a bounded probe leaves
    /// most of the table with no verdict at all.
    pub probe_top: usize,
    /// How many files to stat per package.
    pub witness_budget: usize,
    /// Whether to probe access times at all.
    pub probe_usage: bool,
    /// Whether to run directory-size walks, which cost real time on spinning
    /// disks and huge home directories.
    pub measure_directories: bool,
    /// Maximum directory entries any single walk may visit.
    pub walk_budget: usize,
    /// Maximum entries to visit when dating one package's home state.
    pub home_budget: usize,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            root: PathBuf::from("/"),
            db_path: PathBuf::from("/var/lib/pacman"),
            cache_dirs: vec![PathBuf::from("/var/cache/pacman/pkg")],
            home: None,
            probe_top: 0,
            witness_budget: 24,
            probe_usage: true,
            measure_directories: true,
            walk_budget: 400_000,
            home_budget: 400,
        }
    }
}

impl Config {
    /// Derive the default paths beneath `root`, honouring `/etc/pacman.conf`.
    pub fn for_root(root: &Path) -> Config {
        // Per-user caches belong to the running user, so they are only
        // measured when the scan is of the running system. Pointing `--root`
        // at a mounted disk and then reporting this machine's `~/.cache` would
        // be a lie about where the space is.
        let home = if root == Path::new("/") {
            capability::home_dir()
        } else {
            None
        };

        let mut config = Config {
            root: root.to_path_buf(),
            db_path: root.join("var/lib/pacman"),
            cache_dirs: vec![root.join("var/cache/pacman/pkg")],
            home,
            ..Config::default()
        };

        if let Ok(text) = capability::read_text(&root.join("etc/pacman.conf")) {
            let (db_path, cache_dirs) = pacman_conf_paths(&text, root);
            if let Some(db_path) = db_path {
                config.db_path = db_path;
            }
            if !cache_dirs.is_empty() {
                config.cache_dirs = cache_dirs;
            }
        }

        config
    }
}

/// Extract `DBPath` and `CacheDir` from a `pacman.conf`.
///
/// Only the `[options]` section carries these, and both are absolute paths in
/// the file, so they are re-anchored under `root` to keep `--root` working.
pub fn pacman_conf_paths(text: &str, root: &Path) -> (Option<PathBuf>, Vec<PathBuf>) {
    let mut db_path = None;
    let mut cache_dirs = Vec::new();
    let mut in_options = false;

    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            in_options = line == "[options]";
            continue;
        }
        if !in_options {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }

        match key.trim() {
            "DBPath" => db_path = Some(reanchor(value, root)),
            "CacheDir" => {
                for entry in value.split_whitespace() {
                    cache_dirs.push(reanchor(entry, root));
                }
            }
            _ => {}
        }
    }

    (db_path, cache_dirs)
}

/// Re-anchor an absolute path from `pacman.conf` under the scan root.
fn reanchor(value: &str, root: &Path) -> PathBuf {
    let relative = value.trim_start_matches('/');
    root.join(relative)
}

/// Read the system and produce a complete inventory.
pub fn scan(config: &Config) -> Result<Inventory, Error> {
    let mut warnings = Vec::new();
    let local = config.db_path.join("local");

    let entries = capability::list_dir(&local).map_err(|error| Error::NoDatabase {
        path: local.clone(),
        reason: error.to_string(),
    })?;

    let mut packages: Vec<Package> = Vec::new();
    for entry in entries {
        if !entry.is_dir {
            continue;
        }
        let desc = entry.path.join("desc");
        let Ok(text) = capability::read_text(&desc) else {
            continue;
        };
        match localdb::parse_desc(&text, &entry.path) {
            Some(package) => packages.push(package),
            None => warnings.push(format!("skipped unreadable database entry {}", entry.name)),
        }
    }

    if packages.is_empty() {
        return Err(Error::EmptyDatabase { path: local });
    }

    packages.sort_by(|left, right| left.name.cmp(&right.name));

    let graph = Graph::build(&packages);
    let origins = resolve_origins(&packages, &mut warnings);
    let now = capability::now();

    let atime_support = if config.probe_usage {
        detect_atime_support(config)
    } else {
        AtimeSupport::Unknown
    };

    let probe_list = choose_probe_targets(&packages, &graph, &origins, config);
    let index = config
        .home
        .as_ref()
        .map(|home| home_index(home))
        .unwrap_or_default();

    let mut usages: BTreeMap<usize, UsageEvidence> = BTreeMap::new();
    if config.probe_usage {
        for position in &probe_list {
            let Some(package) = packages.get(*position) else {
                continue;
            };
            usages.insert(
                *position,
                probe_package(package, &index, config, atime_support),
            );
        }
    }

    if !atime_support.is_meaningful() {
        let recovered = usages
            .values()
            .filter(|evidence| evidence.is_used())
            .count();
        warnings.push(format!(
            "access times are frozen on the filesystem holding /usr (noatime), so most packages \
             cannot be dated. {recovered} were dated from what they wrote under your home \
             directory instead. Mounting with relatime would date the rest."
        ));
    }

    let mut inventory_entries: Vec<Entry> = Vec::new();
    let mut index = BTreeMap::new();

    for (position, package) in packages.iter().enumerate() {
        let mut seed = BTreeSet::new();
        seed.insert(position);

        let facts = Facts {
            required_by: graph.required_by(position),
            optional_for: graph.optional_for(position),
            origin: origins
                .get(&package.name)
                .cloned()
                .unwrap_or(Origin::Unknown),
            usage: usages
                .get(&position)
                .cloned()
                .unwrap_or(UsageEvidence::NotProbed),
            reclaimable: graph.reclaimable(&seed),
            frees: graph.dragged_along(position),
            protected: graph.is_protected(position),
        };

        index.insert(package.name.clone(), position);
        inventory_entries.push(Entry {
            package: package.clone(),
            facts,
        });
    }

    let targets = collect_targets(&inventory_entries, config, &mut warnings);

    Ok(Inventory {
        entries: inventory_entries,
        index,
        targets,
        atime_support,
        scanned_at: now,
        probed: usages.len(),
        warnings,
    })
}

/// Ask pacman which repository each package came from.
///
/// `pacman -Sl` lists every package in every configured sync repository in one
/// pass, which is both the authoritative answer and cheaper than one query per
/// package. Anything absent from that list is foreign: an AUR build or a
/// locally built package. Without pacman on `PATH` the question is left open
/// rather than guessed at.
fn resolve_origins(packages: &[Package], warnings: &mut Vec<String>) -> BTreeMap<String, Origin> {
    let mut origins = BTreeMap::new();

    if !capability::has_program("pacman") {
        warnings.push(
            "pacman is not on PATH, so repository membership and AUR detection are unavailable"
                .to_owned(),
        );
        return origins;
    }

    let listing = match capability::run_captured("pacman", &["-Sl".to_owned()]) {
        Ok(output) if output.ok() => output.stdout,
        Ok(output) => {
            warnings.push(format!(
                "pacman -Sl failed: {}",
                output.stderr.trim().lines().next().unwrap_or("no detail")
            ));
            return origins;
        }
        Err(error) => {
            warnings.push(format!("could not run pacman -Sl: {error}"));
            return origins;
        }
    };

    for line in listing.lines() {
        let mut fields = line.split_whitespace();
        let (Some(repository), Some(name)) = (fields.next(), fields.next()) else {
            continue;
        };
        origins.insert(name.to_owned(), Origin::Repository(repository.to_owned()));
    }

    for package in packages {
        origins
            .entry(package.name.clone())
            .or_insert(Origin::Foreign);
    }

    origins
}

/// Decide how far to trust access times, from the mount table.
fn detect_atime_support(config: &Config) -> AtimeSupport {
    match capability::read_text(Path::new("/proc/mounts")) {
        Ok(mounts) => {
            let probe = config.root.join("usr");
            usage::atime_support(&mounts, &probe.to_string_lossy())
        }
        Err(_error) => AtimeSupport::Unknown,
    }
}

/// Pick which packages are worth the cost of an access-time probe.
///
/// Everything, unless `probe_top` bounds it. When it does, the largest
/// packages are the point of the exercise, but foreign packages and orphans
/// are included regardless of size: an unused 12 MiB AUR build is a better
/// removal candidate than a 400 MiB toolchain in daily use.
fn choose_probe_targets(
    packages: &[Package],
    graph: &Graph<'_>,
    origins: &BTreeMap<String, Origin>,
    config: &Config,
) -> Vec<usize> {
    if config.probe_top == 0 {
        return (0..packages.len()).collect();
    }

    let mut by_size: Vec<usize> = (0..packages.len()).collect();
    by_size.sort_by_key(|position| {
        std::cmp::Reverse(
            packages
                .get(*position)
                .map(|package| package.size)
                .unwrap_or(0),
        )
    });

    let mut chosen: BTreeSet<usize> = by_size.iter().copied().take(config.probe_top).collect();

    for (position, package) in packages.iter().enumerate() {
        let foreign = origins
            .get(&package.name)
            .map(Origin::is_foreign)
            .unwrap_or(false);
        let orphan = package.reason == crate::model::InstallReason::Dependency
            && graph.required_by(position).is_empty();
        if foreign || orphan {
            chosen.insert(position);
        }
    }

    // Each probe reads a package's file list and stats up to `witness_budget`
    // of them. On a machine with a few thousand AUR packages that would turn
    // an interactive scan into a coffee break, so the extras are capped and
    // the largest kept.
    let cap = config.probe_top.saturating_mul(4).max(400);
    if chosen.len() > cap {
        chosen = by_size
            .into_iter()
            .filter(|position| chosen.contains(position))
            .take(cap)
            .collect();
    }

    chosen.into_iter().collect()
}

/// An index of the per-user state directories present under `$HOME`.
///
/// Built once and shared across every package, so establishing whether a
/// package has home state is a map lookup rather than a stat storm.
type HomeIndex = BTreeMap<String, Vec<PathBuf>>;

/// List the per-user state directories that exist under `home`.
fn home_index(home: &Path) -> HomeIndex {
    let mut index: HomeIndex = BTreeMap::new();

    for relative in usage::HOME_STATE_DIRS {
        let Ok(entries) = capability::list_dir(&home.join(relative)) else {
            continue;
        };
        for entry in entries {
            if !entry.is_dir {
                continue;
            }
            index
                .entry(entry.name.to_lowercase())
                .or_default()
                .push(entry.path);
        }
    }

    // Legacy dotted directories, e.g. `~/.vlc`, live directly in the home
    // directory rather than under the XDG paths.
    if let Ok(entries) = capability::list_dir(home) {
        for entry in entries {
            if !entry.is_dir {
                continue;
            }
            let Some(name) = entry.name.strip_prefix('.') else {
                continue;
            };
            if name.is_empty() || name == "config" || name == "cache" || name == "local" {
                continue;
            }
            index
                .entry(name.to_lowercase())
                .or_default()
                .push(entry.path);
        }
    }

    index
}

/// The most recent write this package made under the user's home directory.
fn home_activity(
    package: &Package,
    files: &[String],
    index: &HomeIndex,
    config: &Config,
) -> Option<usage::HomeActivity> {
    let mut best: Option<usage::HomeActivity> = None;

    for name in usage::home_state_names(&package.name, files) {
        let Some(directories) = index.get(&name) else {
            continue;
        };
        for directory in directories {
            let Some((mtime, path)) = capability::newest_mtime(directory, config.home_budget)
            else {
                continue;
            };
            let newer = match &best {
                Some(current) => mtime > current.mtime,
                None => true,
            };
            if newer {
                best = Some(usage::HomeActivity {
                    path: path.to_string_lossy().into_owned(),
                    mtime,
                });
            }
        }
    }

    best
}

/// Stat a package's witness files and judge when it was last used.
fn probe_package(
    package: &Package,
    index: &HomeIndex,
    config: &Config,
    support: AtimeSupport,
) -> UsageEvidence {
    let Ok(text) = capability::read_text(&package.db_dir.join("files")) else {
        return UsageEvidence::NoWitness;
    };

    let files = localdb::parse_files(&text);
    let witnesses = usage::witnesses(&files, config.witness_budget);

    let observations: Vec<usage::Observation<'_>> = witnesses
        .into_iter()
        .filter_map(|relative| {
            capability::stat(&config.root.join(relative)).map(|stat| usage::Observation {
                path: relative,
                atime: stat.atime,
            })
        })
        .collect();

    let home = home_activity(package, &files, index, config);
    usage::evaluate(&observations, home.as_ref(), package.install_date, support)
}

/// Build the list of non-package cleanup targets.
fn collect_targets(entries: &[Entry], config: &Config, warnings: &mut Vec<String>) -> Vec<Target> {
    let mut targets = Vec::new();

    targets.extend(cache_targets(entries, config));
    targets.extend(package_handoff_targets(entries));
    targets.extend(aur_helper_targets(config));
    targets.extend(system_targets(config));
    targets.extend(home_targets(config));
    targets.extend(kernel_module_targets(entries, config, warnings));
    targets.extend(flatpak_target());

    targets.retain(|target| target.items > 0 || target.bytes.is_none());
    targets.sort_by(|left, right| {
        right
            .known_bytes()
            .cmp(&left.known_bytes())
            .then_with(|| left.title.cmp(&right.title))
    });
    targets
}

/// Analyse the package cache directories.
fn cache_targets(entries: &[Entry], config: &Config) -> Vec<Target> {
    let mut archives: Vec<CachedArchive> = Vec::new();

    for directory in &config.cache_dirs {
        let Ok(listing) = capability::list_dir(directory) else {
            continue;
        };
        for item in listing {
            if item.is_dir {
                continue;
            }
            let Some((name, version)) = crate::janitor::parse_archive_name(&item.name) else {
                continue;
            };
            let Some(stat) = capability::stat(&item.path) else {
                continue;
            };
            archives.push(CachedArchive {
                path: item.path,
                name,
                version,
                bytes: stat.disk_size,
                mtime: stat.mtime,
            });
        }
    }

    if archives.is_empty() {
        return Vec::new();
    }

    let installed: BTreeMap<String, String> = entries
        .iter()
        .map(|entry| (entry.package.name.clone(), entry.package.version.clone()))
        .collect();

    let plan = crate::janitor::plan_cache(&archives, &installed, 1);
    let has_paccache = capability::has_program("paccache");
    let location = config
        .cache_dirs
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<String>>()
        .join(", ");

    vec![
        Target {
            kind: Kind::PacmanCacheUninstalled,
            title: "Cached archives for packages you removed".to_owned(),
            location: location.clone(),
            detail: "Downloaded packages that are not installed any more. Nothing on the system \
                     references them; they are kept only so a reinstall can skip the download."
                .to_owned(),
            bytes: Some(plan.uninstalled_bytes),
            items: plan.uninstalled.len(),
            safety: Safety::Safe,
            reclaim: if has_paccache {
                Reclaim::Run {
                    command: Command::new("paccache", &["-r", "-u", "-k0"], true),
                }
            } else {
                Reclaim::Paths {
                    paths: plan.uninstalled.clone(),
                    needs_root: true,
                }
            },
        },
        Target {
            kind: Kind::PacmanCacheSuperseded,
            title: "Superseded versions of installed packages".to_owned(),
            location,
            detail: "Older builds of packages you still have. The installed version and one \
                     rollback version are kept, so a bad upgrade can still be reverted offline."
                .to_owned(),
            bytes: Some(plan.superseded_bytes),
            items: plan.superseded.len(),
            safety: Safety::Safe,
            reclaim: if has_paccache {
                Reclaim::Run {
                    command: Command::new("paccache", &["-r", "-k1"], true),
                }
            } else {
                Reclaim::Paths {
                    paths: plan.superseded.clone(),
                    needs_root: true,
                }
            },
        },
    ]
}

/// Summarise the package-side findings so the janitor tab shows the whole picture.
fn package_handoff_targets(entries: &[Entry]) -> Vec<Target> {
    let orphans: Vec<&Entry> = entries
        .iter()
        .filter(|entry| {
            entry.package.reason == crate::model::InstallReason::Dependency
                && entry.facts.is_orphan()
                && !entry.facts.protected
        })
        .collect();
    let orphan_bytes = orphans
        .iter()
        .map(|entry| entry.package.size)
        .fold(0u64, u64::saturating_add);

    let unused_foreign: Vec<&Entry> = entries
        .iter()
        .filter(|entry| entry.facts.origin.is_foreign() && entry.facts.usage.is_unused())
        .collect();
    let unused_foreign_bytes = unused_foreign
        .iter()
        .map(|entry| entry.package.size)
        .fold(0u64, u64::saturating_add);

    let debug: Vec<&Entry> = entries
        .iter()
        .filter(|entry| entry.package.name.ends_with("-debug"))
        .collect();
    let debug_bytes = debug
        .iter()
        .map(|entry| entry.package.size)
        .fold(0u64, u64::saturating_add);

    vec![
        Target {
            kind: Kind::Orphans,
            title: "Orphaned dependencies".to_owned(),
            location: "installed packages".to_owned(),
            detail: "Installed to satisfy something that is now gone. Nothing left on the system \
                     depends on them."
                .to_owned(),
            bytes: Some(orphan_bytes),
            items: orphans.len(),
            safety: Safety::Safe,
            reclaim: Reclaim::Handoff {
                hint: "packages tab, filter: o".to_owned(),
            },
        },
        Target {
            kind: Kind::UnusedForeign,
            title: "AUR packages never used since install".to_owned(),
            location: "installed packages".to_owned(),
            detail: "Built from the AUR, and no file they own has been read since the day they \
                     were installed. These are usually the things you tried once."
                .to_owned(),
            bytes: Some(unused_foreign_bytes),
            items: unused_foreign.len(),
            safety: Safety::Review,
            reclaim: Reclaim::Handoff {
                hint: "packages tab, filter: a then n".to_owned(),
            },
        },
        Target {
            kind: Kind::DebugPackages,
            title: "Debug symbol packages".to_owned(),
            location: "installed packages".to_owned(),
            detail: "Detached debug symbols. Only useful when you are debugging that exact build \
                     with gdb; they can be reinstalled at any time."
                .to_owned(),
            bytes: Some(debug_bytes),
            items: debug.len(),
            safety: Safety::Safe,
            reclaim: Reclaim::Handoff {
                hint: "packages tab, search: -debug".to_owned(),
            },
        },
    ]
}

/// Measure the build caches of the AUR helpers.
fn aur_helper_targets(config: &Config) -> Vec<Target> {
    let Some(home) = config.home.as_ref() else {
        return Vec::new();
    };
    if !config.measure_directories {
        return Vec::new();
    }

    let candidates = [
        ("yay", home.join(".cache/yay")),
        ("paru", home.join(".cache/paru")),
        ("pikaur", home.join(".cache/pikaur")),
        ("trizen", home.join(".cache/trizen")),
        ("aurutils", home.join(".cache/aurutils")),
        ("makepkg", home.join(".cache/makepkg")),
    ];

    let mut targets = Vec::new();
    for (helper, path) in candidates {
        if !capability::exists(&path) {
            continue;
        }
        let usage = capability::tree_usage(&path, config.walk_budget);
        if usage.bytes == 0 {
            continue;
        }
        targets.push(Target {
            kind: Kind::AurHelperCache,
            title: format!("{helper} build cache"),
            location: path.to_string_lossy().into_owned(),
            detail: format!(
                "A git clone and a full build tree for every package {helper} has ever built. \
                 Deleting it costs a re-clone on the next build and nothing else."
            ),
            bytes: Some(usage.bytes),
            items: usage.files,
            safety: Safety::Safe,
            reclaim: Reclaim::Paths {
                paths: vec![path],
                needs_root: false,
            },
        });
    }

    targets
}

/// System-wide leftovers: configuration files, coredumps and the journal.
fn system_targets(config: &Config) -> Vec<Target> {
    let mut targets = Vec::new();

    let etc = config.root.join("etc");
    let leftovers = capability::find_by_suffix(&etc, &[".pacnew", ".pacsave", ".pacorig"], 200_000);
    if !leftovers.is_empty() {
        let bytes = leftovers
            .iter()
            .map(|(_path, size)| *size)
            .fold(0u64, u64::saturating_add);
        targets.push(Target {
            kind: Kind::ConfigLeftovers,
            title: ".pacnew and .pacsave files".to_owned(),
            location: etc.to_string_lossy().into_owned(),
            detail: "Configuration files pacman could not merge automatically. Each one is either \
                     an upgrade you never reviewed or a config left behind by a removed package. \
                     Merge them with pacdiff before deleting."
                .to_owned(),
            bytes: Some(bytes),
            items: leftovers.len(),
            safety: Safety::Careful,
            reclaim: if capability::has_program("pacdiff") {
                Reclaim::Run {
                    command: Command::new("pacdiff", &[], true),
                }
            } else {
                Reclaim::Advice {
                    text: "install pacman-contrib and run `sudo pacdiff` to review each file"
                        .to_owned(),
                }
            },
        });
    }

    if !config.measure_directories {
        return targets;
    }

    let coredumps = config.root.join("var/lib/systemd/coredump");
    if capability::exists(&coredumps) {
        let usage = capability::tree_usage(&coredumps, config.walk_budget);
        targets.push(Target {
            kind: Kind::Coredumps,
            title: "Systemd coredumps".to_owned(),
            location: coredumps.to_string_lossy().into_owned(),
            detail: "Memory dumps from crashed processes. Useful for about a day after a crash \
                     you intend to debug, and dead weight after that."
                .to_owned(),
            bytes: Some(usage.bytes),
            items: usage.files,
            safety: Safety::Safe,
            reclaim: Reclaim::Run {
                command: Command::new("journalctl", &["--vacuum-time=1s"], true),
            },
        });
    }

    let journal = config.root.join("var/log/journal");
    if capability::exists(&journal) {
        let usage = capability::tree_usage(&journal, config.walk_budget);
        targets.push(Target {
            kind: Kind::Journal,
            title: "Systemd journal".to_owned(),
            location: journal.to_string_lossy().into_owned(),
            detail: "System logs. Capping the journal keeps recent history and drops the rest; \
                     set SystemMaxUse in journald.conf to make the cap permanent."
                .to_owned(),
            bytes: Some(usage.bytes),
            items: usage.files,
            safety: Safety::Review,
            reclaim: Reclaim::Run {
                command: Command::new("journalctl", &["--vacuum-size=256M"], true),
            },
        });
    }

    targets
}

/// Directories worth measuring under the user's home.
const DEVELOPER_CACHES: [(&str, &str); 8] = [
    (".cargo/registry", "Rust crate sources and downloads"),
    (".npm/_cacache", "npm package cache"),
    (".cache/go-build", "Go build cache"),
    ("go/pkg/mod", "Go module cache"),
    (".gradle/caches", "Gradle cache"),
    (".m2/repository", "Maven repository"),
    (".cache/pip", "pip wheel cache"),
    (".cache/uv", "uv package cache"),
];

/// Per-user caches and the trash.
fn home_targets(config: &Config) -> Vec<Target> {
    let Some(home) = config.home.as_ref() else {
        return Vec::new();
    };
    if !config.measure_directories {
        return Vec::new();
    }

    let mut targets = Vec::new();

    let mut developer_bytes = 0u64;
    let mut developer_files = 0usize;
    let mut developer_paths = Vec::new();
    let mut described = Vec::new();

    for (relative, description) in DEVELOPER_CACHES {
        let path = home.join(relative);
        if !capability::exists(&path) {
            continue;
        }
        let usage = capability::tree_usage(&path, config.walk_budget);
        if usage.bytes == 0 {
            continue;
        }
        developer_bytes = developer_bytes.saturating_add(usage.bytes);
        developer_files = developer_files.saturating_add(usage.files);
        developer_paths.push(path);
        described.push(description);
    }

    if !developer_paths.is_empty() {
        targets.push(Target {
            kind: Kind::DeveloperCache,
            title: "Language toolchain caches".to_owned(),
            location: described.join(", "),
            detail: "Downloaded dependencies and build artefacts for language toolchains. Every \
                     byte is re-fetchable, and these grow without bound because nothing ever \
                     prunes them."
                .to_owned(),
            bytes: Some(developer_bytes),
            items: developer_files,
            safety: Safety::Safe,
            reclaim: Reclaim::Paths {
                paths: developer_paths,
                needs_root: false,
            },
        });
    }

    let cache = home.join(".cache");
    if capability::exists(&cache) {
        let usage = capability::tree_usage(&cache, config.walk_budget);
        targets.push(Target {
            kind: Kind::UserCache,
            title: "Application cache directory".to_owned(),
            location: cache.to_string_lossy().into_owned(),
            detail: "Everything under ~/.cache, AUR helper caches included. Applications are \
                     required to treat this as disposable, but review the largest subdirectories \
                     before clearing the lot."
                .to_owned(),
            bytes: Some(usage.bytes),
            items: usage.files,
            safety: Safety::Review,
            reclaim: Reclaim::Advice {
                text: "inspect with `du -sh ~/.cache/* | sort -h` and remove the offenders"
                    .to_owned(),
            },
        });
    }

    let trash = home.join(".local/share/Trash");
    if capability::exists(&trash) {
        let usage = capability::tree_usage(&trash, config.walk_budget);
        targets.push(Target {
            kind: Kind::Trash,
            title: "Desktop trash".to_owned(),
            location: trash.to_string_lossy().into_owned(),
            detail: "Files you already deleted once.".to_owned(),
            bytes: Some(usage.bytes),
            items: usage.files,
            safety: Safety::Review,
            reclaim: Reclaim::Paths {
                paths: vec![trash.join("files"), trash.join("info")],
                needs_root: false,
            },
        });
    }

    targets
}

/// Find kernel module trees whose owning package is gone.
///
/// Arch kernel packages write `/usr/lib/modules/<release>/pkgbase` naming the
/// package that owns the tree. If that package is not installed, the whole
/// directory is a leftover — typically hundreds of megabytes, and invisible to
/// every other cleanup tool because pacman does not know the files exist.
fn kernel_module_targets(
    entries: &[Entry],
    config: &Config,
    warnings: &mut Vec<String>,
) -> Vec<Target> {
    let modules = config.root.join("usr/lib/modules");
    let Ok(listing) = capability::list_dir(&modules) else {
        return Vec::new();
    };

    let installed: BTreeSet<&str> = entries
        .iter()
        .map(|entry| entry.package.name.as_str())
        .collect();

    let mut stale = Vec::new();
    let mut bytes = 0u64;

    for item in listing {
        if !item.is_dir {
            continue;
        }

        let pkgbase = item.path.join("pkgbase");
        let owner = capability::read_text(&pkgbase)
            .ok()
            .map(|text| text.trim().to_owned());

        let orphaned = match owner {
            Some(ref name) if installed.contains(name.as_str()) => false,
            Some(_) => true,
            None => {
                // No pkgbase at all: either a very old kernel package or a
                // directory left by a removed one. Flagging it without proof
                // risks deleting a running kernel's modules, so say so instead.
                warnings.push(format!(
                    "{} has no pkgbase file; check by hand whether it belongs to an installed kernel",
                    item.path.display()
                ));
                false
            }
        };

        if !orphaned {
            continue;
        }

        if config.measure_directories {
            bytes =
                bytes.saturating_add(capability::tree_usage(&item.path, config.walk_budget).bytes);
        }
        stale.push(item.path);
    }

    if stale.is_empty() {
        return Vec::new();
    }

    vec![Target {
        kind: Kind::StaleKernelModules,
        title: "Module trees from removed kernels".to_owned(),
        location: modules.to_string_lossy().into_owned(),
        detail: "Each directory names its owning package in a pkgbase file, and that package is \
                 no longer installed. pacman cannot see these files, so nothing else cleans them \
                 up. Make sure you are not booted into one of these kernels first."
            .to_owned(),
        bytes: Some(bytes),
        items: stale.len(),
        safety: Safety::Careful,
        reclaim: Reclaim::Paths {
            paths: stale,
            needs_root: true,
        },
    }]
}

/// Offer the flatpak cleanup when flatpak is installed.
///
/// Unused runtimes are often several gigabytes, but only flatpak itself can
/// say which runtimes nothing references, and asking costs a subprocess on
/// every scan. The target is advisory: it carries no size claim it cannot
/// back up.
fn flatpak_target() -> Vec<Target> {
    if !capability::has_program("flatpak") {
        return Vec::new();
    }

    vec![Target {
        kind: Kind::FlatpakUnused,
        title: "Unused flatpak runtimes".to_owned(),
        location: "flatpak".to_owned(),
        detail: "Runtimes and SDK extensions that no installed application references. Only \
                 flatpak can work out which those are, so pacpurge does not guess at a size."
            .to_owned(),
        bytes: None,
        items: 1,
        safety: Safety::Safe,
        reclaim: Reclaim::Run {
            command: Command::new("flatpak", &["uninstall", "--unused"], false),
        },
    }]
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::pacman_conf_paths;

    #[test]
    fn pacman_conf_paths_are_read_from_the_options_section() {
        let text = "\
[options]
DBPath = /custom/db/
CacheDir = /custom/cache/ /second/cache/
[core]
DBPath = /ignored/
";
        let (db_path, cache_dirs) = pacman_conf_paths(text, Path::new("/"));
        assert_eq!(db_path, Some(PathBuf::from("/custom/db/")));
        assert_eq!(
            cache_dirs,
            vec![
                PathBuf::from("/custom/cache/"),
                PathBuf::from("/second/cache/")
            ]
        );
    }

    #[test]
    fn pacman_conf_paths_are_reanchored_under_the_scan_root() {
        let text = "[options]\nDBPath = /var/lib/pacman/\n";
        let (db_path, _cache) = pacman_conf_paths(text, Path::new("/mnt/other"));
        assert_eq!(db_path, Some(PathBuf::from("/mnt/other/var/lib/pacman/")));
    }

    #[test]
    fn comments_and_blank_settings_are_ignored() {
        let text = "[options]\n#DBPath = /wrong/\nDBPath =\nCacheDir = /right/ # trailing\n";
        let (db_path, cache_dirs) = pacman_conf_paths(text, Path::new("/"));
        assert_eq!(db_path, None);
        assert_eq!(cache_dirs, vec![PathBuf::from("/right/")]);
    }
}
