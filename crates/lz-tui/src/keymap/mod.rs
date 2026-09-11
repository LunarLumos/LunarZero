//! Keybindings: chord strings (`"<leader>n"`,
//! `"ctrl+shift+p"`, `"a,b"`, `"none"`), a leader key with timeout, and a
//! table of named actions with defaults overridable from `tui.json`.

use std::collections::HashMap;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::Value;

pub const LEADER_DEFAULT: &str = "ctrl+x";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Chord {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub sup: bool,
    /// Preceded by the leader key.
    pub leader: bool,
}

impl Chord {
    pub fn parse(s: &str) -> Option<Chord> {
        let mut s = s.trim();
        let mut leader = false;
        if let Some(rest) = s.strip_prefix("<leader>") {
            leader = true;
            s = rest;
        }
        let mut chord = Chord {
            code: KeyCode::Null,
            ctrl: false,
            alt: false,
            shift: false,
            sup: false,
            leader,
        };
        let parts: Vec<&str> = s.split('+').collect();
        let (mods, key) = parts.split_at(parts.len().saturating_sub(1));
        for m in mods {
            match m.to_lowercase().as_str() {
                "ctrl" | "control" => chord.ctrl = true,
                "alt" | "meta" | "option" => chord.alt = true,
                "shift" => chord.shift = true,
                "super" | "cmd" | "hyper" => chord.sup = true,
                _ => return None,
            }
        }
        let key = key.first().copied().unwrap_or("");
        chord.code = match key.to_lowercase().as_str() {
            "return" | "enter" => KeyCode::Enter,
            "escape" | "esc" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backspace" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "insert" => KeyCode::Insert,
            "space" => KeyCode::Char(' '),
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            k if k.starts_with('f') && k[1..].parse::<u8>().is_ok() => KeyCode::F(k[1..].parse().unwrap()),
            _ => {
                let mut chars = key.chars();
                let c = chars.next()?;
                if chars.next().is_some() {
                    return None;
                }
                if c.is_ascii_uppercase() {
                    chord.shift = true;
                }
                KeyCode::Char(c.to_ascii_lowercase())
            }
        };
        Some(chord)
    }

    /// Normalize a crossterm key event into a chord (no leader).
    pub fn from_event(ev: &KeyEvent) -> Chord {
        let mut code = ev.code;
        let mut shift = ev.modifiers.contains(KeyModifiers::SHIFT);
        if let KeyCode::Char(c) = code {
            if c.is_ascii_uppercase() {
                shift = true;
                code = KeyCode::Char(c.to_ascii_lowercase());
            }
            if c == '\n' || c == '\r' {
                code = KeyCode::Enter;
            }
        }
        if code == KeyCode::BackTab {
            code = KeyCode::Tab;
            shift = true;
        }
        Chord {
            code,
            ctrl: ev.modifiers.contains(KeyModifiers::CONTROL),
            alt: ev.modifiers.contains(KeyModifiers::ALT),
            shift,
            sup: ev.modifiers.contains(KeyModifiers::SUPER) || ev.modifiers.contains(KeyModifiers::META),
            leader: false,
        }
    }

    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        if self.leader {
            parts.push("<leader>".to_string());
        }
        if self.ctrl {
            parts.push("ctrl".into());
        }
        if self.alt {
            parts.push("alt".into());
        }
        if self.shift {
            parts.push("shift".into());
        }
        if self.sup {
            parts.push("super".into());
        }
        parts.push(match self.code {
            KeyCode::Enter => "enter".into(),
            KeyCode::Esc => "esc".into(),
            KeyCode::Tab => "tab".into(),
            KeyCode::Backspace => "backspace".into(),
            KeyCode::Delete => "delete".into(),
            KeyCode::Up => "↑".into(),
            KeyCode::Down => "↓".into(),
            KeyCode::Left => "←".into(),
            KeyCode::Right => "→".into(),
            KeyCode::Home => "home".into(),
            KeyCode::End => "end".into(),
            KeyCode::PageUp => "pgup".into(),
            KeyCode::PageDown => "pgdn".into(),
            KeyCode::F(n) => format!("f{n}"),
            KeyCode::Char(' ') => "space".into(),
            KeyCode::Char(c) => c.to_string(),
            _ => "?".into(),
        });
        let s = parts.join("+");
        s.replace("<leader>+", "<leader>")
    }
}

/// Named actions with their default chords.
pub const DEFAULTS: &[(&str, &str, &str)] = &[
    ("app_exit", "ctrl+c,ctrl+d,<leader>q", "Exit the application"),
    ("command_list", "ctrl+p", "List available commands"),
    ("help_show", "<leader>?", "Open help dialog"),
    ("editor_open", "<leader>e", "Open external editor"),
    ("theme_list", "<leader>t", "List available themes"),
    ("sidebar_toggle", "<leader>b", "Toggle sidebar"),
    ("status_view", "<leader>s", "View status"),
    ("session_export", "<leader>x", "Export session"),
    ("session_new", "<leader>n", "Create a new session"),
    ("session_list", "<leader>l", "List all sessions"),
    ("session_rename", "ctrl+r", "Rename session"),
    ("session_delete", "ctrl+d", "Delete session"),
    ("session_interrupt", "escape", "Interrupt current session"),
    ("session_compact", "<leader>c", "Compact the session"),
    ("session_child_cycle", "right", "Go to next child session"),
    (
        "session_child_cycle_reverse",
        "left",
        "Go to previous child session",
    ),
    ("session_parent", "up", "Go to parent session"),
    ("session_pin_toggle", "ctrl+f", "Pin or unpin session"),
    ("model_list", "<leader>m", "List available models"),
    ("model_cycle_recent", "f2", "Next recently used model"),
    (
        "model_cycle_recent_reverse",
        "shift+f2",
        "Previous recently used model",
    ),
    ("model_favorite_toggle", "ctrl+f", "Toggle model favorite"),
    (
        "model_provider_list",
        "ctrl+a",
        "Open provider list from model dialog",
    ),
    ("mcp_list", "none", "List MCP servers"),
    ("provider_connect", "none", "Connect provider"),
    ("agent_list", "<leader>a", "List agents"),
    ("agent_cycle", "tab", "Next agent"),
    ("agent_cycle_reverse", "shift+tab", "Previous agent"),
    ("variant_cycle", "ctrl+t", "Cycle model variants"),
    (
        "messages_page_up",
        "pageup,ctrl+alt+b",
        "Scroll messages up by one page",
    ),
    (
        "messages_page_down",
        "pagedown,ctrl+alt+f",
        "Scroll messages down by one page",
    ),
    ("messages_line_up", "ctrl+alt+y", "Scroll messages up by one line"),
    (
        "messages_line_down",
        "ctrl+alt+e",
        "Scroll messages down by one line",
    ),
    (
        "messages_half_page_up",
        "ctrl+alt+u",
        "Scroll messages up by half page",
    ),
    (
        "messages_half_page_down",
        "ctrl+alt+d",
        "Scroll messages down by half page",
    ),
    ("messages_first", "ctrl+g,home", "Navigate to first message"),
    ("messages_last", "ctrl+alt+g,end", "Navigate to last message"),
    ("messages_copy", "<leader>y", "Copy last message"),
    ("messages_undo", "<leader>u", "Undo message"),
    ("messages_redo", "<leader>r", "Redo message"),
    (
        "messages_toggle_conceal",
        "<leader>h",
        "Toggle code block concealment",
    ),
    ("tool_details", "none", "Toggle tool details visibility"),
    ("display_thinking", "none", "Toggle thinking blocks visibility"),
    ("input_clear", "ctrl+c", "Clear input field"),
    ("input_paste", "ctrl+v", "Paste from clipboard"),
    ("input_submit", "return", "Submit input"),
    (
        "input_newline",
        "shift+return,ctrl+return,alt+return,ctrl+j",
        "Insert newline in input",
    ),
    ("input_move_left", "left,ctrl+b", "Move cursor left"),
    ("input_move_right", "right,ctrl+f", "Move cursor right"),
    ("input_move_up", "up", "Move cursor up"),
    ("input_move_down", "down", "Move cursor down"),
    ("input_line_home", "ctrl+a", "Move to start of line"),
    ("input_line_end", "ctrl+e", "Move to end of line"),
    ("input_buffer_home", "home", "Move to start of buffer"),
    ("input_buffer_end", "end", "Move to end of buffer"),
    ("input_delete_to_line_end", "ctrl+k", "Delete to end of line"),
    ("input_delete_to_line_start", "ctrl+u", "Delete to start of line"),
    ("input_backspace", "backspace,shift+backspace", "Backspace"),
    ("input_delete", "ctrl+d,delete,shift+delete", "Delete character"),
    ("input_undo", "ctrl+-,super+z", "Undo in input"),
    ("input_redo", "ctrl+.,super+shift+z", "Redo in input"),
    (
        "input_word_forward",
        "alt+f,alt+right,ctrl+right",
        "Move word forward",
    ),
    (
        "input_word_backward",
        "alt+b,alt+left,ctrl+left",
        "Move word backward",
    ),
    (
        "input_delete_word_forward",
        "alt+d,alt+delete,ctrl+delete",
        "Delete word forward",
    ),
    (
        "input_delete_word_backward",
        "ctrl+w,ctrl+backspace,alt+backspace",
        "Delete word backward",
    ),
    ("history_previous", "up", "Previous history item"),
    ("history_next", "down", "Next history item"),
    ("terminal_suspend", "ctrl+z", "Suspend terminal"),
    ("tips_toggle", "<leader>h", "Toggle tips on home screen"),
    ("which_key_toggle", "ctrl+alt+k", "Toggle which-key panel"),
    ("dialog_select_prev", "up,ctrl+p", "Previous item"),
    ("dialog_select_next", "down,ctrl+n", "Next item"),
    ("dialog_select_submit", "return", "Select item"),
    ("dialog_close", "escape", "Close dialog"),
    ("autocomplete_select", "return", "Insert completion"),
    ("autocomplete_complete", "tab", "Complete"),
    ("autocomplete_hide", "escape", "Hide completions"),
    ("autocomplete_prev", "up,ctrl+p", "Previous completion"),
    ("autocomplete_next", "down,ctrl+n", "Next completion"),
    (
        "permission_fullscreen",
        "ctrl+f",
        "Toggle fullscreen permission view",
    ),
];

pub struct Binding {
    pub action: String,
    pub chords: Vec<Chord>,
    pub description: String,
}

pub struct Keymap {
    pub leader: Chord,
    pub leader_timeout: Duration,
    bindings: Vec<Binding>,
    index: HashMap<Chord, Vec<String>>,
}

fn parse_value(v: &Value) -> Option<Vec<Chord>> {
    match v {
        Value::Bool(false) => Some(Vec::new()),
        Value::String(s) if s == "none" => Some(Vec::new()),
        Value::String(s) => Some(s.split(',').filter_map(Chord::parse).collect()),
        Value::Array(items) => Some(items.iter().filter_map(parse_value).flatten().collect()),
        Value::Object(o) => o.get("key").and_then(parse_value),
        _ => None,
    }
}

impl Keymap {
    pub fn new(overrides: Option<&serde_json::Map<String, Value>>, leader_timeout_ms: u64) -> Self {
        let leader_str = overrides
            .and_then(|o| o.get("leader"))
            .and_then(Value::as_str)
            .unwrap_or(LEADER_DEFAULT);
        let leader = Chord::parse(leader_str).unwrap_or_else(|| Chord::parse(LEADER_DEFAULT).unwrap());
        let mut bindings = Vec::new();
        for (action, default, description) in DEFAULTS {
            let chords = overrides
                .and_then(|o| o.get(*action))
                .and_then(parse_value)
                .unwrap_or_else(|| default.split(',').filter_map(Chord::parse).collect());
            bindings.push(Binding {
                action: action.to_string(),
                chords,
                description: description.to_string(),
            });
        }
        let mut index: HashMap<Chord, Vec<String>> = HashMap::new();
        for b in &bindings {
            for c in &b.chords {
                index.entry(c.clone()).or_default().push(b.action.clone());
            }
        }
        Self {
            leader,
            leader_timeout: Duration::from_millis(leader_timeout_ms),
            bindings,
            index,
        }
    }

    /// All actions bound to a chord (a chord may serve several contexts).
    pub fn actions_for(&self, chord: &Chord) -> Vec<&str> {
        self.index
            .get(chord)
            .map(|v| v.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }

    pub fn is_leader(&self, chord: &Chord) -> bool {
        chord.code == self.leader.code
            && chord.ctrl == self.leader.ctrl
            && chord.alt == self.leader.alt
            && chord.sup == self.leader.sup
    }

    pub fn chords_for(&self, action: &str) -> Vec<&Chord> {
        self.bindings
            .iter()
            .find(|b| b.action == action)
            .map(|b| b.chords.iter().collect())
            .unwrap_or_default()
    }

    pub fn label(&self, action: &str) -> String {
        self.chords_for(action)
            .first()
            .map(|c| c.label())
            .unwrap_or_default()
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    /// Leader-prefixed chords for the which-key overlay.
    pub fn leader_bindings(&self) -> Vec<(String, &str, &str)> {
        let mut out: Vec<(String, &str, &str)> = self
            .bindings
            .iter()
            .flat_map(|b| {
                b.chords.iter().filter(|c| c.leader).map(move |c| {
                    (
                        c.label().replace("<leader>", ""),
                        b.action.as_str(),
                        b.description.as_str(),
                    )
                })
            })
            .collect();
        out.sort();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_chords() {
        let c = Chord::parse("ctrl+shift+p").unwrap();
        assert!(c.ctrl && c.shift && !c.leader);
        assert_eq!(c.code, KeyCode::Char('p'));
        let l = Chord::parse("<leader>n").unwrap();
        assert!(l.leader && l.code == KeyCode::Char('n'));
        assert_eq!(Chord::parse("f2").unwrap().code, KeyCode::F(2));
        assert!(Chord::parse("E").unwrap().shift);
        assert!(Chord::parse("bogus+x").is_none());
    }

    #[test]
    fn overrides_and_lookup() {
        let mut o = serde_json::Map::new();
        o.insert("session_new".into(), Value::String("ctrl+n".into()));
        o.insert("session_list".into(), Value::String("none".into()));
        let km = Keymap::new(Some(&o), 2000);
        let chord = Chord::parse("ctrl+n").unwrap();
        assert!(km.actions_for(&chord).contains(&"session_new"));
        assert!(km.chords_for("session_new").len() == 1);
        assert!(km.chords_for("session_list").is_empty());
        assert_eq!(km.label("model_list"), "<leader>m");
        let ev = KeyEvent::new(KeyCode::Char('P'), KeyModifiers::CONTROL);
        let c = Chord::from_event(&ev);
        assert!(c.ctrl && c.shift && c.code == KeyCode::Char('p'));
    }
}
