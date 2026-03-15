//! Binary dictionary loading and lookup.
//!
//! ## Responsibilities
//!
//! - Define the on-disk binary format produced by `tools/dict-builder` and
//!   consumed here at runtime (a compact sorted structure suitable for
//!   prefix-range queries).
//! - Memory-map the compiled `.dict` file from `data/dict/` for zero-copy
//!   access after startup.
//! - Expose `lookup(reading: &str) -> impl Iterator<Item = Entry>` returning
//!   all dictionary entries whose reading matches the given kana prefix.
//! - Define the [`Entry`] type (reading, surface form, part-of-speech, cost).
//!
//! ## Non-goals
//!
//! This module does **not** rank or filter candidates; that belongs to
//! [`crate::candidate`]. It also does not convert romaji; see
//! [`crate::romaji`].

use std::collections::BTreeMap;

use thiserror::Error;

use crate::candidate::{Candidate, CandidateSource};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur while loading or parsing a dictionary.
#[derive(Debug, Error)]
pub enum DictionaryError {
    /// A line in the dictionary file could not be parsed.
    #[error("malformed dictionary line {line_number}: {detail}")]
    MalformedLine { line_number: usize, detail: String },

    /// An I/O error occurred while reading the dictionary file.
    #[error("I/O error reading dictionary: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Trait for any type that can look up conversion candidates by reading.
pub trait DictionaryLookup {
    /// Return all candidates whose reading matches `reading`, sorted by
    /// descending score (best candidate first).
    fn lookup(&self, reading: &str) -> Vec<Candidate>;

    /// Return all entries whose reading is a prefix of `text`, sorted by
    /// descending reading length (longest prefix first).
    ///
    /// Each element is `(reading, candidates)` where `reading` is the prefix
    /// that matched and `candidates` is the sorted candidate list for that
    /// reading.
    ///
    /// Example: `common_prefix_search("きょうは")` might return
    /// `[("きょう", [...]), ("きょ", [...]), ("き", [...])]`
    fn common_prefix_search(&self, text: &str) -> Vec<(String, Vec<Candidate>)> {
        // Default implementation: brute-force over all prefixes.
        let mut results = Vec::new();
        let chars: Vec<char> = text.chars().collect();
        for len in 1..=chars.len() {
            let prefix: String = chars[..len].iter().collect();
            let candidates = self.lookup(&prefix);
            if !candidates.is_empty() {
                results.push((prefix, candidates));
            }
        }
        // Sort by descending reading length (longest match first).
        results.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        results
    }
}

// ---------------------------------------------------------------------------
// Internal storage types
// ---------------------------------------------------------------------------

/// A single entry stored inside [`SimpleDictionary`].
#[derive(Debug, Clone)]
pub struct DictEntry {
    /// The surface form (e.g. `"変換"`).
    pub surface: String,
    /// Pre-computed score; higher is better.
    pub score: u32,
    /// Optional frequency count from an external corpus or annotation.
    /// When present, the score is boosted by `log2(frequency) * 3`.
    pub frequency: Option<u32>,
}

// ---------------------------------------------------------------------------
// SimpleDictionary
// ---------------------------------------------------------------------------

/// An in-memory dictionary backed by a [`HashMap`] keyed on reading strings.
///
/// Entries are loaded from SKK-format text via [`SimpleDictionary::load_from_skk_text`].
///
/// # SKK format
///
/// ```text
/// ; this is a comment
/// へんかん /変換/偏官/
/// にほんご /日本語/
/// ```
///
/// Each non-comment line must begin with a reading, followed by a space,
/// followed by a `/`-delimited candidate list that starts **and** ends with
/// `/`.  Candidates are assigned scores in descending order of their position
/// so that the first-listed candidate (which SKK treats as the most frequent)
/// gets the highest score.
pub struct SimpleDictionary {
    /// `reading → [DictEntry, …]` — entries are stored in insertion order;
    /// callers receive them sorted by score.
    entries: BTreeMap<String, Vec<DictEntry>>,
}

impl SimpleDictionary {
    /// Parse an SKK-format dictionary from a string slice.
    ///
    /// # Errors
    ///
    /// Returns [`DictionaryError::MalformedLine`] if a non-comment, non-empty
    /// line cannot be split into a reading and a valid candidate list.
    pub fn load_from_skk_text(text: &str) -> Result<Self, DictionaryError> {
        let mut entries: BTreeMap<String, Vec<DictEntry>> = BTreeMap::new();

        for (idx, raw_line) in text.lines().enumerate() {
            let line_number = idx + 1; // 1-based for human-readable errors
            let line = raw_line.trim();

            // Skip blank lines and comment lines (lines starting with ';').
            if line.is_empty() || line.starts_with(';') {
                continue;
            }

            // Split on the first ASCII space: `reading /cand1/cand2/.../`
            let (reading, rest) = line.split_once(' ').ok_or_else(|| {
                DictionaryError::MalformedLine {
                    line_number,
                    detail: "missing space separator between reading and candidates".to_string(),
                }
            })?;

            if reading.is_empty() {
                return Err(DictionaryError::MalformedLine {
                    line_number,
                    detail: "reading is empty".to_string(),
                });
            }

            // Candidate list must start with '/'.
            let rest = rest.trim();
            if !rest.starts_with('/') {
                return Err(DictionaryError::MalformedLine {
                    line_number,
                    detail: "candidate list must start with '/'".to_string(),
                });
            }

            // Strip the leading '/' and split on '/', discarding empty tokens
            // (the trailing '/' produces an empty token that we intentionally
            // skip, as do consecutive '/' characters).
            let candidates: Vec<&str> = rest[1..]
                .split('/')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();

            if candidates.is_empty() {
                return Err(DictionaryError::MalformedLine {
                    line_number,
                    detail: "no candidates found in candidate list".to_string(),
                });
            }

            // Assign scores: first candidate gets the highest score.  We use
            // a simple linear scheme: score = total_count - position_index,
            // multiplied by 10 to leave room for future bias adjustments.
            // An optional `;freq=N` annotation per candidate adds a frequency
            // bonus of `log2(N) * 3`.
            let total = candidates.len() as u32;
            let dict_entries: Vec<DictEntry> = candidates
                .into_iter()
                .enumerate()
                .map(|(i, raw_surface)| {
                    // Parse optional `;freq=N` annotation.
                    let (surface, frequency) = if let Some((s, ann)) = raw_surface.split_once(';') {
                        let freq = ann
                            .strip_prefix("freq=")
                            .and_then(|v| v.parse::<u32>().ok());
                        (s, freq)
                    } else {
                        (raw_surface, None)
                    };

                    let position_score = (total - i as u32) * 10;
                    let freq_bonus = frequency
                        .map(|f| if f > 0 { ((f as f64).log2() as u32) * 3 } else { 0 })
                        .unwrap_or(0);

                    DictEntry {
                        surface: surface.to_string(),
                        score: position_score + freq_bonus,
                        frequency,
                    }
                })
                .collect();

            entries
                .entry(reading.to_string())
                .or_default()
                .extend(dict_entries);
        }

        Ok(Self { entries })
    }

    /// Load a dictionary from a UTF-8 SKK-format file on disk.
    ///
    /// Reads the entire file into memory and delegates to
    /// [`SimpleDictionary::load_from_skk_text`].
    ///
    /// # Errors
    ///
    /// Returns [`DictionaryError::Io`] if the file cannot be read, or
    /// [`DictionaryError::MalformedLine`] if the content is not valid SKK
    /// format.
    pub fn load_from_file(path: &std::path::Path) -> Result<Self, DictionaryError> {
        let text = std::fs::read_to_string(path)?;
        Self::load_from_skk_text(&text)
    }

    /// Return the total number of (reading, surface) pairs stored.
    pub fn entry_count(&self) -> usize {
        self.entries.values().map(|v| v.len()).sum()
    }
}

impl DictionaryLookup for SimpleDictionary {
    fn lookup(&self, reading: &str) -> Vec<Candidate> {
        let Some(dict_entries) = self.entries.get(reading) else {
            return Vec::new();
        };

        let mut candidates: Vec<Candidate> = dict_entries
            .iter()
            .map(|e| {
                Candidate::new(
                    e.surface.clone(),
                    reading,
                    e.score,
                    CandidateSource::System,
                )
            })
            .collect();

        // Sort best-first; Candidate's Ord impl puts higher scores first.
        candidates.sort();
        candidates
    }

    fn common_prefix_search(&self, text: &str) -> Vec<(String, Vec<Candidate>)> {
        let mut results = Vec::new();

        // Try each prefix length from 1 char up to the full text.
        // Use BTreeMap range to quickly check if a prefix exists.
        let chars: Vec<char> = text.chars().collect();
        for len in 1..=chars.len() {
            let prefix: String = chars[..len].iter().collect();
            if let Some(dict_entries) = self.entries.get(&prefix) {
                let mut candidates: Vec<Candidate> = dict_entries
                    .iter()
                    .map(|e| {
                        Candidate::new(
                            e.surface.clone(),
                            &prefix,
                            e.score,
                            CandidateSource::System,
                        )
                    })
                    .collect();
                candidates.sort();
                results.push((prefix, candidates));
            }
        }

        // Sort by descending reading length (longest match first).
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

    const SAMPLE_SKK: &str = r#"
; SKK dictionary sample
; another comment

へんかん /変換/偏官/返還/
にほんご /日本語/
てすと /テスト/試験/
"#;

    // ------------------------------------------------------------------
    // 1. Basic parse succeeds
    // ------------------------------------------------------------------
    #[test]
    fn parse_sample_succeeds() {
        let dict = SimpleDictionary::load_from_skk_text(SAMPLE_SKK)
            .expect("sample SKK text should parse without error");
        // 3 readings × (3 + 1 + 2) entries = 6 total
        assert_eq!(dict.entry_count(), 6);
    }

    // ------------------------------------------------------------------
    // 2. Lookup returns correct surface forms
    // ------------------------------------------------------------------
    #[test]
    fn lookup_returns_correct_candidates() {
        let dict = SimpleDictionary::load_from_skk_text(SAMPLE_SKK).unwrap();
        let results = dict.lookup("へんかん");
        let surfaces: Vec<&str> = results.iter().map(|c| c.surface.as_str()).collect();
        assert_eq!(surfaces, vec!["変換", "偏官", "返還"]);
    }

    // ------------------------------------------------------------------
    // 3. First candidate has the highest score
    // ------------------------------------------------------------------
    #[test]
    fn first_candidate_has_highest_score() {
        let dict = SimpleDictionary::load_from_skk_text(SAMPLE_SKK).unwrap();
        let results = dict.lookup("へんかん");
        assert!(!results.is_empty());
        // Already sorted; first element must have the highest score.
        let scores: Vec<u32> = results.iter().map(|c| c.score).collect();
        let mut sorted_desc = scores.clone();
        sorted_desc.sort_by(|a, b| b.cmp(a));
        assert_eq!(scores, sorted_desc, "lookup results must be sorted by descending score");
    }

    // ------------------------------------------------------------------
    // 4. Missing reading returns empty vec
    // ------------------------------------------------------------------
    #[test]
    fn missing_reading_returns_empty() {
        let dict = SimpleDictionary::load_from_skk_text(SAMPLE_SKK).unwrap();
        let results = dict.lookup("そんざいしない");
        assert!(results.is_empty());
    }

    // ------------------------------------------------------------------
    // 5. Comment lines are skipped (entry count is not inflated)
    // ------------------------------------------------------------------
    #[test]
    fn comment_lines_are_skipped() {
        let text = "; just a comment\nにほんご /日本語/\n";
        let dict = SimpleDictionary::load_from_skk_text(text).unwrap();
        assert_eq!(dict.entry_count(), 1);
    }

    // ------------------------------------------------------------------
    // 6. Blank lines are skipped without error
    // ------------------------------------------------------------------
    #[test]
    fn blank_lines_are_skipped() {
        let text = "\n\nへんかん /変換/\n\n";
        let dict = SimpleDictionary::load_from_skk_text(text).unwrap();
        assert_eq!(dict.entry_count(), 1);
    }

    // ------------------------------------------------------------------
    // 7. Lookup reading field is propagated correctly
    // ------------------------------------------------------------------
    #[test]
    fn lookup_reading_field_is_correct() {
        let dict = SimpleDictionary::load_from_skk_text(SAMPLE_SKK).unwrap();
        for candidate in dict.lookup("にほんご") {
            assert_eq!(candidate.reading, "にほんご");
        }
    }

    // ------------------------------------------------------------------
    // 8. Source is always CandidateSource::System
    // ------------------------------------------------------------------
    #[test]
    fn lookup_source_is_system() {
        let dict = SimpleDictionary::load_from_skk_text(SAMPLE_SKK).unwrap();
        for candidate in dict.lookup("てすと") {
            assert_eq!(candidate.source, CandidateSource::System);
        }
    }

    // ------------------------------------------------------------------
    // 9. Malformed line (no space) returns error
    // ------------------------------------------------------------------
    #[test]
    fn malformed_line_no_space_returns_error() {
        let text = "へんかん/変換/\n";
        let result = SimpleDictionary::load_from_skk_text(text);
        assert!(result.is_err(), "line without space separator should fail");
    }

    // ------------------------------------------------------------------
    // 10. Malformed line (no leading slash in candidate list) returns error
    // ------------------------------------------------------------------
    #[test]
    fn malformed_line_no_leading_slash_returns_error() {
        let text = "へんかん 変換/偏官/\n";
        let result = SimpleDictionary::load_from_skk_text(text);
        assert!(result.is_err(), "candidate list must start with '/'");
    }

    // ------------------------------------------------------------------
    // 11. Single candidate is handled correctly
    // ------------------------------------------------------------------
    #[test]
    fn single_candidate_works() {
        let dict = SimpleDictionary::load_from_skk_text("にほんご /日本語/").unwrap();
        let results = dict.lookup("にほんご");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].surface, "日本語");
        assert_eq!(results[0].score, 10);
    }

    // ------------------------------------------------------------------
    // 12. entry_count aggregates across all readings
    // ------------------------------------------------------------------
    #[test]
    fn entry_count_aggregates_all_readings() {
        let text = "あ /亜/阿/\nい /以/位/\n";
        let dict = SimpleDictionary::load_from_skk_text(text).unwrap();
        assert_eq!(dict.entry_count(), 4);
    }

    // ------------------------------------------------------------------
    // 13. load_from_file succeeds with a valid temp file
    // ------------------------------------------------------------------
    #[test]
    fn load_from_file_parses_valid_file() {
        use std::io::Write as IoWrite;
        let mut tmp = tempfile::NamedTempFile::new().expect("failed to create temp file");
        writeln!(tmp, "; comment").unwrap();
        writeln!(tmp, "にほんご /日本語/").unwrap();
        writeln!(tmp, "へんかん /変換/偏官/").unwrap();
        tmp.flush().unwrap();

        let dict = SimpleDictionary::load_from_file(tmp.path())
            .expect("load_from_file should succeed on a valid SKK file");
        assert_eq!(dict.entry_count(), 3);
        assert_eq!(dict.lookup("にほんご")[0].surface, "日本語");
    }

    // ------------------------------------------------------------------
    // 14. load_from_file returns Io error for a missing file
    // ------------------------------------------------------------------
    #[test]
    fn load_from_file_returns_io_error_for_missing_file() {
        let result = SimpleDictionary::load_from_file(std::path::Path::new(
            "/tmp/myme_test_nonexistent_file_xxxxxxx.dict",
        ));
        assert!(
            matches!(result, Err(DictionaryError::Io(_))),
            "expected DictionaryError::Io for a missing file"
        );
    }

    // ------------------------------------------------------------------
    // 15. common_prefix_search returns all prefix matches
    // ------------------------------------------------------------------
    #[test]
    fn common_prefix_search_multiple_matches() {
        let text = "き /木/\nきょ /虚/\nきょう /今日/京/\n";
        let dict = SimpleDictionary::load_from_skk_text(text).unwrap();
        let results = dict.common_prefix_search("きょうは");
        let readings: Vec<&str> = results.iter().map(|r| r.0.as_str()).collect();
        // Longest first
        assert_eq!(readings, vec!["きょう", "きょ", "き"]);
    }

    // ------------------------------------------------------------------
    // 16. common_prefix_search returns empty for no match
    // ------------------------------------------------------------------
    #[test]
    fn common_prefix_search_no_match() {
        let dict = SimpleDictionary::load_from_skk_text(SAMPLE_SKK).unwrap();
        let results = dict.common_prefix_search("zzz");
        assert!(results.is_empty());
    }

    // ------------------------------------------------------------------
    // 17. common_prefix_search single-char match
    // ------------------------------------------------------------------
    #[test]
    fn common_prefix_search_single_char() {
        let text = "あ /亜/\n";
        let dict = SimpleDictionary::load_from_skk_text(text).unwrap();
        let results = dict.common_prefix_search("あいうえお");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "あ");
        assert_eq!(results[0].1[0].surface, "亜");
    }

    // ------------------------------------------------------------------
    // 18. common_prefix_search candidates are sorted by score
    // ------------------------------------------------------------------
    #[test]
    fn common_prefix_search_candidates_sorted() {
        let text = "きょう /今日/京/教/\n";
        let dict = SimpleDictionary::load_from_skk_text(text).unwrap();
        let results = dict.common_prefix_search("きょうは");
        assert_eq!(results.len(), 1);
        let surfaces: Vec<&str> = results[0].1.iter().map(|c| c.surface.as_str()).collect();
        assert_eq!(surfaces, vec!["今日", "京", "教"]);
    }

    // ------------------------------------------------------------------
    // 19. Frequency annotation boosts score
    // ------------------------------------------------------------------
    #[test]
    fn frequency_annotation_boosts_score() {
        // Without freq, second candidate gets score 10, first gets 20.
        // With freq=1024 on second: log2(1024)=10, bonus=30.
        // Second candidate: position_score=10 + freq_bonus=30 = 40 (beats first's 20).
        let text = "てすと /テスト/試験;freq=1024/\n";
        let dict = SimpleDictionary::load_from_skk_text(text).unwrap();
        let results = dict.lookup("てすと");
        // 試験 (score 40) should now rank before テスト (score 20).
        assert_eq!(results[0].surface, "試験");
        assert_eq!(results[1].surface, "テスト");
    }

    // ------------------------------------------------------------------
    // 20. Missing frequency uses fallback (position-only scoring)
    // ------------------------------------------------------------------
    #[test]
    fn missing_frequency_uses_fallback() {
        let text = "てすと /テスト/試験/\n";
        let dict = SimpleDictionary::load_from_skk_text(text).unwrap();
        let results = dict.lookup("てすと");
        // First-listed gets highest position score.
        assert_eq!(results[0].surface, "テスト");
        assert_eq!(results[1].surface, "試験");
    }

    // ------------------------------------------------------------------
    // 21. Frequency annotation with zero freq
    // ------------------------------------------------------------------
    #[test]
    fn frequency_zero_gives_no_bonus() {
        let text = "あ /亜;freq=0/阿/\n";
        let dict = SimpleDictionary::load_from_skk_text(text).unwrap();
        let results = dict.lookup("あ");
        // 亜 has position score 20+0=20, 阿 has position score 10.
        assert_eq!(results[0].surface, "亜");
    }
}
