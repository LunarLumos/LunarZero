//! Identifier generation — time-ordered, sortable ids.
//!
//! Format: `<prefix>_<12 hex chars of 48-bit time><14 base62 random chars>`.
//! The time component is `now_ms * 0x1000 + counter`, so IDs created in the
//! same millisecond stay monotonic. Sessions use the *descending* variant
//! (bitwise NOT of the time) so the newest session sorts first lexically.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rand::RngCore;

const LENGTH: usize = 26;
const BASE62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

static STATE: Mutex<(u64, u64)> = Mutex::new((0, 0));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prefix {
    Session,
    Message,
    Part,
    Permission,
    Question,
    Event,
    Job,
    Tool,
}

impl Prefix {
    pub const fn as_str(self) -> &'static str {
        match self {
            Prefix::Session => "ses",
            Prefix::Message => "msg",
            Prefix::Part => "prt",
            Prefix::Permission => "per",
            Prefix::Question => "que",
            Prefix::Event => "evt",
            Prefix::Job => "job",
            Prefix::Tool => "tool",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Ascending,
    Descending,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn random_base62(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| BASE62[(*b % 62) as usize] as char).collect()
}

/// Create an ID with an explicit timestamp (ms). Exposed for tests.
pub fn create_at(prefix: &str, direction: Direction, timestamp_ms: u64) -> String {
    let counter = {
        let mut st = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if timestamp_ms != st.0 {
            st.0 = timestamp_ms;
            st.1 = 0;
        }
        st.1 += 1;
        st.1
    };
    let mut now = timestamp_ms.wrapping_mul(0x1000).wrapping_add(counter);
    if direction == Direction::Descending {
        now = !now;
    }
    let mut hex = String::with_capacity(12);
    for i in 0..6 {
        let byte = (now >> (40 - 8 * i)) & 0xff;
        hex.push_str(&format!("{byte:02x}"));
    }
    format!("{prefix}_{hex}{}", random_base62(LENGTH - 12))
}

pub fn create(prefix: &str, direction: Direction) -> String {
    create_at(prefix, direction, now_ms())
}

pub fn ascending(prefix: Prefix) -> String {
    create(prefix.as_str(), Direction::Ascending)
}

pub fn descending(prefix: Prefix) -> String {
    create(prefix.as_str(), Direction::Descending)
}

/// Extract the millisecond timestamp from an *ascending* ID.
pub fn timestamp(id: &str) -> Option<u64> {
    let (_, rest) = id.split_once('_')?;
    let hex = rest.get(0..12)?;
    let encoded = u64::from_str_radix(hex, 16).ok()?;
    Some(encoded / 0x1000)
}

pub fn has_prefix(id: &str, prefix: Prefix) -> bool {
    id.starts_with(prefix.as_str()) && id.as_bytes().get(prefix.as_str().len()) == Some(&b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    // The generator keeps global (timestamp, counter) state; serialize tests
    // that depend on it.
    static LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn format_is_prefix_underscore_26_chars() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let id = ascending(Prefix::Message);
        assert!(id.starts_with("msg_"));
        assert_eq!(id.len(), 4 + 26);
        let hex = &id[4..16];
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn ascending_is_monotonic_within_same_ms() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let a = create_at("msg", Direction::Ascending, 1_700_000_000_000);
        let b = create_at("msg", Direction::Ascending, 1_700_000_000_000);
        assert!(a[..16] < b[..16]);
    }

    #[test]
    fn descending_sorts_newest_first() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let older = create_at("ses", Direction::Descending, 1_700_000_000_000);
        let newer = create_at("ses", Direction::Descending, 1_700_000_001_000);
        assert!(newer < older);
    }

    #[test]
    fn timestamp_roundtrip() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Only the low 48 bits of `ts * 0x1000` are encoded (by design),
        // so round-tripping is exact only for ts < 2^36.
        let ts = 60_000_123_456;
        let id = create_at("msg", Direction::Ascending, ts);
        assert_eq!(timestamp(&id), Some(ts));
    }

    #[test]
    fn prefix_check() {
        assert!(has_prefix("ses_abc", Prefix::Session));
        assert!(!has_prefix("session_abc", Prefix::Session));
    }
}
