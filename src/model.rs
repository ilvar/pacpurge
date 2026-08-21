//! Core data types shared by the scanner, the analysis passes and the UI.
//!
//! Everything in this module is pure data. Nothing here touches the
//! filesystem; values are produced by [`crate::scan`] and consumed by the
//! analysis and rendering layers.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

/// Why pacman believes a package is installed.
///
/// A local database entry with no `%REASON%` section was installed
/// explicitly, which is why that is the default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InstallReason {
    /// Requested directly by the user.
    #[default]
    Explicit,
    /// Pulled in to satisfy another package's dependency.
    Dependency,
}

impl InstallReason {
    /// Short label used in table cells.
    pub fn label(self) -> &'static str {
        match self {
            InstallReason::Explicit => "explicit",
            InstallReason::Dependency => "dep",
        }
    }
}

/// Where a package came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "name", rename_all = "lowercase")]
pub enum Origin {
    /// Present in a configured sync repository, e.g. `extra` or `multilib`.
    Repository(String),
    /// Not in any sync repository: an AUR build, or a locally built package.
    Foreign,
    /// Repository membership could not be determined on this system.
    Unknown,
}

impl Origin {
    /// Short label used in table cells.
    pub fn label(&self) -> &str {
        match self {
            Origin::Repository(name) => name,
            Origin::Foreign => "aur/local",
            Origin::Unknown => "?",
        }
    }

    /// Whether the package is foreign (AUR or locally built).
    pub fn is_foreign(&self) -> bool {
        match self {
            Origin::Foreign => true,
            Origin::Repository(_) | Origin::Unknown => false,
        }
    }
}

/// A single installed package as recorded in the local pacman database.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Package {
    /// Package name, e.g. `ripgrep`.
    pub name: String,
    /// Full version string including the pkgrel, e.g. `14.1.1-1`.
    pub version: String,
    /// Installed size in bytes, as recorded by pacman.
    pub size: u64,
    /// Unix timestamp of installation, when recorded.
    pub install_date: Option<i64>,
    /// Unix timestamp of the upstream build, when recorded.
    pub build_date: Option<i64>,
    /// Install reason.
    pub reason: InstallReason,
    /// One-line description.
    pub description: String,
    /// Upstream URL.
    pub url: String,
    /// Person or service that built the package.
    pub packager: String,
    /// Groups the package belongs to, e.g. `base-devel`.
    pub groups: Vec<String>,
    /// Hard dependencies, with version constraints still attached.
    pub depends: Vec<String>,
    /// Optional dependencies, with their trailing `: reason` still attached.
    pub optdepends: Vec<String>,
    /// Virtual names this package provides.
    pub provides: Vec<String>,
    /// Packages this one replaces.
    pub replaces: Vec<String>,
    /// Directory holding this package's entry in the local database.
    #[serde(skip)]
    pub db_dir: PathBuf,
}

/// What the atime probe was able to establish about a package's last use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum UsageEvidence {
    /// A witness file was read more recently than the package was installed.
    Used {
        /// Unix timestamp of the most recent access.
        at: i64,
        /// The file that produced that timestamp, relative to the root.
        witness: String,
    },
    /// The application wrote state in the user's home directory.
    ///
    /// Independent of access times entirely, so this is the only evidence
    /// available on a `noatime` filesystem — and it is stronger evidence than
    /// an access time even where both exist, because a program writing its own
    /// configuration back means a person ran it, not that something read it.
    UsedFromHome {
        /// Unix timestamp of the most recent write.
        at: i64,
        /// Absolute path that carried the timestamp.
        witness: String,
    },
    /// Witness files exist, but none has been read since install time.
    NeverSinceInstall {
        /// Most recent access seen, which tracks installation.
        at: i64,
    },
    /// The package ships no file whose access time would mean anything.
    NoWitness,
    /// The filesystem holding the witnesses is mounted `noatime`.
    AtimeDisabled,
    /// This package was outside the probe budget.
    NotProbed,
}

impl UsageEvidence {
    /// The observed timestamp, when there was one.
    pub fn timestamp(&self) -> Option<i64> {
        match self {
            UsageEvidence::Used { at, witness: _ } => Some(*at),
            UsageEvidence::UsedFromHome { at, witness: _ } => Some(*at),
            UsageEvidence::NeverSinceInstall { at } => Some(*at),
            UsageEvidence::NoWitness | UsageEvidence::AtimeDisabled | UsageEvidence::NotProbed => {
                None
            }
        }
    }

    /// Whether the evidence positively shows the package being used.
    pub fn is_used(&self) -> bool {
        match self {
            UsageEvidence::Used { at: _, witness: _ }
            | UsageEvidence::UsedFromHome { at: _, witness: _ } => true,
            UsageEvidence::NeverSinceInstall { at: _ }
            | UsageEvidence::NoWitness
            | UsageEvidence::AtimeDisabled
            | UsageEvidence::NotProbed => false,
        }
    }

    /// Whether the evidence positively shows the package going unused.
    pub fn is_unused(&self) -> bool {
        match self {
            UsageEvidence::NeverSinceInstall { at: _ } => true,
            UsageEvidence::Used { at: _, witness: _ }
            | UsageEvidence::UsedFromHome { at: _, witness: _ }
            | UsageEvidence::NoWitness
            | UsageEvidence::AtimeDisabled
            | UsageEvidence::NotProbed => false,
        }
    }
}

/// Everything derived about one package beyond its raw database entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Facts {
    /// Installed packages that hard-depend on this one.
    pub required_by: Vec<String>,
    /// Installed packages that list this one as an optional dependency.
    pub optional_for: Vec<String>,
    /// Repository membership.
    pub origin: Origin,
    /// Result of the last-use probe.
    pub usage: UsageEvidence,
    /// Bytes this package's own files occupy, plus the bytes of dependencies
    /// that only it keeps alive. This is what removing it actually frees.
    pub reclaimable: u64,
    /// Packages that would become orphans if this one were removed.
    pub frees: Vec<String>,
    /// Whether removing this package is considered unsafe.
    pub protected: bool,
}

impl Facts {
    /// A package installed as a dependency that nothing hard-depends on.
    pub fn is_orphan(&self) -> bool {
        self.required_by.is_empty()
    }

    /// An orphan whose only remaining tie is an optional dependency.
    pub fn is_optional_orphan(&self) -> bool {
        self.required_by.is_empty() && !self.optional_for.is_empty()
    }
}

/// A package together with everything derived about it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Entry {
    /// The raw database record.
    pub package: Package,
    /// Derived analysis.
    pub facts: Facts,
}

/// Whether access-time data can be trusted on this system.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AtimeSupport {
    /// atime updates at most daily. The normal Arch default, and good enough.
    Relatime,
    /// atime updates on every read. Ideal.
    Strict,
    /// atime is frozen. Last-use data is meaningless.
    Disabled,
    /// The mount table could not be read.
    Unknown,
}

impl AtimeSupport {
    /// A sentence explaining what the mode means for the last-use column.
    pub fn caveat(self) -> &'static str {
        match self {
            AtimeSupport::Relatime => {
                "relatime: access times update at most once a day, which is enough to tell used from unused"
            }
            AtimeSupport::Strict => "strictatime: access times are exact",
            AtimeSupport::Disabled => {
                "noatime: access times are frozen on this mount, so last-use data is NOT meaningful"
            }
            AtimeSupport::Unknown => "mount options unknown: treat last-use data with care",
        }
    }

    /// Whether last-use figures should be shown at all.
    pub fn is_meaningful(self) -> bool {
        match self {
            AtimeSupport::Relatime | AtimeSupport::Strict | AtimeSupport::Unknown => true,
            AtimeSupport::Disabled => false,
        }
    }
}

/// The complete result of a scan.
#[derive(Clone, Debug, Serialize)]
pub struct Inventory {
    /// Every installed package, in database order.
    pub entries: Vec<Entry>,
    /// Index from package name to its position in `entries`.
    #[serde(skip)]
    pub index: BTreeMap<String, usize>,
    /// Reclaimable space found outside the package set.
    pub targets: Vec<crate::janitor::Target>,
    /// Whether access times mean anything on this system.
    pub atime_support: AtimeSupport,
    /// Unix timestamp the scan was taken at.
    pub scanned_at: i64,
    /// Number of packages whose access times were probed.
    pub probed: usize,
    /// Non-fatal problems encountered during the scan.
    pub warnings: Vec<String>,
}

impl Inventory {
    /// Total installed size of every package.
    pub fn total_size(&self) -> u64 {
        self.entries
            .iter()
            .map(|entry| entry.package.size)
            .fold(0u64, u64::saturating_add)
    }

    /// Look up an entry by package name.
    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.index
            .get(name)
            .and_then(|position| self.entries.get(*position))
    }
}
