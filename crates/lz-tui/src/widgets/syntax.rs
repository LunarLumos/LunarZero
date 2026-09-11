//! Code highlighting via syntect, mapped onto the theme's `syntax*` keys.

use std::sync::LazyLock;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::highlighting::{ScopeSelectors, ThemeSettings};
use syntect::parsing::{ParseState, ScopeStack, ScopeStackOp, SyntaxSet};

use crate::theme::Theme;

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);

struct Rule {
    selector: ScopeSelectors,
    key: &'static str,
    bold: bool,
    italic: bool,
}

static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    let mk = |sel: &str, key: &'static str, bold: bool, italic: bool| Rule {
        selector: sel.parse().unwrap(),
        key,
        bold,
        italic,
    };
    vec![
        mk("comment", "syntaxComment", false, true),
        mk(
            "keyword, storage.modifier, storage.type.function, keyword.control",
            "syntaxKeyword",
            false,
            false,
        ),
        mk(
            "storage.type, entity.name.type, entity.name.class, support.type, support.class, entity.other.inherited-class",
            "syntaxType",
            false,
            false,
        ),
        mk(
            "entity.name.function, support.function, meta.function-call",
            "syntaxFunction",
            false,
            false,
        ),
        mk("string, string.quoted", "syntaxString", false, false),
        mk(
            "constant.numeric, constant.language, constant.character",
            "syntaxNumber",
            false,
            false,
        ),
        mk(
            "variable, variable.parameter, variable.other",
            "syntaxVariable",
            false,
            false,
        ),
        mk("keyword.operator", "syntaxOperator", false, false),
        mk("punctuation", "syntaxPunctuation", false, false),
        mk("entity.name.tag", "syntaxKeyword", false, false),
        mk("markup.heading", "markdownHeading", true, false),
    ]
});

/// Highlight `code` written in `lang` (file extension or language name).
pub fn highlight(code: &str, lang: &str, theme: &Theme) -> Vec<Line<'static>> {
    let ss = &*SYNTAXES;
    let syntax = ss
        .find_syntax_by_token(lang)
        .or_else(|| ss.find_syntax_by_extension(lang))
        .or_else(|| ss.find_syntax_by_first_line(code.lines().next().unwrap_or("")));
    let base = theme.fg("markdownCodeBlock");
    let Some(syntax) = syntax else {
        return code
            .lines()
            .map(|l| Line::from(Span::styled(l.to_string(), base)))
            .collect();
    };
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut out = Vec::new();
    let _ = ThemeSettings::default();
    for line in syntect::util::LinesWithEndings::from(code) {
        let ops = state.parse_line(line, ss).unwrap_or_default();
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut last = 0usize;
        for (pos, op) in ops {
            if pos > last {
                let text = &line[last..pos];
                spans.push(Span::styled(
                    text.trim_end_matches('\n').to_string(),
                    style_for(&stack, theme, base),
                ));
                last = pos;
            }
            let _ = stack.apply(&op);
            let _: &ScopeStackOp = &op;
        }
        if last < line.len() {
            spans.push(Span::styled(
                line[last..].trim_end_matches('\n').to_string(),
                style_for(&stack, theme, base),
            ));
        }
        out.push(Line::from(spans));
    }
    out
}

fn style_for(stack: &ScopeStack, theme: &Theme, base: Style) -> Style {
    let scopes = stack.as_slice();
    for rule in RULES.iter() {
        if rule.selector.does_match(scopes).is_some() {
            let mut s = theme.fg(rule.key);
            if rule.bold {
                s = s.add_modifier(Modifier::BOLD);
            }
            if rule.italic {
                s = s.add_modifier(Modifier::ITALIC);
            }
            return s;
        }
    }
    base
}

/// Best-effort language token from a fenced-code info string or a filename.
pub fn lang_token(info: &str) -> String {
    let t = info.split_whitespace().next().unwrap_or("").to_lowercase();
    match t.as_str() {
        "rs" => "rust".into(),
        "ts" | "typescript" => "typescript".into(),
        "tsx" => "typescriptreact".into(),
        "js" | "javascript" | "mjs" | "cjs" => "javascript".into(),
        "py" | "python" => "python".into(),
        "sh" | "bash" | "zsh" | "shell" => "bash".into(),
        "yml" => "yaml".into(),
        "md" => "markdown".into(),
        other => other.to_string(),
    }
}
