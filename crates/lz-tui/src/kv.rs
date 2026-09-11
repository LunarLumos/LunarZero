//! Tiny persistent key/value store (`~/.local/state/lunarzero/kv.json`) for
//! UI preferences: theme, recent/favorite models, sidebar state, history.

use std::path::PathBuf;

use serde_json::{Map, Value};

pub struct Kv {
    path: PathBuf,
    data: Map<String, Value>,
}

impl Kv {
    pub fn open(path: PathBuf) -> Kv {
        let data = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<Map<String, Value>>(&s).ok())
            .unwrap_or_default();
        Kv { path, data }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.data.get(key)
    }
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.data.get(key).and_then(Value::as_str)
    }
    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        self.data.get(key).and_then(Value::as_bool).unwrap_or(default)
    }
    pub fn get_list(&self, key: &str) -> Vec<String> {
        self.data
            .get(key)
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    }
    pub fn set(&mut self, key: &str, value: Value) {
        self.data.insert(key.to_string(), value);
        self.flush();
    }
    pub fn set_list(&mut self, key: &str, list: &[String]) {
        self.set(
            key,
            Value::Array(list.iter().map(|s| Value::String(s.clone())).collect()),
        );
    }
    pub fn remove(&mut self, key: &str) {
        self.data.remove(key);
        self.flush();
    }
    fn flush(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(s) = serde_json::to_string_pretty(&self.data) {
            let tmp = self.path.with_extension("json.tmp");
            if std::fs::write(&tmp, s).is_ok() {
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
    }
}
