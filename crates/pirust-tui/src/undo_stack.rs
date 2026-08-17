//! Port of `packages/tui/src/undo-stack.ts` — a generic undo stack with
//! clone-on-push semantics. See `docs/analysis/05-tui.md` §9. Internal helper
//! — not part of `index.ts`'s public surface (§2 lists no `undo-stack`
//! exports); `editor.rs` (Wave 6) will use this directly. No oracle fixture:
//! this is a generic container with no Pi-specific behavior to verify against
//! (`structuredClone` maps to `Clone::clone`, a stdlib-level equivalence, not
//! something Pi's runtime can diverge on).

/// Generic undo stack (`UndoStack<S>`, undo-stack.ts:7). Stores clones of
/// state snapshots; popped snapshots are returned directly (no re-cloning)
/// since they are already detached.
#[derive(Debug, Default, Clone)]
pub struct UndoStack<S: Clone> {
    stack: Vec<S>,
}

impl<S: Clone> UndoStack<S> {
    pub fn new() -> Self {
        Self { stack: Vec::new() }
    }

    /// Push a clone of the given state onto the stack (`push`, undo-stack.ts:11).
    pub fn push(&mut self, state: &S) {
        self.stack.push(state.clone());
    }

    /// Pop and return the most recent snapshot, or `None` if empty (`pop`, undo-stack.ts:16).
    pub fn pop(&mut self) -> Option<S> {
        self.stack.pop()
    }

    /// Remove all snapshots (`clear`, undo-stack.ts:21).
    pub fn clear(&mut self) {
        self.stack.clear();
    }

    pub fn len(&self) -> usize {
        self.stack.len()
    }

    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_then_pop_roundtrips() {
        let mut stack: UndoStack<String> = UndoStack::new();
        stack.push(&"hello".to_string());
        assert_eq!(stack.len(), 1);
        assert_eq!(stack.pop(), Some("hello".to_string()));
        assert!(stack.is_empty());
    }

    #[test]
    fn pop_on_empty_returns_none() {
        let mut stack: UndoStack<u32> = UndoStack::new();
        assert_eq!(stack.pop(), None);
    }

    #[test]
    fn push_clones_so_later_mutation_of_the_original_does_not_affect_the_snapshot() {
        let mut stack: UndoStack<Vec<i32>> = UndoStack::new();
        let mut state = vec![1, 2, 3];
        stack.push(&state);
        state.push(4);
        assert_eq!(stack.pop(), Some(vec![1, 2, 3]));
    }

    #[test]
    fn clear_empties_the_stack() {
        let mut stack: UndoStack<i32> = UndoStack::new();
        stack.push(&1);
        stack.push(&2);
        stack.clear();
        assert!(stack.is_empty());
    }

    #[test]
    fn pop_returns_most_recent_first() {
        let mut stack: UndoStack<i32> = UndoStack::new();
        stack.push(&1);
        stack.push(&2);
        assert_eq!(stack.pop(), Some(2));
        assert_eq!(stack.pop(), Some(1));
    }
}
