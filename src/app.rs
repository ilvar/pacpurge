//! The interactive state machine.
//!
//! No effects: [`App::handle`] takes an [`Intent`] and returns an [`Action`]
//! describing what the outer loop should do about it. Running commands and
//! re-scanning happen in `main`, which keeps every state transition testable
//! without a terminal or an Arch system.

use std::collections::BTreeSet;

use ratatui::widgets::TableState;

use crate::filter::{self, SortKey, Toggle, View};
use crate::format;
use crate::janitor::{Reclaim, Target};
use crate::model::Inventory;
use crate::plan::{self, Plan, Step};

/// Which tab is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    /// The package table.
    Packages,
    /// Reclaimable space outside the package set.
    Janitor,
}

impl Tab {
    /// Tab title.
    pub fn title(self) -> &'static str {
        match self {
            Tab::Packages => "Packages",
            Tab::Janitor => "Reclaim",
        }
    }

    /// The other tab.
    pub fn other(self) -> Tab {
        match self {
            Tab::Packages => Tab::Janitor,
            Tab::Janitor => Tab::Packages,
        }
    }
}

/// Something the user asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    /// Leave the program.
    Quit,
    /// Move to the next tab.
    NextTab,
    /// Move to the previous tab.
    PrevTab,
    /// Move the cursor up one row.
    Up,
    /// Move the cursor down one row.
    Down,
    /// Move the cursor up one screen.
    PageUp,
    /// Move the cursor down one screen.
    PageDown,
    /// Move to the first row.
    First,
    /// Move to the last row.
    Last,
    /// Select or deselect the row under the cursor.
    ToggleSelect,
    /// Select the row even though it is protected.
    ForceSelect,
    /// Deselect everything.
    ClearSelection,
    /// Focus the search field.
    StartSearch,
    /// Add a character to the search field.
    SearchInput(char),
    /// Delete the last character of the search field.
    SearchBackspace,
    /// Leave the search field, keeping the query.
    SearchCommit,
    /// Leave the search field, discarding the query.
    SearchCancel,
    /// Turn a filter on or off.
    Filter(Toggle),
    /// Show or hide protected packages.
    ToggleProtected,
    /// Widen or narrow the search to package descriptions.
    ToggleDescriptions,
    /// Move to the next sort column.
    CycleSort,
    /// Reverse the current sort.
    ReverseSort,
    /// Sort by a specific column.
    SortBy(SortKey),
    /// Review what the current selection would do.
    Review,
    /// Confirm the pending action.
    Accept,
    /// Dismiss an overlay, or clear the search.
    Cancel,
    /// Show the key bindings.
    Help,
    /// Re-read the system.
    Rescan,
}

/// What the outer loop should do after a state transition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing changed; no redraw needed.
    Idle,
    /// Redraw the interface.
    Redraw,
    /// Leave the program.
    Quit,
    /// Leave the alternate screen, run these steps, then come back.
    Run {
        /// Steps in the order they should run.
        steps: Vec<Step>,
        /// What to tell the user before running them.
        summary: String,
    },
    /// Re-read the system and rebuild the state.
    Rescan,
}

/// A modal shown over the main interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Overlay {
    /// No modal.
    None,
    /// The key bindings.
    Help,
    /// A confirmation prompt.
    Confirm {
        /// Modal title.
        title: String,
        /// Body lines.
        lines: Vec<String>,
        /// Exactly what will run.
        steps: Vec<Step>,
        /// Whether to draw the modal as a warning.
        danger: bool,
    },
    /// Something the user should read, with nothing to confirm.
    Notice {
        /// Modal title.
        title: String,
        /// Body lines.
        lines: Vec<String>,
    },
    /// A proposal to widen the selection, which the user can accept.
    Offer {
        /// Modal title.
        title: String,
        /// Body lines.
        lines: Vec<String>,
        /// Positions to add to the selection on acceptance.
        add: Vec<usize>,
    },
}

/// The complete interface state.
pub struct App {
    /// The scan this interface is showing.
    pub inventory: Inventory,
    /// Filter and sort state.
    pub view: View,
    /// Which tab is showing.
    pub tab: Tab,
    /// Positions of the visible packages, in display order.
    pub order: Vec<usize>,
    /// Table scroll and cursor state for the packages tab.
    pub table: TableState,
    /// Table scroll and cursor state for the reclaim tab.
    pub targets_table: TableState,
    /// Packages marked for removal, as positions into the inventory.
    pub selection: BTreeSet<usize>,
    /// Whether the search field has focus.
    pub searching: bool,
    /// The modal currently showing.
    pub overlay: Overlay,
    /// One-line message under the table.
    pub status: String,
    /// Whether execution is suppressed.
    pub dry_run: bool,
    /// Rows the table can show, used for paging.
    pub page: usize,
}

impl App {
    /// Build the interface state around a scan.
    pub fn new(inventory: Inventory, view: View, dry_run: bool) -> App {
        let mut app = App {
            order: Vec::new(),
            inventory,
            view,
            tab: Tab::Packages,
            table: TableState::default(),
            targets_table: TableState::default(),
            selection: BTreeSet::new(),
            searching: false,
            overlay: Overlay::None,
            status: String::new(),
            dry_run,
            page: 20,
        };
        app.refresh();
        app.status = app.opening_status();
        app
    }

    /// Replace the scan, keeping filters and cursor position where possible.
    pub fn adopt(&mut self, inventory: Inventory) -> bool {
        self.inventory = inventory;
        self.selection.clear();
        self.refresh();
        self.status = self.opening_status();
        true
    }

    /// Recompute the visible order and clamp the cursor into it.
    pub fn refresh(&mut self) -> bool {
        self.order = filter::order(
            &self.inventory.entries,
            &self.view,
            self.inventory.scanned_at,
        );

        let selected = match self.order.is_empty() {
            true => None,
            false => Some(
                self.table
                    .selected()
                    .unwrap_or(0)
                    .min(self.order.len().saturating_sub(1)),
            ),
        };
        self.table.select(selected);

        let target_selected = match self.inventory.targets.is_empty() {
            true => None,
            false => Some(
                self.targets_table
                    .selected()
                    .unwrap_or(0)
                    .min(self.inventory.targets.len().saturating_sub(1)),
            ),
        };
        self.targets_table.select(target_selected);
        true
    }

    /// The message shown when a scan finishes.
    fn opening_status(&self) -> String {
        let summary = format!(
            "{} packages, {} installed, {} probed for last use",
            self.inventory.entries.len(),
            format::bytes(self.inventory.total_size()),
            self.inventory.probed
        );
        if self.inventory.atime_support.is_meaningful() {
            summary
        } else {
            format!("{summary} — {}", self.inventory.atime_support.caveat())
        }
    }

    /// The entry under the cursor on the packages tab.
    pub fn current_entry(&self) -> Option<&crate::model::Entry> {
        let row = self.table.selected()?;
        let position = self.order.get(row)?;
        self.inventory.entries.get(*position)
    }

    /// The target under the cursor on the reclaim tab.
    pub fn current_target(&self) -> Option<&Target> {
        let row = self.targets_table.selected()?;
        self.inventory.targets.get(row)
    }

    /// Bytes the current selection would free, cascade included.
    pub fn selection_bytes(&self) -> u64 {
        self.plan().bytes
    }

    /// The removal plan for the current selection.
    pub fn plan(&self) -> Plan {
        plan::build(&self.inventory, &self.selection)
    }

    /// Apply an intent.
    pub fn handle(&mut self, intent: Intent) -> Action {
        // A modal owns the keyboard while it is open, so nothing leaks
        // through to the table underneath.
        if !matches!(self.overlay, Overlay::None) {
            let overlay = std::mem::replace(&mut self.overlay, Overlay::None);
            return self.handle_modal(overlay, modal_intent(intent));
        }

        match intent {
            Intent::Quit => Action::Quit,
            Intent::Help => {
                self.overlay = Overlay::Help;
                Action::Redraw
            }
            Intent::Rescan => Action::Rescan,
            Intent::NextTab | Intent::PrevTab => {
                self.tab = self.tab.other();
                Action::Redraw
            }

            Intent::Up => self.move_cursor(-1),
            Intent::Down => self.move_cursor(1),
            Intent::PageUp => {
                let page = i64::try_from(self.page).unwrap_or(20);
                self.move_cursor(-page)
            }
            Intent::PageDown => {
                let page = i64::try_from(self.page).unwrap_or(20);
                self.move_cursor(page)
            }
            Intent::First => self.jump(0),
            Intent::Last => {
                let last = self.row_count().saturating_sub(1);
                self.jump(last)
            }

            Intent::ToggleSelect => self.toggle_selection(false),
            Intent::ForceSelect => self.toggle_selection(true),
            Intent::ClearSelection => {
                self.selection.clear();
                "selection cleared".clone_into(&mut self.status);
                Action::Redraw
            }

            Intent::StartSearch => {
                self.searching = true;
                "type to filter by name or description; Enter to keep it, Esc to drop it"
                    .clone_into(&mut self.status);
                Action::Redraw
            }
            Intent::SearchInput(character) => {
                self.view.query.push(character);
                self.refresh();
                Action::Redraw
            }
            Intent::SearchBackspace => {
                self.view.query.pop();
                self.refresh();
                Action::Redraw
            }
            Intent::SearchCommit => {
                self.searching = false;
                Action::Redraw
            }
            Intent::SearchCancel => {
                self.searching = false;
                self.view.query.clear();
                self.refresh();
                Action::Redraw
            }
            Intent::Cancel => {
                if self.view.query.is_empty() && self.view.toggles.is_empty() {
                    return Action::Idle;
                }
                self.view.query.clear();
                self.view.toggles.clear();
                self.refresh();
                "filters cleared".clone_into(&mut self.status);
                Action::Redraw
            }

            Intent::Filter(toggle) => {
                self.view.toggle(toggle);
                self.refresh();
                self.status = self.describe_filters();
                Action::Redraw
            }
            Intent::ToggleProtected => {
                self.view.hide_protected = !self.view.hide_protected;
                self.refresh();
                self.status = if self.view.hide_protected {
                    "hiding packages that the base system depends on".to_owned()
                } else {
                    "showing every package".to_owned()
                };
                Action::Redraw
            }
            Intent::ToggleDescriptions => {
                let widened = self.view.toggle_descriptions();
                self.refresh();
                self.status = if widened {
                    "searching names and descriptions".to_owned()
                } else {
                    "searching names only".to_owned()
                };
                Action::Redraw
            }

            Intent::CycleSort => {
                let next = self.view.sort.next();
                self.view.sort_by(next);
                self.refresh();
                self.status = format!("sorted by {}", self.view.sort.label());
                Action::Redraw
            }
            Intent::ReverseSort => {
                self.view.descending = !self.view.descending;
                self.refresh();
                Action::Redraw
            }
            Intent::SortBy(key) => {
                self.view.sort_by(key);
                self.refresh();
                self.status = format!("sorted by {}", self.view.sort.label());
                Action::Redraw
            }

            Intent::Review => self.review(),
            Intent::Accept => self.review(),
        }
    }

    /// Apply an intent to whichever modal is open.
    ///
    /// The overlay is passed by value and re-installed on [`ModalIntent::Ignore`],
    /// which keeps each arm free to consume the modal's contents.
    fn handle_modal(&mut self, overlay: Overlay, intent: ModalIntent) -> Action {
        match overlay {
            Overlay::None => Action::Idle,

            Overlay::Help => match intent {
                ModalIntent::Accept | ModalIntent::Dismiss => Action::Redraw,
                ModalIntent::Ignore => {
                    self.overlay = Overlay::Help;
                    Action::Idle
                }
            },

            Overlay::Notice { title, lines } => match intent {
                ModalIntent::Accept | ModalIntent::Dismiss => Action::Redraw,
                ModalIntent::Ignore => {
                    self.overlay = Overlay::Notice { title, lines };
                    Action::Idle
                }
            },

            Overlay::Confirm {
                title,
                lines,
                steps,
                danger,
            } => match intent {
                ModalIntent::Accept => {
                    let summary = self.describe(&steps);
                    if self.dry_run {
                        "dry run: nothing was executed. The commands are printed above."
                            .clone_into(&mut self.status);
                        return Action::Run {
                            steps: Vec::new(),
                            summary,
                        };
                    }
                    Action::Run { steps, summary }
                }
                ModalIntent::Dismiss => {
                    "cancelled".clone_into(&mut self.status);
                    Action::Redraw
                }
                ModalIntent::Ignore => {
                    self.overlay = Overlay::Confirm {
                        title,
                        lines,
                        steps,
                        danger,
                    };
                    Action::Idle
                }
            },

            Overlay::Offer { title, lines, add } => match intent {
                ModalIntent::Accept => {
                    let added = add.len();
                    self.selection.extend(add);
                    self.status = format!("marked {added} more package(s)");
                    // Straight back to the review, which is now consistent.
                    self.review_packages()
                }
                ModalIntent::Dismiss => {
                    "left the selection as it was".clone_into(&mut self.status);
                    Action::Redraw
                }
                ModalIntent::Ignore => {
                    self.overlay = Overlay::Offer { title, lines, add };
                    Action::Idle
                }
            },
        }
    }

    /// Rows in the table currently showing.
    fn row_count(&self) -> usize {
        match self.tab {
            Tab::Packages => self.order.len(),
            Tab::Janitor => self.inventory.targets.len(),
        }
    }

    /// Move the cursor by `delta` rows, clamped to the table.
    fn move_cursor(&mut self, delta: i64) -> Action {
        let count = self.row_count();
        if count == 0 {
            return Action::Idle;
        }

        let state = match self.tab {
            Tab::Packages => &mut self.table,
            Tab::Janitor => &mut self.targets_table,
        };

        let current = i64::try_from(state.selected().unwrap_or(0)).unwrap_or(0);
        let last = i64::try_from(count.saturating_sub(1)).unwrap_or(0);
        let next = current.saturating_add(delta).clamp(0, last);
        let next = usize::try_from(next).unwrap_or(0);

        if state.selected() == Some(next) {
            return Action::Idle;
        }
        state.select(Some(next));
        Action::Redraw
    }

    /// Move the cursor to an absolute row.
    fn jump(&mut self, row: usize) -> Action {
        if self.row_count() == 0 {
            return Action::Idle;
        }
        let row = row.min(self.row_count().saturating_sub(1));
        match self.tab {
            Tab::Packages => self.table.select(Some(row)),
            Tab::Janitor => self.targets_table.select(Some(row)),
        }
        Action::Redraw
    }

    /// Mark or unmark the row under the cursor.
    fn toggle_selection(&mut self, force: bool) -> Action {
        match self.tab {
            Tab::Packages => {
                let Some(row) = self.table.selected() else {
                    return Action::Idle;
                };
                let Some(position) = self.order.get(row).copied() else {
                    return Action::Idle;
                };
                let Some(entry) = self.inventory.entries.get(position) else {
                    return Action::Idle;
                };

                if self.selection.contains(&position) {
                    self.selection.remove(&position);
                    self.status = format!("{} unmarked", entry.package.name);
                    return Action::Redraw;
                }

                if entry.facts.protected && !force {
                    self.status = format!(
                        "{} is part of the base system. Press P to mark it anyway.",
                        entry.package.name
                    );
                    return Action::Redraw;
                }

                self.selection.insert(position);
                let bytes = self.selection_bytes();
                self.status = format!(
                    "{} marked — {} would be freed in total",
                    entry.package.name,
                    format::bytes(bytes)
                );
                Action::Redraw
            }
            Tab::Janitor => {
                let _ = force;
                self.review()
            }
        }
    }

    /// Build the confirmation modal for whatever the current tab is showing.
    fn review(&mut self) -> Action {
        match self.tab {
            Tab::Packages => self.review_packages(),
            Tab::Janitor => self.review_target(),
        }
    }

    /// Confirmation modal for the marked packages.
    fn review_packages(&mut self) -> Action {
        let plan = self.plan();

        if plan.selected.is_empty() {
            "nothing is marked — press space to mark a package".clone_into(&mut self.status);
            return Action::Redraw;
        }

        if !plan.broken.is_empty() {
            return self.offer_to_widen(&plan);
        }

        let mut lines = vec![
            format!("Remove {} package(s):", plan.selected.len()),
            format!("  {}", plan.selected.join(" ")),
        ];

        if !plan.cascade.is_empty() {
            lines.push(String::new());
            lines.push(format!(
                "pacman will also take {} dependency package(s) nothing else needs:",
                plan.cascade.len()
            ));
            lines.push(format!("  {}", plan.cascade.join(" ")));
        }

        if !plan.protected.is_empty() {
            lines.push(String::new());
            lines.push(format!(
                "WARNING: {} is part of the base system.",
                plan.protected.join(", ")
            ));
        }

        lines.push(String::new());
        lines.push(format!("Frees {}.", format::bytes(plan.bytes)));
        lines.push(String::new());
        lines.push(format!("Runs: {}", plan.command().to_shell()));
        lines.push(
            "pacman will ask for its own confirmation before it removes anything.".to_owned(),
        );

        self.overlay = Overlay::Confirm {
            title: "Review removal".to_owned(),
            lines,
            steps: vec![Step::Run {
                command: plan.command(),
            }],
            danger: !plan.protected.is_empty(),
        };
        Action::Redraw
    }

    /// Propose marking everything that depends on the current selection.
    ///
    /// pacman would refuse the transaction as it stands, and the fix is nearly
    /// always to remove the dependants too. Rather than leave the user to
    /// discover the closure one refusal at a time, the whole set is offered at
    /// once — unless it reaches the base system, in which case accepting a
    /// prompt is far too casual a way to uninstall half the machine.
    fn offer_to_widen(&mut self, plan: &Plan) -> Action {
        let widening = plan::widen(&self.inventory, &self.selection);

        if !widening.is_offerable() {
            let mut lines = vec![format!(
                "These packages still need what you marked: {}",
                plan.broken.join(", ")
            )];

            if widening.protected.is_empty() {
                lines.push(String::new());
                lines.push(
                    "Unmark whatever they depend on. pacman would refuse this transaction as it \
                     stands."
                        .to_owned(),
                );
            } else {
                lines.push(String::new());
                lines.push(format!(
                    "Removing them in turn would reach {} package(s) the base system depends on, \
                     including {}.",
                    widening.protected.len(),
                    preview(&widening.protected, 4)
                ));
                lines.push(String::new());
                lines.push(
                    "pacpurge will not offer to mark those for you. Unmark whatever they depend \
                     on instead, or mark them one at a time with P if you really mean it."
                        .to_owned(),
                );
            }

            self.overlay = Overlay::Notice {
                title: "That would break installed software".to_owned(),
                lines,
            };
            return Action::Redraw;
        }

        let lines = vec![
            format!(
                "{} package(s) still need what you marked:",
                widening.names.len()
            ),
            format!("  {}", preview(&widening.names, 12)),
            String::new(),
            "pacman would refuse the removal while they are installed. Marking them too makes it \
             consistent."
                .to_owned(),
            String::new(),
            format!(
                "That brings the total to {}, up from {}.",
                format::bytes(widening.bytes),
                format::bytes(plan.bytes)
            ),
        ];

        self.overlay = Overlay::Offer {
            title: "Mark what depends on this too?".to_owned(),
            lines,
            add: widening.positions,
        };
        Action::Redraw
    }

    /// Confirmation modal for the reclaim target under the cursor.
    fn review_target(&mut self) -> Action {
        let Some(target) = self.current_target() else {
            return Action::Idle;
        };

        let title = target.title.clone();
        let detail = target.detail.clone();
        let size = match target.bytes {
            Some(bytes) => format::bytes(bytes),
            None => "an unknown amount".to_owned(),
        };

        let Some(step) = plan::step_for(target) else {
            let advice = match &target.reclaim {
                Reclaim::Handoff { hint } => format!("Handled on the {hint}."),
                Reclaim::Advice { text } => text.clone(),
                Reclaim::Run { command: _ }
                | Reclaim::Paths {
                    paths: _,
                    needs_root: _,
                } => String::new(),
            };
            self.overlay = Overlay::Notice {
                title,
                lines: vec![detail, String::new(), advice],
            };
            return Action::Redraw;
        };

        let mut lines = vec![detail, String::new(), format!("Frees {size}.")];

        if let Reclaim::Paths { paths, needs_root } = &target.reclaim {
            let _ = needs_root;
            lines.push(String::new());
            lines.push(format!("Deletes {} path(s), starting with:", paths.len()));
            for path in paths.iter().take(6) {
                lines.push(format!("  {}", path.display()));
            }
            if paths.len() > 6 {
                lines.push(format!("  … and {} more", paths.len().saturating_sub(6)));
            }
        }

        lines.push(String::new());
        lines.push(format!("Runs: {}", step.to_shell()));

        let danger = target.safety == crate::janitor::Safety::Careful;
        self.overlay = Overlay::Confirm {
            title,
            lines,
            steps: vec![step],
            danger,
        };
        Action::Redraw
    }

    /// A one-line description of what is about to run.
    fn describe(&self, steps: &[Step]) -> String {
        steps
            .iter()
            .map(Step::to_shell)
            .collect::<Vec<String>>()
            .join("\n")
    }

    /// A status line naming the active filters.
    fn describe_filters(&self) -> String {
        if self.view.toggles.is_empty() {
            return format!("{} packages", self.order.len());
        }

        let names: Vec<&str> = self
            .view
            .toggles
            .iter()
            .map(|toggle| toggle.label())
            .collect();
        let bytes = self
            .order
            .iter()
            .filter_map(|position| self.inventory.entries.get(*position))
            .map(|entry| entry.package.size)
            .fold(0u64, u64::saturating_add);

        format!(
            "{} packages match {} — {} installed",
            self.order.len(),
            names.join(" + "),
            format::bytes(bytes)
        )
    }
}

/// Join names for a modal, trimming a long list to `limit` entries.
fn preview(names: &[String], limit: usize) -> String {
    if names.len() <= limit {
        return names.join(" ");
    }
    let shown = names
        .iter()
        .take(limit)
        .cloned()
        .collect::<Vec<String>>()
        .join(" ");
    let rest = names.len().saturating_sub(limit);
    format!("{shown} … and {rest} more")
}

/// How an intent is treated while a modal has focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModalIntent {
    /// Say yes to whatever the modal is asking.
    Accept,
    /// Close the modal without acting on it.
    Dismiss,
    /// Swallowed, so a stray key cannot reach the table underneath.
    Ignore,
}

/// Classify an intent for a modal.
///
/// One exhaustive match rather than one per overlay: a new [`Intent`] variant
/// then forces a decision in a single place instead of four.
fn modal_intent(intent: Intent) -> ModalIntent {
    match intent {
        Intent::Accept => ModalIntent::Accept,
        // `?` closes whatever is open, the same as Esc.
        Intent::Quit | Intent::Cancel | Intent::Help => ModalIntent::Dismiss,
        Intent::NextTab
        | Intent::PrevTab
        | Intent::Up
        | Intent::Down
        | Intent::PageUp
        | Intent::PageDown
        | Intent::First
        | Intent::Last
        | Intent::ToggleSelect
        | Intent::ForceSelect
        | Intent::ClearSelection
        | Intent::StartSearch
        | Intent::SearchInput(_)
        | Intent::SearchBackspace
        | Intent::SearchCommit
        | Intent::SearchCancel
        | Intent::Filter(_)
        | Intent::ToggleProtected
        | Intent::ToggleDescriptions
        | Intent::CycleSort
        | Intent::ReverseSort
        | Intent::SortBy(_)
        | Intent::Review
        | Intent::Rescan => ModalIntent::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, App, Intent, Overlay, Tab};
    use crate::filter::{SortKey, Toggle, View};
    use crate::model::{
        AtimeSupport, Entry, Facts, InstallReason, Inventory, Origin, Package, UsageEvidence,
    };

    fn entry(
        name: &str,
        size: u64,
        reason: InstallReason,
        protected: bool,
        depends: &[&str],
    ) -> Entry {
        Entry {
            package: Package {
                name: name.to_owned(),
                version: "1-1".to_owned(),
                size,
                reason,
                depends: depends.iter().map(|item| (*item).to_owned()).collect(),
                install_date: Some(0),
                ..Package::default()
            },
            facts: Facts {
                required_by: Vec::new(),
                optional_for: Vec::new(),
                origin: Origin::Foreign,
                usage: UsageEvidence::NeverSinceInstall { at: 0 },
                reclaimable: size,
                frees: Vec::new(),
                protected,
            },
        }
    }

    fn app() -> App {
        let entries = vec![
            entry("big", 900, InstallReason::Explicit, false, &[]),
            entry("medium", 500, InstallReason::Explicit, false, &["small"]),
            entry("small", 100, InstallReason::Dependency, false, &[]),
            entry("glibc", 300, InstallReason::Explicit, true, &[]),
        ];
        let inventory = Inventory {
            index: entries
                .iter()
                .enumerate()
                .map(|(position, entry)| (entry.package.name.clone(), position))
                .collect(),
            entries,
            targets: Vec::new(),
            atime_support: AtimeSupport::Relatime,
            scanned_at: 86_400,
            probed: 4,
            warnings: Vec::new(),
        };
        App::new(inventory, View::default(), false)
    }

    fn current(app: &App) -> String {
        app.current_entry()
            .map(|entry| entry.package.name.clone())
            .unwrap_or_default()
    }

    #[test]
    fn the_cursor_starts_on_the_biggest_package() {
        assert_eq!(current(&app()), "big");
    }

    #[test]
    fn navigation_clamps_at_both_ends() {
        let mut app = app();
        assert_eq!(app.handle(Intent::Up), Action::Idle);
        assert_eq!(current(&app), "big");

        assert_eq!(app.handle(Intent::Last), Action::Redraw);
        assert_eq!(current(&app), "small");
        assert_eq!(app.handle(Intent::Down), Action::Idle);
    }

    #[test]
    fn marking_a_package_reports_what_it_frees() {
        let mut app = app();
        // Default order is largest first: big, medium, glibc, small.
        app.handle(Intent::Down);
        assert_eq!(current(&app), "medium");

        assert_eq!(app.handle(Intent::ToggleSelect), Action::Redraw);
        // medium is the last thing needing small, so both go: 500 + 100.
        assert_eq!(app.selection_bytes(), 600);
        assert!(app.status.contains("600 B"), "status was: {}", app.status);
    }

    #[test]
    fn protected_packages_need_an_explicit_override() {
        let mut app = app();
        app.view.sort_by(SortKey::Name);
        app.refresh();
        app.handle(Intent::First);
        assert_eq!(current(&app), "big");
        app.handle(Intent::Down);
        assert_eq!(current(&app), "glibc");

        app.handle(Intent::ToggleSelect);
        assert!(app.selection.is_empty());
        assert!(app.status.contains("Press P"), "status was: {}", app.status);

        app.handle(Intent::ForceSelect);
        assert_eq!(app.selection.len(), 1);
    }

    #[test]
    fn toggling_a_selection_twice_clears_it() {
        let mut app = app();
        app.handle(Intent::ToggleSelect);
        assert_eq!(app.selection.len(), 1);
        app.handle(Intent::ToggleSelect);
        assert!(app.selection.is_empty());
    }

    #[test]
    fn reviewing_nothing_says_so_rather_than_opening_a_modal() {
        let mut app = app();
        app.handle(Intent::Review);
        assert_eq!(app.overlay, Overlay::None);
        assert!(app.status.contains("nothing is marked"));
    }

    /// Mark `small`, which `medium` still depends on.
    fn app_with_a_breaking_selection() -> App {
        let mut app = app();
        app.view.sort_by(SortKey::Name);
        app.refresh();
        app.handle(Intent::Last);
        assert_eq!(current(&app), "small");
        app.handle(Intent::ToggleSelect);
        app
    }

    #[test]
    fn reviewing_a_breaking_removal_offers_to_mark_the_dependants() {
        let mut app = app_with_a_breaking_selection();
        app.handle(Intent::Review);

        match &app.overlay {
            Overlay::Offer { title, lines, add } => {
                assert!(title.contains("depends on this"), "{title}");
                assert!(
                    lines.iter().any(|line| line.contains("medium")),
                    "{lines:?}"
                );
                assert_eq!(add.len(), 1);
            }
            Overlay::None | Overlay::Help | Overlay::Notice { .. } | Overlay::Confirm { .. } => {
                panic!("expected an offer, got {:?}", app.overlay)
            }
        }
    }

    #[test]
    fn accepting_the_offer_marks_them_and_reopens_the_review() {
        let mut app = app_with_a_breaking_selection();
        app.handle(Intent::Review);
        assert_eq!(app.handle(Intent::Accept), Action::Redraw);

        // `medium` joined the selection, and the review is now consistent.
        assert_eq!(app.selection.len(), 2);
        let plan = app.plan();
        assert!(plan.is_executable());
        assert_eq!(plan.selected, vec!["medium".to_owned(), "small".to_owned()]);
        assert!(matches!(app.overlay, Overlay::Confirm { .. }));
    }

    #[test]
    fn declining_the_offer_leaves_the_selection_untouched() {
        let mut app = app_with_a_breaking_selection();
        app.handle(Intent::Review);
        assert_eq!(app.handle(Intent::Cancel), Action::Redraw);

        assert_eq!(app.selection.len(), 1);
        assert_eq!(app.overlay, Overlay::None);
        assert!(app.status.contains("left the selection"), "{}", app.status);
    }

    #[test]
    fn keys_do_not_leak_through_an_offer() {
        let mut app = app_with_a_breaking_selection();
        app.handle(Intent::Review);
        assert_eq!(app.handle(Intent::Down), Action::Idle);
        assert!(matches!(app.overlay, Overlay::Offer { .. }));
    }

    #[test]
    fn a_breakage_that_reaches_the_base_system_is_refused_not_offered() {
        // `base` depends on `small`, so widening the selection would sweep
        // the base system up behind a single keystroke. Protection is derived
        // from the package data — the `base` group here — rather than set on
        // the facts by hand, because that is what `plan::widen` recomputes.
        let mut entries = vec![
            entry("small", 100, InstallReason::Dependency, false, &[]),
            entry("base", 300, InstallReason::Explicit, true, &["small"]),
        ];
        if let Some(base) = entries.get_mut(1) {
            base.package.groups = vec!["base".to_owned()];
        }
        let entries = entries;
        let inventory = Inventory {
            index: entries
                .iter()
                .enumerate()
                .map(|(position, entry)| (entry.package.name.clone(), position))
                .collect(),
            entries,
            targets: Vec::new(),
            atime_support: AtimeSupport::Relatime,
            scanned_at: 86_400,
            probed: 2,
            warnings: Vec::new(),
        };
        let mut app = App::new(inventory, View::default(), false);
        app.selection.insert(0);
        app.handle(Intent::Review);

        match &app.overlay {
            Overlay::Notice { title, lines } => {
                assert!(title.contains("break"), "{title}");
                assert!(
                    lines.iter().any(|line| line.contains("base system")),
                    "{lines:?}"
                );
            }
            Overlay::None | Overlay::Help | Overlay::Offer { .. } | Overlay::Confirm { .. } => {
                panic!("expected a refusal, got {:?}", app.overlay)
            }
        }
        assert_eq!(app.selection.len(), 1);
    }

    #[test]
    fn the_description_toggle_widens_and_narrows_the_search() {
        let mut app = app();
        assert!(!app.view.match_descriptions);
        app.handle(Intent::ToggleDescriptions);
        assert!(app.view.match_descriptions);
        assert!(app.status.contains("descriptions"), "{}", app.status);
        app.handle(Intent::ToggleDescriptions);
        assert!(!app.view.match_descriptions);
        assert!(app.status.contains("names only"), "{}", app.status);
    }

    #[test]
    fn searching_matches_names_rather_than_descriptions() {
        let mut app = app();
        app.handle(Intent::StartSearch);
        for character in "small".chars() {
            app.handle(Intent::SearchInput(character));
        }
        assert_eq!(app.order.len(), 1);
        assert_eq!(current(&app), "small");
    }

    #[test]
    fn a_confirmed_removal_produces_the_pacman_command() {
        let mut app = app();
        app.handle(Intent::ToggleSelect);
        app.handle(Intent::Review);
        assert!(matches!(app.overlay, Overlay::Confirm { .. }));

        match app.handle(Intent::Accept) {
            Action::Run { steps, summary } => {
                assert_eq!(steps.len(), 1);
                assert_eq!(summary, "sudo pacman -Rns big");
            }
            other => panic!("expected a run action, got {other:?}"),
        }
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn a_dry_run_confirms_without_executing() {
        let mut app = app();
        app.dry_run = true;
        app.handle(Intent::ToggleSelect);
        app.handle(Intent::Review);

        match app.handle(Intent::Accept) {
            Action::Run { steps, summary } => {
                assert!(steps.is_empty());
                assert_eq!(summary, "sudo pacman -Rns big");
            }
            other => panic!("expected an empty run action, got {other:?}"),
        }
        assert!(app.status.contains("dry run"));
    }

    #[test]
    fn cancelling_a_confirmation_leaves_the_selection_alone() {
        let mut app = app();
        app.handle(Intent::ToggleSelect);
        app.handle(Intent::Review);
        assert_eq!(app.handle(Intent::Cancel), Action::Redraw);
        assert_eq!(app.overlay, Overlay::None);
        assert_eq!(app.selection.len(), 1);
    }

    #[test]
    fn keys_do_not_leak_through_a_confirmation() {
        let mut app = app();
        app.handle(Intent::ToggleSelect);
        app.handle(Intent::Review);
        assert_eq!(app.handle(Intent::Down), Action::Idle);
        assert_eq!(current(&app), "big");
    }

    #[test]
    fn filters_narrow_the_table_and_report_the_total() {
        let mut app = app();
        app.handle(Intent::Filter(Toggle::Orphans));
        assert_eq!(app.order.len(), 1);
        assert_eq!(current(&app), "small");
        assert!(app.status.contains("orphans"), "status was: {}", app.status);
    }

    #[test]
    fn escape_clears_every_filter_at_once() {
        let mut app = app();
        app.handle(Intent::Filter(Toggle::Orphans));
        app.handle(Intent::StartSearch);
        app.handle(Intent::SearchInput('s'));
        app.handle(Intent::SearchCommit);
        assert_eq!(app.handle(Intent::Cancel), Action::Redraw);
        assert!(app.view.is_unfiltered());
        assert_eq!(app.order.len(), 4);
    }

    #[test]
    fn cancelling_a_search_discards_the_query() {
        let mut app = app();
        app.handle(Intent::StartSearch);
        app.handle(Intent::SearchInput('b'));
        // "b" matches `big` and `glibc`.
        assert_eq!(app.order.len(), 2);
        app.handle(Intent::SearchCancel);
        assert!(!app.searching);
        assert_eq!(app.order.len(), 4);
    }

    #[test]
    fn the_cursor_stays_inside_a_shrinking_table() {
        let mut app = app();
        app.handle(Intent::Last);
        app.handle(Intent::Filter(Toggle::Orphans));
        assert_eq!(app.order.len(), 1);
        assert_eq!(app.table.selected(), Some(0));
    }

    #[test]
    fn tabs_alternate() {
        let mut app = app();
        assert_eq!(app.tab, Tab::Packages);
        app.handle(Intent::NextTab);
        assert_eq!(app.tab, Tab::Janitor);
        app.handle(Intent::NextTab);
        assert_eq!(app.tab, Tab::Packages);
    }

    #[test]
    fn an_empty_reclaim_tab_ignores_navigation() {
        let mut app = app();
        app.handle(Intent::NextTab);
        assert_eq!(app.handle(Intent::Down), Action::Idle);
        assert_eq!(app.handle(Intent::Review), Action::Idle);
    }

    #[test]
    fn help_opens_and_closes() {
        let mut app = app();
        app.handle(Intent::Help);
        assert_eq!(app.overlay, Overlay::Help);
        app.handle(Intent::Cancel);
        assert_eq!(app.overlay, Overlay::None);
    }

    #[test]
    fn quitting_from_the_table_leaves() {
        let mut app = app();
        assert_eq!(app.handle(Intent::Quit), Action::Quit);
    }
}
