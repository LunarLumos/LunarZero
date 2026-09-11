//! Markdown → styled `Line`s using pulldown-cmark and the theme's `markdown*`
//! keys. Fenced code goes through the syntax highlighter.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::syntax;
use crate::theme::Theme;

struct Ctx<'a> {
    theme: &'a Theme,
    width: usize,
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    styles: Vec<Style>,
    list_stack: Vec<Option<u64>>,
    quote_depth: usize,
    in_code: Option<(String, String)>,
    pending_blank: bool,
    link: Option<String>,
}

impl Ctx<'_> {
    fn style(&self) -> Style {
        self.styles
            .last()
            .copied()
            .unwrap_or_else(|| self.theme.fg("markdownText"))
    }
    fn prefix(&self) -> String {
        let mut p = String::new();
        for _ in 0..self.quote_depth {
            p.push_str("▍ ");
        }
        for _ in 1..self.list_stack.len() {
            p.push_str("  ");
        }
        p
    }
    fn flush(&mut self) {
        if self.current.is_empty() {
            return;
        }
        let spans = std::mem::take(&mut self.current);
        let prefix = self.prefix();
        let avail = self.width.saturating_sub(prefix.width()).max(10);
        for line in wrap_spans(spans, avail) {
            let mut all = Vec::with_capacity(line.len() + 1);
            if !prefix.is_empty() {
                all.push(Span::styled(prefix.clone(), self.theme.fg("markdownBlockQuote")));
            }
            all.extend(line);
            self.lines.push(Line::from(all));
        }
    }
    fn blank(&mut self) {
        self.flush();
        if !self.lines.is_empty() && !self.pending_blank {
            self.lines.push(Line::from(""));
            self.pending_blank = true;
        }
    }
    fn push_text(&mut self, text: &str, style: Style) {
        self.pending_blank = false;
        for (i, seg) in text.split('\n').enumerate() {
            if i > 0 {
                self.flush();
            }
            if !seg.is_empty() {
                self.current.push(Span::styled(seg.to_string(), style));
            }
        }
    }
}

/// Greedy word-wrap over styled spans.
fn wrap_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Vec<Span<'static>>> {
    let mut lines: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut col = 0usize;
    for span in spans {
        let style = span.style;
        let text = span.content.to_string();
        let mut first = true;
        for word in text.split(' ') {
            let w = word.width();
            if !first {
                if col + 1 + w > width && col > 0 {
                    lines.push(Vec::new());
                    col = 0;
                } else {
                    lines.last_mut().unwrap().push(Span::styled(" ", style));
                    col += 1;
                }
            }
            first = false;
            if w == 0 {
                continue;
            }
            if w > width && col == 0 {
                // hard-break very long tokens
                let mut chunk = String::new();
                for ch in word.chars() {
                    if chunk.width() + 1 > width {
                        lines.last_mut().unwrap().push(Span::styled(chunk.clone(), style));
                        lines.push(Vec::new());
                        chunk.clear();
                    }
                    chunk.push(ch);
                }
                col = chunk.width();
                lines.last_mut().unwrap().push(Span::styled(chunk, style));
                continue;
            }
            if col + w > width && col > 0 {
                lines.push(Vec::new());
                col = 0;
            }
            lines
                .last_mut()
                .unwrap()
                .push(Span::styled(word.to_string(), style));
            col += w;
        }
    }
    lines
}

pub fn render(text: &str, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(text, opts);
    let mut ctx = Ctx {
        theme,
        width: width.max(10),
        lines: Vec::new(),
        current: Vec::new(),
        styles: Vec::new(),
        list_stack: Vec::new(),
        quote_depth: 0,
        in_code: None,
        pending_blank: true,
        link: None,
    };
    let mut table: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut in_table = false;

    for ev in parser {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { level, .. } => {
                    ctx.blank();
                    let hashes = "#".repeat(match level {
                        HeadingLevel::H1 => 1,
                        HeadingLevel::H2 => 2,
                        HeadingLevel::H3 => 3,
                        HeadingLevel::H4 => 4,
                        HeadingLevel::H5 => 5,
                        HeadingLevel::H6 => 6,
                    });
                    let st = theme.bold("markdownHeading");
                    ctx.current.push(Span::styled(format!("{hashes} "), st));
                    ctx.styles.push(st);
                }
                Tag::BlockQuote(_) => {
                    ctx.blank();
                    ctx.quote_depth += 1;
                    ctx.styles
                        .push(theme.fg("markdownBlockQuote").add_modifier(Modifier::ITALIC));
                }
                Tag::CodeBlock(kind) => {
                    ctx.blank();
                    let lang = match kind {
                        CodeBlockKind::Fenced(info) => syntax::lang_token(&info),
                        CodeBlockKind::Indented => String::new(),
                    };
                    ctx.in_code = Some((lang, String::new()));
                }
                Tag::List(start) => {
                    if ctx.list_stack.is_empty() {
                        ctx.blank();
                    } else {
                        ctx.flush();
                    }
                    ctx.list_stack.push(start);
                }
                Tag::Item => {
                    ctx.flush();
                    let marker = match ctx.list_stack.last_mut() {
                        Some(Some(n)) => {
                            let m = format!("{n}. ");
                            *n += 1;
                            m
                        }
                        _ => "• ".into(),
                    };
                    ctx.pending_blank = false;
                    ctx.current
                        .push(Span::styled(marker, theme.fg("markdownListEnumeration")));
                }
                Tag::Emphasis => ctx.styles.push(
                    ctx.style()
                        .patch(theme.fg("markdownEmph"))
                        .add_modifier(Modifier::ITALIC),
                ),
                Tag::Strong => ctx.styles.push(
                    ctx.style()
                        .patch(theme.fg("markdownStrong"))
                        .add_modifier(Modifier::BOLD),
                ),
                Tag::Strikethrough => ctx.styles.push(ctx.style().add_modifier(Modifier::CROSSED_OUT)),
                Tag::Link { dest_url, .. } => {
                    ctx.link = Some(dest_url.to_string());
                    ctx.styles
                        .push(theme.fg("markdownLinkText").add_modifier(Modifier::UNDERLINED));
                }
                Tag::Image { dest_url, .. } => {
                    ctx.push_text(&format!("[image: {dest_url}]"), theme.fg("markdownImageText"));
                }
                Tag::Table(_) => {
                    ctx.blank();
                    in_table = true;
                    table.clear();
                }
                Tag::TableHead | Tag::TableRow => row.clear(),
                Tag::TableCell => cell.clear(),
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => ctx.blank(),
                TagEnd::Heading(_) => {
                    ctx.styles.pop();
                    ctx.blank();
                }
                TagEnd::BlockQuote(_) => {
                    ctx.flush();
                    ctx.quote_depth = ctx.quote_depth.saturating_sub(1);
                    ctx.styles.pop();
                    ctx.blank();
                }
                TagEnd::CodeBlock => {
                    if let Some((lang, code)) = ctx.in_code.take() {
                        let bg = theme.bg("backgroundElement");
                        let mut code_lines = if lang.is_empty() {
                            code.lines()
                                .map(|l| {
                                    Line::from(Span::styled(l.to_string(), theme.fg("markdownCodeBlock")))
                                })
                                .collect::<Vec<_>>()
                        } else {
                            syntax::highlight(&code, &lang, theme)
                        };
                        let prefix = ctx.prefix();
                        for l in code_lines.iter_mut() {
                            let mut spans = vec![Span::styled(format!("{prefix}  "), bg)];
                            spans.extend(
                                l.spans
                                    .drain(..)
                                    .map(|s| Span::styled(s.content, s.style.patch(bg))),
                            );
                            *l = Line::from(spans).style(bg);
                        }
                        ctx.lines.extend(code_lines);
                        ctx.pending_blank = false;
                        ctx.blank();
                    }
                }
                TagEnd::List(_) => {
                    ctx.flush();
                    ctx.list_stack.pop();
                    if ctx.list_stack.is_empty() {
                        ctx.blank();
                    }
                }
                TagEnd::Item => ctx.flush(),
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                    ctx.styles.pop();
                }
                TagEnd::Link => {
                    ctx.styles.pop();
                    if let Some(url) = ctx.link.take() {
                        let last_text = ctx
                            .current
                            .last()
                            .map(|s| s.content.to_string())
                            .unwrap_or_default();
                        if last_text != url {
                            ctx.push_text(&format!(" ({url})"), theme.fg("markdownLink"));
                        }
                    }
                }
                TagEnd::TableCell => row.push(cell.trim().to_string()),
                TagEnd::TableHead | TagEnd::TableRow => table.push(std::mem::take(&mut row)),
                TagEnd::Table => {
                    in_table = false;
                    let cols = table.iter().map(Vec::len).max().unwrap_or(0);
                    let widths: Vec<usize> = (0..cols)
                        .map(|c| {
                            table
                                .iter()
                                .map(|r| r.get(c).map(|s| s.width()).unwrap_or(0))
                                .max()
                                .unwrap_or(0)
                        })
                        .collect();
                    for (i, r) in table.iter().enumerate() {
                        let mut s = String::new();
                        for (c, w) in widths.iter().enumerate() {
                            let v = r.get(c).cloned().unwrap_or_default();
                            s.push_str(&format!("{v:<w$}  "));
                        }
                        let st = if i == 0 {
                            theme.bold("markdownStrong")
                        } else {
                            theme.fg("markdownText")
                        };
                        ctx.lines
                            .push(Line::from(Span::styled(s.trim_end().to_string(), st)));
                        if i == 0 {
                            ctx.lines.push(Line::from(Span::styled(
                                widths
                                    .iter()
                                    .map(|w| "─".repeat(*w))
                                    .collect::<Vec<_>>()
                                    .join("  "),
                                theme.fg("markdownHorizontalRule"),
                            )));
                        }
                    }
                    ctx.pending_blank = false;
                    ctx.blank();
                }
                _ => {}
            },
            Event::Text(t) => {
                if let Some((_, code)) = &mut ctx.in_code {
                    code.push_str(&t);
                } else if in_table {
                    cell.push_str(&t);
                } else {
                    let st = ctx.style();
                    ctx.push_text(&t, st);
                }
            }
            Event::Code(c) => {
                if in_table {
                    cell.push_str(&c);
                } else {
                    ctx.pending_blank = false;
                    ctx.current.push(Span::styled(
                        c.to_string(),
                        theme.fg("markdownCode").bg(theme.color("backgroundElement")),
                    ));
                }
            }
            Event::SoftBreak => {
                if in_table {
                    cell.push(' ');
                } else {
                    ctx.pending_blank = false;
                    ctx.current.push(Span::styled(" ", ctx.style()));
                }
            }
            Event::HardBreak => ctx.flush(),
            Event::Rule => {
                ctx.blank();
                ctx.lines.push(Line::from(Span::styled(
                    "─".repeat(width.clamp(10, 60)),
                    theme.fg("markdownHorizontalRule"),
                )));
                ctx.pending_blank = false;
                ctx.blank();
            }
            Event::TaskListMarker(done) => {
                ctx.current.push(Span::styled(
                    if done { "☑ " } else { "☐ " }.to_string(),
                    theme.fg("markdownListItem"),
                ));
            }
            Event::Html(h) | Event::InlineHtml(h) => {
                let st = ctx.style();
                ctx.push_text(&h, st);
            }
            _ => {}
        }
    }
    ctx.flush();
    while ctx.lines.last().is_some_and(|l| l.width() == 0) {
        ctx.lines.pop();
    }
    ctx.lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Mode, Theme};

    fn theme() -> Theme {
        Theme::system(Mode::Dark, None, None)
    }

    #[test]
    fn renders_headings_lists_code() {
        let md = "# Title\n\nSome *emph* and **strong** text.\n\n- a\n- b\n\n```rust\nfn main() {}\n```\n";
        let lines = render(md, 40, &theme());
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect())
            .collect();
        assert_eq!(text[0], "# Title");
        assert!(text.iter().any(|l| l.contains("• a")));
        assert!(text.iter().any(|l| l.contains("fn main() {}")));
    }

    #[test]
    fn wraps_long_lines() {
        let md = "word ".repeat(30);
        let lines = render(&md, 20, &theme());
        assert!(lines.len() > 5);
        assert!(lines.iter().all(|l| l.width() <= 20));
    }
}
