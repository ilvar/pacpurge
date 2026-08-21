//! Non-interactive output: the plain-text tables and the JSON document.
//!
//! These modes exist so the analysis is scriptable and so the tool can be
//! tested end-to-end against a fixture root without driving a terminal.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::filter::{self, View};
use crate::format;
use crate::janitor::Reclaim;
use crate::model::{Inventory, UsageEvidence};
use crate::plan;

/// Headline numbers for the JSON document.
#[derive(Debug, Serialize)]
pub struct Summary {
    /// Number of installed packages.
    pub packages: usize,
    /// Their combined installed size in bytes.
    pub installed_bytes: u64,
    /// Orphaned dependencies.
    pub orphans: usize,
    /// Bytes held by orphaned dependencies.
    pub orphan_bytes: u64,
    /// Packages not in any sync repository.
    pub foreign: usize,
    /// Packages with no read since installation.
    pub never_used: usize,
    /// Bytes held by those packages.
    pub never_used_bytes: u64,
    /// Bytes reclaimable from non-package targets.
    pub target_bytes: u64,
}

/// The complete JSON document.
#[derive(Debug, Serialize)]
pub struct Document<'a> {
    /// Headline numbers.
    pub summary: Summary,
    /// The full scan.
    pub inventory: &'a Inventory,
}

/// Compute the headline numbers.
pub fn summarise(inventory: &Inventory) -> Summary {
    let orphans: Vec<&crate::model::Entry> = inventory
        .entries
        .iter()
        .filter(|entry| {
            entry.package.reason == crate::model::InstallReason::Dependency
                && entry.facts.is_orphan()
        })
        .collect();

    let never_used: Vec<&crate::model::Entry> = inventory
        .entries
        .iter()
        .filter(|entry| entry.facts.usage.is_unused())
        .collect();

    Summary {
        packages: inventory.entries.len(),
        installed_bytes: inventory.total_size(),
        orphans: orphans.len(),
        orphan_bytes: orphans
            .iter()
            .map(|entry| entry.package.size)
            .fold(0u64, u64::saturating_add),
        foreign: inventory
            .entries
            .iter()
            .filter(|entry| entry.facts.origin.is_foreign())
            .count(),
        never_used: never_used.len(),
        never_used_bytes: never_used
            .iter()
            .map(|entry| entry.package.size)
            .fold(0u64, u64::saturating_add),
        target_bytes: inventory
            .targets
            .iter()
            .map(crate::janitor::Target::known_bytes)
            .fold(0u64, u64::saturating_add),
    }
}

/// Render the whole analysis as one JSON document.
pub fn json(inventory: &Inventory) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&Document {
        summary: summarise(inventory),
        inventory,
    })
}

/// Render the package table as plain text.
pub fn list(inventory: &Inventory, view: &View, limit: usize) -> String {
    let mut output = String::new();
    let order = filter::order(&inventory.entries, view, inventory.scanned_at);

    output.push_str(&format!(
        "{:<32} {:>10} {:>10} {:>10} {:>10} {:<9} {}\n",
        "PACKAGE", "SIZE", "FREES", "LAST USED", "INSTALLED", "REASON", "ORIGIN"
    ));

    for position in order.iter().take(limit) {
        let Some(entry) = inventory.entries.get(*position) else {
            continue;
        };

        output.push_str(&format!(
            "{:<32} {:>10} {:>10} {:>10} {:>10} {:<9} {}\n",
            format::truncate(&entry.package.name, 32),
            format::bytes(entry.package.size),
            format::bytes(entry.facts.reclaimable),
            usage_cell(&entry.facts.usage, inventory),
            format::age(inventory.scanned_at, entry.package.install_date),
            entry.package.reason.label(),
            entry.facts.origin.label(),
        ));
    }

    let shown = order.len().min(limit);
    output.push_str(&format!(
        "\n{shown} of {} packages shown; {} installed in total\n",
        order.len(),
        format::bytes(inventory.total_size())
    ));

    if !inventory.atime_support.is_meaningful() {
        output.push_str(&format!("note: {}\n", inventory.atime_support.caveat()));
    }

    output
}

/// The last-used cell for one entry.
fn usage_cell(usage: &UsageEvidence, inventory: &Inventory) -> String {
    match usage {
        UsageEvidence::Used { at, witness: _ } => format::age(inventory.scanned_at, Some(*at)),
        UsageEvidence::UsedFromHome { at, witness: _ } => {
            format!("~{}", format::age(inventory.scanned_at, Some(*at)))
        }
        UsageEvidence::NeverSinceInstall { at: _ } => "never".to_owned(),
        UsageEvidence::NoWitness => "n/a".to_owned(),
        UsageEvidence::AtimeDisabled => "off".to_owned(),
        UsageEvidence::NotProbed => "-".to_owned(),
    }
}

/// Render the cleanup targets as plain text.
pub fn clean(inventory: &Inventory) -> String {
    let mut output = String::new();
    let mut total = 0u64;

    for target in &inventory.targets {
        let size = match target.bytes {
            Some(bytes) => format::bytes(bytes),
            None => "unknown".to_owned(),
        };
        total = total.saturating_add(target.known_bytes());

        output.push_str(&format!(
            "{:>10}  {:<8} {}\n            {}\n",
            size,
            target.safety.label(),
            target.title,
            target.location
        ));

        let action = match &target.reclaim {
            Reclaim::Run { command } => format!("run: {}", command.to_shell()),
            Reclaim::Paths { paths, needs_root } => {
                let step = plan::Step::Delete {
                    paths: paths.clone(),
                    needs_root: *needs_root,
                };
                let rendered = step.to_shell();
                format!("run: {}", format::truncate(&rendered, 100))
            }
            Reclaim::Handoff { hint } => format!("see: {hint}"),
            Reclaim::Advice { text } => format!("hint: {text}"),
        };
        output.push_str(&format!("            {action}\n\n"));
    }

    output.push_str(&format!(
        "{} reclaimable across {} targets\n",
        format::bytes(total),
        inventory.targets.len()
    ));

    for warning in &inventory.warnings {
        output.push_str(&format!("warning: {warning}\n"));
    }

    output
}

/// Explain what the last-use probe was able to see.
///
/// Exists because "the column is empty" has several possible causes that look
/// identical from the outside: a `noatime` mount, a package that ships nothing
/// worth stat-ing, or a probe that was bounded away. Guessing between them
/// from a screenshot is no way to debug a tool.
pub fn diagnose(inventory: &Inventory) -> String {
    let mut output = String::new();

    output.push_str("LAST-USE DIAGNOSIS\n\n");
    output.push_str(&format!(
        "  access times     {:?}\n                   {}\n",
        inventory.atime_support,
        inventory.atime_support.caveat()
    ));
    output.push_str(&format!(
        "  packages         {}\n  probed           {}\n\n",
        inventory.entries.len(),
        inventory.probed
    ));

    let mut counts: BTreeMap<&str, (usize, u64)> = BTreeMap::new();
    for entry in &inventory.entries {
        let label = match &entry.facts.usage {
            UsageEvidence::Used { at: _, witness: _ } => "dated from a file's access time",
            UsageEvidence::UsedFromHome { at: _, witness: _ } => "dated from home-directory state",
            UsageEvidence::NeverSinceInstall { at: _ } => "not read since it was installed",
            UsageEvidence::NoWitness => "ships nothing worth checking",
            UsageEvidence::AtimeDisabled => "no evidence available (access times frozen)",
            UsageEvidence::NotProbed => "not probed (raise --top)",
        };
        let slot = counts.entry(label).or_insert((0, 0));
        slot.0 = slot.0.saturating_add(1);
        slot.1 = slot.1.saturating_add(entry.package.size);
    }

    output.push_str("  WHERE EACH VERDICT CAME FROM\n");
    for (label, (count, bytes)) in &counts {
        output.push_str(&format!(
            "  {count:>6}  {:>10}  {label}\n",
            format::bytes(*bytes)
        ));
    }

    let dated: Vec<&crate::model::Entry> = inventory
        .entries
        .iter()
        .filter(|entry| entry.facts.usage.is_used())
        .collect();

    if !dated.is_empty() {
        output.push_str("\n  EXAMPLES OF WHAT IT FOUND\n");
        for entry in dated.iter().take(5) {
            let (source, witness) = match &entry.facts.usage {
                UsageEvidence::Used { at: _, witness } => ("access time", format!("/{witness}")),
                UsageEvidence::UsedFromHome { at: _, witness } => ("home state", witness.clone()),
                UsageEvidence::NeverSinceInstall { at: _ }
                | UsageEvidence::NoWitness
                | UsageEvidence::AtimeDisabled
                | UsageEvidence::NotProbed => continue,
            };
            output.push_str(&format!(
                "  {:<24} {:>8}  {source:<12} {witness}\n",
                format::truncate(&entry.package.name, 24),
                format::age(inventory.scanned_at, entry.facts.usage.timestamp()),
            ));
        }
    }

    if !inventory.atime_support.is_meaningful() {
        output.push_str(
            "\n  WHY THE COLUMN IS MOSTLY EMPTY\n               The filesystem holding /usr is mounted noatime, which stops the kernel recording\n               when a file was read. Package files therefore carry the timestamp of the last\n               upgrade and nothing else, so pacpurge will not pretend that is a last-use date.\n\n               Packages that write state under your home directory are still dated, because\n               modification times are unaffected. To date the rest, drop `noatime` from the root\n               filesystem's options in /etc/fstab (relatime is the default and costs one write\n               per file per day), then remount:\n\n                   sudo mount -o remount,relatime /\n",
        );
    }

    for warning in &inventory.warnings {
        output.push_str(&format!("\n  warning: {warning}\n"));
    }

    output
}

#[cfg(test)]
mod tests {
    use super::{clean, json, list, summarise};
    use crate::filter::View;
    use crate::janitor::{Kind, Reclaim, Safety, Target};
    use crate::model::{
        AtimeSupport, Entry, Facts, InstallReason, Inventory, Origin, Package, UsageEvidence,
    };

    fn inventory() -> Inventory {
        let entries = vec![
            Entry {
                package: Package {
                    name: "big-unused".to_owned(),
                    version: "1-1".to_owned(),
                    size: 1_048_576,
                    reason: InstallReason::Dependency,
                    install_date: Some(0),
                    ..Package::default()
                },
                facts: Facts {
                    required_by: Vec::new(),
                    optional_for: Vec::new(),
                    origin: Origin::Foreign,
                    usage: UsageEvidence::NeverSinceInstall { at: 0 },
                    reclaimable: 1_048_576,
                    frees: Vec::new(),
                    protected: false,
                },
            },
            Entry {
                package: Package {
                    name: "small-used".to_owned(),
                    version: "1-1".to_owned(),
                    size: 1_024,
                    reason: InstallReason::Explicit,
                    install_date: Some(0),
                    ..Package::default()
                },
                facts: Facts {
                    required_by: Vec::new(),
                    optional_for: Vec::new(),
                    origin: Origin::Repository("extra".to_owned()),
                    usage: UsageEvidence::Used {
                        at: 86_400,
                        witness: "usr/bin/small".to_owned(),
                    },
                    reclaimable: 1_024,
                    frees: Vec::new(),
                    protected: false,
                },
            },
        ];

        Inventory {
            index: entries
                .iter()
                .enumerate()
                .map(|(position, entry)| (entry.package.name.clone(), position))
                .collect(),
            entries,
            targets: vec![Target {
                kind: Kind::PacmanCacheSuperseded,
                title: "Superseded versions".to_owned(),
                location: "/var/cache/pacman/pkg".to_owned(),
                detail: "old builds".to_owned(),
                bytes: Some(2_097_152),
                items: 4,
                safety: Safety::Safe,
                reclaim: Reclaim::Advice {
                    text: "run paccache".to_owned(),
                },
            }],
            atime_support: AtimeSupport::Relatime,
            scanned_at: 172_800,
            probed: 2,
            warnings: vec!["something was odd".to_owned()],
        }
    }

    #[test]
    fn the_summary_counts_each_category() {
        let summary = summarise(&inventory());
        assert_eq!(summary.packages, 2);
        assert_eq!(summary.installed_bytes, 1_049_600);
        assert_eq!(summary.orphans, 1);
        assert_eq!(summary.orphan_bytes, 1_048_576);
        assert_eq!(summary.foreign, 1);
        assert_eq!(summary.never_used, 1);
        assert_eq!(summary.target_bytes, 2_097_152);
    }

    #[test]
    fn the_list_puts_the_biggest_first_and_labels_unused_packages() {
        let rendered = list(&inventory(), &View::default(), 10);
        let mut lines = rendered.lines();
        assert!(lines.next().unwrap_or_default().starts_with("PACKAGE"));
        let first = lines.next().unwrap_or_default();
        assert!(first.starts_with("big-unused"), "got: {first}");
        assert!(first.contains("1.0 MiB"), "got: {first}");
        assert!(first.contains("never"), "got: {first}");
        assert!(rendered.contains("2 of 2 packages shown"));
    }

    #[test]
    fn the_list_respects_its_limit() {
        let rendered = list(&inventory(), &View::default(), 1);
        assert!(rendered.contains("1 of 2 packages shown"));
        assert!(!rendered.contains("small-used"));
    }

    #[test]
    fn a_noatime_system_says_so_in_the_list() {
        let mut inventory = inventory();
        inventory.atime_support = AtimeSupport::Disabled;
        assert!(list(&inventory, &View::default(), 10).contains("noatime"));
    }

    #[test]
    fn the_clean_report_totals_and_carries_warnings() {
        let rendered = clean(&inventory());
        assert!(rendered.contains("Superseded versions"));
        assert!(rendered.contains("2.0 MiB"));
        assert!(rendered.contains("hint: run paccache"));
        assert!(rendered.contains("warning: something was odd"));
    }

    #[test]
    fn the_json_document_carries_the_summary_and_the_entries() {
        let rendered = json(&inventory()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed["summary"]["packages"], 2);
        assert_eq!(
            parsed["inventory"]["entries"][0]["package"]["name"],
            "big-unused"
        );
        assert_eq!(
            parsed["inventory"]["entries"][0]["facts"]["usage"]["state"],
            "never-since-install"
        );
        assert_eq!(parsed["inventory"]["atime_support"], "relatime");
    }
}
