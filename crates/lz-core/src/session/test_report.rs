//! Structured failures out of raw build/test output, so a repair round tells
//! the model *which* test, *which* file:line and *which* assertion instead of
//! handing it a log to dig through. Best-effort parsers for the common
//! runners; the raw tail still follows the summary.

use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Failure {
    /// test or item name (empty for compiler errors)
    pub name: String,
    /// `path:line` when known
    pub location: String,
    /// first line of the message / assertion
    pub message: String,
}

const MAX: usize = 15;

fn clean(s: &str) -> String {
    s.trim().chars().take(200).collect()
}

/// cargo test / rustc: `test x ... FAILED`, `---- x stdout ----` blocks,
/// `thread 'x' panicked at file:line:col:` + message, `error[E...]` + `-->`.
fn rust(out: &str) -> Vec<Failure> {
    let lines: Vec<&str> = out.lines().collect();
    let mut fails = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        if let Some(rest) = t.strip_prefix("thread '")
            && let Some((name, after)) = rest.split_once("' panicked at ")
        {
            let location = after.trim_end_matches(':').to_string();
            let message = lines.get(i + 1).map(|m| clean(m)).unwrap_or_default();
            fails.push(Failure {
                name: name.to_string(),
                location: location
                    .rsplit(':')
                    .skip(1)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join(":"),
                message,
            });
        } else if (t.starts_with("error[") || t.starts_with("error:"))
            && !t.starts_with("error: test failed")
            && !t.starts_with("error: could not compile")
        {
            let message = clean(t.trim_start_matches("error").trim_start_matches(':').trim());
            let location = lines[i + 1..]
                .iter()
                .take(4)
                .find_map(|x| x.trim().strip_prefix("--> ").map(|s| s.trim().to_string()))
                .unwrap_or_default();
            fails.push(Failure {
                name: String::new(),
                location: location
                    .rsplit(':')
                    .skip(1)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join(":"),
                message,
            });
        }
    }
    fails
}

/// pytest: `FAILED tests/x.py::test_y - AssertionError: ...` and
/// `tests/x.py:12: AssertionError` / `E   assert ...` lines.
fn pytest(out: &str) -> Vec<Failure> {
    let mut fails = Vec::new();
    let lines: Vec<&str> = out.lines().collect();
    for l in &lines {
        let t = l.trim();
        if let Some(rest) = t.strip_prefix("FAILED ") {
            let (name, msg) = rest.split_once(" - ").unwrap_or((rest, ""));
            let location = name.split("::").next().unwrap_or("").to_string();
            fails.push(Failure {
                name: name.to_string(),
                location,
                message: clean(msg),
            });
        }
    }
    // enrich with the `path:line: Error` lines that precede each short summary
    for f in &mut fails {
        let file = f.name.split("::").next().unwrap_or("");
        if let Some(l) = lines
            .iter()
            .find(|l| l.starts_with(file) && l.contains(':') && (l.contains("Error") || l.contains("assert")))
            && let Some((loc, _)) = l.split_once(": ")
        {
            f.location = loc.to_string();
        }
    }
    fails
}

/// jest / vitest: `● Suite › name` (jest) or `FAIL file > name` / `×` lines
/// (vitest) followed by the assertion and an `at file:line:col` frame.
fn js(out: &str) -> Vec<Failure> {
    let lines: Vec<&str> = out.lines().collect();
    let mut fails = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        let name = if let Some(n) = t.strip_prefix("● ") {
            n.to_string()
        } else if let Some(n) = t
            .strip_prefix("× ")
            .or_else(|| t.strip_prefix("✗ "))
            .or_else(|| t.strip_prefix("✕ "))
        {
            n.to_string()
        } else if t.starts_with("FAIL ") && t.contains(" > ") {
            t.trim_start_matches("FAIL ").to_string()
        } else {
            continue;
        };
        if name.starts_with("Test suite failed") || name.contains("›") && name.ends_with("›") {
            continue;
        }
        let window = &lines[i + 1..(i + 30).min(lines.len())];
        let message = window
            .iter()
            .map(|x| x.trim())
            .find(|x| {
                x.starts_with("expect(")
                    || x.starts_with("Expected")
                    || x.starts_with("AssertionError")
                    || x.starts_with("Error:")
                    || x.starts_with("TypeError")
                    || x.starts_with("ReferenceError")
            })
            .map(clean)
            .unwrap_or_default();
        let location = window
            .iter()
            .map(|x| x.trim())
            .find_map(|x| {
                let s = x.strip_prefix("at ")?;
                let inner = s
                    .rsplit_once('(')
                    .map(|(_, r)| r.trim_end_matches(')'))
                    .unwrap_or(s);
                (inner.contains(':') && !inner.contains("node_modules")).then(|| {
                    inner
                        .rsplit(':')
                        .skip(1)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join(":")
                })
            })
            .unwrap_or_default();
        fails.push(Failure {
            name: name.chars().take(160).collect(),
            location,
            message,
        });
    }
    fails
}

/// go test: `--- FAIL: TestX (0.01s)` then `    x_test.go:12: message`.
fn gotest(out: &str) -> Vec<Failure> {
    let lines: Vec<&str> = out.lines().collect();
    let mut fails = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if let Some(rest) = l.trim().strip_prefix("--- FAIL: ") {
            let name = rest.split_whitespace().next().unwrap_or("").to_string();
            let detail = lines[i + 1..(i + 12).min(lines.len())]
                .iter()
                .map(|x| x.trim())
                .find(|x| x.contains("_test.go:") || x.contains(".go:"));
            let (location, message) = match detail.and_then(|d| d.split_once(": ")) {
                Some((loc, msg)) => (loc.to_string(), clean(msg)),
                None => (String::new(), String::new()),
            };
            fails.push(Failure {
                name,
                location,
                message,
            });
        }
    }
    fails
}

/// tsc / eslint: `file.ts(12,5): error TS2322: msg` and `file.ts:12:5: error msg` / `  12:5  error  msg  rule`.
fn ts(out: &str) -> Vec<Failure> {
    let mut fails = Vec::new();
    let mut current_file = String::new();
    for l in out.lines() {
        let t = l.trim_end();
        if let Some((file, rest)) = t.split_once("): error ")
            && let Some((path, pos)) = file.rsplit_once('(')
        {
            let line = pos.split(',').next().unwrap_or("");
            fails.push(Failure {
                name: String::new(),
                location: format!("{path}:{line}"),
                message: clean(rest),
            });
        } else if !t.starts_with(' ')
            && (t.ends_with(".ts") || t.ends_with(".tsx") || t.ends_with(".js") || t.ends_with(".jsx"))
        {
            current_file = t.trim().to_string();
        } else if !current_file.is_empty() {
            let parts: Vec<&str> = t.split_whitespace().collect();
            if parts.len() >= 3 && parts[1] == "error" && parts[0].contains(':') {
                let line = parts[0].split(':').next().unwrap_or("");
                fails.push(Failure {
                    name: String::new(),
                    location: format!("{current_file}:{line}"),
                    message: clean(&parts[2..].join(" ")),
                });
            }
        }
    }
    fails
}

/// A compact, deduplicated summary of the failures in `output`, or `None`
/// when no parser recognised anything.
pub fn summarize(command: &str, output: &str) -> Option<String> {
    let c = command.to_lowercase();
    let mut fails: Vec<Failure> = if c.contains("cargo") {
        rust(output)
    } else if c.contains("pytest") {
        pytest(output)
    } else if c.contains("go test") || c.contains("go build") || c.contains("go vet") {
        let mut f = gotest(output);
        if f.is_empty() {
            f = ts(output); // go build errors look like `file.go:12:5: msg`
        }
        f
    } else if c.contains("tsc") || c.contains("eslint") || c.contains("lint") {
        ts(output)
    } else if c.contains("jest")
        || c.contains("vitest")
        || c.contains("npm test")
        || c.contains("pnpm test")
        || c.contains("yarn test")
        || c.contains("bun test")
    {
        js(output)
    } else {
        // unknown runner: try them all
        let mut f = rust(output);
        f.extend(pytest(output));
        f.extend(js(output));
        f.extend(gotest(output));
        f.extend(ts(output));
        f
    };
    fails.retain(|f| !(f.name.is_empty() && f.location.is_empty() && f.message.is_empty()));
    let uniq: BTreeSet<Failure> = fails.into_iter().collect();
    if uniq.is_empty() {
        return None;
    }
    let total = uniq.len();
    let mut out = format!("Failures ({total}):\n");
    for f in uniq.iter().take(MAX) {
        let mut line = String::from("- ");
        if !f.name.is_empty() {
            line.push_str(&f.name);
        }
        if !f.location.is_empty() {
            if !f.name.is_empty() {
                line.push_str(" — ");
            }
            line.push_str(&f.location);
        }
        if !f.message.is_empty() {
            line.push_str(": ");
            line.push_str(&f.message);
        }
        out.push_str(&line);
        out.push('\n');
    }
    if total > MAX {
        out.push_str(&format!("… and {} more\n", total - MAX));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_test_and_rustc() {
        let out = "running 2 tests\ntest parse::ok ... ok\ntest parse::empty ... FAILED\n\nfailures:\n\n---- parse::empty stdout ----\nthread 'parse::empty' panicked at src/parse.rs:42:9:\nassertion `left == right` failed\n  left: 1\n right: 0\n";
        let s = summarize("cargo test", out).unwrap();
        assert!(
            s.contains("parse::empty — src/parse.rs:42: assertion `left == right` failed"),
            "{s}"
        );
        let out = "error[E0308]: mismatched types\n --> src/lib.rs:4:5\n  |\n4 |     \"str\"\n";
        let s = summarize("cargo check", out).unwrap();
        assert!(s.contains("src/lib.rs:4: [E0308]: mismatched types"), "{s}");
    }

    #[test]
    fn pytest_and_go_and_ts() {
        let out = "tests/test_x.py:12: AssertionError\nE   assert 1 == 2\n=== short test summary info ===\nFAILED tests/test_x.py::test_add - AssertionError: assert 1 == 2\n";
        let s = summarize("pytest", out).unwrap();
        assert!(
            s.contains("tests/test_x.py::test_add — tests/test_x.py:12: AssertionError: assert 1 == 2"),
            "{s}"
        );
        let out = "--- FAIL: TestAdd (0.00s)\n    add_test.go:9: want 3, got 4\nFAIL\n";
        let s = summarize("go test ./...", out).unwrap();
        assert!(s.contains("TestAdd — add_test.go:9: want 3, got 4"), "{s}");
        let out = "src/app.ts(12,5): error TS2322: Type 'string' is not assignable to type 'number'.\n";
        let s = summarize("npx tsc --noEmit", out).unwrap();
        assert!(s.contains("src/app.ts:12: TS2322"), "{s}");
    }

    #[test]
    fn jest_block() {
        let out = "FAIL src/sum.test.js\n  ● adds numbers\n\n    expect(received).toBe(expected)\n\n    Expected: 3\n    Received: 4\n\n      at Object.<anonymous> (src/sum.test.js:5:17)\n";
        let s = summarize("npm test", out).unwrap();
        assert!(
            s.contains("adds numbers — src/sum.test.js:5: expect(received).toBe(expected)"),
            "{s}"
        );
        assert!(summarize("npm test", "all good\n").is_none());
    }
}
