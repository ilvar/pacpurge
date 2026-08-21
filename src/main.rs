//! pacpurge: find and reclaim space on an Arch system.

#![cfg_attr(
    not(test),
    deny(clippy::expect_used, clippy::indexing_slicing, clippy::unwrap_used)
)]

use std::io::{self, Write};
// `ExitCode` is a value type, not an effect, so it is imported through the
// module path rather than pulling `std::process::` into this file.
use std::process;
use std::time::Duration;

use pacpurge::app::{Action, App, FollowUp};
use pacpurge::capability;
use pacpurge::cli::{self, Mode, Options};
use pacpurge::filter::View;
use pacpurge::keys;
use pacpurge::model::Inventory;
use pacpurge::plan::Step;
use pacpurge::report;
use pacpurge::scan::{self, Config};
use pacpurge::ui;

use ratatui::crossterm::event::{self, Event};

/// Exit status for a failed run.
const FAILURE: u8 = 1;
/// Exit status for a bad invocation.
const MISUSE: u8 = 2;

fn main() -> process::ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();

    let options = match cli::parse(&arguments) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("pacpurge: {error}");
            return process::ExitCode::from(MISUSE);
        }
    };

    match run(&options) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("pacpurge: {message}");
            process::ExitCode::from(FAILURE)
        }
    }
}

/// Dispatch on the selected mode.
fn run(options: &Options) -> Result<process::ExitCode, String> {
    match options.mode {
        Mode::Help => {
            print!("{}", cli::HELP);
            Ok(process::ExitCode::SUCCESS)
        }
        Mode::Version => {
            println!("pacpurge {}", env!("CARGO_PKG_VERSION"));
            Ok(process::ExitCode::SUCCESS)
        }
        Mode::Json => {
            let inventory = collect(options)?;
            let rendered = report::json(&inventory)
                .map_err(|error| format!("could not encode the report: {error}"))?;
            println!("{rendered}");
            Ok(process::ExitCode::SUCCESS)
        }
        Mode::List => {
            let inventory = collect(options)?;
            print!(
                "{}",
                report::list(&inventory, &view_for(options), options.limit)
            );
            Ok(process::ExitCode::SUCCESS)
        }
        Mode::Clean => {
            let inventory = collect(options)?;
            print!("{}", report::clean(&inventory));
            Ok(process::ExitCode::SUCCESS)
        }
        Mode::Diagnose => {
            let inventory = collect(options)?;
            print!("{}", report::diagnose(&inventory));
            Ok(process::ExitCode::SUCCESS)
        }
        Mode::Interactive => interactive(options),
    }
}

/// Build the scan configuration and run it.
fn collect(options: &Options) -> Result<Inventory, String> {
    let mut config = Config::for_root(&options.root);
    config.probe_top = options.probe_top;
    config.probe_usage = options.probe_usage;
    config.measure_directories = options.measure_directories;
    if let Some(db_path) = options.db_path.as_ref() {
        config.db_path = db_path.clone();
    }

    scan::scan(&config).map_err(|error| error.to_string())
}

/// The initial filter state implied by the options.
fn view_for(options: &Options) -> View {
    View {
        stale_days: options.stale_days,
        ..View::default()
    }
}

/// Run the terminal interface.
fn interactive(options: &Options) -> Result<process::ExitCode, String> {
    eprintln!("pacpurge: scanning…");
    let inventory = collect(options)?;
    let mut app = App::new(inventory, view_for(options), options.dry_run);

    let mut terminal =
        ratatui::try_init().map_err(|error| format!("could not set up the terminal: {error}"))?;

    let outcome = event_loop(&mut terminal, &mut app, options);

    ratatui::try_restore().map_err(|error| format!("could not restore the terminal: {error}"))?;

    outcome?;
    Ok(process::ExitCode::SUCCESS)
}

/// The draw/read/apply loop.
fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    options: &Options,
) -> Result<(), String> {
    let mut dirty = true;

    loop {
        if dirty {
            terminal
                .draw(|frame| {
                    ui::draw(frame, app);
                })
                .map_err(|error| format!("could not draw the interface: {error}"))?;
            dirty = false;
        }

        // A poll rather than a blocking read so that a resize repaints
        // promptly without a key press.
        let ready = event::poll(Duration::from_millis(250))
            .map_err(|error| format!("could not read input: {error}"))?;
        if !ready {
            continue;
        }

        let event = event::read().map_err(|error| format!("could not read input: {error}"))?;
        let intent = match event {
            Event::Key(key) => keys::intent(key, app.searching),
            Event::Resize(_columns, _rows) => {
                dirty = true;
                continue;
            }
            Event::FocusGained | Event::FocusLost | Event::Mouse(_) | Event::Paste(_) => continue,
        };

        let Some(intent) = intent else {
            continue;
        };

        match app.handle(intent) {
            Action::Idle => {}
            Action::Redraw => dirty = true,
            Action::Quit => return Ok(()),
            Action::Rescan => {
                let inventory = collect(options)?;
                app.adopt(inventory);
                dirty = true;
            }
            Action::Run {
                steps,
                summary,
                follow_up,
            } => {
                execute(terminal, app, &steps, &summary, follow_up, options)?;
                dirty = true;
            }
        }
    }
}

/// Leave the alternate screen, run the steps, and come back.
///
/// Handing the terminal back matters: `sudo` needs it to prompt for a
/// password, and pacman's own confirmation is a better final check than
/// anything drawn here.
fn execute(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    steps: &[Step],
    summary: &str,
    follow_up: FollowUp,
    options: &Options,
) -> Result<(), String> {
    if steps.is_empty() {
        // Dry run: show the commands without leaving the interface.
        app.status = format!("dry run — would have run: {}", summary.replace('\n', "; "));
        return Ok(());
    }

    ratatui::try_restore().map_err(|error| format!("could not restore the terminal: {error}"))?;

    let mut failures = 0usize;
    for step in steps {
        println!("\n$ {}", step.to_shell());
        match perform(step) {
            Ok(true) => {}
            Ok(false) => {
                failures = failures.saturating_add(1);
                eprintln!("pacpurge: that step did not complete successfully");
            }
            Err(message) => {
                failures = failures.saturating_add(1);
                eprintln!("pacpurge: {message}");
            }
        }
    }

    print!("\nPress Enter to return to pacpurge. ");
    let _ = io::stdout().flush();
    let mut discard = String::new();
    let _ = io::stdin().read_line(&mut discard);

    *terminal = ratatui::try_init()
        .map_err(|error| format!("could not set up the terminal again: {error}"))?;

    let inventory = collect(options)?;
    app.adopt(inventory);
    app.status = if failures == 0 {
        "done — rescanned".to_owned()
    } else {
        format!("{failures} step(s) did not complete; rescanned anyway")
    };

    // Proposed only after the rescan, so the figures it quotes are the ones
    // the run actually produced.
    if failures == 0 {
        app.offer_follow_up(follow_up);
    }

    Ok(())
}

/// Run one step. Returns whether it succeeded.
fn perform(step: &Step) -> Result<bool, String> {
    match step {
        Step::Run { command } => {
            let (program, args) =
                with_privilege(&command.program, &command.args, command.needs_root);
            let code = capability::run_interactive(&program, &args)
                .map_err(|error| format!("could not run {program}: {error}"))?;
            Ok(code == Some(0))
        }
        Step::Delete { paths, needs_root } => {
            if *needs_root && capability::effective_uid() != 0 {
                // Deleting root-owned paths from this process would fail
                // halfway through; hand the whole list to one `sudo rm`
                // instead so the user authenticates once.
                let mut args = vec!["rm".to_owned(), "-rf".to_owned()];
                args.extend(paths.iter().map(|path| path.to_string_lossy().into_owned()));
                let code = capability::run_interactive("sudo", &args)
                    .map_err(|error| format!("could not run sudo rm: {error}"))?;
                return Ok(code == Some(0));
            }

            let mut ok = true;
            for path in paths {
                if let Err(error) = capability::remove(path) {
                    eprintln!("pacpurge: could not remove {}: {error}", path.display());
                    ok = false;
                }
            }
            Ok(ok)
        }
    }
}

/// Prefix a command with `sudo` when it needs root and this process is not root.
fn with_privilege(program: &str, args: &[String], needs_root: bool) -> (String, Vec<String>) {
    if !needs_root || capability::effective_uid() == 0 {
        return (program.to_owned(), args.to_vec());
    }

    let mut elevated = vec![program.to_owned()];
    elevated.extend(args.iter().cloned());
    ("sudo".to_owned(), elevated)
}
