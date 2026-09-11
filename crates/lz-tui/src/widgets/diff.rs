//! Unified diff rendering with theme colors. Accepts a unified-diff patch
//! (as stored on tool parts) and colors +/- lines and hunk headers.

use ratatui::text::{Line, Span};

use crate::theme::Theme;

pub fn render(patch: &str, width: usize, theme: &Theme, max_lines: Option<usize>) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut old_no: u64 = 0;
    let mut new_no: u64 = 0;
    let numw = 4usize;
    let body = width.saturating_sub(numw * 2 + 4).max(10);
    for raw in patch.lines() {
        if raw.starts_with("---")
            || raw.starts_with("+++")
            || raw.starts_with("diff ")
            || raw.starts_with("index ")
        {
            continue;
        }
        if let Some(rest) = raw.strip_prefix("@@") {
            // @@ -a,b +c,d @@
            let nums: Vec<&str> = rest.split_whitespace().take(2).collect();
            if let Some(o) = nums
                .first()
                .and_then(|s| s.trim_start_matches('-').split(',').next())
            {
                old_no = o.parse().unwrap_or(1);
            }
            if let Some(n) = nums
                .get(1)
                .and_then(|s| s.trim_start_matches('+').split(',').next())
            {
                new_no = n.parse().unwrap_or(1);
            }
            out.push(Line::from(Span::styled(
                format!("@@{rest}"),
                theme.fg("diffHunkHeader"),
            )));
            continue;
        }
        let (kind, text) = match raw.chars().next() {
            Some('+') => ('+', &raw[1..]),
            Some('-') => ('-', &raw[1..]),
            Some(' ') => (' ', &raw[1..]),
            Some('\\') => continue,
            _ => (' ', raw),
        };
        let (fg, bg, lnbg, l, r) = match kind {
            '+' => {
                new_no += 1;
                (
                    theme.fg("diffAdded"),
                    theme.bg("diffAddedBg"),
                    theme.bg("diffAddedLineNumberBg"),
                    String::from("    "),
                    format!("{:>4}", new_no),
                )
            }
            '-' => {
                old_no += 1;
                (
                    theme.fg("diffRemoved"),
                    theme.bg("diffRemovedBg"),
                    theme.bg("diffRemovedLineNumberBg"),
                    format!("{:>4}", old_no),
                    String::from("    "),
                )
            }
            _ => {
                old_no += 1;
                new_no += 1;
                (
                    theme.fg("diffContext"),
                    theme.bg("diffContextBg"),
                    theme.bg("diffContextBg"),
                    format!("{:>4}", old_no),
                    format!("{:>4}", new_no),
                )
            }
        };
        let expanded = text.replace('\t', "    ");
        let mut chunks: Vec<String> = Vec::new();
        let mut cur = String::new();
        for ch in expanded.chars() {
            if unicode_width::UnicodeWidthStr::width(cur.as_str()) >= body {
                chunks.push(std::mem::take(&mut cur));
            }
            cur.push(ch);
        }
        chunks.push(cur);
        for (i, chunk) in chunks.into_iter().enumerate() {
            let (l, r) = if i == 0 {
                (l.clone(), r.clone())
            } else {
                ("    ".into(), "    ".into())
            };
            let pad = " ".repeat(body.saturating_sub(unicode_width::UnicodeWidthStr::width(chunk.as_str())));
            out.push(Line::from(vec![
                Span::styled(format!("{l} {r} "), theme.fg("diffLineNumber").patch(lnbg)),
                Span::styled(format!("{kind} "), fg.patch(bg)),
                Span::styled(chunk, fg.patch(bg)),
                Span::styled(pad, bg),
            ]));
        }
        if let Some(max) = max_lines
            && out.len() >= max
        {
            out.push(Line::from(Span::styled("…", theme.muted())));
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Mode, load};

    #[test]
    fn renders_hunks_and_truncates() {
        let theme = load("lunar", Mode::Dark, &[], (None, None)).unwrap();
        let patch = "--- a/x.rs\n+++ b/x.rs\n@@ -1,3 +1,3 @@\n a\n-b\n+c\n d\n";
        let lines = render(patch, 40, &theme, None);
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .collect();
        assert!(text.iter().any(|l| l.contains("b")));
        assert!(text.iter().any(|l| l.contains("c")));
        let short = render(patch, 40, &theme, Some(2));
        assert!(short.len() <= 4, "{}", short.len());
    }
}
