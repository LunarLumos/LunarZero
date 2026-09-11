//! Some models (Gemma, Qwen, DeepSeek distills, …) put their reasoning inline
//! in the text as `<thought>…</thought>` / `<think>…</think>` instead of a
//! separate channel. This filter rewrites the streamed text events so that
//! content inside those tags becomes reasoning events and never reaches the
//! visible answer. It is streaming-safe: a tag split across chunks is held
//! back until it can be classified.

use super::types::LlmEvent;

const TAGS: &[&str] = &["thought", "think", "thinking", "reasoning"];

#[derive(Default)]
pub struct ThinkTagFilter {
    /// text held back because it may be the start of a tag
    pending: String,
    inside: Option<&'static str>,
    text_id: String,
    reasoning_id: Option<String>,
    counter: u32,
    /// the model did emit a tag at least once (so we keep scanning cheaply)
    seen_any: bool,
    /// a text part is open upstream / we opened a continuation part
    text_open: bool,
    /// visible text was emitted into the currently open text part
    visible: bool,
}

impl ThinkTagFilter {
    /// Rewrite one event into zero or more events.
    pub fn push(&mut self, event: LlmEvent) -> Vec<LlmEvent> {
        match event {
            LlmEvent::TextStart { id } => {
                self.text_id = id.clone();
                self.text_open = true;
                self.visible = false;
                vec![LlmEvent::TextStart { id }]
            }
            LlmEvent::TextDelta { id, text } => {
                if !self.text_open && self.text_id.is_empty() {
                    self.text_id = id;
                    self.text_open = true;
                }
                self.pending.push_str(&text);
                self.drain(false)
            }
            LlmEvent::TextEnd { .. } => {
                let mut out = self.drain(true);
                if let Some(rid) = self.reasoning_id.take() {
                    out.push(LlmEvent::ReasoningEnd {
                        id: rid,
                        metadata: None,
                    });
                    self.inside = None;
                }
                if self.text_open {
                    out.push(LlmEvent::TextEnd {
                        id: self.text_id.clone(),
                    });
                    self.text_open = false;
                }
                out
            }
            other => vec![other],
        }
    }

    /// Flush whatever is held back (call at end of stream).
    pub fn finish(&mut self) -> Vec<LlmEvent> {
        let mut out = self.drain(true);
        if let Some(rid) = self.reasoning_id.take() {
            out.push(LlmEvent::ReasoningEnd {
                id: rid,
                metadata: None,
            });
        }
        out
    }

    fn emit_text(&mut self, out: &mut Vec<LlmEvent>, s: &str) {
        if s.is_empty() {
            return;
        }
        if !self.text_open {
            // text resumes after a thought: open a fresh part so it sorts after the reasoning
            self.counter += 1;
            self.text_id = format!("{}-c{}", self.text_id, self.counter);
            out.push(LlmEvent::TextStart {
                id: self.text_id.clone(),
            });
            self.text_open = true;
        }
        self.visible = true;
        out.push(LlmEvent::TextDelta {
            id: self.text_id.clone(),
            text: s.to_string(),
        });
    }

    /// A thought is starting: close an empty text part so the reasoning
    /// part is created (and displayed) before the answer text.
    fn close_empty_text(&mut self, out: &mut Vec<LlmEvent>) {
        if self.text_open && !self.visible {
            out.push(LlmEvent::TextEnd {
                id: self.text_id.clone(),
            });
            self.text_open = false;
        }
    }

    fn emit_reasoning(&mut self, out: &mut Vec<LlmEvent>, s: &str) {
        if s.is_empty() {
            return;
        }
        if self.reasoning_id.is_none() {
            self.counter += 1;
            let id = format!("{}-think-{}", self.text_id, self.counter);
            out.push(LlmEvent::ReasoningStart { id: id.clone() });
            self.reasoning_id = Some(id);
        }
        out.push(LlmEvent::ReasoningDelta {
            id: self.reasoning_id.clone().unwrap(),
            text: s.to_string(),
        });
    }

    fn drain(&mut self, eof: bool) -> Vec<LlmEvent> {
        let mut out = Vec::new();
        loop {
            let buf = std::mem::take(&mut self.pending);
            if buf.is_empty() {
                break;
            }
            match self.inside {
                None => {
                    // look for an opening tag
                    match buf.find('<') {
                        None => {
                            self.emit_text(&mut out, &buf);
                        }
                        Some(i) => {
                            let (before, rest) = buf.split_at(i);
                            let mut matched = None;
                            for tag in TAGS {
                                let open = format!("<{tag}>");
                                if rest.starts_with(&open) {
                                    matched = Some((*tag, open.len()));
                                    break;
                                }
                            }
                            if let Some((tag, len)) = matched {
                                // drop a single newline that usually follows the tag
                                self.emit_text(&mut out, before.trim_end_matches('\n'));
                                self.close_empty_text(&mut out);
                                self.inside = Some(tag);
                                self.seen_any = true;
                                self.pending = rest[len..].to_string();
                                continue;
                            }
                            // could still be a prefix of a tag ("<tho") — hold back unless at eof
                            let could_be_prefix =
                                !eof && TAGS.iter().any(|t| format!("<{t}>").starts_with(rest));
                            if could_be_prefix {
                                self.emit_text(&mut out, before);
                                self.pending = rest.to_string();
                                break;
                            }
                            // a '<' that is not ours: emit it and keep scanning after it
                            self.emit_text(&mut out, &buf[..i + 1]);
                            self.pending = rest[1..].to_string();
                            continue;
                        }
                    }
                }
                Some(tag) => {
                    let close = format!("</{tag}>");
                    match buf.find(&close) {
                        Some(i) => {
                            let inner = buf[..i].to_string();
                            self.emit_reasoning(&mut out, &inner);
                            if let Some(rid) = self.reasoning_id.take() {
                                out.push(LlmEvent::ReasoningEnd {
                                    id: rid,
                                    metadata: None,
                                });
                            }
                            self.inside = None;
                            self.pending = buf[i + close.len()..].trim_start_matches('\n').to_string();
                            continue;
                        }
                        None => {
                            // keep a possible partial closing tag in the buffer
                            let keep = if eof { 0 } else { partial_suffix(&buf, &close) };
                            let (emit, hold) = buf.split_at(buf.len() - keep);
                            let emit = emit.to_string();
                            self.emit_reasoning(&mut out, &emit);
                            self.pending = hold.to_string();
                            break;
                        }
                    }
                }
            }
        }
        out
    }
}

/// Length of the longest suffix of `buf` that is a prefix of `pat`.
fn partial_suffix(buf: &str, pat: &str) -> usize {
    for n in (1..pat.len()).rev() {
        if buf.len() >= n && buf.is_char_boundary(buf.len() - n) && pat.starts_with(&buf[buf.len() - n..]) {
            return n;
        }
    }
    0
}

/// Strip think blocks from a complete string (titles, summaries).
pub fn strip(text: &str) -> String {
    let mut out = text.to_string();
    for tag in TAGS {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        while let Some(i) = out.find(&open) {
            match out[i..].find(&close) {
                Some(j) => out.replace_range(i..i + j + close.len(), ""),
                None => {
                    out.truncate(i);
                    break;
                }
            }
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(chunks: &[&str]) -> (String, String) {
        let mut f = ThinkTagFilter::default();
        let mut text = String::new();
        let mut reasoning = String::new();
        let collect = |evs: Vec<LlmEvent>, text: &mut String, reasoning: &mut String| {
            for e in evs {
                match e {
                    LlmEvent::TextDelta { text: t, .. } => text.push_str(&t),
                    LlmEvent::ReasoningDelta { text: t, .. } => reasoning.push_str(&t),
                    _ => {}
                }
            }
        };
        for c in chunks {
            let evs = f.push(LlmEvent::TextDelta {
                id: "t".into(),
                text: c.to_string(),
            });
            collect(evs, &mut text, &mut reasoning);
        }
        collect(f.finish(), &mut text, &mut reasoning);
        (text, reasoning)
    }

    #[test]
    fn splits_thought_blocks_even_across_chunks() {
        let (t, r) = run(&["<thou", "ght>The user said hi.</th", "ought>Hi! How can I help?"]);
        assert_eq!(t, "Hi! How can I help?");
        assert_eq!(r, "The user said hi.");
    }

    #[test]
    fn leaves_ordinary_angle_brackets_alone() {
        let (t, r) = run(&["use std::vec<T>; a < b && c > d", " <div>ok</div>"]);
        assert_eq!(t, "use std::vec<T>; a < b && c > d <div>ok</div>");
        assert_eq!(r, "");
    }

    #[test]
    fn unterminated_block_at_eof_is_reasoning() {
        let (t, r) = run(&["<think>still thinking"]);
        assert_eq!(t, "");
        assert_eq!(r, "still thinking");
    }

    #[test]
    fn reasoning_part_precedes_answer_text() {
        let mut f = ThinkTagFilter::default();
        let mut seq: Vec<String> = Vec::new();
        let mut push = |f: &mut ThinkTagFilter, e: LlmEvent| {
            for ev in f.push(e) {
                seq.push(match ev {
                    LlmEvent::TextStart { id } => format!("ts:{id}"),
                    LlmEvent::TextEnd { id } => format!("te:{id}"),
                    LlmEvent::ReasoningStart { .. } => "rs".into(),
                    LlmEvent::ReasoningEnd { .. } => "re".into(),
                    LlmEvent::TextDelta { text, .. } => format!("t:{text}"),
                    LlmEvent::ReasoningDelta { text, .. } => format!("r:{text}"),
                    _ => "?".into(),
                });
            }
        };
        push(&mut f, LlmEvent::TextStart { id: "a".into() });
        push(
            &mut f,
            LlmEvent::TextDelta {
                id: "a".into(),
                text: "<thought>hmm</thought>Hello".into(),
            },
        );
        push(&mut f, LlmEvent::TextEnd { id: "a".into() });
        assert_eq!(
            seq,
            vec![
                "ts:a", "te:a", "rs", "r:hmm", "re", "ts:a-c2", "t:Hello", "te:a-c2"
            ]
        );
    }

    #[test]
    fn strip_for_titles() {
        assert_eq!(strip("<thought>x</thought>Fix login bug"), "Fix login bug");
        assert_eq!(strip("Plain title"), "Plain title");
    }
}
