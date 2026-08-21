//! Sorting and filtering the package table.
//!
//! Kept separate from the UI so that the ordering rules — which are the whole
//! point of the tool — can be tested without a terminal.

use serde::Serialize;

use crate::model::{Entry, InstallReason};

/// A column the table can be ordered by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SortKey {
    /// Package name, alphabetically.
    Name,
    /// Installed size of the package's own files.
    Size,
    /// Size freed by removing it, dependencies included.
    Reclaimable,
    /// When the package was last read.
    LastUsed,
    /// When the package was installed.
    Installed,
    /// How many packages depend on it.
    RequiredBy,
}

impl SortKey {
    /// Column heading.
    pub fn label(self) -> &'static str {
        match self {
            SortKey::Name => "name",
            SortKey::Size => "size",
            SortKey::Reclaimable => "frees",
            SortKey::LastUsed => "last used",
            SortKey::Installed => "installed",
            SortKey::RequiredBy => "needed by",
        }
    }

    /// The next key in the cycle, for a single keystroke to walk them all.
    pub fn next(self) -> SortKey {
        match self {
            SortKey::Size => SortKey::Reclaimable,
            SortKey::Reclaimable => SortKey::LastUsed,
            SortKey::LastUsed => SortKey::Installed,
            SortKey::Installed => SortKey::Name,
            SortKey::Name => SortKey::RequiredBy,
            SortKey::RequiredBy => SortKey::Size,
        }
    }

    /// Whether the key reads most naturally largest-first.
    pub fn defaults_descending(self) -> bool {
        match self {
            SortKey::Size | SortKey::Reclaimable | SortKey::RequiredBy => true,
            SortKey::Name | SortKey::LastUsed | SortKey::Installed => false,
        }
    }
}

/// A predicate narrowing the package list.
///
/// Toggles compose with AND: turning on `Orphans` and `Foreign` shows AUR
/// packages that nothing needs, which is the highest-value list on the system.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Toggle {
    /// Installed as a dependency, and nothing requires it.
    Orphans,
    /// Not in any sync repository: AUR or locally built.
    Foreign,
    /// Explicitly installed by the user.
    Explicit,
    /// No file has been read since installation.
    NeverUsed,
    /// Not read within the staleness window.
    Stale,
}

impl Toggle {
    /// Label shown in the filter bar.
    pub fn label(self) -> &'static str {
        match self {
            Toggle::Orphans => "orphans",
            Toggle::Foreign => "aur",
            Toggle::Explicit => "explicit",
            Toggle::NeverUsed => "never-used",
            Toggle::Stale => "stale",
        }
    }

    /// The key that toggles it.
    pub fn key(self) -> char {
        match self {
            Toggle::Orphans => 'o',
            Toggle::Foreign => 'a',
            Toggle::Explicit => 'e',
            Toggle::NeverUsed => 'n',
            Toggle::Stale => 'u',
        }
    }
}

/// Every toggle, in display order.
pub const TOGGLES: [Toggle; 5] = [
    Toggle::Orphans,
    Toggle::Foreign,
    Toggle::Explicit,
    Toggle::NeverUsed,
    Toggle::Stale,
];

/// The complete filter and sort state of the table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct View {
    /// Substring the package name must contain. When `match_descriptions` is
    /// on, a hit in the description counts too.
    pub query: String,
    /// Whether the query is also matched against package descriptions.
    ///
    /// Off by default: searching `lib` should find the packages *called*
    /// `lib…`, not the several hundred whose description happens to contain
    /// the word.
    pub match_descriptions: bool,
    /// Active toggles.
    pub toggles: Vec<Toggle>,
    /// Column to order by.
    pub sort: SortKey,
    /// Whether the order is reversed from the column's natural direction.
    pub descending: bool,
    /// Days without a read before a package counts as stale.
    pub stale_days: i64,
    /// Hide packages that cannot be removed without an override.
    pub hide_protected: bool,
}

impl Default for View {
    fn default() -> View {
        View {
            query: String::new(),
            match_descriptions: false,
            toggles: Vec::new(),
            sort: SortKey::Size,
            descending: true,
            stale_days: 180,
            hide_protected: false,
        }
    }
}

impl View {
    /// Turn a toggle on or off.
    pub fn toggle(&mut self, toggle: Toggle) -> bool {
        if let Some(position) = self.toggles.iter().position(|active| *active == toggle) {
            self.toggles.remove(position);
        } else {
            self.toggles.push(toggle);
        }
        true
    }

    /// Whether a toggle is on.
    pub fn is_active(&self, toggle: Toggle) -> bool {
        self.toggles.contains(&toggle)
    }

    /// Switch to `key`, or flip the direction if it is already the sort key.
    pub fn sort_by(&mut self, key: SortKey) -> bool {
        if self.sort == key {
            self.descending = !self.descending;
        } else {
            self.sort = key;
            self.descending = key.defaults_descending();
        }
        true
    }

    /// Widen or narrow the search to package descriptions.
    pub fn toggle_descriptions(&mut self) -> bool {
        self.match_descriptions = !self.match_descriptions;
        self.match_descriptions
    }

    /// What the search field is currently matching against.
    pub fn search_scope(&self) -> &'static str {
        if self.match_descriptions {
            "name+description"
        } else {
            "name"
        }
    }

    /// Whether the view is showing everything.
    pub fn is_unfiltered(&self) -> bool {
        self.query.is_empty() && self.toggles.is_empty() && !self.hide_protected
    }
}

/// Whether one entry passes the view's filters.
pub fn matches(entry: &Entry, view: &View, now: i64) -> bool {
    if view.hide_protected && entry.facts.protected {
        return false;
    }

    if !view.query.is_empty() {
        let needle = view.query.to_lowercase();
        let mut hit = entry.package.name.to_lowercase().contains(&needle);
        if !hit && view.match_descriptions {
            hit = entry.package.description.to_lowercase().contains(&needle);
        }
        if !hit {
            return false;
        }
    }

    view.toggles
        .iter()
        .all(|toggle| passes(entry, *toggle, view.stale_days, now))
}

/// Whether one entry passes a single toggle.
fn passes(entry: &Entry, toggle: Toggle, stale_days: i64, now: i64) -> bool {
    match toggle {
        Toggle::Orphans => {
            entry.package.reason == InstallReason::Dependency && entry.facts.is_orphan()
        }
        Toggle::Foreign => entry.facts.origin.is_foreign(),
        Toggle::Explicit => entry.package.reason == InstallReason::Explicit,
        Toggle::NeverUsed => entry.facts.usage.is_unused(),
        Toggle::Stale => match entry.facts.usage.timestamp() {
            Some(timestamp) => crate::format::days_since(now, timestamp) >= stale_days,
            None => false,
        },
    }
}

/// Order the indices of `entries` according to `view`.
///
/// Package name breaks every tie, so the order is total and the table never
/// reshuffles between redraws. Entries with no last-use timestamp sort as
/// oldest: they are the ones most worth looking at.
pub fn order(entries: &[Entry], view: &View, now: i64) -> Vec<usize> {
    let mut visible: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_position, entry)| matches(entry, view, now))
        .map(|(position, _entry)| position)
        .collect();

    visible.sort_by(|left, right| {
        let (Some(left_entry), Some(right_entry)) = (entries.get(*left), entries.get(*right))
        else {
            return std::cmp::Ordering::Equal;
        };

        let ordering = match view.sort {
            SortKey::Name => left_entry.package.name.cmp(&right_entry.package.name),
            SortKey::Size => left_entry.package.size.cmp(&right_entry.package.size),
            SortKey::Reclaimable => left_entry
                .facts
                .reclaimable
                .cmp(&right_entry.facts.reclaimable),
            SortKey::LastUsed => sort_timestamp(left_entry).cmp(&sort_timestamp(right_entry)),
            SortKey::Installed => left_entry
                .package
                .install_date
                .unwrap_or(i64::MIN)
                .cmp(&right_entry.package.install_date.unwrap_or(i64::MIN)),
            SortKey::RequiredBy => left_entry
                .facts
                .required_by
                .len()
                .cmp(&right_entry.facts.required_by.len()),
        };

        let ordering = if view.descending {
            ordering.reverse()
        } else {
            ordering
        };

        ordering.then_with(|| left_entry.package.name.cmp(&right_entry.package.name))
    });

    visible
}

/// Timestamp used when ordering by last use.
fn sort_timestamp(entry: &Entry) -> i64 {
    entry.facts.usage.timestamp().unwrap_or(i64::MIN)
}

#[cfg(test)]
mod tests {
    use super::{matches, order, SortKey, Toggle, View};
    use crate::model::{Entry, Facts, InstallReason, Origin, Package, UsageEvidence};

    const NOW: i64 = 1_700_000_000;
    const DAY: i64 = 86_400;

    fn entry(
        name: &str,
        size: u64,
        reason: InstallReason,
        origin: Origin,
        usage: UsageEvidence,
        required_by: &[&str],
    ) -> Entry {
        Entry {
            package: Package {
                name: name.to_owned(),
                size,
                reason,
                install_date: Some(NOW - 400 * DAY),
                description: format!("the {name} package"),
                ..Package::default()
            },
            facts: Facts {
                required_by: required_by.iter().map(|item| (*item).to_owned()).collect(),
                optional_for: Vec::new(),
                origin,
                usage,
                reclaimable: size,
                frees: Vec::new(),
                protected: false,
            },
        }
    }

    fn corpus() -> Vec<Entry> {
        vec![
            entry(
                "aur-hog",
                900,
                InstallReason::Explicit,
                Origin::Foreign,
                UsageEvidence::NeverSinceInstall {
                    at: NOW - 400 * DAY,
                },
                &[],
            ),
            entry(
                "daily-driver",
                100,
                InstallReason::Explicit,
                Origin::Repository("extra".to_owned()),
                UsageEvidence::Used {
                    at: NOW - DAY,
                    witness: "usr/bin/dd".to_owned(),
                },
                &[],
            ),
            entry(
                "leftover-lib",
                500,
                InstallReason::Dependency,
                Origin::Repository("extra".to_owned()),
                UsageEvidence::NoWitness,
                &[],
            ),
            entry(
                "busy-lib",
                300,
                InstallReason::Dependency,
                Origin::Repository("core".to_owned()),
                UsageEvidence::Used {
                    at: NOW - 10 * DAY,
                    witness: "usr/lib/libbusy.so".to_owned(),
                },
                &["daily-driver", "aur-hog"],
            ),
        ]
    }

    fn names(entries: &[Entry], view: &View) -> Vec<String> {
        order(entries, view, NOW)
            .into_iter()
            .filter_map(|position| entries.get(position))
            .map(|entry| entry.package.name.clone())
            .collect()
    }

    #[test]
    fn the_default_view_is_biggest_first() {
        let entries = corpus();
        assert_eq!(
            names(&entries, &View::default()),
            vec!["aur-hog", "leftover-lib", "busy-lib", "daily-driver"]
        );
    }

    #[test]
    fn sorting_by_a_new_key_uses_its_natural_direction() {
        let mut view = View::default();
        view.sort_by(SortKey::Name);
        assert!(!view.descending);
        assert_eq!(
            names(&corpus(), &view),
            vec!["aur-hog", "busy-lib", "daily-driver", "leftover-lib"]
        );
    }

    #[test]
    fn sorting_by_the_same_key_twice_reverses_it() {
        let mut view = View::default();
        assert!(view.descending);
        view.sort_by(SortKey::Size);
        assert!(!view.descending);
        assert_eq!(
            names(&corpus(), &view),
            vec!["daily-driver", "busy-lib", "leftover-lib", "aur-hog"]
        );
    }

    #[test]
    fn unknown_last_use_sorts_as_oldest() {
        let mut view = View::default();
        view.sort_by(SortKey::LastUsed);
        // Ascending: no evidence first, then oldest read, then newest.
        assert_eq!(
            names(&corpus(), &view),
            vec!["leftover-lib", "aur-hog", "busy-lib", "daily-driver"]
        );
    }

    #[test]
    fn the_orphan_toggle_needs_both_conditions() {
        let mut view = View::default();
        view.toggle(Toggle::Orphans);
        // busy-lib is a dependency but two packages need it.
        assert_eq!(names(&corpus(), &view), vec!["leftover-lib"]);
    }

    #[test]
    fn toggles_compose_with_and() {
        let mut view = View::default();
        view.toggle(Toggle::Foreign);
        view.toggle(Toggle::NeverUsed);
        assert_eq!(names(&corpus(), &view), vec!["aur-hog"]);

        view.toggle(Toggle::Orphans);
        assert!(names(&corpus(), &view).is_empty());
    }

    #[test]
    fn the_stale_toggle_respects_the_window() {
        let mut view = View {
            stale_days: 5,
            ..View::default()
        };
        view.toggle(Toggle::Stale);
        assert_eq!(names(&corpus(), &view), vec!["aur-hog", "busy-lib"]);

        view.stale_days = 365;
        assert_eq!(names(&corpus(), &view), vec!["aur-hog"]);
    }

    #[test]
    fn the_query_matches_names_case_insensitively() {
        let mut view = View::default();
        "HOG".clone_into(&mut view.query);
        assert_eq!(names(&corpus(), &view), vec!["aur-hog"]);

        "-lib".clone_into(&mut view.query);
        assert_eq!(names(&corpus(), &view), vec!["leftover-lib", "busy-lib"]);

        "nothing-matches".clone_into(&mut view.query);
        assert!(names(&corpus(), &view).is_empty());
    }

    #[test]
    fn descriptions_are_searched_only_when_asked_for() {
        let mut view = View::default();
        // Every fixture description is "the <name> package", so a query that
        // only appears in descriptions must find nothing by default.
        "package".clone_into(&mut view.query);
        assert!(names(&corpus(), &view).is_empty());

        assert!(view.toggle_descriptions());
        assert_eq!(names(&corpus(), &view).len(), 4);
        assert_eq!(view.search_scope(), "name+description");

        assert!(!view.toggle_descriptions());
        assert!(names(&corpus(), &view).is_empty());
        assert_eq!(view.search_scope(), "name");
    }

    #[test]
    fn protected_packages_can_be_hidden() {
        let mut entries = corpus();
        if let Some(first) = entries.get_mut(0) {
            first.facts.protected = true;
        }
        let view = View {
            hide_protected: true,
            ..View::default()
        };
        assert!(!names(&entries, &view).contains(&"aur-hog".to_owned()));
    }

    #[test]
    fn ties_are_broken_by_name_so_the_order_is_stable() {
        let entries = vec![
            entry(
                "zzz",
                100,
                InstallReason::Explicit,
                Origin::Unknown,
                UsageEvidence::NotProbed,
                &[],
            ),
            entry(
                "aaa",
                100,
                InstallReason::Explicit,
                Origin::Unknown,
                UsageEvidence::NotProbed,
                &[],
            ),
        ];
        assert_eq!(names(&entries, &View::default()), vec!["aaa", "zzz"]);
    }

    #[test]
    fn an_unfiltered_view_reports_itself_as_such() {
        let mut view = View::default();
        assert!(view.is_unfiltered());
        view.toggle(Toggle::Orphans);
        assert!(!view.is_unfiltered());
    }

    #[test]
    fn matches_is_consistent_with_order() {
        let entries = corpus();
        let mut view = View::default();
        view.toggle(Toggle::Orphans);
        let ordered = order(&entries, &view, NOW).len();
        let matched = entries
            .iter()
            .filter(|entry| matches(entry, &view, NOW))
            .count();
        assert_eq!(ordered, matched);
    }
}
