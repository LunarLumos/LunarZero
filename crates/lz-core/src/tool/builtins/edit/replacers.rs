//! Fuzzy `oldString` matching strategies for the `edit` tool, tried in order.

pub type Replacer = fn(&str, &str) -> Vec<String>;

const SINGLE_CANDIDATE_SIMILARITY_THRESHOLD: f64 = 0.65;
const MULTIPLE_CANDIDATES_SIMILARITY_THRESHOLD: f64 = 0.65;

pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() || b.is_empty() {
        return a.len().max(b.len());
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Byte offset of the start of line `i` in `lines` (lines split on '\n').
fn line_start(lines: &[&str], i: usize) -> usize {
    lines[..i].iter().map(|l| l.len() + 1).sum()
}

fn block(content: &str, lines: &[&str], start: usize, end_inclusive: usize) -> String {
    let s = line_start(lines, start);
    let e = s
        + lines[start..=end_inclusive]
            .iter()
            .map(|l| l.len())
            .sum::<usize>()
        + (end_inclusive - start);
    content[s..e].to_string()
}

pub fn simple(_content: &str, find: &str) -> Vec<String> {
    vec![find.to_string()]
}

pub fn line_trimmed(content: &str, find: &str) -> Vec<String> {
    let original: Vec<&str> = content.split('\n').collect();
    let mut search: Vec<&str> = find.split('\n').collect();
    if search.last() == Some(&"") {
        search.pop();
    }
    if search.is_empty() || search.len() > original.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for i in 0..=original.len() - search.len() {
        if (0..search.len()).all(|j| original[i + j].trim() == search[j].trim()) {
            out.push(block(content, &original, i, i + search.len() - 1));
        }
    }
    out
}

fn middle_similarity(original: &[&str], search: &[&str], start: usize, end: usize, early_exit: bool) -> f64 {
    let actual = end - start + 1;
    let to_check = (search.len() as isize - 2).min(actual as isize - 2);
    if to_check <= 0 {
        return 1.0;
    }
    let to_check = to_check as f64;
    let mut similarity = 0.0;
    let mut j = 1;
    while j < search.len() - 1 && j < actual - 1 {
        let o = original[start + j].trim();
        let s = search[j].trim();
        let max_len = o.chars().count().max(s.chars().count());
        if max_len > 0 {
            let d = levenshtein(o, s) as f64;
            if early_exit {
                similarity += (1.0 - d / max_len as f64) / to_check;
                if similarity >= SINGLE_CANDIDATE_SIMILARITY_THRESHOLD {
                    break;
                }
            } else {
                similarity += 1.0 - d / max_len as f64;
            }
        }
        j += 1;
    }
    if early_exit {
        similarity
    } else {
        similarity / to_check
    }
}

pub fn block_anchor(content: &str, find: &str) -> Vec<String> {
    let original: Vec<&str> = content.split('\n').collect();
    let mut search: Vec<&str> = find.split('\n').collect();
    if search.len() < 3 {
        return Vec::new();
    }
    if search.last() == Some(&"") {
        search.pop();
    }
    let first = search[0].trim();
    let last = search[search.len() - 1].trim();
    let size = search.len();
    let max_delta = 1.max(size / 4);
    let mut candidates: Vec<(usize, usize)> = Vec::new();
    for i in 0..original.len() {
        if original[i].trim() != first {
            continue;
        }
        for (j, line) in original.iter().enumerate().skip(i + 2) {
            if line.trim() == last {
                let actual = j - i + 1;
                if (actual as isize - size as isize).unsigned_abs() <= max_delta {
                    candidates.push((i, j));
                }
                break;
            }
        }
    }
    if candidates.is_empty() {
        return Vec::new();
    }
    if candidates.len() == 1 {
        let (s, e) = candidates[0];
        let sim = middle_similarity(&original, &search, s, e, true);
        return if sim >= SINGLE_CANDIDATE_SIMILARITY_THRESHOLD {
            vec![block(content, &original, s, e)]
        } else {
            Vec::new()
        };
    }
    let mut best: Option<(usize, usize)> = None;
    let mut max_sim = -1.0;
    for &(s, e) in &candidates {
        let sim = middle_similarity(&original, &search, s, e, false);
        if sim > max_sim {
            max_sim = sim;
            best = Some((s, e));
        }
    }
    match best {
        Some((s, e)) if max_sim >= MULTIPLE_CANDIDATES_SIMILARITY_THRESHOLD => {
            vec![block(content, &original, s, e)]
        }
        _ => Vec::new(),
    }
}

fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn whitespace_normalized(content: &str, find: &str) -> Vec<String> {
    let nfind = normalize_ws(find);
    let lines: Vec<&str> = content.split('\n').collect();
    let mut out = Vec::new();
    for line in &lines {
        let nline = normalize_ws(line);
        if nline == nfind {
            out.push(line.to_string());
        } else if nline.contains(&nfind) {
            let words: Vec<&str> = find.split_whitespace().collect();
            if !words.is_empty() {
                let pattern = words
                    .iter()
                    .map(|w| regex::escape(w))
                    .collect::<Vec<_>>()
                    .join(r"\s+");
                if let Ok(re) = regex::Regex::new(&pattern)
                    && let Some(m) = re.find(line)
                {
                    out.push(m.as_str().to_string());
                }
            }
        }
    }
    let find_lines: Vec<&str> = find.split('\n').collect();
    if find_lines.len() > 1 && find_lines.len() <= lines.len() {
        for i in 0..=lines.len() - find_lines.len() {
            let b = lines[i..i + find_lines.len()].join("\n");
            if normalize_ws(&b) == nfind {
                out.push(b);
            }
        }
    }
    out
}

fn remove_indentation(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let min = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min();
    let Some(min) = min else { return text.to_string() };
    lines
        .iter()
        .map(|l| {
            if l.trim().is_empty() {
                l.to_string()
            } else {
                l.chars().skip(min).collect()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn indentation_flexible(content: &str, find: &str) -> Vec<String> {
    let nfind = remove_indentation(find);
    let lines: Vec<&str> = content.split('\n').collect();
    let n = find.split('\n').count();
    if n > lines.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for i in 0..=lines.len() - n {
        let b = lines[i..i + n].join("\n");
        if remove_indentation(&b) == nfind {
            out.push(b);
        }
    }
    out
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('n') => {
                out.push('\n');
                chars.next();
            }
            Some('t') => {
                out.push('\t');
                chars.next();
            }
            Some('r') => {
                out.push('\r');
                chars.next();
            }
            Some(&q) if matches!(q, '\'' | '"' | '`' | '\\' | '\n' | '$') => {
                out.push(q);
                chars.next();
            }
            _ => out.push('\\'),
        }
    }
    out
}

pub fn escape_normalized(content: &str, find: &str) -> Vec<String> {
    let ufind = unescape(find);
    let mut out = Vec::new();
    if content.contains(&ufind) {
        out.push(ufind.clone());
    }
    let lines: Vec<&str> = content.split('\n').collect();
    let n = ufind.split('\n').count();
    if n <= lines.len() {
        for i in 0..=lines.len() - n {
            let b = lines[i..i + n].join("\n");
            if unescape(&b) == ufind {
                out.push(b);
            }
        }
    }
    out
}

pub fn trimmed_boundary(content: &str, find: &str) -> Vec<String> {
    let tfind = find.trim();
    if tfind == find {
        return Vec::new();
    }
    let mut out = Vec::new();
    if content.contains(tfind) {
        out.push(tfind.to_string());
    }
    let lines: Vec<&str> = content.split('\n').collect();
    let n = find.split('\n').count();
    if n <= lines.len() {
        for i in 0..=lines.len() - n {
            let b = lines[i..i + n].join("\n");
            if b.trim() == tfind {
                out.push(b);
            }
        }
    }
    out
}

pub fn context_aware(content: &str, find: &str) -> Vec<String> {
    let mut find_lines: Vec<&str> = find.split('\n').collect();
    if find_lines.len() < 3 {
        return Vec::new();
    }
    if find_lines.last() == Some(&"") {
        find_lines.pop();
    }
    let lines: Vec<&str> = content.split('\n').collect();
    let first = find_lines[0].trim();
    let last = find_lines[find_lines.len() - 1].trim();
    let mut out = Vec::new();
    for i in 0..lines.len() {
        if lines[i].trim() != first {
            continue;
        }
        for j in i + 2..lines.len() {
            if lines[j].trim() == last {
                let blk = &lines[i..=j];
                if blk.len() == find_lines.len() {
                    let mut matching = 0;
                    let mut total = 0;
                    for k in 1..blk.len() - 1 {
                        let (b, f) = (blk[k].trim(), find_lines[k].trim());
                        if !b.is_empty() || !f.is_empty() {
                            total += 1;
                            if b == f {
                                matching += 1;
                            }
                        }
                    }
                    if total == 0 || matching as f64 / total as f64 >= 0.5 {
                        out.push(blk.join("\n"));
                        break;
                    }
                }
                break;
            }
        }
    }
    out
}

pub fn multi_occurrence(content: &str, find: &str) -> Vec<String> {
    if find.is_empty() {
        return Vec::new();
    }
    content.matches(find).map(|_| find.to_string()).collect()
}

pub const REPLACERS: &[(&str, Replacer)] = &[
    ("simple", simple),
    ("line_trimmed", line_trimmed),
    ("block_anchor", block_anchor),
    ("whitespace_normalized", whitespace_normalized),
    ("indentation_flexible", indentation_flexible),
    ("escape_normalized", escape_normalized),
    ("trimmed_boundary", trimmed_boundary),
    ("context_aware", context_aware),
    ("multi_occurrence", multi_occurrence),
];

fn is_disproportionate(search: &str, old: &str) -> bool {
    let old_lines = old.split('\n').count();
    let search_lines = search.split('\n').count();
    if search_lines >= (old_lines + 3).max(old_lines * 2) {
        return true;
    }
    if old_lines == 1 {
        return false;
    }
    let (s, o) = (search.trim().len(), old.trim().len());
    s > (o + 500).max(o * 4)
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ReplaceError {
    #[error("No changes to apply: oldString and newString are identical.")]
    Identical,
    #[error(
        "oldString cannot be empty when editing an existing file. Provide the exact text to replace, or use write for an intentional full-file replacement."
    )]
    Empty,
    #[error(
        "Could not find oldString in the file. It must match exactly, including whitespace, indentation, and line endings."
    )]
    NotFound,
    #[error(
        "Found multiple matches for oldString. Provide more surrounding context to make the match unique."
    )]
    Multiple,
    #[error(
        "Refusing replacement because the matched span is much larger than oldString. Re-read the file and provide the full exact oldString for the intended replacement."
    )]
    Disproportionate,
}

pub fn replace(content: &str, old: &str, new: &str, replace_all: bool) -> Result<String, ReplaceError> {
    if old == new {
        return Err(ReplaceError::Identical);
    }
    if old.is_empty() {
        return Err(ReplaceError::Empty);
    }
    let mut not_found = true;
    for (_, replacer) in REPLACERS {
        for search in replacer(content, old) {
            let Some(index) = content.find(&search) else {
                continue;
            };
            not_found = false;
            if is_disproportionate(&search, old) {
                return Err(ReplaceError::Disproportionate);
            }
            if replace_all {
                return Ok(content.replace(&search, new));
            }
            let last = content.rfind(&search).unwrap_or(index);
            if index != last {
                continue;
            }
            return Ok(format!(
                "{}{}{}",
                &content[..index],
                new,
                &content[index + search.len()..]
            ));
        }
    }
    Err(if not_found {
        ReplaceError::NotFound
    } else {
        ReplaceError::Multiple
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_and_unique() {
        assert_eq!(replace("a b c", "b", "x", false).unwrap(), "a x c");
        assert_eq!(replace("b a b", "b", "x", false), Err(ReplaceError::Multiple));
        assert_eq!(replace("b a b", "b", "x", true).unwrap(), "x a x");
        assert_eq!(replace("abc", "zzz", "x", false), Err(ReplaceError::NotFound));
    }

    #[test]
    fn line_trimmed_matches_differing_indent() {
        let content = "fn main() {\n    let x = 1;\n    println!(\"{x}\");\n}\n";
        // newString is inserted verbatim (indentation is the model's job), by design
        let out = replace(content, "let x = 1;\nprintln!(\"{x}\");", "    let x = 2;", false).unwrap();
        assert_eq!(out, "fn main() {\n    let x = 2;\n}\n");
    }

    #[test]
    fn block_anchor_fuzzy_middle() {
        let content = "start\n  alpha = 1\n  beta = 2\n  gamma = 3\nend\n";
        let find = "start\n  alpha = 1\n  betta = 2\n  gamma = 3\nend";
        let out = replace(content, find, "REPLACED", false).unwrap();
        assert_eq!(out, "REPLACED\n");
    }

    #[test]
    fn whitespace_normalized_single_line() {
        let content = "let   a =    b;\n";
        assert_eq!(
            replace(content, "let a = b;", "let a = c;", false).unwrap(),
            "let a = c;\n"
        );
    }

    #[test]
    fn escape_normalized() {
        let content = "console.log(\"hi\\nthere\")";
        // model sent literal backslash-n which should match the file's escaped form
        let out = replace(content, "console.log(\"hi\\\\nthere\")", "x", false).unwrap();
        assert_eq!(out, "x");
    }

    #[test]
    fn disproportionate_guard() {
        assert!(is_disproportionate("a\nb\nc\nd\ne\nf", "a\nb\nc"));
        assert!(!is_disproportionate("a\nb\nc\nd", "a\nb\nc"));
        assert!(!is_disproportionate("x".repeat(2000).as_str(), "x"));
        let big = format!("a\n{}\nc", "y".repeat(600));
        assert!(is_disproportionate(&big, "a\nb\nc"));
        // anchors far apart are rejected by the block-delta rule, not matched disproportionately
        let content = "a\nb\nc\nd\ne\nf\ng\nh\n";
        assert_eq!(
            replace(content, "a\nzz\nh", "x", false),
            Err(ReplaceError::NotFound)
        );
    }

    #[test]
    fn levenshtein_basic() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "abc"), 3);
    }
}
