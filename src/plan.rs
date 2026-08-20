//! Turning a selection into the exact commands that will run.
//!
//! Nothing here executes anything. A [`Plan`] is a proposal the user reviews,
//! and it deliberately carries the bad news — what breaks, what gets dragged
//! along, what is protected — next to the number of bytes it promises.

use std::collections::BTreeSet;

use crate::graph::Graph;
use crate::janitor::{Command, Reclaim, Target};
use crate::model::Inventory;

/// A proposed package removal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// Packages the user picked, in display order.
    pub selected: Vec<String>,
    /// Packages pacman will additionally remove as unneeded dependencies.
    pub cascade: Vec<String>,
    /// Installed packages that depend on the selection and were not picked.
    ///
    /// Non-empty means pacman will refuse the transaction, which is the right
    /// outcome: it is a mistake, not something to work around.
    pub broken: Vec<String>,
    /// Selected packages that carry the protected flag.
    pub protected: Vec<String>,
    /// Total bytes the transaction frees, cascade included.
    pub bytes: u64,
}

impl Plan {
    /// Whether the plan can be executed as it stands.
    pub fn is_executable(&self) -> bool {
        !self.selected.is_empty() && self.broken.is_empty()
    }

    /// Every package the transaction removes.
    pub fn all_removed(&self) -> Vec<String> {
        let mut names = self.selected.clone();
        names.extend(self.cascade.iter().cloned());
        names
    }

    /// The command pacman would run.
    pub fn command(&self) -> Command {
        let mut args = vec!["-Rns".to_owned()];
        args.extend(self.selected.iter().cloned());
        Command {
            program: "pacman".to_owned(),
            args,
            needs_root: true,
        }
    }

    /// The same transaction, but only printing what it would do.
    pub fn dry_run_command(&self) -> Command {
        let mut args = vec!["-Rns".to_owned(), "--print".to_owned()];
        args.extend(self.selected.iter().cloned());
        Command {
            program: "pacman".to_owned(),
            args,
            // `--print` changes nothing, so it does not need root and can be
            // run without a password prompt.
            needs_root: false,
        }
    }
}

/// Work out what removing `selection` would really do.
pub fn build(inventory: &Inventory, selection: &BTreeSet<usize>) -> Plan {
    let packages: Vec<crate::model::Package> = inventory
        .entries
        .iter()
        .map(|entry| entry.package.clone())
        .collect();
    let graph = Graph::build(&packages);

    let removed = graph.cascade(selection);

    let name_of = |position: &usize| -> Option<String> {
        packages.get(*position).map(|package| package.name.clone())
    };

    let mut selected: Vec<String> = selection.iter().filter_map(name_of).collect();
    selected.sort();

    let mut cascade: Vec<String> = removed
        .iter()
        .filter(|position| !selection.contains(*position))
        .filter_map(name_of)
        .collect();
    cascade.sort();

    let broken = graph.broken_by(&removed);

    let mut protected: Vec<String> = selection
        .iter()
        .filter(|position| graph.is_protected(**position))
        .filter_map(name_of)
        .collect();
    protected.sort();

    let bytes = removed
        .iter()
        .filter_map(|position| packages.get(*position))
        .map(|package| package.size)
        .fold(0u64, u64::saturating_add);

    Plan {
        selected,
        cascade,
        broken,
        protected,
        bytes,
    }
}

/// A single step the user has asked to run, ready for execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Run a command with the terminal attached.
    Run {
        /// The command.
        command: Command,
    },
    /// Delete these paths.
    Delete {
        /// Absolute paths.
        paths: Vec<std::path::PathBuf>,
        /// Whether root is required.
        needs_root: bool,
    },
}

impl Step {
    /// Render the step as a shell line the user could paste.
    pub fn to_shell(&self) -> String {
        match self {
            Step::Run { command } => command.to_shell(),
            Step::Delete { paths, needs_root } => {
                let rendered: Vec<String> = paths
                    .iter()
                    .map(|path| quote(&path.to_string_lossy()))
                    .collect();
                let prefix = if *needs_root { "sudo " } else { "" };
                format!("{prefix}rm -rf {}", rendered.join(" "))
            }
        }
    }
}

/// The step a janitor target needs, if it has one that can be run.
pub fn step_for(target: &Target) -> Option<Step> {
    match &target.reclaim {
        Reclaim::Run { command } => Some(Step::Run {
            command: command.clone(),
        }),
        Reclaim::Paths { paths, needs_root } => Some(Step::Delete {
            paths: paths.clone(),
            needs_root: *needs_root,
        }),
        Reclaim::Handoff { hint: _ } | Reclaim::Advice { text: _ } => None,
    }
}

/// Quote a path for display in a shell line.
///
/// Only used for the copy-pasteable preview; execution never goes through a
/// shell, so this cannot become an injection path.
fn quote(value: &str) -> String {
    if value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "._-/@+:=".contains(character))
    {
        return value.to_owned();
    }
    let escaped = value.replace('\'', "'\\''");
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use super::{build, quote, Step};
    use crate::model::{
        AtimeSupport, Entry, Facts, InstallReason, Inventory, Origin, Package, UsageEvidence,
    };

    fn entry(name: &str, size: u64, reason: InstallReason, depends: &[&str]) -> Entry {
        Entry {
            package: Package {
                name: name.to_owned(),
                version: "1-1".to_owned(),
                size,
                reason,
                depends: depends.iter().map(|item| (*item).to_owned()).collect(),
                ..Package::default()
            },
            facts: Facts {
                required_by: Vec::new(),
                optional_for: Vec::new(),
                origin: Origin::Unknown,
                usage: UsageEvidence::NotProbed,
                reclaimable: size,
                frees: Vec::new(),
                protected: false,
            },
        }
    }

    fn inventory(entries: Vec<Entry>) -> Inventory {
        let index = entries
            .iter()
            .enumerate()
            .map(|(position, entry)| (entry.package.name.clone(), position))
            .collect();
        Inventory {
            entries,
            index,
            targets: Vec::new(),
            atime_support: AtimeSupport::Relatime,
            scanned_at: 0,
            probed: 0,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn a_plan_reports_the_cascade_and_its_size() {
        let inventory = inventory(vec![
            entry("app", 100, InstallReason::Explicit, &["lib"]),
            entry("lib", 900, InstallReason::Dependency, &[]),
        ]);
        let mut selection = BTreeSet::new();
        selection.insert(0);

        let plan = build(&inventory, &selection);
        assert_eq!(plan.selected, vec!["app".to_owned()]);
        assert_eq!(plan.cascade, vec!["lib".to_owned()]);
        assert_eq!(plan.bytes, 1_000);
        assert!(plan.broken.is_empty());
        assert!(plan.is_executable());
    }

    #[test]
    fn a_plan_that_breaks_something_is_not_executable() {
        let inventory = inventory(vec![
            entry("app", 100, InstallReason::Explicit, &["lib"]),
            entry("lib", 900, InstallReason::Dependency, &[]),
        ]);
        let mut selection = BTreeSet::new();
        selection.insert(1);

        let plan = build(&inventory, &selection);
        assert_eq!(plan.broken, vec!["app".to_owned()]);
        assert!(!plan.is_executable());
    }

    #[test]
    fn an_empty_selection_is_not_executable() {
        let inventory = inventory(vec![entry("app", 1, InstallReason::Explicit, &[])]);
        let plan = build(&inventory, &BTreeSet::new());
        assert!(!plan.is_executable());
        assert_eq!(plan.bytes, 0);
    }

    #[test]
    fn the_command_removes_only_what_was_picked() {
        let inventory = inventory(vec![
            entry("app", 100, InstallReason::Explicit, &["lib"]),
            entry("lib", 900, InstallReason::Dependency, &[]),
        ]);
        let mut selection = BTreeSet::new();
        selection.insert(0);

        let plan = build(&inventory, &selection);
        // `lib` is left to pacman's own `-s`, so the command stays honest
        // about what the user chose.
        assert_eq!(plan.command().to_shell(), "sudo pacman -Rns app");
        assert_eq!(plan.dry_run_command().to_shell(), "pacman -Rns --print app");
    }

    #[test]
    fn delete_steps_render_as_a_shell_line() {
        let step = Step::Delete {
            paths: vec![PathBuf::from("/var/cache/pacman/pkg/a.pkg.tar.zst")],
            needs_root: true,
        };
        assert_eq!(
            step.to_shell(),
            "sudo rm -rf /var/cache/pacman/pkg/a.pkg.tar.zst"
        );
    }

    #[test]
    fn paths_with_spaces_are_quoted_for_display() {
        assert_eq!(quote("/home/me/a b"), "'/home/me/a b'");
        assert_eq!(quote("/usr/lib/libc.so.6"), "/usr/lib/libc.so.6");
        assert_eq!(quote("it's"), "'it'\\''s'");
    }
}
