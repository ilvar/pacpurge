//! Rendering tests.
//!
//! The interface is drawn into an in-memory backend and the resulting cells
//! are read back as text. This catches the failures that unit tests on the
//! state machine cannot: a panicking layout, a column that never gets drawn,
//! or a number that reaches the screen in the wrong form.

use pacpurge::app::{App, Intent};
use pacpurge::filter::View;
use pacpurge::janitor::{Command, Kind, Reclaim, Safety, Target};
use pacpurge::model::{
    AtimeSupport, Entry, Facts, InstallReason, Inventory, Origin, Package, UsageEvidence,
};
use pacpurge::ui;

use ratatui::backend::TestBackend;
use ratatui::Terminal;

/// Build a small inventory covering every visual state the table has.
fn inventory() -> Inventory {
    let entries = vec![
        Entry {
            package: Package {
                name: "abandoned-toy".to_owned(),
                version: "0.2-1".to_owned(),
                size: 300_000_000,
                install_date: Some(0),
                reason: InstallReason::Explicit,
                description: "a thing installed once and never opened".to_owned(),
                ..Package::default()
            },
            facts: Facts {
                required_by: Vec::new(),
                optional_for: Vec::new(),
                origin: Origin::Foreign,
                usage: UsageEvidence::NeverSinceInstall { at: 0 },
                reclaimable: 390_000_000,
                frees: vec!["lonely-lib".to_owned()],
                protected: false,
            },
        },
        Entry {
            package: Package {
                name: "glibc".to_owned(),
                version: "2.39-1".to_owned(),
                size: 50_000_000,
                install_date: Some(0),
                reason: InstallReason::Dependency,
                description: "the C library".to_owned(),
                ..Package::default()
            },
            facts: Facts {
                required_by: vec!["everything".to_owned()],
                optional_for: Vec::new(),
                origin: Origin::Repository("core".to_owned()),
                usage: UsageEvidence::Used {
                    at: 86_400 * 30,
                    witness: "usr/lib/libc.so.6".to_owned(),
                },
                reclaimable: 50_000_000,
                frees: Vec::new(),
                protected: true,
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
            title: "Superseded versions of installed packages".to_owned(),
            location: "/var/cache/pacman/pkg".to_owned(),
            detail: "Older builds of packages you still have.".to_owned(),
            bytes: Some(2_000_000_000),
            items: 214,
            safety: Safety::Safe,
            reclaim: Reclaim::Run {
                command: Command::new("paccache", &["-r", "-k1"], true),
            },
        }],
        atime_support: AtimeSupport::Relatime,
        scanned_at: 86_400 * 400,
        probed: 2,
        warnings: Vec::new(),
    }
}

/// Draw one frame and return the screen as newline-separated text.
fn screen(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal =
        Terminal::new(TestBackend::new(width, height)).expect("the test backend should start");
    terminal
        .draw(|frame| {
            ui::draw(frame, app);
        })
        .expect("drawing should succeed");

    let buffer = terminal.backend().buffer().clone();
    let mut rendered = String::new();
    for row in 0..buffer.area.height {
        for column in 0..buffer.area.width {
            if let Some(cell) = buffer.cell((column, row)) {
                rendered.push_str(cell.symbol());
            }
        }
        rendered.push('\n');
    }
    rendered
}

fn app() -> App {
    App::new(inventory(), View::default(), false)
}

#[test]
fn the_package_table_draws_every_column() {
    let rendered = screen(&mut app(), 140, 30);

    assert!(rendered.contains("pacpurge"), "{rendered}");
    assert!(rendered.contains("Packages"), "{rendered}");
    assert!(rendered.contains("Reclaim"), "{rendered}");
    assert!(rendered.contains("abandoned-toy"), "{rendered}");
    assert!(rendered.contains("glibc"), "{rendered}");
    assert!(rendered.contains("never"), "{rendered}");
    assert!(rendered.contains("orphan"), "{rendered}");
    assert!(rendered.contains("last used"), "{rendered}");
}

#[test]
fn sizes_reach_the_screen_in_human_units() {
    let rendered = screen(&mut app(), 140, 30);
    assert!(rendered.contains("286.1 MiB"), "{rendered}");
    assert!(rendered.contains("371.9 MiB"), "{rendered}");
}

#[test]
fn the_detail_pane_explains_the_cascade() {
    let rendered = screen(&mut app(), 140, 30);
    assert!(
        rendered.contains("also removes 1 dependencies"),
        "{rendered}"
    );
    assert!(rendered.contains("lonely-lib"), "{rendered}");
}

#[test]
fn the_detail_pane_is_dropped_on_a_narrow_terminal() {
    let rendered = screen(&mut app(), 90, 30);
    assert!(rendered.contains("abandoned-toy"), "{rendered}");
    assert!(!rendered.contains("also removes"), "{rendered}");
}

#[test]
fn a_protected_package_says_so_in_its_detail_pane() {
    let mut app = app();
    app.handle(Intent::Down);
    let rendered = screen(&mut app, 140, 30);
    assert!(rendered.contains("part of the base system"), "{rendered}");
}

#[test]
fn the_reclaim_tab_draws_its_targets_and_command() {
    let mut app = app();
    app.handle(Intent::NextTab);
    let rendered = screen(&mut app, 140, 30);

    assert!(rendered.contains("Superseded versions"), "{rendered}");
    assert!(rendered.contains("1.8 GiB"), "{rendered}");
    assert!(rendered.contains("sudo paccache -r -k1"), "{rendered}");
}

#[test]
fn the_confirmation_modal_shows_the_exact_command() {
    let mut app = app();
    app.handle(Intent::ToggleSelect);
    app.handle(Intent::Review);
    let rendered = screen(&mut app, 140, 30);

    assert!(rendered.contains("Review removal"), "{rendered}");
    assert!(rendered.contains("pacman -Rns abandoned-toy"), "{rendered}");
    assert!(rendered.contains("y to run"), "{rendered}");
}

#[test]
fn the_help_modal_lists_the_bindings() {
    let mut app = app();
    app.handle(Intent::Help);
    let rendered = screen(&mut app, 140, 40);

    assert!(rendered.contains("space"), "{rendered}");
    assert!(rendered.contains("orphans"), "{rendered}");
    assert!(rendered.contains("re-scan"), "{rendered}");
}

#[test]
fn the_search_field_shows_what_is_being_typed() {
    let mut app = app();
    app.handle(Intent::StartSearch);
    app.handle(Intent::SearchInput('t'));
    app.handle(Intent::SearchInput('o'));
    app.handle(Intent::SearchInput('y'));
    let rendered = screen(&mut app, 140, 30);

    assert!(rendered.contains("search: toy"), "{rendered}");
    assert!(rendered.contains("abandoned-toy"), "{rendered}");
    assert!(!rendered.contains("2.39-1"), "{rendered}");
}

#[test]
fn a_noatime_system_shows_dashes_rather_than_a_misleading_date() {
    let mut inventory = inventory();
    inventory.atime_support = AtimeSupport::Disabled;
    let mut app = App::new(inventory, View::default(), false);
    let rendered = screen(&mut app, 140, 30);

    // The filter bar still offers the `never-used` toggle; what must not
    // appear is a last-used verdict in the table, since there is no evidence
    // for one. The column shows an em dash and the caveat is spelled out.
    let table_rows: String = rendered
        .lines()
        .filter(|line| line.contains("abandoned-toy") || line.contains("glibc"))
        .collect();
    assert!(!table_rows.contains("never"), "{rendered}");
    assert!(table_rows.contains('\u{2014}'), "{rendered}");
    assert!(rendered.contains("noatime"), "{rendered}");
}

#[test]
fn a_dry_run_is_announced_in_the_header() {
    let mut app = App::new(inventory(), View::default(), true);
    let rendered = screen(&mut app, 140, 30);
    assert!(rendered.contains("DRY RUN"), "{rendered}");
}

#[test]
fn an_empty_table_still_draws() {
    let empty = Inventory {
        entries: Vec::new(),
        index: Default::default(),
        targets: Vec::new(),
        atime_support: AtimeSupport::Relatime,
        scanned_at: 0,
        probed: 0,
        warnings: Vec::new(),
    };
    let mut app = App::new(empty, View::default(), false);
    let rendered = screen(&mut app, 140, 30);
    assert!(rendered.contains("no package selected"), "{rendered}");
}

#[test]
fn a_tiny_terminal_does_not_panic() {
    // Terminals get resized to absurd sizes; a layout that assumes room is a
    // crash waiting to happen mid-session.
    for (width, height) in [(20u16, 6u16), (40, 10), (1, 1), (200, 4)] {
        let mut app = app();
        app.handle(Intent::Help);
        let _rendered = screen(&mut app, width, height);
    }
}

#[test]
fn no_column_truncates_the_value_it_holds() {
    // Regression: a padding width wider than the column's layout constraint
    // silently chops the last characters off, turning `1.0y` into `1.` and
    // `orphan` into `orpha`. Every cell must survive intact.
    let rendered = screen(&mut app(), 130, 22);
    // Matched on the size cell rather than the name: the detail pane repeats
    // the name on the header row of the same screen line.
    let row = rendered
        .lines()
        .find(|line| line.contains("286.1 MiB"))
        .unwrap_or_default()
        .to_owned();

    assert!(row.contains("286.1 MiB"), "{row}");
    assert!(row.contains("371.9 MiB"), "{row}");
    assert!(row.contains("never"), "{row}");
    assert!(row.contains("1.0y"), "{row}");
    assert!(row.contains("aur/local"), "{row}");
    assert!(row.contains("orphan"), "{row}");
}
