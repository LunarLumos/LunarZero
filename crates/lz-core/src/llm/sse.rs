//! Minimal Server-Sent-Events framer: feed bytes, get `data:` payloads.

#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    /// Max bytes buffered before we give up on a runaway line.
    pub max_buffer: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseMessage {
    pub event: Option<String>,
    pub data: String,
}

impl SseParser {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            max_buffer: 16 * 1024 * 1024,
        }
    }

    /// Push a chunk and return every complete event it completes.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseMessage>, String> {
        self.buf.extend_from_slice(chunk);
        if self.buf.len() > self.max_buffer {
            return Err("SSE buffer exceeded limit".into());
        }
        let mut out = Vec::new();
        // find a blank line (\n\n or \r\n\r\n)
        while let Some((end, sep)) = find_blank_line(&self.buf) {
            let raw = self.buf.drain(..end + sep).collect::<Vec<u8>>();
            let text = String::from_utf8_lossy(&raw[..end]);
            if let Some(msg) = parse_block(&text) {
                out.push(msg);
            }
        }
        Ok(out)
    }

    /// Flush any trailing block without a terminating blank line.
    pub fn finish(&mut self) -> Option<SseMessage> {
        if self.buf.is_empty() {
            return None;
        }
        let text = String::from_utf8_lossy(&self.buf).to_string();
        self.buf.clear();
        parse_block(&text)
    }
}

fn find_blank_line(buf: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < buf.len() {
        if buf[i] == b'\n' {
            if buf.get(i + 1) == Some(&b'\n') {
                return Some((i, 2));
            }
            if buf.get(i + 1) == Some(&b'\r') && buf.get(i + 2) == Some(&b'\n') {
                return Some((i, 3));
            }
        }
        i += 1;
    }
    None
}

fn parse_block(text: &str) -> Option<SseMessage> {
    let mut event = None;
    let mut data: Vec<&str> = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => event = Some(value.to_string()),
            "data" => data.push(value),
            _ => {}
        }
    }
    if data.is_empty() {
        return None;
    }
    Some(SseMessage {
        event,
        data: data.join("\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_events_across_chunks() {
        let mut p = SseParser::new();
        let a = p.push(b"data: {\"a\":1}\n\ndata: {\"b\"").unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].data, "{\"a\":1}");
        let b = p
            .push(b":2}\n\n: comment\n\nevent: done\ndata: [DONE]\n\n")
            .unwrap();
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].data, "{\"b\":2}");
        assert_eq!(b[1].event.as_deref(), Some("done"));
        assert_eq!(b[1].data, "[DONE]");
    }

    #[test]
    fn multiline_data_and_crlf() {
        let mut p = SseParser::new();
        let out = p.push(b"data: line1\r\ndata: line2\r\n\r\n").unwrap();
        assert_eq!(out[0].data, "line1\nline2");
    }
}
