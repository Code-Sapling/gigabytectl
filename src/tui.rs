//! Terminal lifecycle and the TUI event loop.

use std::{
    io::{self, Stdout, Write},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::{
    app::{App, Flow},
    config::Config,
    sysfs, system, ui,
};

/// How often the screen is redrawn while waiting for input, so the "last
/// refresh" clock keeps moving between refreshes.
const REDRAW_INTERVAL: Duration = Duration::from_millis(250);

type Tui = Terminal<CrosstermBackend<Stdout>>;

pub fn run(config: Config) -> Result<()> {
    ensure_root()?;
    ensure!(sysfs::driver_present(), "{}", sysfs::driver_missing_message());

    install_panic_hook();
    let mut terminal = setup_terminal()?;
    let result = event_loop(&mut terminal, App::new(config));
    restore_terminal(&mut terminal);
    result
}

/// The TUI writes to sysfs, so it needs root. Offer to re-run under sudo rather
/// than just failing.
fn ensure_root() -> Result<()> {
    if system::is_root() {
        return Ok(());
    }

    println!("This program requires root privileges.");
    print!("Do you want to run with sudo? [Y/n]: ");
    io::stdout().flush().ok();

    let mut answer = String::new();
    io::stdin().read_line(&mut answer).context("reading answer")?;

    match answer.trim().to_lowercase().as_str() {
        "" | "y" | "yes" => {
            system::run_sudo()?;
            unreachable!("run_sudo replaces this process");
        }
        _ => {
            println!("Exiting.");
            std::process::exit(1);
        }
    }
}

fn event_loop(terminal: &mut Tui, mut app: App) -> Result<()> {
    let refresh_interval = app.config.refresh_interval();

    loop {
        terminal.draw(|frame| ui::draw(frame, &app)).context("drawing ui")?;

        // Wake up in time for the next refresh, but often enough to keep the
        // display responsive.
        let timeout = refresh_interval
            .saturating_sub(app.last_refresh.elapsed())
            .min(REDRAW_INTERVAL);
        if event::poll(timeout).context("polling for input")?
            && let Event::Key(key) = event::read().context("reading input")?
            // Terminals that report key releases would otherwise act twice.
            && !key.is_release()
            && app.handle_key(key) == Flow::Exit
        {
            break;
        }

        if app.last_refresh.elapsed() >= refresh_interval {
            app.refresh();
        }
    }
    Ok(())
}

fn setup_terminal() -> Result<Tui> {
    terminal::enable_raw_mode().context("enabling raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("entering alternate screen")?;
    Terminal::new(CrosstermBackend::new(stdout)).context("creating terminal")
}

fn restore_terminal(terminal: &mut Tui) {
    let _ = terminal::disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();
}

/// Ensures a panic doesn't leave the terminal stuck in raw/alternate-screen mode.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        default_hook(info);
    }));
}
