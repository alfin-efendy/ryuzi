#[derive(Clone, Copy)]
pub enum Tone {
    Ok,
    Warn,
    Bad,
}

pub fn color_enabled() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").map(|t| t != "dumb").unwrap_or(true)
}

pub fn paint(text: &str, tone: Tone, bold: bool) -> String {
    if !color_enabled() {
        return text.to_string();
    }
    let code = match tone {
        Tone::Ok => 32,
        Tone::Warn => 33,
        Tone::Bad => 31,
    };
    if bold {
        format!("\x1b[1;{code}m{text}\x1b[0m")
    } else {
        format!("\x1b[{code}m{text}\x1b[0m")
    }
}
