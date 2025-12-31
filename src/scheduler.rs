//! Time Scheduler
//!
//! Manages priority queues for time-based and beat-based waits.
//! Processes timeslices deterministically.

use crate::pq::MinPq;
use crate::tempo::TempoMap;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::task::Waker;
use std::time::Instant;

/// Scheduler execution mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchedulerMode {
    Realtime,
    Offline,
}

/// Shared wait state between future and scheduler.
#[derive(Clone)]
pub struct WaitState {
    inner: Rc<RefCell<WaitStateInner>>,
}

struct WaitStateInner {
    done: bool,
    cancelled: bool,
    waker: Option<Waker>,
}

impl WaitState {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(WaitStateInner {
                done: false,
                cancelled: false,
                waker: None,
            })),
        }
    }

    pub fn set_waker(&self, w: &Waker) {
        self.inner.borrow_mut().waker = Some(w.clone());
    }

    pub fn complete_ok(&self) {
        let mut s = self.inner.borrow_mut();
        if s.done || s.cancelled {
            return;
        }
        s.done = true;
        if let Some(w) = s.waker.take() {
            w.wake();
        }
    }

    pub fn complete_cancelled(&self) {
        let mut s = self.inner.borrow_mut();
        if s.done || s.cancelled {
            return;
        }
        s.cancelled = true;
        if let Some(w) = s.waker.take() {
            w.wake();
        }
    }

    pub fn is_done(&self) -> bool {
        self.inner.borrow().done
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.borrow().cancelled
    }

    pub fn is_complete(&self) -> bool {
        let s = self.inner.borrow();
        s.done || s.cancelled
    }
}

impl Default for WaitState {
    fn default() -> Self {
        Self::new()
    }
}

/// Metadata for a time-based wait.
/// The context reference is not stored here - the future handles context updates.
#[derive(Clone)]
pub struct TimeWaitMeta {
    pub seq: u64,
    pub target_time: f64,
    pub state: WaitState,
    pub waiter_id: u64,
}

/// Metadata for a beat-based wait.
#[derive(Clone)]
pub struct BeatWaitMeta {
    pub seq: u64,
    pub tempo: Rc<RefCell<TempoMap>>,
    pub target_beat: f64,
    pub state: WaitState,
    pub waiter_id: u64,
    pub tempo_id: u64,
}

/// Metadata for tempo head tracking in the scheduler.
#[derive(Clone)]
pub struct TempoHeadMeta {
    pub tempo_id: u64,
}

/// Metadata for a frame wait.
#[derive(Clone)]
pub struct FrameWaitMeta {
    pub seq: u64,
    pub state: WaitState,
    pub waiter_id: u64,
}

/// Type of waiter (for cancellation cleanup).
#[derive(Clone, Copy, Debug)]
pub enum WaiterKind {
    Time,
    Beat { tempo_id: u64 },
    Frame,
}

/// The main time scheduler.
pub struct TimeScheduler {
    pub mode: SchedulerMode,

    /// Deterministic sequence counter.
    seq: u64,

    /// Global alignment: tracks the most recent descendent time.
    pub most_recent_desc_time: f64,

    /// Time dilation anchors (realtime only).
    rate: f64,
    wall_anchor: Instant,
    logical_anchor: f64,

    /// Offline clock.
    pub offline_now: f64,

    /// Priority queue for time-based waits.
    time_pq: MinPq<TimeWaitMeta>,

    /// Map of tempo_id -> PQ for beat waits.
    beat_pqs: HashMap<u64, MinPq<BeatWaitMeta>>,

    /// PQ tracking the next due time for each tempo's head beat wait.
    tempo_head_pq: MinPq<TempoHeadMeta>,

    /// Frame waiters (resolved on frame tick, not by PQ).
    frame_waiters: HashMap<u64, FrameWaitMeta>,
}

impl TimeScheduler {
    /// Create a new scheduler.
    pub fn new(mode: SchedulerMode) -> Self {
        Self {
            mode,
            seq: 0,
            most_recent_desc_time: 0.0,
            rate: 1.0,
            wall_anchor: Instant::now(),
            logical_anchor: 0.0,
            offline_now: 0.0,
            time_pq: MinPq::new(),
            beat_pqs: HashMap::new(),
            tempo_head_pq: MinPq::new(),
            frame_waiters: HashMap::new(),
        }
    }

    /// Allocate a deterministic sequence number.
    pub fn alloc_seq(&mut self) -> u64 {
        let s = self.seq;
        self.seq += 1;
        s
    }

    /// Get the current logical time.
    pub fn now(&self) -> f64 {
        match self.mode {
            SchedulerMode::Offline => self.offline_now,
            SchedulerMode::Realtime => {
                let wall_now = Instant::now();
                let dt = wall_now.duration_since(self.wall_anchor).as_secs_f64();
                self.logical_anchor + dt * self.rate
            }
        }
    }

    /// Set the time dilation rate (realtime only).
    pub fn set_rate(&mut self, rate: f64) {
        if self.mode == SchedulerMode::Offline {
            return;
        }
        let r = if rate.is_finite() && rate > 0.0 {
            rate
        } else {
            1.0
        };
        let l = self.now();
        self.logical_anchor = l;
        self.wall_anchor = Instant::now();
        self.rate = r;
    }

    /// Get the current rate.
    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// Peek the next event time (minimum of time PQ and tempo head PQ).
    pub fn peek_next_event_time(&mut self) -> Option<f64> {
        let t1 = self.time_pq.peek_deadline().unwrap_or(f64::INFINITY);
        let t2 = self.tempo_head_pq.peek_deadline().unwrap_or(f64::INFINITY);
        let next = t1.min(t2);
        if next.is_finite() {
            Some(next)
        } else {
            None
        }
    }

    /// Add a time-based wait.
    pub fn add_time_wait(&mut self, meta: TimeWaitMeta) {
        let id = meta.waiter_id;
        let deadline = meta.target_time;
        let seq = meta.seq;
        self.time_pq.add(id, deadline, seq, meta);
    }

    /// Add a beat-based wait.
    pub fn add_beat_wait(&mut self, meta: BeatWaitMeta) {
        let tempo_id = meta.tempo_id;
        let id = meta.waiter_id;
        let target_beat = meta.target_beat;
        let seq = meta.seq;

        let pq = self.beat_pqs.entry(tempo_id).or_insert_with(MinPq::new);
        pq.add(id, target_beat, seq, meta);

        self.refresh_tempo_head(tempo_id);
    }

    /// Add a frame wait.
    pub fn add_frame_wait(&mut self, meta: FrameWaitMeta) {
        self.frame_waiters.insert(meta.waiter_id, meta);
    }

    /// Refresh the tempo head PQ entry for a given tempo.
    fn refresh_tempo_head(&mut self, tempo_id: u64) {
        let head = self
            .beat_pqs
            .get_mut(&tempo_id)
            .and_then(|pq| pq.peek_meta());

        if let Some((_, _, _, meta)) = head {
            let due_time = meta.tempo.borrow().time_at_beats(meta.target_beat);
            if due_time.is_finite() {
                if !self.tempo_head_pq.adjust_deadline(tempo_id, due_time) {
                    self.tempo_head_pq
                        .add(tempo_id, due_time, 0, TempoHeadMeta { tempo_id });
                }
            }
        } else {
            self.tempo_head_pq.remove(tempo_id);
        }
    }

    /// Called after tempo changes to retime beat waits.
    pub fn on_tempo_changed(&mut self, tempo_id: u64) {
        self.refresh_tempo_head(tempo_id);
    }

    /// Cancel a waiter and wake it as cancelled.
    pub fn cancel_waiter(&mut self, id: u64, kind: WaiterKind) {
        match kind {
            WaiterKind::Time => {
                if let Some(meta) = self.time_pq.remove(id) {
                    meta.state.complete_cancelled();
                }
            }
            WaiterKind::Beat { tempo_id } => {
                if let Some(pq) = self.beat_pqs.get_mut(&tempo_id) {
                    if let Some(meta) = pq.remove(id) {
                        meta.state.complete_cancelled();
                    }
                }
                self.refresh_tempo_head(tempo_id);
            }
            WaiterKind::Frame => {
                if let Some(meta) = self.frame_waiters.remove(&id) {
                    meta.state.complete_cancelled();
                }
            }
        }
    }

    /// Process exactly one timeslice at the given time.
    /// Returns the waiters that were resolved (for debugging/testing).
    pub fn process_one_timeslice(&mut self, _t_slice: f64) -> Vec<u64> {
        let t_time = self.time_pq.peek_deadline().unwrap_or(f64::INFINITY);
        let t_beat = self.tempo_head_pq.peek_deadline().unwrap_or(f64::INFINITY);

        if t_time <= t_beat {
            self.process_time_waiters_at(t_time)
        } else {
            self.process_beat_waiters_for_earliest_tempo()
        }
    }

    /// Process all time waiters at exact deadline t.
    fn process_time_waiters_at(&mut self, t: f64) -> Vec<u64> {
        let mut batch: Vec<TimeWaitMeta> = Vec::new();

        // Pop all waiters with exact deadline == t
        while let Some(dl) = self.time_pq.peek_deadline() {
            if dl.to_bits() != t.to_bits() {
                break;
            }
            let (_, _, _, meta) = self.time_pq.pop().unwrap();
            batch.push(meta);
        }

        // Sort by sequence for deterministic ordering
        batch.sort_by_key(|m| m.seq);

        let mut resolved_ids = Vec::new();

        for w in batch {
            w.state.complete_ok();
            resolved_ids.push(w.waiter_id);
        }

        resolved_ids
    }

    /// Process beat waiters for the earliest tempo head.
    fn process_beat_waiters_for_earliest_tempo(&mut self) -> Vec<u64> {
        let Some((tempo_id, _, _, _)) = self.tempo_head_pq.pop() else {
            return Vec::new();
        };

        let Some(beat_pq) = self.beat_pqs.get_mut(&tempo_id) else {
            return Vec::new();
        };

        let Some((_, _, _, head_meta)) = beat_pq.peek_meta() else {
            self.refresh_tempo_head(tempo_id);
            return Vec::new();
        };

        let due_time = head_meta.tempo.borrow().time_at_beats(head_meta.target_beat);
        if !due_time.is_finite() {
            let _ = beat_pq.pop();
            self.refresh_tempo_head(tempo_id);
            return Vec::new();
        }

        let target_beat = head_meta.target_beat;

        // Pop all waiters with same target beat
        let mut batch: Vec<BeatWaitMeta> = Vec::new();
        while let Some((_, _, _, meta)) = beat_pq.peek_meta() {
            if meta.target_beat.to_bits() != target_beat.to_bits() {
                break;
            }
            let (_, _, _, meta) = beat_pq.pop().unwrap();
            batch.push(meta);
        }

        batch.sort_by_key(|m| m.seq);

        let mut resolved_ids = Vec::new();

        for w in batch {
            w.state.complete_ok();
            resolved_ids.push(w.waiter_id);
        }

        self.refresh_tempo_head(tempo_id);
        resolved_ids
    }

    /// Resolve all frame waiters at the given time.
    pub fn resolve_all_frame_waiters(&mut self, _t: f64) -> Vec<u64> {
        if self.frame_waiters.is_empty() {
            return Vec::new();
        }

        let mut entries: Vec<_> = self.frame_waiters.drain().collect();
        entries.sort_by_key(|(_, m)| m.seq);

        let mut resolved_ids = Vec::new();

        for (id, meta) in entries {
            meta.state.complete_ok();
            resolved_ids.push(id);
        }

        resolved_ids
    }

    /// Check if there are any pending waits.
    pub fn has_pending_waits(&self) -> bool {
        !self.time_pq.is_empty()
            || self.beat_pqs.values().any(|pq| !pq.is_empty())
            || !self.frame_waiters.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scheduler_creation() {
        let sched = TimeScheduler::new(SchedulerMode::Offline);
        assert_eq!(sched.mode, SchedulerMode::Offline);
        assert!((sched.now() - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_alloc_seq() {
        let mut sched = TimeScheduler::new(SchedulerMode::Offline);
        assert_eq!(sched.alloc_seq(), 0);
        assert_eq!(sched.alloc_seq(), 1);
        assert_eq!(sched.alloc_seq(), 2);
    }

    #[test]
    fn test_peek_next_event_time_empty() {
        let mut sched = TimeScheduler::new(SchedulerMode::Offline);
        assert_eq!(sched.peek_next_event_time(), None);
    }
}
