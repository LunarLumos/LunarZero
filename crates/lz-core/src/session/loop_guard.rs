//! Detects a model that has started repeating itself ("I'll run the
//! command. Actually, I'll just run it. …" forever) so the stream can be cut
//! instead of burning minutes of output.

/// Repetition verdict for a growing text.
pub fn is_looping(text: &str) -> bool {
    // 1. the same non-trivial line appears four or more times
    let mut lines: Vec<&str> = text.lines().map(str::trim).filter(|l| l.len() >= 12).collect();
    if lines.len() >= 8 {
        lines.sort_unstable();
        let mut run = 1;
        for w in lines.windows(2) {
            if w[0] == w[1] {
                run += 1;
                if run >= 4 {
                    return true;
                }
            } else {
                run = 1;
            }
        }
    }
    // 2. the tail repeats: last N chars occur ≥3 times in the last 3000 chars
    let tail_len = 120;
    if text.len() >= tail_len * 3 {
        let window: String = text
            .chars()
            .rev()
            .take(3000)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let tail: String = window
            .chars()
            .rev()
            .take(tail_len)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if tail.trim().len() >= 40 && window.matches(tail.as_str()).count() >= 3 {
            return true;
        }
    }
    false
}

/// Hard cap on a single text part while tools are available: a coding agent
/// that has written this much prose without acting is not going to act.
pub const MAX_TEXT_CHARS: usize = 24_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_repeated_lines() {
        let block = "I'll run the command.\n\nActually, I'll just run it.\n\nWait, I'll also check if example-server is in the lunarzero.json under mcp. Yes.\n\n";
        assert!(!is_looping(&block.repeat(2)));
        assert!(is_looping(&block.repeat(5)));
    }

    #[test]
    fn normal_text_passes() {
        let t = (1..60)
            .map(|i| format!("Step {i}: do something different with value {}\n", i * 7))
            .collect::<String>();
        assert!(!is_looping(&t));
    }
}
