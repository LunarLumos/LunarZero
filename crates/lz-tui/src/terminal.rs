//! Terminal setup/teardown, background-color detection (OSC 11), notifications
//! and window title.

use std::io::{Read, Write};
use std::time::Duration;

use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};

use crate::theme::{Mode, Rgba};

pub struct Guard {
    pub keyboard_enhanced: bool,
    pub mouse: bool,
}

impl Guard {
    pub fn enter(mouse: bool) -> anyhow::Result<Guard> {
        enable_raw_mode()?;
        let mut out = std::io::stdout();
        execute!(
            out,
            EnterAlternateScreen,
            crossterm::event::EnableBracketedPaste,
            crossterm::event::EnableFocusChange
        )?;
        if mouse {
            let _ = execute!(out, crossterm::event::EnableMouseCapture);
        }
        let keyboard_enhanced = matches!(crossterm::terminal::supports_keyboard_enhancement(), Ok(true));
        if keyboard_enhanced {
            use crossterm::event::{KeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
            let _ = execute!(
                out,
                PushKeyboardEnhancementFlags(
                    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                        | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                )
            );
        }
        Ok(Guard {
            keyboard_enhanced,
            mouse,
        })
    }

    pub fn restore(keyboard_enhanced: bool, mouse: bool) {
        let mut out = std::io::stdout();
        if keyboard_enhanced {
            let _ = execute!(out, crossterm::event::PopKeyboardEnhancementFlags);
        }
        if mouse {
            let _ = execute!(out, crossterm::event::DisableMouseCapture);
        }
        let _ = execute!(
            out,
            crossterm::event::DisableFocusChange,
            crossterm::event::DisableBracketedPaste,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        );
        let _ = disable_raw_mode();
        let _ = write!(out, "\x1b]0;\x07");
        let _ = out.flush();
    }

    /// Temporarily leave the TUI (for `$EDITOR` / suspend), run `f`, re-enter.
    pub fn suspend<T>(&self, f: impl FnOnce() -> T) -> anyhow::Result<T> {
        Self::restore(self.keyboard_enhanced, self.mouse);
        let r = f();
        enable_raw_mode()?;
        let mut out = std::io::stdout();
        execute!(
            out,
            EnterAlternateScreen,
            crossterm::event::EnableBracketedPaste,
            crossterm::event::EnableFocusChange
        )?;
        if self.mouse {
            let _ = execute!(out, crossterm::event::EnableMouseCapture);
        }
        if self.keyboard_enhanced {
            use crossterm::event::{KeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
            let _ = execute!(
                out,
                PushKeyboardEnhancementFlags(
                    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                        | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                )
            );
        }
        Ok(r)
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        Self::restore(self.keyboard_enhanced, self.mouse);
    }
}

pub fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        Guard::restore(true, true);
        prev(info);
    }));
}

/// Query the terminal's default background/foreground (OSC 11 / OSC 10).
/// Must run in raw mode *before* the crossterm event stream starts.
pub fn query_colors(timeout: Duration) -> (Option<Rgba>, Option<Rgba>) {
    if std::env::var("LZ_NO_OSC").is_ok() || std::env::var("TERM").map(|t| t == "dumb").unwrap_or(false) {
        return (None, None);
    }
    let Ok(_raw) = RawGuard::new() else {
        return (None, None);
    };
    let mut out = std::io::stdout();
    if write!(out, "\x1b]11;?\x1b\\\x1b]10;?\x1b\\\x1b[c")
        .and_then(|_| out.flush())
        .is_err()
    {
        return (None, None);
    }
    let mut buf = Vec::new();
    let deadline = std::time::Instant::now() + timeout;
    let mut stdin = std::io::stdin();
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let mut pfd = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        let r = unsafe { libc::poll(&mut pfd, 1, remaining.as_millis() as i32) };
        if r <= 0 {
            break;
        }
        let mut chunk = [0u8; 256];
        match stdin.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
        // DA1 reply (`ESC [ ? ... c`) terminates the exchange
        if buf.windows(2).any(|w| w == b"[?") && buf.last() == Some(&b'c') {
            break;
        }
    }
    let s = String::from_utf8_lossy(&buf).to_string();
    (parse_osc(&s, 11), parse_osc(&s, 10))
}

fn parse_osc(s: &str, code: u8) -> Option<Rgba> {
    let key = format!("]{code};");
    let i = s.find(&key)? + key.len();
    let rest = &s[i..];
    let end = rest.find(['\x07', '\x1b']).unwrap_or(rest.len());
    let spec = &rest[..end];
    let spec = spec.strip_prefix("rgb:")?;
    let parts: Vec<&str> = spec.split('/').collect();
    if parts.len() != 3 {
        return None;
    }
    let ch = |p: &str| -> Option<u8> {
        let v = u32::from_str_radix(p, 16).ok()?;
        Some(match p.len() {
            1 => (v * 17) as u8,
            2 => v as u8,
            3 => (v >> 4) as u8,
            4 => (v >> 8) as u8,
            _ => return None,
        })
    };
    Some(Rgba {
        r: ch(parts[0])?,
        g: ch(parts[1])?,
        b: ch(parts[2])?,
        a: 255,
    })
}

pub fn detect_mode(bg: Option<Rgba>) -> Mode {
    if let Some(bg) = bg {
        return if bg.luminance() > 0.5 {
            Mode::Light
        } else {
            Mode::Dark
        };
    }
    if let Ok(v) = std::env::var("COLORFGBG")
        && let Some(bg) = v.rsplit(';').next().and_then(|b| b.parse::<u8>().ok())
    {
        return if (7..=15).contains(&bg) && bg != 8 {
            Mode::Light
        } else {
            Mode::Dark
        };
    }
    Mode::Dark
}

struct RawGuard(bool);
impl RawGuard {
    fn new() -> std::io::Result<RawGuard> {
        let was = crossterm::terminal::is_raw_mode_enabled()?;
        if !was {
            enable_raw_mode()?;
        }
        Ok(RawGuard(was))
    }
}
impl Drop for RawGuard {
    fn drop(&mut self) {
        if !self.0 {
            let _ = disable_raw_mode();
        }
    }
}

pub fn set_title(title: &str) {
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]0;{title}\x07");
    let _ = out.flush();
}

/// Desktop notification via OSC 777 (rxvt/kitty/wezterm) + OSC 9 (iTerm2), plus a bell.
pub fn notify(title: &str, body: &str, bell: bool) {
    let mut out = std::io::stdout();
    let _ = write!(
        out,
        "\x1b]777;notify;{title};{body}\x07\x1b]9;{title}: {body}\x07"
    );
    if bell {
        let _ = write!(out, "\x07");
    }
    let _ = out.flush();
}
