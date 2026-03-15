//! Greedy longest-match segmentation of kana input.
//!
//! Splits a kana reading into conversion segments using dictionary matches.
//! Each segment carries its reading, the list of dictionary candidates, and
//! the currently selected candidate index.

use crate::candidate::Candidate;
use crate::dictionary::DictionaryLookup;

// ---------------------------------------------------------------------------
// Segment
// ---------------------------------------------------------------------------

/// A single conversion segment produced by the segmenter.
#[derive(Debug, Clone)]
pub struct Segment {
    /// The kana reading for this segment (e.g. `"きょう"`).
    pub reading: String,
    /// Candidates from the dictionary for this reading, sorted best-first.
    /// For unknown segments this contains a single candidate whose surface
    /// equals the reading.
    pub candidates: Vec<Candidate>,
    /// Zero-based index of the currently selected candidate.
    pub selected: usize,
}

impl Segment {
    /// Returns the surface form of the currently selected candidate.
    pub fn selected_surface(&self) -> &str {
        self.candidates
            .get(self.selected)
            .map(|c| c.surface.as_str())
            .unwrap_or(&self.reading)
    }
}

// ---------------------------------------------------------------------------
// Segmentation
// ---------------------------------------------------------------------------

/// Segments a kana reading into conversion units using greedy longest-match.
///
/// At each position the algorithm finds the longest prefix with a dictionary
/// hit.  If no match is found, a single character is emitted as an "unknown"
/// segment whose sole candidate is the character itself.
///
/// Returns an empty `Vec` for an empty input string.
pub fn segment(reading: &str, dict: &dyn DictionaryLookup) -> Vec<Segment> {
    if reading.is_empty() {
        return Vec::new();
    }

    let chars: Vec<char> = reading.chars().collect();
    let mut segments = Vec::new();
    let mut pos = 0;

    while pos < chars.len() {
        // Gather the remaining text from this position.
        let remaining: String = chars[pos..].iter().collect();

        // Find all prefix matches; they come back longest-first.
        let prefix_matches = dict.common_prefix_search(&remaining);

        if let Some((matched_reading, candidates)) = prefix_matches.into_iter().next() {
            // Use the longest match.
            let matched_chars = matched_reading.chars().count();
            segments.push(Segment {
                reading: matched_reading,
                candidates,
                selected: 0,
            });
            pos += matched_chars;
        } else {
            // No dictionary match — emit the single character as-is.
            let ch: String = chars[pos].to_string();
            let candidate = Candidate::new(
                ch.clone(),
                ch.clone(),
                0,
                crate::candidate::CandidateSource::System,
            );
            segments.push(Segment {
                reading: ch,
                candidates: vec![candidate],
                selected: 0,
            });
            pos += 1;
        }
    }

    segments
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::{Candidate, CandidateSource};
    use crate::dictionary::DictionaryLookup;

    /// Mock dictionary with a few entries for testing segmentation.
    struct SegmentDict;

    impl DictionaryLookup for SegmentDict {
        fn lookup(&self, reading: &str) -> Vec<Candidate> {
            match reading {
                "きょう" => vec![
                    Candidate::new("今日", "きょう", 30, CandidateSource::System),
                    Candidate::new("京", "きょう", 20, CandidateSource::System),
                ],
                "き" => vec![
                    Candidate::new("木", "き", 10, CandidateSource::System),
                ],
                "は" => vec![
                    Candidate::new("は", "は", 10, CandidateSource::System),
                ],
                "いい" => vec![
                    Candidate::new("いい", "いい", 10, CandidateSource::System),
                    Candidate::new("良い", "いい", 5, CandidateSource::System),
                ],
                "てんき" => vec![
                    Candidate::new("天気", "てんき", 20, CandidateSource::System),
                ],
                "です" => vec![
                    Candidate::new("です", "です", 10, CandidateSource::System),
                ],
                "てん" => vec![
                    Candidate::new("天", "てん", 10, CandidateSource::System),
                ],
                _ => vec![],
            }
        }
    }

    #[test]
    fn single_word_segment() {
        let dict = SegmentDict;
        let segs = segment("きょう", &dict);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].reading, "きょう");
        assert_eq!(segs[0].candidates[0].surface, "今日");
    }

    #[test]
    fn two_word_split() {
        let dict = SegmentDict;
        let segs = segment("きょうは", &dict);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].reading, "きょう");
        assert_eq!(segs[1].reading, "は");
    }

    #[test]
    fn unknown_chars_become_single_segments() {
        let dict = SegmentDict;
        let segs = segment("xyz", &dict);
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].reading, "x");
        assert_eq!(segs[0].candidates[0].surface, "x");
        assert_eq!(segs[1].reading, "y");
        assert_eq!(segs[2].reading, "z");
    }

    #[test]
    fn empty_input_returns_empty() {
        let dict = SegmentDict;
        let segs = segment("", &dict);
        assert!(segs.is_empty());
    }

    #[test]
    fn greedy_prefers_longer_match() {
        // "きょう" should be matched as one segment, not "き" + "ょう"
        let dict = SegmentDict;
        let segs = segment("きょう", &dict);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].reading, "きょう");
    }

    #[test]
    fn multi_segment_sentence() {
        let dict = SegmentDict;
        let segs = segment("きょうはいいてんきです", &dict);
        let readings: Vec<&str> = segs.iter().map(|s| s.reading.as_str()).collect();
        assert_eq!(readings, vec!["きょう", "は", "いい", "てんき", "です"]);
    }

    #[test]
    fn selected_surface_returns_first_candidate() {
        let dict = SegmentDict;
        let segs = segment("きょう", &dict);
        assert_eq!(segs[0].selected_surface(), "今日");
    }

    #[test]
    fn mixed_known_unknown() {
        let dict = SegmentDict;
        let segs = segment("きょうxは", &dict);
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0].reading, "きょう");
        assert_eq!(segs[1].reading, "x");
        assert_eq!(segs[2].reading, "は");
    }
}
