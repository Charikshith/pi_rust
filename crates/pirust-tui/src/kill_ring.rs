//! Port of `packages/tui/src/kill-ring.ts` — an Emacs-style kill/yank ring
//! buffer. See `docs/analysis/05-tui.md` §9. Internal helper — not part of
//! `index.ts`'s public surface (§2 lists no `kill-ring` exports); `editor.rs`
//! (Wave 6) will use this directly via `pirust_tui::kill_ring`. Given its
//! triviality (46 TS lines), plain unit tests are sufficient — no oracle
//! fixture, matching the proportionality precedent set by feat-005's
//! `auth_guidance.rs`.

/// Ring buffer for Emacs-style kill/yank operations (`KillRing`, kill-ring.ts:8).
/// Tracks killed (deleted) text entries; consecutive kills can accumulate into
/// a single entry.
#[derive(Debug, Default, Clone)]
pub struct KillRing {
    ring: Vec<String>,
}

impl KillRing {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add text to the kill ring (`push`, kill-ring.ts:19).
    ///
    /// - `prepend`: when accumulating, prepend (backward deletion) or append
    ///   (forward deletion).
    /// - `accumulate`: merge with the most recent entry instead of creating a
    ///   new one.
    pub fn push(&mut self, text: &str, prepend: bool, accumulate: bool) {
        if text.is_empty() {
            return;
        }
        if accumulate {
            if let Some(last) = self.ring.pop() {
                self.ring.push(if prepend {
                    format!("{text}{last}")
                } else {
                    format!("{last}{text}")
                });
                return;
            }
        }
        self.ring.push(text.to_string());
    }

    /// Get the most recent entry without modifying the ring (`peek`, kill-ring.ts:31).
    pub fn peek(&self) -> Option<&str> {
        self.ring.last().map(String::as_str)
    }

    /// Move the last entry to the front, for yank-pop cycling (`rotate`, kill-ring.ts:36).
    pub fn rotate(&mut self) {
        if self.ring.len() > 1 {
            if let Some(last) = self.ring.pop() {
                self.ring.insert(0, last);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.ring.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_creates_new_entry_by_default() {
        let mut ring = KillRing::new();
        ring.push("a", false, false);
        ring.push("b", false, false);
        assert_eq!(ring.len(), 2);
        assert_eq!(ring.peek(), Some("b"));
    }

    #[test]
    fn empty_text_is_a_no_op() {
        let mut ring = KillRing::new();
        ring.push("", false, false);
        assert!(ring.is_empty());
    }

    #[test]
    fn accumulate_appends_when_not_prepend() {
        let mut ring = KillRing::new();
        ring.push("foo", false, false);
        ring.push("bar", false, true);
        assert_eq!(ring.len(), 1);
        assert_eq!(ring.peek(), Some("foobar"));
    }

    #[test]
    fn accumulate_prepends_for_backward_deletion() {
        let mut ring = KillRing::new();
        ring.push("foo", true, false);
        ring.push("bar", true, true);
        assert_eq!(ring.peek(), Some("barfoo"));
    }

    #[test]
    fn accumulate_on_empty_ring_just_pushes() {
        let mut ring = KillRing::new();
        ring.push("first", false, true);
        assert_eq!(ring.len(), 1);
        assert_eq!(ring.peek(), Some("first"));
    }

    #[test]
    fn rotate_is_a_no_op_below_two_entries() {
        let mut ring = KillRing::new();
        ring.push("only", false, false);
        ring.rotate();
        assert_eq!(ring.peek(), Some("only"));
    }

    #[test]
    fn rotate_moves_last_entry_to_front_for_yank_pop() {
        let mut ring = KillRing::new();
        ring.push("a", false, false);
        ring.push("b", false, false);
        ring.push("c", false, false);
        ring.rotate();
        // ring is now [c, a, b]; peek (last) is "b".
        assert_eq!(ring.peek(), Some("b"));
        ring.rotate();
        // ring is now [b, c, a]; peek (last) is "a".
        assert_eq!(ring.peek(), Some("a"));
    }
}
