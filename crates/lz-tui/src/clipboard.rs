//! Clipboard access: native (arboard / platform tools) with an OSC 52 fallback
//! so copying works over SSH too.

use std::io::Write;

pub fn copy(text: &str) {
    let mut ok = false;
    if let Ok(mut cb) = arboard::Clipboard::new() {
        ok = cb.set_text(text.to_string()).is_ok();
    }
    if !ok {
        let _ = platform_copy(text);
    }
    // OSC 52 always, harmless when unsupported
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]52;c;{b64}\x07");
    let _ = out.flush();
}

fn platform_copy(text: &str) -> bool {
    let cmds: &[&[&str]] = if cfg!(target_os = "macos") {
        &[&["pbcopy"]]
    } else {
        &[
            &["wl-copy"],
            &["xclip", "-selection", "clipboard"],
            &["xsel", "--clipboard", "--input"],
        ]
    };
    for cmd in cmds {
        if let Ok(mut child) = std::process::Command::new(cmd[0])
            .args(&cmd[1..])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            if child.wait().map(|s| s.success()).unwrap_or(false) {
                return true;
            }
        }
    }
    false
}

pub fn paste_text() -> Option<String> {
    if let Ok(mut cb) = arboard::Clipboard::new()
        && let Ok(t) = cb.get_text()
    {
        return Some(t);
    }
    let cmds: &[&[&str]] = if cfg!(target_os = "macos") {
        &[&["pbpaste"]]
    } else {
        &[
            &["wl-paste", "--no-newline"],
            &["xclip", "-selection", "clipboard", "-o"],
            &["xsel", "--clipboard", "--output"],
        ]
    };
    for cmd in cmds {
        if let Ok(out) = std::process::Command::new(cmd[0]).args(&cmd[1..]).output()
            && out.status.success()
        {
            return Some(String::from_utf8_lossy(&out.stdout).to_string());
        }
    }
    None
}

/// Image on the clipboard as a PNG data URL, if any.
pub fn paste_image() -> Option<(String, String)> {
    let bytes: Option<Vec<u8>> = if cfg!(target_os = "macos") {
        std::process::Command::new("pngpaste").arg("-").output().ok().filter(|o| o.status.success() && !o.stdout.is_empty()).map(|o| o.stdout).or_else(|| {
            let tmp = std::env::temp_dir().join(format!("lz-paste-{}.png", std::process::id()));
            let script = format!(
                "set f to POSIX file \"{}\"\ntry\nset img to the clipboard as «class PNGf»\nset fh to open for access f with write permission\nwrite img to fh\nclose access fh\nreturn \"ok\"\non error\nreturn \"no\"\nend try",
                tmp.display()
            );
            let out = std::process::Command::new("osascript").arg("-e").arg(script).output().ok()?;
            if String::from_utf8_lossy(&out.stdout).trim() != "ok" {
                return None;
            }
            let b = std::fs::read(&tmp).ok();
            let _ = std::fs::remove_file(&tmp);
            b
        })
    } else {
        std::process::Command::new("wl-paste")
            .args(["--type", "image/png"])
            .output()
            .ok()
            .filter(|o| o.status.success() && !o.stdout.is_empty())
            .map(|o| o.stdout)
            .or_else(|| {
                std::process::Command::new("xclip")
                    .args(["-selection", "clipboard", "-t", "image/png", "-o"])
                    .output()
                    .ok()
                    .filter(|o| o.status.success() && !o.stdout.is_empty())
                    .map(|o| o.stdout)
            })
    };
    let bytes = bytes?;
    if infer::get(&bytes).map(|t| t.mime_type().starts_with("image/")) != Some(true) {
        return None;
    }
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(("image/png".into(), format!("data:image/png;base64,{b64}")))
}
