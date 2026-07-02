pub mod controller;
pub mod dashboard;
pub mod render;
pub mod theme;
pub mod widgets;
pub mod wizard;

use std::io;
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use controller::{AppController, ControllerDeps};
use dashboard::DashboardState;
use render::{draw_dashboard, draw_wizard, RenderCtx};
use wizard::WizardState;

/// Terminal-agnostic key events consumed by the TUI's pure state machines
/// (the setup wizard in Task 5; the main app view in Task 6). The ratatui
/// event loop maps crossterm `KeyEvent`s into this enum; the state machines
/// themselves never touch a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Space,
    Enter,
    Esc,
    Backspace,
    Char(char),
    Tab,
    Left,
    Right,
}

/// Which pure state machine is currently driving the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Wizard,
    Dashboard,
}

/// TS parity: the wizard runs first-run, the dashboard once settings are
/// complete (`is_configured`); re-checked on every `AppController::new` (the
/// wizard pre-checks whichever gateways/runtimes are already enabled).
async fn initial_mode(controller: &AppController) -> Mode {
    if controller.is_configured().await {
        Mode::Dashboard
    } else {
        Mode::Wizard
    }
}

type Term = Terminal<CrosstermBackend<io::Stdout>>;

/// Sync wrapper building a tokio runtime, like every other CLI command.
pub fn launch_ui(deps: &mut crate::dispatch::Deps) -> u8 {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(run_ui(deps))
}

async fn run_ui(deps: &mut crate::dispatch::Deps) -> u8 {
    let store = match crate::db::open_store(deps).await {
        Ok(s) => s,
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    };
    let data_dir = deps
        .db_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let controller = AppController::new(ControllerDeps {
        store: Arc::new(store),
        data_dir,
        detect_git: deps.detect_git,
        detect_claude: deps.detect_claude,
        spawn_daemon: None,
        kill_daemon: None,
    });

    let mut terminal = match setup_terminal() {
        Ok(t) => t,
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    };
    let code = ui_loop(&mut terminal, &controller).await;
    restore_terminal();
    code
}

/// Raw mode + alternate screen, plus a panic hook that restores the
/// terminal before the default hook prints — so a panic never leaves the
/// user's shell raw/alternate-screened.
fn setup_terminal() -> io::Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    install_panic_hook();
    Terminal::new(CrosstermBackend::new(stdout))
}

fn install_panic_hook() {
    let original = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        original(info);
    }));
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
}

fn to_key(ev: &KeyEvent) -> Option<Key> {
    match ev.code {
        KeyCode::Up => Some(Key::Up),
        KeyCode::Down => Some(Key::Down),
        KeyCode::Left => Some(Key::Left),
        KeyCode::Right => Some(Key::Right),
        KeyCode::Tab => Some(Key::Tab),
        KeyCode::Enter => Some(Key::Enter),
        KeyCode::Esc => Some(Key::Esc),
        KeyCode::Backspace => Some(Key::Backspace),
        KeyCode::Char(' ') => Some(Key::Space),
        KeyCode::Char(c) => Some(Key::Char(c)),
        _ => None,
    }
}

/// Immediate-mode redraw loop: mode starts as wizard/dashboard per
/// `initial_mode`, redraws after every key press and every 1s tick (crossterm
/// `EventStream` + `tokio::select!` — this replaces TS's `EventEmitter`).
/// Only `KeyEventKind::Press` is handled (crossterm on some platforms also
/// reports Release/Repeat). A completed wizard hands off to a freshly loaded
/// `DashboardState`; either machine's `exit` ends the loop with code 0.
async fn ui_loop(terminal: &mut Term, controller: &AppController) -> u8 {
    let mut mode = initial_mode(controller).await;
    let mut wizard = match mode {
        Mode::Wizard => Some(WizardState::new(controller).await),
        Mode::Dashboard => None,
    };
    let mut dashboard = match mode {
        Mode::Dashboard => Some(DashboardState::new(controller).await),
        Mode::Wizard => None,
    };

    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(1));

    redraw(
        terminal,
        mode,
        wizard.as_ref(),
        dashboard.as_ref(),
        controller,
    )
    .await;

    loop {
        tokio::select! {
            maybe_ev = events.next() => {
                match maybe_ev {
                    Some(Ok(Event::Key(ke))) if ke.kind == KeyEventKind::Press => {
                        if let Some(key) = to_key(&ke) {
                            match mode {
                                Mode::Wizard => {
                                    let w = wizard.as_mut().expect("wizard state while in Mode::Wizard");
                                    w.handle(key, controller).await;
                                    if w.exit {
                                        return 0;
                                    }
                                    if w.done {
                                        dashboard = Some(DashboardState::new(controller).await);
                                        wizard = None;
                                        mode = Mode::Dashboard;
                                    }
                                }
                                Mode::Dashboard => {
                                    let d = dashboard.as_mut().expect("dashboard state while in Mode::Dashboard");
                                    d.handle(key, controller).await;
                                    if d.exit {
                                        return 0;
                                    }
                                }
                            }
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) => {}
                    None => return 0,
                }
            }
            _ = ticker.tick() => {
                if let Some(d) = dashboard.as_mut() {
                    d.refresh(controller).await;
                }
            }
        }
        redraw(
            terminal,
            mode,
            wizard.as_ref(),
            dashboard.as_ref(),
            controller,
        )
        .await;
    }
}

async fn redraw(
    terminal: &mut Term,
    mode: Mode,
    wizard: Option<&WizardState>,
    dashboard: Option<&DashboardState>,
    controller: &AppController,
) {
    let ctx = RenderCtx::gather(controller).await;
    let _ = terminal.draw(|f| match mode {
        Mode::Wizard => {
            if let Some(w) = wizard {
                draw_wizard(f, w, &ctx);
            }
        }
        Mode::Dashboard => {
            if let Some(d) = dashboard {
                draw_dashboard(f, d, &ctx);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use controller::controller_in;

    #[tokio::test]
    async fn initial_mode_is_wizard_until_configured_then_dashboard() {
        let dir = tempfile::tempdir().unwrap();
        let c = controller_in(dir.path()).await;
        assert_eq!(initial_mode(&c).await, Mode::Wizard);
        for (k, v) in [
            ("discord.token", "t"),
            ("discord.app_id", "a"),
            ("discord.guild_id", "g"),
            ("workdir_root", "/repos"),
        ] {
            c.set(k, v).await.unwrap();
        }
        assert_eq!(initial_mode(&c).await, Mode::Dashboard);
    }
}
