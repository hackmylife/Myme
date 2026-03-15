//! Selection history learning — tracks which candidates the user confirms
//! and boosts their scores in future conversions.
//!
//! ## Persistence
//!
//! The learning store is written as a TSV file:
//! ```text
//! reading\tsurface\tcount\tlast_used
//! ```
//! Stored at `~/Library/Application Support/myme/learning.tsv`.
//!
//! ## Garbage collection
//!
//! Entries older than 90 days are discarded on load.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// LearnEntry
// ---------------------------------------------------------------------------

/// A single learning record for a (reading, surface) pair.
#[derive(Debug, Clone)]
pub struct LearnEntry {
    /// Number of times this candidate was selected.
    pub count: u32,
    /// Unix timestamp of the last selection.
    pub last_used: u64,
}

// ---------------------------------------------------------------------------
// LearningStore
// ---------------------------------------------------------------------------

/// In-memory store of candidate selection history.
pub struct LearningStore {
    entries: HashMap<(String, String), LearnEntry>,
    path: Option<PathBuf>,
    dirty: bool,
    commit_count: u32,
}

/// Maximum age in seconds for a learning entry (90 days).
const GC_MAX_AGE_SECS: u64 = 90 * 24 * 60 * 60;

/// Flush interval: persist after this many commits.
const FLUSH_INTERVAL: u32 = 5;

impl LearningStore {
    /// Create an empty store that does not persist.
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            path: None,
            dirty: false,
            commit_count: 0,
        }
    }

    /// Create a store backed by a TSV file on disk.
    ///
    /// Loads existing data if the file exists; applies GC on load.
    pub fn load(path: &Path) -> Self {
        let mut store = Self {
            entries: HashMap::new(),
            path: Some(path.to_path_buf()),
            dirty: false,
            commit_count: 0,
        };

        if path.exists() {
            if let Ok(text) = std::fs::read_to_string(path) {
                let now = current_timestamp();
                for line in text.lines() {
                    let parts: Vec<&str> = line.split('\t').collect();
                    if parts.len() >= 4 {
                        if let (Ok(count), Ok(last_used)) =
                            (parts[2].parse::<u32>(), parts[3].parse::<u64>())
                        {
                            // GC: skip entries older than 90 days.
                            if now.saturating_sub(last_used) <= GC_MAX_AGE_SECS {
                                store.entries.insert(
                                    (parts[0].to_string(), parts[1].to_string()),
                                    LearnEntry { count, last_used },
                                );
                            }
                        }
                    }
                }
            }
        }

        store
    }

    /// Record that the user selected `surface` for `reading`.
    pub fn record(&mut self, reading: &str, surface: &str) {
        let key = (reading.to_string(), surface.to_string());
        let now = current_timestamp();

        let entry = self.entries.entry(key).or_insert(LearnEntry {
            count: 0,
            last_used: now,
        });
        entry.count += 1;
        entry.last_used = now;
        self.dirty = true;
        self.commit_count += 1;

        if self.commit_count >= FLUSH_INTERVAL {
            self.flush();
            self.commit_count = 0;
        }
    }

    /// Returns the score boost for a (reading, surface) pair.
    ///
    /// Boost = `min(count * 10, 200)` — diminishing returns, capped.
    pub fn boost(&self, reading: &str, surface: &str) -> u32 {
        let key = (reading.to_string(), surface.to_string());
        self.entries
            .get(&key)
            .map(|e| (e.count * 10).min(200))
            .unwrap_or(0)
    }

    /// Write the store to disk if it has been modified.
    pub fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        let Some(path) = &self.path else { return };

        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let mut lines = Vec::new();
        for ((reading, surface), entry) in &self.entries {
            lines.push(format!(
                "{}\t{}\t{}\t{}",
                reading, surface, entry.count, entry.last_used
            ));
        }
        lines.sort(); // deterministic output

        let content = lines.join("\n") + "\n";
        if std::fs::write(path, content).is_ok() {
            self.dirty = false;
        }
    }
}

impl Drop for LearningStore {
    fn drop(&mut self) {
        self.flush();
    }
}

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_increments_count() {
        let mut store = LearningStore::new();
        store.record("てすと", "テスト");
        assert_eq!(store.boost("てすと", "テスト"), 10);
        store.record("てすと", "テスト");
        assert_eq!(store.boost("てすと", "テスト"), 20);
    }

    #[test]
    fn boost_caps_at_200() {
        let mut store = LearningStore::new();
        for _ in 0..30 {
            store.record("てすと", "テスト");
        }
        assert_eq!(store.boost("てすと", "テスト"), 200);
    }

    #[test]
    fn unknown_pair_returns_zero_boost() {
        let store = LearningStore::new();
        assert_eq!(store.boost("unknown", "pair"), 0);
    }

    #[test]
    fn persistence_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_learning.tsv");

        // Write
        {
            let mut store = LearningStore::load(&path);
            store.record("てすと", "テスト");
            store.record("てすと", "テスト");
            store.flush();
        }

        // Read back
        {
            let store = LearningStore::load(&path);
            assert_eq!(store.boost("てすと", "テスト"), 20);
        }
    }

    #[test]
    fn gc_removes_old_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_gc.tsv");

        // Write an entry with a very old timestamp.
        let old_ts = 1000; // very old
        let content = format!("old\tentry\t5\t{old_ts}\n");
        std::fs::write(&path, content).unwrap();

        let store = LearningStore::load(&path);
        // Old entry should have been garbage-collected.
        assert_eq!(store.boost("old", "entry"), 0);
    }
}
