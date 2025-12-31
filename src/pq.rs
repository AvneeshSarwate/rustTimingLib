//! Priority Queue with lazy deletion
//!
//! Uses std::collections::BinaryHeap with a HashMap for O(1) lookup.
//! Supports remove() and adjust_deadline() via lazy deletion pattern.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

/// Key for the priority queue heap.
/// Uses (deadline, tie, id) for deterministic ordering.
#[derive(Clone, Debug)]
struct Key {
    deadline: f64,
    tie: u64,
    id: u64,
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.deadline.to_bits() == other.deadline.to_bits()
            && self.tie == other.tie
            && self.id == other.id
    }
}

impl Eq for Key {}

impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// BinaryHeap is a max-heap, so we reverse the ordering for min-heap behavior.
// Uses total_cmp for deterministic float ordering (handles -0, NaN, etc.)
impl Ord for Key {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.deadline.total_cmp(&other.deadline) {
            Ordering::Equal => match self.tie.cmp(&other.tie) {
                Ordering::Equal => self.id.cmp(&other.id),
                o => o,
            },
            o => o,
        }
        .reverse() // Reverse for min-heap behavior
    }
}

/// A min-priority queue with support for remove and adjust_deadline.
/// Uses lazy deletion pattern: the HashMap is the source of truth,
/// and stale heap entries are discarded on peek/pop.
pub struct MinPq<M> {
    heap: BinaryHeap<Key>,
    live: HashMap<u64, (f64, u64, M)>, // id -> (deadline, tie, metadata)
}

impl<M> Default for MinPq<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M> MinPq<M> {
    pub fn new() -> Self {
        Self {
            heap: BinaryHeap::new(),
            live: HashMap::new(),
        }
    }

    /// Add an item to the queue. Returns false if id already exists.
    pub fn add(&mut self, id: u64, deadline: f64, tie: u64, meta: M) -> bool {
        if self.live.contains_key(&id) {
            return false;
        }
        self.live.insert(id, (deadline, tie, meta));
        self.heap.push(Key { deadline, tie, id });
        true
    }

    /// Remove an item by id. Returns the metadata if found.
    pub fn remove(&mut self, id: u64) -> Option<M> {
        self.live.remove(&id).map(|(_, _, m)| m)
    }

    /// Adjust the deadline of an existing item. Returns false if id not found.
    /// Pushes a new key to the heap; the old one becomes stale.
    pub fn adjust_deadline(&mut self, id: u64, new_deadline: f64) -> bool {
        if let Some((deadline, tie, _)) = self.live.get_mut(&id) {
            *deadline = new_deadline;
            let t = *tie;
            self.heap.push(Key {
                deadline: new_deadline,
                tie: t,
                id,
            });
            true
        } else {
            false
        }
    }

    /// Peek the minimum deadline without removing. Returns None if empty.
    pub fn peek_deadline(&mut self) -> Option<f64> {
        self.clean_top();
        self.heap.peek().map(|k| k.deadline)
    }

    /// Pop the minimum item. Returns (id, deadline, tie, metadata).
    pub fn pop(&mut self) -> Option<(u64, f64, u64, M)> {
        loop {
            let k = self.heap.pop()?;
            let Some((dl, tie, _)) = self.live.get(&k.id) else {
                continue; // stale entry
            };
            if dl.to_bits() != k.deadline.to_bits() || *tie != k.tie {
                continue; // stale entry (deadline was adjusted)
            }
            let (dl, tie, meta) = self.live.remove(&k.id).unwrap();
            return Some((k.id, dl, tie, meta));
        }
    }

    /// Remove stale entries from the top of the heap.
    fn clean_top(&mut self) {
        while let Some(k) = self.heap.peek() {
            let ok = match self.live.get(&k.id) {
                Some((dl, tie, _)) => dl.to_bits() == k.deadline.to_bits() && *tie == k.tie,
                None => false,
            };
            if ok {
                break;
            }
            self.heap.pop();
        }
    }

    /// Check if the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    /// Get the number of live items.
    pub fn len(&self) -> usize {
        self.live.len()
    }
}

impl<M: Clone> MinPq<M> {
    /// Peek the metadata of the minimum item without removing.
    pub fn peek_meta(&mut self) -> Option<(u64, f64, u64, M)> {
        self.clean_top();
        let k = self.heap.peek()?;
        let (dl, tie, meta) = self.live.get(&k.id)?;
        Some((k.id, *dl, *tie, meta.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_operations() {
        let mut pq: MinPq<&str> = MinPq::new();

        assert!(pq.add(1, 0.5, 0, "first"));
        assert!(pq.add(2, 0.2, 1, "second"));
        assert!(pq.add(3, 0.8, 2, "third"));

        // Should return second (smallest deadline)
        let (id, dl, _, meta) = pq.pop().unwrap();
        assert_eq!(id, 2);
        assert!((dl - 0.2).abs() < 1e-10);
        assert_eq!(meta, "second");

        // Next should be first
        let (id, dl, _, meta) = pq.pop().unwrap();
        assert_eq!(id, 1);
        assert!((dl - 0.5).abs() < 1e-10);
        assert_eq!(meta, "first");
    }

    #[test]
    fn test_remove() {
        let mut pq: MinPq<i32> = MinPq::new();

        pq.add(1, 0.5, 0, 100);
        pq.add(2, 0.2, 1, 200);
        pq.add(3, 0.8, 2, 300);

        // Remove the minimum
        let removed = pq.remove(2);
        assert_eq!(removed, Some(200));

        // Now first should be the minimum
        let (id, _, _, _) = pq.pop().unwrap();
        assert_eq!(id, 1);
    }

    #[test]
    fn test_adjust_deadline() {
        let mut pq: MinPq<&str> = MinPq::new();

        pq.add(1, 0.5, 0, "a");
        pq.add(2, 0.2, 1, "b");

        // Adjust so 1 is now smaller
        assert!(pq.adjust_deadline(1, 0.1));

        let (id, dl, _, _) = pq.pop().unwrap();
        assert_eq!(id, 1);
        assert!((dl - 0.1).abs() < 1e-10);
    }

    #[test]
    fn test_tie_breaking() {
        let mut pq: MinPq<u32> = MinPq::new();

        // Same deadline, different tie values
        pq.add(1, 0.5, 2, 100);
        pq.add(2, 0.5, 0, 200);
        pq.add(3, 0.5, 1, 300);

        // Should come out in tie order: 0, 1, 2
        let (id, _, _, _) = pq.pop().unwrap();
        assert_eq!(id, 2);

        let (id, _, _, _) = pq.pop().unwrap();
        assert_eq!(id, 3);

        let (id, _, _, _) = pq.pop().unwrap();
        assert_eq!(id, 1);
    }
}
