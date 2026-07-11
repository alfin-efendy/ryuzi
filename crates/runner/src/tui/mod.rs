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

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
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

/// The wizard runs first-run, the dashboard once settings are complete
/// (`is_configured`); re-checked on every `AppController::new` (the wizard
/// pre-checks whichever gateways are already enabled).
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
        spawn_daemon: None,
        kill_daemon: None,
    });

    let mut terminal = match setup_terminal() {
        Ok(t) => t,
        Err(e) => {
            // `setup_terminal` may have already enabled raw mode (or even
            // entered the alternate screen) before the failing step —
            // `restore_terminal` is best-effort and safe to call even when
            // those weren't engaged, so always run it before bailing out.
            restore_terminal();
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
/// user's shell raw/alternate-screened. The panic hook is installed right
/// after `enable_raw_mode` succeeds and *before* the next fallible step
/// (`EnterAlternateScreen`), so the window during which the terminal is raw
/// but unprotected by the hook is as small as possible.
fn setup_terminal() -> io::Result<Term> {
    enable_raw_mode()?;
    install_panic_hook();
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

fn install_panic_hook() {
    let original = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        original(info);
    }));
}

/// Best-effort terminal teardown: both steps swallow their `Result`, so this
/// is safe to call even when raw mode was never enabled or the alternate
/// screen was never entered (e.g. `setup_terminal` failed partway through).
fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
}

/// What the event loop should do with one raw `KeyEvent`, computed by
/// `map_key_event` before the mode-specific state machines ever see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoopAction {
    /// Ctrl+C (any phase): end `ui_loop` immediately — the user must always
    /// have an exit, even mid-wizard.
    Quit,
    /// Maps to a state-machine `Key`.
    Key(Key),
    /// Not handled — e.g. any other ctrl-modified char, which must not leak
    /// into a wizard/dashboard draft.
    None,
}

/// Maps one crossterm `KeyEvent` (assumed `KeyEventKind::Press` — the caller
/// filters that before calling) into a `LoopAction`. Ctrl+C always produces
/// `Quit`, from *any* phase, including the wizard's Fields phase where Esc is
/// a deliberate no-op and would otherwise leave the user with no way out.
/// Every other ctrl-modified `Char` is swallowed to `None` rather than falling
/// through to a plain `Key::Char`, so e.g. ctrl+x never types an 'x' into a
/// draft. Non-ctrl keys map the same as before (shift is reflected in the
/// char crossterm reports, e.g. shift+'q' arrives as `Char('Q')`).
pub(crate) fn map_key_event(ev: &KeyEvent) -> LoopAction {
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        return match ev.code {
            KeyCode::Char('c' | 'C') => LoopAction::Quit,
            _ => LoopAction::None,
        };
    }
    match ev.code {
        KeyCode::Up => LoopAction::Key(Key::Up),
        KeyCode::Down => LoopAction::Key(Key::Down),
        KeyCode::Left => LoopAction::Key(Key::Left),
        KeyCode::Right => LoopAction::Key(Key::Right),
        KeyCode::Tab => LoopAction::Key(Key::Tab),
        KeyCode::Enter => LoopAction::Key(Key::Enter),
        KeyCode::Esc => LoopAction::Key(Key::Esc),
        KeyCode::Backspace => LoopAction::Key(Key::Backspace),
        KeyCode::Char(' ') => LoopAction::Key(Key::Space),
        KeyCode::Char(c) => LoopAction::Key(Key::Char(c)),
        _ => LoopAction::None,
    }
}

/// Immediate-mode redraw loop: mode starts as wizard/dashboard per
/// `initial_mode`, redraws after every key press and every 1s tick (crossterm
/// `EventStream` + `tokio::select!`).
/// Only `KeyEventKind::Press` is handled (crossterm on some platforms also
/// reports Release/Repeat). Every key event is mapped via `map_key_event`
/// before either state machine sees it: Ctrl+C ends the loop immediately
/// from any phase (the user must always have an exit), and any
/// other ctrl combo is swallowed rather than leaking into a wizard/dashboard
/// draft as a plain character. A completed wizard hands off to a freshly
/// loaded `DashboardState`; either machine's `exit` ends the loop with code 0.
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
                        match map_key_event(&ke) {
                            LoopAction::Quit => return 0,
                            LoopAction::Key(key) => {
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
                            LoopAction::None => {}
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

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn ctrl_c_quits_from_any_phase() {
        assert_eq!(
            map_key_event(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            LoopAction::Quit
        );
        // Terminals commonly report the shifted form too; either must quit.
        assert_eq!(
            map_key_event(&key(KeyCode::Char('C'), KeyModifiers::CONTROL)),
            LoopAction::Quit
        );
    }

    #[test]
    fn other_ctrl_combos_are_swallowed_not_leaked_as_chars() {
        assert_eq!(
            map_key_event(&key(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            LoopAction::None
        );
    }

    #[test]
    fn plain_char_maps_through() {
        assert_eq!(
            map_key_event(&key(KeyCode::Char('c'), KeyModifiers::NONE)),
            LoopAction::Key(Key::Char('c'))
        );
    }

    #[test]
    fn shift_modified_char_maps_through() {
        assert_eq!(
            map_key_event(&key(KeyCode::Char('Q'), KeyModifiers::SHIFT)),
            LoopAction::Key(Key::Char('Q'))
        );
    }

    #[test]
    fn navigation_and_control_keys_map_correctly() {
        let cases = [
            (KeyCode::Esc, Key::Esc),
            (KeyCode::Enter, Key::Enter),
            (KeyCode::Backspace, Key::Backspace),
            (KeyCode::Tab, Key::Tab),
            (KeyCode::Up, Key::Up),
            (KeyCode::Down, Key::Down),
            (KeyCode::Left, Key::Left),
            (KeyCode::Right, Key::Right),
            (KeyCode::Char(' '), Key::Space),
        ];
        for (code, expected) in cases {
            assert_eq!(
                map_key_event(&key(code, KeyModifiers::NONE)),
                LoopAction::Key(expected),
                "{code:?}"
            );
        }
    }
}
