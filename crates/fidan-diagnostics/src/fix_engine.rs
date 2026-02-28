//! Fix engine — Phase 4.
//!
//! Provides static helper methods that enrich diagnostics with "did you mean"
//! suggestions and structured fix edits.  Call-sites (typechecker, parser)
//! instantiate `FixEngine` once and call its methods when emitting diagnostics.

/// Threshold below which a Jaro-Winkler score is not considered a useful match.
const SIMILARITY_THRESHOLD: f64 = 0.75;

pub struct FixEngine;

impl FixEngine {
    pub fn new() -> Self {
        FixEngine
    }

    /// Find the most similar name in `candidates` to `name`.
    ///
    /// Returns the **candidate name** (not a formatted string) when a match
    /// exceeds `SIMILARITY_THRESHOLD` (Jaro-Winkler).  The caller owns the
    /// message and can build a `Suggestion::fix(...)` with a `SourceEdit`.
    ///
    /// Returns `None` when no candidate is close enough to be useful.
    ///
    /// # Example
    /// ```
    /// use fidan_diagnostics::FixEngine;
    /// let best = FixEngine::suggest_name("pritn", ["print", "println"].into_iter());
    /// assert_eq!(best, Some("print".to_string()));
    /// ```
    pub fn suggest_name<'a>(
        name: &str,
        candidates: impl Iterator<Item = &'a str>,
    ) -> Option<String> {
        let mut best_name: &str = "";
        let mut best_score: f64 = 0.0;

        for candidate in candidates {
            // Skip the name itself and internal error-recovery markers.
            if candidate == name || candidate.starts_with('<') {
                continue;
            }
            let score = strsim::jaro_winkler(name, candidate);
            if score > best_score {
                best_score = score;
                best_name = candidate;
            }
        }

        if best_score >= SIMILARITY_THRESHOLD {
            Some(best_name.to_string())
        } else {
            None
        }
    }
}

impl Default for FixEngine {
    fn default() -> Self {
        Self::new()
    }
}
