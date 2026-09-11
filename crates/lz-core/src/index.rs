//! Symbol index built with tree-sitter: definitions (functions, types,
//! classes, …) and which files reference them, for Rust, Python,
//! JavaScript/TypeScript and Go.
//!
//! Two consumers: the system prompt gets the handful of definitions the
//! user's message names (`<symbols>`), so the model reads the right file
//! instead of exploring; and the `symbol` tool answers "where is X defined /
//! used" without a grep round-trip. Parsing is incremental (mtime + size per
//! file) and cached under the cache dir, so a 50k-line tree costs well under
//! a second on first index and milliseconds afterwards.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime};

use serde::{Deserialize, Serialize};
use tree_sitter::{Language, Node, Parser};

const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_FILES: usize = 6_000;
/// Re-scan the tree for changed files at most this often.
const REFRESH_SECS: u64 = 5;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Symbol {
    pub name: String,
    /// `fn`, `struct`, `enum`, `trait`, `impl`, `type`, `class`, `interface`, `method`, `const`, `mod`
    pub kind: String,
    /// worktree-relative path
    pub file: String,
    pub line: u32,
    pub end_line: u32,
    /// first line of the definition, trimmed
    pub signature: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct FileEntry {
    mtime: u64,
    len: u64,
    symbols: Vec<Symbol>,
    /// identifiers used in this file (kept only for names defined somewhere)
    idents: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Snapshot {
    files: BTreeMap<String, FileEntry>,
}

#[derive(Default)]
struct State {
    snap: Snapshot,
    /// name → symbols (rebuilt from `snap` after each refresh)
    by_name: HashMap<String, Vec<Symbol>>,
    /// name → files referencing it
    refs: HashMap<String, Vec<String>>,
    lines: u64,
    last_scan: Option<Instant>,
    loaded: bool,
}

pub struct Index {
    worktree: PathBuf,
    cache_file: PathBuf,
    state: Mutex<State>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    Go,
}

fn lang_of(path: &Path) -> Option<Lang> {
    match path.extension()?.to_str()? {
        "rs" => Some(Lang::Rust),
        "py" | "pyi" => Some(Lang::Python),
        "js" | "mjs" | "cjs" | "jsx" => Some(Lang::JavaScript),
        "ts" | "mts" | "cts" => Some(Lang::TypeScript),
        "tsx" => Some(Lang::Tsx),
        "go" => Some(Lang::Go),
        _ => None,
    }
}

fn language(lang: Lang) -> Language {
    match lang {
        Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
        Lang::Python => tree_sitter_python::LANGUAGE.into(),
        Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Lang::Go => tree_sitter_go::LANGUAGE.into(),
    }
}

/// Node kinds that define a named symbol, with the field holding the name.
fn definition_kind(lang: Lang, node: &Node) -> Option<(&'static str, &'static str)> {
    let k = node.kind();
    let hit = match lang {
        Lang::Rust => match k {
            "function_item" | "function_signature_item" => ("fn", "name"),
            "struct_item" => ("struct", "name"),
            "enum_item" => ("enum", "name"),
            "trait_item" => ("trait", "name"),
            "impl_item" => ("impl", "type"),
            "type_item" => ("type", "name"),
            "const_item" => ("const", "name"),
            "static_item" => ("static", "name"),
            "mod_item" => ("mod", "name"),
            "macro_definition" => ("macro", "name"),
            _ => return None,
        },
        Lang::Python => match k {
            "function_definition" => ("fn", "name"),
            "class_definition" => ("class", "name"),
            _ => return None,
        },
        Lang::JavaScript | Lang::TypeScript | Lang::Tsx => match k {
            "function_declaration" | "generator_function_declaration" => ("fn", "name"),
            "class_declaration" | "abstract_class_declaration" => ("class", "name"),
            "method_definition" | "method_signature" => ("method", "name"),
            "interface_declaration" => ("interface", "name"),
            "type_alias_declaration" => ("type", "name"),
            "enum_declaration" => ("enum", "name"),
            "variable_declarator" => ("const", "name"),
            _ => return None,
        },
        Lang::Go => match k {
            "function_declaration" => ("fn", "name"),
            "method_declaration" => ("method", "name"),
            "type_spec" => ("type", "name"),
            _ => return None,
        },
    };
    Some(hit)
}

fn is_identifier(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "type_identifier"
            | "field_identifier"
            | "property_identifier"
            | "shorthand_property_identifier"
    )
}

fn node_text<'a>(node: &Node, src: &'a [u8]) -> &'a str {
    std::str::from_utf8(&src[node.byte_range()]).unwrap_or("")
}

fn first_line(text: &str) -> String {
    let l = text.lines().next().unwrap_or("").trim();
    // the header only: stop at the body's opening brace
    let l = l.split_once(" {").map_or(l, |(head, _)| head);
    let l = l.trim_end_matches('{').trim_end_matches(':').trim();
    let mut s: String = l.chars().take(160).collect();
    if l.chars().count() > 160 {
        s.push('…');
    }
    s
}

/// Parse one file into its definitions and identifier set.
fn parse_file(lang: Lang, rel: &str, src: &[u8]) -> (Vec<Symbol>, Vec<String>) {
    let mut parser = Parser::new();
    if parser.set_language(&language(lang)).is_err() {
        return (Vec::new(), Vec::new());
    }
    let Some(tree) = parser.parse(src, None) else {
        return (Vec::new(), Vec::new());
    };
    let mut symbols = Vec::new();
    let mut idents: HashSet<String> = HashSet::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if is_identifier(node.kind()) {
            let t = node_text(&node, src);
            if t.len() >= 3 && t.len() <= 80 {
                idents.insert(t.to_string());
            }
        }
        if let Some((kind, field)) = definition_kind(lang, &node)
            && let Some(name_node) = node.child_by_field_name(field)
        {
            let name = node_text(&name_node, src).trim().to_string();
            // JS `const x = 1` is noise; keep declarators that hold a function/class
            let keep = if node.kind() == "variable_declarator" {
                node.child_by_field_name("value").is_some_and(|v| {
                    matches!(
                        v.kind(),
                        "arrow_function"
                            | "function_expression"
                            | "function"
                            | "class"
                            | "generator_function"
                    )
                })
            } else {
                true
            };
            if keep && !name.is_empty() && name.len() <= 120 {
                symbols.push(Symbol {
                    name,
                    kind: kind.into(),
                    file: rel.into(),
                    line: node.start_position().row as u32 + 1,
                    end_line: node.end_position().row as u32 + 1,
                    signature: first_line(node_text(&node, src)),
                });
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    symbols.sort_by_key(|s| s.line);
    let mut idents: Vec<String> = idents.into_iter().collect();
    idents.sort();
    (symbols, idents)
}

fn mtime_of(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Index {
    pub fn new(worktree: PathBuf, cache_dir: &Path) -> Self {
        let key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            worktree.hash(&mut h);
            format!("{:016x}", h.finish())
        };
        Self {
            worktree,
            cache_file: cache_dir.join("index").join(format!("{key}.json")),
            state: Mutex::new(State::default()),
        }
    }

    /// Bring the index up to date with the tree; cheap when nothing changed.
    pub fn refresh(&self) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !st.loaded {
            st.loaded = true;
            if let Ok(text) = std::fs::read_to_string(&self.cache_file)
                && let Ok(snap) = serde_json::from_str::<Snapshot>(&text)
            {
                st.snap = snap;
            }
        }
        if st.last_scan.is_some_and(|t| t.elapsed().as_secs() < REFRESH_SECS) {
            return;
        }
        st.last_scan = Some(Instant::now());

        // walk the tree (gitignore-aware) and find what changed
        let mut seen: HashSet<String> = HashSet::new();
        let mut todo: Vec<(String, PathBuf, Lang, u64, u64)> = Vec::new();
        let walker = ignore::WalkBuilder::new(&self.worktree)
            .hidden(true)
            .git_ignore(true)
            .git_exclude(true)
            .build();
        for entry in walker.flatten() {
            if seen.len() >= MAX_FILES {
                break;
            }
            let path = entry.path();
            let Some(lang) = lang_of(path) else { continue };
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
                continue;
            }
            let rel = path
                .strip_prefix(&self.worktree)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            if rel.contains("node_modules/") || rel.contains("/target/") || rel.starts_with("target/") {
                continue;
            }
            seen.insert(rel.clone());
            let (mtime, len) = (mtime_of(&meta), meta.len());
            let fresh = st
                .snap
                .files
                .get(&rel)
                .is_some_and(|e| e.mtime == mtime && e.len == len);
            if !fresh {
                todo.push((rel, path.to_path_buf(), lang, mtime, len));
            }
        }
        let removed: Vec<String> = st
            .snap
            .files
            .keys()
            .filter(|k| !seen.contains(*k))
            .cloned()
            .collect();
        for k in &removed {
            st.snap.files.remove(k);
        }
        let changed = !todo.is_empty() || !removed.is_empty();
        for (rel, path, lang, mtime, len) in todo {
            let Ok(src) = std::fs::read(&path) else { continue };
            let (symbols, idents) = parse_file(lang, &rel, &src);
            st.snap.files.insert(
                rel,
                FileEntry {
                    mtime,
                    len,
                    symbols,
                    idents,
                },
            );
        }
        if changed || st.by_name.is_empty() {
            self.rebuild(&mut st);
            if changed {
                let _ = std::fs::create_dir_all(self.cache_file.parent().unwrap_or(Path::new(".")));
                if let Ok(text) = serde_json::to_string(&st.snap) {
                    let _ = std::fs::write(&self.cache_file, text);
                }
            }
        }
    }

    fn rebuild(&self, st: &mut State) {
        let mut by_name: HashMap<String, Vec<Symbol>> = HashMap::new();
        for e in st.snap.files.values() {
            for s in &e.symbols {
                by_name.entry(s.name.clone()).or_default().push(s.clone());
            }
        }
        let mut refs: HashMap<String, Vec<String>> = HashMap::new();
        for (file, e) in &st.snap.files {
            for id in &e.idents {
                if by_name.contains_key(id) {
                    refs.entry(id.clone()).or_default().push(file.clone());
                }
            }
        }
        st.lines = st
            .snap
            .files
            .values()
            .map(|e| e.len / 40) // rough: avoids re-reading files just to count
            .sum();
        st.by_name = by_name;
        st.refs = refs;
    }

    pub fn stats(&self) -> (usize, usize) {
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        (st.snap.files.len(), st.by_name.values().map(Vec::len).sum())
    }

    /// Definitions of `name` (exact, then case-insensitive).
    pub fn definitions(&self, name: &str) -> Vec<Symbol> {
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = st.by_name.get(name) {
            return v.clone();
        }
        let lower = name.to_ascii_lowercase();
        st.by_name
            .iter()
            .filter(|(k, _)| k.to_ascii_lowercase() == lower)
            .flat_map(|(_, v)| v.clone())
            .collect()
    }

    /// Files that mention `name` (excluding where it is defined).
    pub fn references(&self, name: &str) -> Vec<String> {
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let defined: HashSet<&str> = st
            .by_name
            .get(name)
            .map(|v| v.iter().map(|s| s.file.as_str()).collect())
            .unwrap_or_default();
        let mut out: Vec<String> = st
            .refs
            .get(name)
            .map(|v| {
                v.iter()
                    .filter(|f| !defined.contains(f.as_str()))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out.dedup();
        out
    }

    /// Symbols whose names appear in `text` (a user prompt), most specific
    /// first, for the `<symbols>` prompt block. Empty when nothing matches.
    pub fn relevant(&self, text: &str, max_chars: usize) -> String {
        let words: Vec<String> = text
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .filter(|w| w.len() >= 3 && w.len() <= 80 && !w.chars().all(|c| c.is_ascii_digit()))
            .map(str::to_string)
            .collect();
        if words.is_empty() {
            return String::new();
        }
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.by_name.is_empty() {
            return String::new();
        }
        let mut picked: Vec<&Symbol> = Vec::new();
        let mut seen: HashSet<(String, String, u32)> = HashSet::new();
        for w in &words {
            let matches = st.by_name.get(w).or_else(|| {
                // case/underscore-tolerant lookup, only for words long enough to
                // be a real identifier (short English words hit too much)
                if w.len() < 6 {
                    return None;
                }
                let lw = w.to_ascii_lowercase().replace('_', "");
                st.by_name
                    .iter()
                    .find(|(k, _)| k.to_ascii_lowercase().replace('_', "") == lw)
                    .map(|(_, v)| v)
            });
            if let Some(list) = matches {
                for s in list.iter().take(4) {
                    if seen.insert((s.file.clone(), s.name.clone(), s.line)) {
                        picked.push(s);
                    }
                }
            }
            if picked.len() >= 16 {
                break;
            }
        }
        if picked.is_empty() {
            return String::new();
        }
        let mut out = String::from("<symbols>\n");
        for s in picked {
            let refs = st.refs.get(&s.name).map(|r| r.len()).unwrap_or(0);
            let line = format!(
                "{} {} — {}:{}{}\n  {}\n",
                s.kind,
                s.name,
                s.file,
                s.line,
                if refs > 1 {
                    format!(" · used in {refs} files")
                } else {
                    String::new()
                },
                s.signature
            );
            if out.len() + line.len() > max_chars {
                break;
            }
            out.push_str(&line);
        }
        out.push_str("</symbols>");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_rust_and_ts_definitions() {
        let rs = b"pub struct Config { pub name: String }\nimpl Config {\n    pub fn load(path: &str) -> Config { todo!() }\n}\nfn helper() {}\n";
        let (syms, idents) = parse_file(Lang::Rust, "src/config.rs", rs);
        let names: Vec<(&str, &str)> = syms.iter().map(|s| (s.kind.as_str(), s.name.as_str())).collect();
        assert!(names.contains(&("struct", "Config")));
        assert!(names.contains(&("impl", "Config")));
        assert!(names.contains(&("fn", "load")));
        assert!(names.contains(&("fn", "helper")));
        assert!(idents.iter().any(|i| i == "Config"));
        let load = syms.iter().find(|s| s.name == "load").unwrap();
        assert_eq!(load.line, 3);
        assert_eq!(load.signature, "pub fn load(path: &str) -> Config");

        let ts = b"export interface User { id: string }\nexport const fetchUser = async (id: string) => { return id }\nclass Repo {\n  find(id: string) { return id }\n}\nconst N = 3;\n";
        let (syms, _) = parse_file(Lang::TypeScript, "src/user.ts", ts);
        let names: Vec<(&str, &str)> = syms.iter().map(|s| (s.kind.as_str(), s.name.as_str())).collect();
        assert!(names.contains(&("interface", "User")));
        assert!(names.contains(&("const", "fetchUser")));
        assert!(names.contains(&("class", "Repo")));
        assert!(names.contains(&("method", "find")));
        assert!(!names.iter().any(|(_, n)| *n == "N"), "{names:?}");
    }

    #[test]
    fn index_refresh_and_relevance() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn render_matrix(rows: usize) -> String { String::new() }\npub struct Matrix;\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("src/main.rs"),
            "fn main() { let _ = crate::render_matrix(3); }\n",
        )
        .unwrap();
        let cache = tempfile::tempdir().unwrap();
        let idx = Index::new(dir.path().to_path_buf(), cache.path());
        idx.refresh();
        assert_eq!(idx.stats().0, 2);
        assert_eq!(idx.definitions("render_matrix").len(), 1);
        assert_eq!(idx.references("render_matrix"), vec!["src/main.rs".to_string()]);
        let block = idx.relevant("please make the matrix export use render_matrix", 800);
        assert!(block.contains("fn render_matrix — src/lib.rs:1"), "{block}");
        assert!(block.contains("struct Matrix"), "{block}");
        assert!(idx.relevant("hello there", 800).is_empty());
        // a fresh Index instance loads the cache and sees no changes
        let again = Index::new(dir.path().to_path_buf(), cache.path());
        again.refresh();
        assert_eq!(again.stats(), idx.stats());
    }
}
