//! Drawing the interface.
//!
//! Rendering only: every value shown here was computed by a pure module, so a
//! wrong number on screen is a bug in the analysis rather than in the drawing.

use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Table, Tabs, Wrap,
};
use ratatui::Frame;

use crate::app::{App, Overlay, Tab};
use crate::filter::{Toggle, TOGGLES};
use crate::format;
use crate::janitor::{Reclaim, Safety, Target};
use crate::model::{Entry, UsageEvidence};

/// Accent used for headings and the active tab.
const ACCENT: Color = Color::Cyan;
/// Colour for figures worth acting on.
const GOOD: Color = Color::Green;
/// Colour for things needing a second look.
const WARN: Color = Color::Yellow;
/// Colour for destructive or protected things.
const DANGER: Color = Color::Red;
/// Colour for secondary text.
const MUTED: Color = Color::DarkGray;

/// Width at which the detail pane is dropped.
const NARROW: u16 = 110;

/// Column widths. Each pair of layout constraint and padding width below must
/// agree, or the padded text overflows its cell and loses its last characters.
const SIZE_WIDTH: usize = 10;
/// Width of the last-used column.
const USAGE_WIDTH: usize = 10;
/// Width of the install-date column.
const ADDED_WIDTH: usize = 8;
/// Width of the dependant-count column.
const NEEDED_WIDTH: usize = 9;

/// Draw one frame.
pub fn draw(frame: &mut Frame<'_>, app: &mut App) -> bool {
    let area = frame.area();
    let [header, body, status] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(2),
    ])
    .areas(area);

    render_header(frame, app, header);

    let wide = area.width >= NARROW;
    let [table_area, detail_area] = if wide {
        Layout::horizontal([Constraint::Min(60), Constraint::Length(44)]).areas(body)
    } else {
        [body, Rect::new(body.x, body.y, 0, 0)]
    };

    // Two rows of chrome sit above the first data row; paging by anything more
    // would scroll past content the user never saw.
    app.page = usize::from(table_area.height.saturating_sub(3)).max(1);

    match app.tab {
        Tab::Packages => {
            render_packages(frame, app, table_area);
            if wide {
                render_package_detail(frame, app, detail_area);
            }
        }
        Tab::Janitor => {
            render_targets(frame, app, table_area);
            if wide {
                render_target_detail(frame, app, detail_area);
            }
        }
    }

    render_status(frame, app, status);
    render_overlay(frame, app, area);
    true
}

/// The tab strip and the headline figures.
fn render_header(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let [tabs_area, summary_area] =
        Layout::horizontal([Constraint::Length(28), Constraint::Min(20)]).areas(area);

    let titles: Vec<Line<'_>> = [Tab::Packages, Tab::Janitor]
        .into_iter()
        .map(|tab| Line::from(tab.title()))
        .collect();
    let selected = match app.tab {
        Tab::Packages => 0,
        Tab::Janitor => 1,
    };

    frame.render_widget(
        Tabs::new(titles)
            .select(selected)
            .highlight_style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::ALL).title(" pacpurge ")),
        tabs_area,
    );

    let reclaimable = app
        .inventory
        .targets
        .iter()
        .map(Target::known_bytes)
        .fold(0u64, u64::saturating_add);
    let marked = app.selection_bytes();

    let mut spans = vec![
        Span::styled("installed ", Style::default().fg(MUTED)),
        Span::raw(format::bytes(app.inventory.total_size())),
        Span::styled("   reclaimable ", Style::default().fg(MUTED)),
        Span::styled(format::bytes(reclaimable), Style::default().fg(GOOD)),
    ];

    if marked > 0 {
        spans.push(Span::styled("   marked ", Style::default().fg(MUTED)));
        spans.push(Span::styled(
            format!("{} ({})", format::bytes(marked), app.selection.len()),
            Style::default().fg(WARN).add_modifier(Modifier::BOLD),
        ));
    }

    if app.dry_run {
        spans.push(Span::styled(
            "   DRY RUN",
            Style::default().fg(DANGER).add_modifier(Modifier::BOLD),
        ));
    }

    frame.render_widget(
        Paragraph::new(Line::from(spans))
            .alignment(Alignment::Right)
            .block(Block::default().borders(Borders::ALL)),
        summary_area,
    );
}

/// The package table.
fn render_packages(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let now = app.inventory.scanned_at;
    let atime_known = app.inventory.atime_support.is_meaningful();

    let header = Row::new(vec![
        Cell::from(" "),
        Cell::from("package"),
        Cell::from("size").style(sort_style(app, crate::filter::SortKey::Size)),
        Cell::from("frees").style(sort_style(app, crate::filter::SortKey::Reclaimable)),
        Cell::from("last used").style(sort_style(app, crate::filter::SortKey::LastUsed)),
        Cell::from("added").style(sort_style(app, crate::filter::SortKey::Installed)),
        Cell::from("origin"),
        Cell::from("needed by").style(sort_style(app, crate::filter::SortKey::RequiredBy)),
    ])
    .style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD));

    let rows: Vec<Row<'_>> = app
        .order
        .iter()
        .filter_map(|position| {
            app.inventory
                .entries
                .get(*position)
                .map(|entry| (position, entry))
        })
        .map(|(position, entry)| {
            package_row(entry, app.selection.contains(position), now, atime_known)
        })
        .collect();

    let title = format!(
        " {} of {} packages ",
        app.order.len(),
        app.inventory.entries.len()
    );

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            Constraint::Min(18),
            width(SIZE_WIDTH),
            width(SIZE_WIDTH),
            width(USAGE_WIDTH),
            width(ADDED_WIDTH),
            Constraint::Length(10),
            width(NEEDED_WIDTH),
        ],
    )
    .header(header)
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(title),
    );

    frame.render_stateful_widget(table, area, &mut app.table);
}

/// A fixed-width column constraint matching a padding width.
fn width(columns: usize) -> Constraint {
    Constraint::Length(u16::try_from(columns).unwrap_or(u16::MAX))
}

/// Highlight the column the table is ordered by.
fn sort_style(app: &App, key: crate::filter::SortKey) -> Style {
    if app.view.sort == key {
        Style::default()
            .fg(WARN)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        Style::default().fg(ACCENT)
    }
}

/// One row of the package table.
fn package_row<'a>(entry: &'a Entry, marked: bool, now: i64, atime_known: bool) -> Row<'a> {
    let mark = if marked {
        Span::styled("●", Style::default().fg(WARN))
    } else if entry.facts.protected {
        Span::styled("·", Style::default().fg(MUTED))
    } else {
        Span::raw(" ")
    };

    let name_style = if entry.facts.protected {
        Style::default().fg(MUTED)
    } else if entry.facts.origin.is_foreign() {
        Style::default().fg(Color::Magenta)
    } else {
        Style::default()
    };

    // A package that frees noticeably more than its own size is dragging
    // dependencies with it, and that is the number worth acting on.
    let frees_style = if entry.facts.reclaimable > entry.package.size {
        Style::default().fg(GOOD).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(MUTED)
    };

    let (usage_text, usage_style) = usage_cell(&entry.facts.usage, now, atime_known);

    Row::new(vec![
        Cell::from(Line::from(mark)),
        Cell::from(Span::styled(entry.package.name.clone(), name_style)),
        Cell::from(right(format::bytes(entry.package.size), SIZE_WIDTH)),
        Cell::from(Span::styled(
            format!("{:>SIZE_WIDTH$}", format::bytes(entry.facts.reclaimable)),
            frees_style,
        )),
        Cell::from(Span::styled(
            format!("{usage_text:>USAGE_WIDTH$}"),
            usage_style,
        )),
        Cell::from(right(
            format::age(now, entry.package.install_date),
            ADDED_WIDTH,
        )),
        Cell::from(Span::styled(
            entry.facts.origin.label().to_owned(),
            if entry.facts.origin.is_foreign() {
                Style::default().fg(Color::Magenta)
            } else {
                Style::default().fg(MUTED)
            },
        )),
        Cell::from(right(
            match entry.facts.required_by.len() {
                0 => "orphan".to_owned(),
                count => count.to_string(),
            },
            NEEDED_WIDTH,
        )),
    ])
}

/// Right-align a short string into a span of exactly `width` columns.
///
/// The width must match the column's layout constraint: padding wider than
/// the constraint pushes the last characters off the end of the cell, which is
/// how a size silently becomes `1.` instead of `1.3y`.
fn right(text: String, width: usize) -> Span<'static> {
    Span::raw(format!("{text:>width$}"))
}

/// The text and colour of the last-used cell.
fn usage_cell(usage: &UsageEvidence, now: i64, atime_known: bool) -> (String, Style) {
    if !atime_known {
        return ("—".to_owned(), Style::default().fg(MUTED));
    }

    match usage {
        UsageEvidence::Used { at, witness: _ } => {
            let days = format::days_since(now, *at);
            let style = if days >= 180 {
                Style::default().fg(WARN)
            } else {
                Style::default().fg(MUTED)
            };
            (format::age(now, Some(*at)), style)
        }
        UsageEvidence::NeverSinceInstall { at: _ } => (
            "never".to_owned(),
            Style::default().fg(DANGER).add_modifier(Modifier::BOLD),
        ),
        UsageEvidence::NoWitness => ("n/a".to_owned(), Style::default().fg(MUTED)),
        UsageEvidence::AtimeDisabled => ("off".to_owned(), Style::default().fg(MUTED)),
        UsageEvidence::NotProbed => ("·".to_owned(), Style::default().fg(MUTED)),
    }
}

/// The detail pane for the selected package.
fn render_package_detail(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(" details ");

    let Some(entry) = app.current_entry() else {
        frame.render_widget(Paragraph::new("no package selected").block(block), area);
        return;
    };

    let mut lines: Vec<Line<'_>> = vec![
        Line::from(Span::styled(
            entry.package.name.clone(),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            entry.package.version.clone(),
            Style::default().fg(MUTED),
        )),
        Line::from(""),
        Line::from(entry.package.description.clone()),
        Line::from(""),
    ];

    lines.push(field("installed size", &format::bytes(entry.package.size)));
    lines.push(field(
        "frees if removed",
        &format::bytes(entry.facts.reclaimable),
    ));
    lines.push(field(
        "installed on",
        &format::date(entry.package.install_date),
    ));
    lines.push(field("reason", entry.package.reason.label()));
    lines.push(field("origin", entry.facts.origin.label()));

    match &entry.facts.usage {
        UsageEvidence::Used { at, witness } => {
            lines.push(field("last read", &format::date(Some(*at))));
            lines.push(Line::from(Span::styled(
                format!("  via /{witness}"),
                Style::default().fg(MUTED),
            )));
        }
        UsageEvidence::NeverSinceInstall { at: _ } => {
            lines.push(Line::from(Span::styled(
                "  not read since it was installed",
                Style::default().fg(DANGER),
            )));
        }
        UsageEvidence::NoWitness => {
            lines.push(field("last read", "no file worth checking"));
        }
        UsageEvidence::AtimeDisabled => {
            lines.push(field("last read", "access times are off"));
        }
        UsageEvidence::NotProbed => {
            lines.push(field("last read", "not probed — raise --top"));
        }
    }

    if !entry.facts.frees.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("also removes {} dependencies:", entry.facts.frees.len()),
            Style::default().fg(GOOD),
        )));
        lines.push(Line::from(Span::styled(
            entry.facts.frees.join(", "),
            Style::default().fg(MUTED),
        )));
    }

    if !entry.facts.required_by.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("needed by {}:", entry.facts.required_by.len()),
            Style::default().fg(WARN),
        )));
        lines.push(Line::from(Span::styled(
            entry.facts.required_by.join(", "),
            Style::default().fg(MUTED),
        )));
    }

    if !entry.facts.optional_for.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("optional for: {}", entry.facts.optional_for.join(", ")),
            Style::default().fg(MUTED),
        )));
    }

    if entry.facts.protected {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "part of the base system — press P to mark it anyway",
            Style::default().fg(DANGER).add_modifier(Modifier::BOLD),
        )));
    }

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .block(block),
        area,
    );
}

/// A `label  value` line for the detail pane.
fn field<'a>(label: &'a str, value: &str) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{label:<18}"), Style::default().fg(MUTED)),
        Span::raw(value.to_owned()),
    ])
}

/// The reclaim table.
fn render_targets(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let header = Row::new(vec![
        Cell::from("size"),
        Cell::from("safety"),
        Cell::from("what"),
        Cell::from("items"),
    ])
    .style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD));

    let rows: Vec<Row<'_>> = app
        .inventory
        .targets
        .iter()
        .map(|target| {
            let size = match target.bytes {
                Some(bytes) => format::bytes(bytes),
                None => "?".to_owned(),
            };
            Row::new(vec![
                Cell::from(Span::styled(
                    format!("{size:>10}"),
                    Style::default().fg(GOOD).add_modifier(Modifier::BOLD),
                )),
                Cell::from(Span::styled(
                    target.safety.label().to_owned(),
                    safety_style(target.safety),
                )),
                Cell::from(target.title.clone()),
                Cell::from(Span::styled(
                    format!("{:>7}", target.items),
                    Style::default().fg(MUTED),
                )),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(11),
            Constraint::Length(8),
            Constraint::Min(24),
            Constraint::Length(8),
        ],
    )
    .header(header)
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" reclaimable space "),
    );

    frame.render_stateful_widget(table, area, &mut app.targets_table);
}

/// Colour for a safety level.
fn safety_style(safety: Safety) -> Style {
    match safety {
        Safety::Safe => Style::default().fg(GOOD),
        Safety::Review => Style::default().fg(WARN),
        Safety::Careful => Style::default().fg(DANGER),
    }
}

/// The detail pane for the selected reclaim target.
fn render_target_detail(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(" details ");

    let Some(target) = app.current_target() else {
        frame.render_widget(
            Paragraph::new("Nothing to reclaim outside the package set.").block(block),
            area,
        );
        return;
    };

    let mut lines = vec![
        Line::from(Span::styled(
            target.title.clone(),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            target.location.clone(),
            Style::default().fg(MUTED),
        )),
        Line::from(""),
        Line::from(target.detail.clone()),
        Line::from(""),
    ];

    match &target.reclaim {
        Reclaim::Run { command } => {
            lines.push(Line::from(Span::styled(
                "press Enter to run:",
                Style::default().fg(GOOD),
            )));
            lines.push(Line::from(Span::styled(
                command.to_shell(),
                Style::default().fg(WARN),
            )));
        }
        Reclaim::Paths { paths, needs_root } => {
            let scope = if *needs_root { "as root" } else { "as you" };
            lines.push(Line::from(Span::styled(
                format!("press Enter to delete {} path(s) {scope}:", paths.len()),
                Style::default().fg(GOOD),
            )));
            for path in paths.iter().take(4) {
                lines.push(Line::from(Span::styled(
                    format!("  {}", path.display()),
                    Style::default().fg(MUTED),
                )));
            }
        }
        Reclaim::Handoff { hint } => {
            lines.push(Line::from(Span::styled(
                format!("handled on the {hint}"),
                Style::default().fg(MUTED),
            )));
        }
        Reclaim::Advice { text } => {
            lines.push(Line::from(Span::styled(
                text.clone(),
                Style::default().fg(MUTED),
            )));
        }
    }

    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .block(block),
        area,
    );
}

/// The filter bar and the status line.
fn render_status(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let [filters, status] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);

    let mut spans: Vec<Span<'_>> = Vec::new();

    if app.searching {
        spans.push(Span::styled(
            "search: ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!("{}▏", app.view.query),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    } else {
        for toggle in TOGGLES {
            let active = app.view.is_active(toggle);
            let style = if active {
                Style::default()
                    .fg(Color::Black)
                    .bg(ACCENT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(MUTED)
            };
            spans.push(Span::styled(format!(" {} ", toggle_label(toggle)), style));
            spans.push(Span::raw(" "));
        }

        if !app.view.query.is_empty() {
            spans.push(Span::styled(
                format!(" /{} ", app.view.query),
                Style::default().fg(Color::Black).bg(WARN),
            ));
            spans.push(Span::raw(" "));
        }

        spans.push(Span::styled(
            format!(
                "sort:{}{}",
                app.view.sort.label(),
                if app.view.descending { "↓" } else { "↑" }
            ),
            Style::default().fg(MUTED),
        ));
        spans.push(Span::styled(
            "   ? help   space mark   Enter apply   q quit",
            Style::default().fg(MUTED),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), filters);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            app.status.clone(),
            Style::default().fg(MUTED),
        ))),
        status,
    );
}

/// The filter bar label, key first.
fn toggle_label(toggle: Toggle) -> String {
    format!("{} {}", toggle.key(), toggle.label())
}

/// Draw whichever modal is open, if any.
fn render_overlay(frame: &mut Frame<'_>, app: &App, area: Rect) {
    match &app.overlay {
        Overlay::None => {}
        Overlay::Help => {
            let lines: Vec<Line<'_>> = HELP.lines().map(Line::from).collect();
            modal(frame, area, " keys ", lines, false, "any key to close");
        }
        Overlay::Notice { title, lines } => {
            let rendered: Vec<Line<'_>> =
                lines.iter().map(|line| Line::from(line.clone())).collect();
            modal(
                frame,
                area,
                &format!(" {title} "),
                rendered,
                true,
                "any key to close",
            );
        }
        Overlay::Confirm {
            title,
            lines,
            steps: _,
            danger,
        } => {
            let rendered: Vec<Line<'_>> =
                lines.iter().map(|line| Line::from(line.clone())).collect();
            modal(
                frame,
                area,
                &format!(" {title} "),
                rendered,
                *danger,
                "y to run   Esc to cancel",
            );
        }
    }
}

/// Draw a centred modal box.
fn modal(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    lines: Vec<Line<'_>>,
    danger: bool,
    footer: &str,
) {
    let width = area.width.saturating_mul(3) / 4;
    let wanted = u16::try_from(lines.len().saturating_add(4)).unwrap_or(u16::MAX);
    let height = wanted.min(area.height.saturating_sub(2)).max(5);

    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };

    let border = if danger { DANGER } else { ACCENT };

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border))
            .title(Span::styled(
                title.to_owned(),
                Style::default().fg(border).add_modifier(Modifier::BOLD),
            ))
            .title_bottom(Span::styled(
                format!(" {footer} "),
                Style::default().fg(MUTED),
            )),
        popup,
    );

    let inner = popup.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    frame.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        inner,
    );
}

/// The text of the help modal.
const HELP: &str = "\
MOVE      j / k / ↑ / ↓     one row
          Ctrl-D / Ctrl-U   half a screen
          g / G             first / last row
          Tab               switch between Packages and Reclaim

CHOOSE    space             mark the package under the cursor
          P                 mark it even though it is part of base
          c                 unmark everything
          Enter             review and run what is marked

FILTER    o                 orphans: installed as a dependency, needed by nothing
          a                 AUR and locally built packages
          e                 explicitly installed packages
          n                 never read since installation
          u                 not read within the staleness window
          p                 hide packages the base system depends on
          /                 search names and descriptions
          Esc               clear every filter

SORT      s                 next column          S   reverse
          1 name   2 size   3 frees   4 last used   5 added   6 needed by

OTHER     r                 re-scan the system
          ?                 this list
          q                 quit

WHAT THE COLUMNS MEAN
  size        what the package's own files occupy
  frees       what removing it actually recovers, including dependencies
              that nothing else keeps alive. Green means it drags others
              along, and that is the number worth sorting by.
  last used   the newest access time across the package's binaries and
              libraries. \"never\" means no file has been read since the day
              it was installed.
  needed by   how many installed packages hard-depend on it.

Removals are handed to `pacman -Rns`, which runs its own checks and asks
for its own confirmation. pacpurge never deletes package files itself.
";
