//! Candidate list construction, scoring, and ranking.
//!
//! ## Responsibilities
//!
//! - Receive raw [`crate::dictionary::Entry`] items from the dictionary layer
//!   and produce an ordered [`CandidateList`] suitable for display in the IME
//!   candidate window.
//! - Apply a cost model that blends:
//!     1. Dictionary entry cost (unigram frequency / language model score).
//!     2. User-history bias (entries confirmed recently rank higher).
//!     3. Contextual signals passed in from the active [`crate::session`].
//! - Persist and load the user-history store (a small bincode file in the
//!   application support directory).
//!
//! ## Non-goals
//!
//! This module does **not** read the dictionary directly; it receives pre-
//! fetched entries. It does not manage the input buffer; see
//! [`crate::session`].

use std::cmp::Ordering;

/// The origin of a conversion candidate, used to apply scoring biases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateSource {
    /// Entry came from the bundled system dictionary.
    System,
    /// Entry was added or edited by the user in their personal dictionary.
    User,
    /// Entry was promoted by the learning-history store (recently confirmed).
    Learning,
}

/// A single conversion candidate ready for display in the candidate window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The surface form shown to the user (e.g. `"変換"`).
    pub surface: String,
    /// The kana reading that produced this candidate (e.g. `"へんかん"`).
    pub reading: String,
    /// Higher scores rank before lower scores in the candidate list.
    pub score: u32,
    /// Where this candidate originated; drives scoring biases.
    pub source: CandidateSource,
}

impl Candidate {
    /// Construct a new [`Candidate`].
    pub fn new(
        surface: impl Into<String>,
        reading: impl Into<String>,
        score: u32,
        source: CandidateSource,
    ) -> Self {
        Self {
            surface: surface.into(),
            reading: reading.into(),
            score,
            source,
        }
    }
}

// ---------------------------------------------------------------------------
// Ordering — sorted by score descending so that the best candidate comes first
// when a collection is sorted with the standard ascending sort.
//
// Tie-break on surface (lexicographic, ascending) to guarantee a stable,
// deterministic total order across all fields that are reflected in Eq.
// ---------------------------------------------------------------------------

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        // Primary: higher score first (reversed comparison).
        other
            .score
            .cmp(&self.score)
            // Secondary: lexicographic surface, ascending.
            .then_with(|| self.surface.cmp(&other.surface))
            // Tertiary: reading, ascending.
            .then_with(|| self.reading.cmp(&other.reading))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(surface: &str, score: u32) -> Candidate {
        Candidate::new(surface, "てすと", score, CandidateSource::System)
    }

    #[test]
    fn higher_score_sorts_first() {
        let a = make("低スコア", 10);
        let b = make("高スコア", 100);
        assert!(b < a, "higher score should sort before lower score");
    }

    #[test]
    fn equal_score_sorts_by_surface() {
        let a = make("aba", 50);
        let b = make("xyz", 50);
        // 'a' < 'x', so a should sort before b (a < b in ascending order)
        assert!(a < b);
    }

    #[test]
    fn new_sets_all_fields() {
        let c = Candidate::new("変換", "へんかん", 42, CandidateSource::User);
        assert_eq!(c.surface, "変換");
        assert_eq!(c.reading, "へんかん");
        assert_eq!(c.score, 42);
        assert_eq!(c.source, CandidateSource::User);
    }

    #[test]
    fn sort_vec_descending_score() {
        let mut candidates = vec![make("C", 5), make("A", 30), make("B", 20)];
        candidates.sort();
        let surfaces: Vec<&str> = candidates.iter().map(|c| c.surface.as_str()).collect();
        assert_eq!(surfaces, vec!["A", "B", "C"]);
    }
}
