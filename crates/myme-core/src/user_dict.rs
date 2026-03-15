//! User dictionary — personal word list that supplements the system dictionary.
//!
//! Stored in SKK format at `~/Library/Application Support/myme/user.dict`.
//! User entries receive a score boost so they rank above system entries with
//! the same reading.

use std::path::Path;

use crate::candidate::{Candidate, CandidateSource};
use crate::dictionary::{DictionaryLookup, SimpleDictionary};

// ---------------------------------------------------------------------------
// UserDictionary
// ---------------------------------------------------------------------------

/// A user-specific dictionary that wraps a [`SimpleDictionary`] but tags all
/// candidates as [`CandidateSource::User`].
pub struct UserDictionary {
    inner: SimpleDictionary,
}

impl UserDictionary {
    /// Load a user dictionary from an SKK-format file.
    ///
    /// Returns an empty dictionary if the file does not exist (not an error —
    /// the user simply hasn't added any words yet).
    pub fn load(path: &Path) -> Self {
        let inner = if path.exists() {
            SimpleDictionary::load_from_file(path).unwrap_or_else(|_| {
                SimpleDictionary::load_from_skk_text("").unwrap()
            })
        } else {
            SimpleDictionary::load_from_skk_text("").unwrap()
        };
        Self { inner }
    }

    /// Create an empty user dictionary.
    pub fn empty() -> Self {
        Self {
            inner: SimpleDictionary::load_from_skk_text("").unwrap(),
        }
    }
}

impl DictionaryLookup for UserDictionary {
    fn lookup(&self, reading: &str) -> Vec<Candidate> {
        self.inner
            .lookup(reading)
            .into_iter()
            .map(|mut c| {
                c.source = CandidateSource::User;
                c
            })
            .collect()
    }

    fn common_prefix_search(&self, text: &str) -> Vec<(String, Vec<Candidate>)> {
        self.inner
            .common_prefix_search(text)
            .into_iter()
            .map(|(reading, candidates)| {
                let tagged: Vec<Candidate> = candidates
                    .into_iter()
                    .map(|mut c| {
                        c.source = CandidateSource::User;
                        c
                    })
                    .collect();
                (reading, tagged)
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// CompositeDictionary
// ---------------------------------------------------------------------------

/// Merges results from a system dictionary and a user dictionary.
///
/// User entries receive a score boost of +50 so they rank above system entries
/// with the same reading.  Duplicate surfaces (same reading + surface) are
/// deduplicated in favor of the higher-scoring entry.
pub struct CompositeDictionary {
    system: SimpleDictionary,
    user: Option<UserDictionary>,
}

/// Score boost applied to user dictionary entries.
const USER_SCORE_BOOST: u32 = 50;

impl CompositeDictionary {
    /// Create a composite dictionary from a system dict and an optional user dict.
    pub fn new(system: SimpleDictionary, user: Option<UserDictionary>) -> Self {
        Self { system, user }
    }

    /// Merge and deduplicate two candidate lists.
    fn merge_candidates(system: Vec<Candidate>, user: Vec<Candidate>) -> Vec<Candidate> {
        use std::collections::HashSet;

        let mut seen = HashSet::new();
        let mut merged = Vec::new();

        // User entries first (boosted).
        for mut c in user {
            c.score += USER_SCORE_BOOST;
            let key = (c.reading.clone(), c.surface.clone());
            if seen.insert(key) {
                merged.push(c);
            }
        }

        // System entries.
        for c in system {
            let key = (c.reading.clone(), c.surface.clone());
            if seen.insert(key) {
                merged.push(c);
            }
        }

        merged.sort();
        merged
    }
}

impl DictionaryLookup for CompositeDictionary {
    fn lookup(&self, reading: &str) -> Vec<Candidate> {
        let system = self.system.lookup(reading);
        let user = self
            .user
            .as_ref()
            .map(|u| u.lookup(reading))
            .unwrap_or_default();
        Self::merge_candidates(system, user)
    }

    fn common_prefix_search(&self, text: &str) -> Vec<(String, Vec<Candidate>)> {
        let sys_results = self.system.common_prefix_search(text);
        let user_results = self
            .user
            .as_ref()
            .map(|u| u.common_prefix_search(text))
            .unwrap_or_default();

        // Merge by reading key.
        use std::collections::BTreeMap;
        let mut by_reading: BTreeMap<String, (Vec<Candidate>, Vec<Candidate>)> = BTreeMap::new();

        for (reading, candidates) in sys_results {
            by_reading.entry(reading).or_default().0 = candidates;
        }
        for (reading, candidates) in user_results {
            by_reading.entry(reading).or_default().1 = candidates;
        }

        let mut results: Vec<(String, Vec<Candidate>)> = by_reading
            .into_iter()
            .map(|(reading, (sys, usr))| {
                let merged = Self::merge_candidates(sys, usr);
                (reading, merged)
            })
            .collect();

        // Sort by descending reading length.
        results.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        results
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dictionary::SimpleDictionary;

    #[test]
    fn user_entries_appear_in_lookup() {
        let user = UserDictionary {
            inner: SimpleDictionary::load_from_skk_text("みめ /myme/\n").unwrap(),
        };
        let results = user.lookup("みめ");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].surface, "myme");
        assert_eq!(results[0].source, CandidateSource::User);
    }

    #[test]
    fn user_entries_rank_above_system() {
        let system = SimpleDictionary::load_from_skk_text("てすと /テスト/試験/\n").unwrap();
        let user = UserDictionary {
            inner: SimpleDictionary::load_from_skk_text("てすと /mytest/\n").unwrap(),
        };
        let composite = CompositeDictionary::new(system, Some(user));
        let results = composite.lookup("てすと");
        // User entry "mytest" should rank first due to +50 boost.
        assert_eq!(results[0].surface, "mytest");
        assert_eq!(results[0].source, CandidateSource::User);
    }

    #[test]
    fn duplicate_dedup_favors_higher_score() {
        let system = SimpleDictionary::load_from_skk_text("てすと /テスト/\n").unwrap();
        let user = UserDictionary {
            inner: SimpleDictionary::load_from_skk_text("てすと /テスト/\n").unwrap(),
        };
        let composite = CompositeDictionary::new(system, Some(user));
        let results = composite.lookup("てすと");
        // Only one entry for テスト (user version wins due to boost).
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].surface, "テスト");
        assert_eq!(results[0].source, CandidateSource::User);
    }

    #[test]
    fn empty_user_dict_is_harmless() {
        let system = SimpleDictionary::load_from_skk_text("てすと /テスト/\n").unwrap();
        let composite = CompositeDictionary::new(system, None);
        let results = composite.lookup("てすと");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].surface, "テスト");
    }

    #[test]
    fn missing_user_dict_file_creates_empty() {
        let user = UserDictionary::load(Path::new("/tmp/nonexistent_myme_user_dict_xxx.dict"));
        let results = user.lookup("anything");
        assert!(results.is_empty());
    }

    #[test]
    fn composite_common_prefix_search_merges() {
        let system = SimpleDictionary::load_from_skk_text("き /木/\nきょう /今日/\n").unwrap();
        let user = UserDictionary {
            inner: SimpleDictionary::load_from_skk_text("き /気/\n").unwrap(),
        };
        let composite = CompositeDictionary::new(system, Some(user));
        let results = composite.common_prefix_search("きょうは");
        // Should have entries for both きょう and き.
        let readings: Vec<&str> = results.iter().map(|r| r.0.as_str()).collect();
        assert!(readings.contains(&"きょう"));
        assert!(readings.contains(&"き"));
        // き should have both 木 and 気.
        let ki = results.iter().find(|r| r.0 == "き").unwrap();
        let surfaces: Vec<&str> = ki.1.iter().map(|c| c.surface.as_str()).collect();
        assert!(surfaces.contains(&"木"));
        assert!(surfaces.contains(&"気"));
    }
}
