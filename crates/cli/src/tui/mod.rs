pub mod controller;
pub mod dashboard;
pub mod theme;
pub mod wizard;

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
