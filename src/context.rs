//! Time Context
//!
//! The main user-facing API for the timing engine.
//! Provides wait primitives, branching, and cancellation.

use crate::rng::DetRng;
use crate::scheduler::{
    BeatWaitMeta, FrameWaitMeta, TimeScheduler, TimeWaitMeta, WaitState, WaiterKind,
};
use crate::tempo::TempoMap;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::task::Waker;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};

static CTX_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_ctx_id() -> u64 {
    CTX_ID_COUNTER.fetch_add(1, Ordering::SeqCst)
}

/// Error returned when a wait is cancelled.
#[derive(Debug, Clone)]
pub struct WaitError {
    pub message: String,
}

impl std::fmt::Display for WaitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for WaitError {}

/// Options for branching contexts.
#[derive(Clone, Debug, Default)]
pub struct BranchOptions {
    /// Whether to share or clone the tempo map.
    pub tempo: TempoMode,
    /// Whether to fork or share the RNG.
    pub rng: RngMode,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TempoMode {
    #[default]
    Shared,
    Cloned,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RngMode {
    #[default]
    Forked,
    Shared,
}

/// Inner state of a context (behind Rc<RefCell<>>).
pub struct CtxInner {
    pub id: u64,
    pub debug_name: String,
    pub time: f64,
    pub start_time: f64,
    pub canceled: bool,

    /// Pending waiters for this context (for cancellation cleanup).
    pub pending_waiters: HashMap<u64, WaiterKind>,

    /// Child contexts.
    pub children: Vec<Weak<RefCell<CtxInner>>>,

    /// Parent context (for tree traversal).
    pub parent: Option<Weak<RefCell<CtxInner>>>,

    /// Root context.
    pub root: Option<Weak<RefCell<CtxInner>>>,

    /// Shared scheduler.
    pub scheduler: Rc<RefCell<TimeScheduler>>,

    /// Shared executor for spawning tasks.
    pub executor: Rc<crate::executor::Executor>,

    /// Tempo map (shared or cloned).
    pub tempo: Rc<RefCell<TempoMap>>,

    /// Deterministic RNG.
    pub rng: Rc<RefCell<DetRng>>,
    pub rng_seed: String,
    pub rng_fork_counter: u64,
}

/// A handle to a time context.
/// Uses Rc<RefCell<>> for interior mutability across .await points.
#[derive(Clone)]
pub struct Ctx(pub Rc<RefCell<CtxInner>>);

impl Ctx {
    /// Create a new root context.
    pub fn new_root(
        scheduler: Rc<RefCell<TimeScheduler>>,
        executor: Rc<crate::executor::Executor>,
        tempo: TempoMap,
        seed: &str,
    ) -> Self {
        let id = next_ctx_id();
        let inner = Rc::new(RefCell::new(CtxInner {
            id,
            debug_name: String::new(),
            time: 0.0,
            start_time: 0.0,
            canceled: false,
            pending_waiters: HashMap::new(),
            children: Vec::new(),
            parent: None,
            root: None,
            scheduler,
            executor,
            tempo: Rc::new(RefCell::new(tempo)),
            rng: Rc::new(RefCell::new(DetRng::new(seed))),
            rng_seed: seed.to_string(),
            rng_fork_counter: 0,
        }));

        // Set root to itself
        inner.borrow_mut().root = Some(Rc::downgrade(&inner));

        Ctx(inner)
    }

    /// Get the context ID.
    pub fn id(&self) -> u64 {
        self.0.borrow().id
    }

    /// Get the current logical time.
    pub fn time(&self) -> f64 {
        self.0.borrow().time
    }

    /// Set the current logical time.
    pub fn set_time(&self, t: f64) {
        self.0.borrow_mut().time = t;
    }

    /// Get the program time (time since context start).
    pub fn prog_time(&self) -> f64 {
        let inner = self.0.borrow();
        inner.time - inner.start_time
    }

    /// Get the current beat position.
    pub fn beats(&self) -> f64 {
        let inner = self.0.borrow();
        let time = inner.time;
        let result = inner.tempo.borrow().beats_at_time(time);
        result
    }

    /// Get the program beats (beats since context start).
    pub fn prog_beats(&self) -> f64 {
        let inner = self.0.borrow();
        let tempo = inner.tempo.borrow();
        tempo.beats_at_time(inner.time) - tempo.beats_at_time(inner.start_time)
    }

    /// Get the current BPM.
    pub fn bpm(&self) -> f64 {
        let inner = self.0.borrow();
        let time = inner.time;
        let result = inner.tempo.borrow().bpm_at_time(time);
        result
    }

    /// Set the BPM at the current root time.
    pub fn set_bpm(&self, bpm: f64) {
        let (tempo_id, scheduler) = {
            let inner = self.0.borrow();
            let root_time = inner.scheduler.borrow().most_recent_desc_time;

            inner.tempo.borrow_mut().set_bpm_at_time(bpm, root_time);

            let tempo_id = inner.tempo.borrow().id;
            (tempo_id, inner.scheduler.clone())
        };

        scheduler.borrow_mut().on_tempo_changed(tempo_id);
    }

    /// Ramp BPM to target over duration.
    pub fn ramp_bpm_to(&self, target_bpm: f64, dur_sec: f64) {
        let (tempo_id, scheduler) = {
            let inner = self.0.borrow();
            let root_time = inner.scheduler.borrow().most_recent_desc_time;

            inner
                .tempo
                .borrow_mut()
                .ramp_to_bpm_at_time(target_bpm, dur_sec, root_time);

            let tempo_id = inner.tempo.borrow().id;
            (tempo_id, inner.scheduler.clone())
        };

        scheduler.borrow_mut().on_tempo_changed(tempo_id);
    }

    /// Generate a deterministic random number in [0, 1).
    pub fn random(&self) -> f64 {
        self.0.borrow().rng.borrow_mut().random()
    }

    /// Check if the context is cancelled.
    pub fn is_canceled(&self) -> bool {
        self.0.borrow().canceled
    }

    /// Cancel this context and all children.
    pub fn cancel(&self) {
        let (waiters, children, scheduler) = {
            let mut inner = self.0.borrow_mut();
            if inner.canceled {
                return;
            }
            inner.canceled = true;

            let waiters: Vec<(u64, WaiterKind)> = inner
                .pending_waiters
                .iter()
                .map(|(id, kind)| (*id, *kind))
                .collect();
            inner.pending_waiters.clear();

            let children = inner.children.clone();
            (waiters, children, inner.scheduler.clone())
        };

        // Cancel waiters in scheduler
        {
            let mut sched = scheduler.borrow_mut();
            for (id, kind) in waiters {
                sched.cancel_waiter(id, kind);
            }
        }

        // Cascade to children
        for child_weak in children {
            if let Some(child_rc) = child_weak.upgrade() {
                Ctx(child_rc).cancel();
            }
        }
    }

    /// Update the most recent descendent time in the root.
    pub fn update_most_recent_desc_time(&self, t: f64) {
        let inner = self.0.borrow();
        let mut sched = inner.scheduler.borrow_mut();
        sched.most_recent_desc_time = sched.most_recent_desc_time.max(t);
    }

    /// Wait for the specified number of seconds.
    pub fn wait_sec(&self, sec: f64) -> TimeWaitFuture {
        let s = if sec.is_finite() && sec > 0.0 { sec } else { 0.0 };

        let (target, scheduler_rc) = {
            let inner = self.0.borrow();
            let base = inner.scheduler.borrow().most_recent_desc_time.max(inner.time);
            (base + s, inner.scheduler.clone())
        };

        TimeWaitFuture::new(self.clone(), scheduler_rc, target)
    }

    /// Wait for the specified number of beats.
    pub fn wait(&self, beats: f64) -> WaitFuture {
        let delta = if beats.is_finite() { beats } else { 0.0 };

        if delta <= 0.0 {
            // wait(0) is a sync point - schedule a time wait at base_time
            let (base_time, scheduler_rc) = {
                let inner = self.0.borrow();
                let base = inner.scheduler.borrow().most_recent_desc_time.max(inner.time);
                (base, inner.scheduler.clone())
            };
            return WaitFuture::Time(TimeWaitFuture::new(self.clone(), scheduler_rc, base_time));
        }

        // Normal beat wait
        let (tempo_rc, tempo_id, target_beat, scheduler_rc) = {
            let inner = self.0.borrow();
            let tempo_rc = inner.tempo.clone();
            let tempo_id = tempo_rc.borrow().id;

            let base_time = inner.scheduler.borrow().most_recent_desc_time.max(inner.time);
            let base_beats = tempo_rc.borrow().beats_at_time(base_time);

            (
                tempo_rc,
                tempo_id,
                base_beats + delta,
                inner.scheduler.clone(),
            )
        };

        WaitFuture::Beat(BeatWaitFuture::new(
            self.clone(),
            scheduler_rc,
            tempo_rc,
            tempo_id,
            target_beat,
        ))
    }

    /// Wait for the next frame (offline mode).
    pub fn wait_frame(&self) -> FrameWaitFuture {
        let scheduler_rc = self.0.borrow().scheduler.clone();
        FrameWaitFuture::new(self.clone(), scheduler_rc)
    }

    /// Create a child context.
    fn create_child(&self, initial_time: f64, opts: BranchOptions) -> Ctx {
        let child_id = next_ctx_id();

        let (tempo, rng, rng_seed, scheduler, executor) = {
            let mut inner = self.0.borrow_mut();

            let tempo = match opts.tempo {
                TempoMode::Shared => inner.tempo.clone(),
                TempoMode::Cloned => Rc::new(RefCell::new(inner.tempo.borrow().clone_with_new_id())),
            };

            let (rng, rng_seed) = match opts.rng {
                RngMode::Shared => (inner.rng.clone(), inner.rng_seed.clone()),
                RngMode::Forked => {
                    let fork_idx = inner.rng_fork_counter;
                    inner.rng_fork_counter += 1;
                    let child_seed = crate::rng::derive_seed(&inner.rng_seed, fork_idx);
                    (Rc::new(RefCell::new(DetRng::new(&child_seed))), child_seed)
                }
            };

            (tempo, rng, rng_seed, inner.scheduler.clone(), inner.executor.clone())
        };

        let child_inner = Rc::new(RefCell::new(CtxInner {
            id: child_id,
            debug_name: String::new(),
            time: initial_time,
            start_time: initial_time,
            canceled: false,
            pending_waiters: HashMap::new(),
            children: Vec::new(),
            parent: Some(Rc::downgrade(&self.0)),
            root: self.0.borrow().root.clone(),
            scheduler,
            executor,
            tempo,
            rng,
            rng_seed,
            rng_fork_counter: 0,
        }));

        // Add to parent's children
        self.0
            .borrow_mut()
            .children
            .push(Rc::downgrade(&child_inner));

        Ctx(child_inner)
    }

    /// Branch: spawn a child task that does NOT advance parent time on completion.
    /// The child is automatically spawned on the executor.
    /// Returns a handle with cancel() method.
    pub fn branch<F, Fut>(&self, f: F, opts: BranchOptions) -> BranchHandle
    where
        F: FnOnce(Ctx) -> Fut,
        Fut: Future<Output = ()> + 'static,
    {
        let initial_time = self.0.borrow().scheduler.borrow().most_recent_desc_time;
        let child = self.create_child(initial_time, opts);
        let child_clone = child.clone();
        let executor = self.0.borrow().executor.clone();

        // Create the future
        let fut = f(child);

        // Wrap in a cleanup future
        let cleanup_child = child_clone.clone();
        let parent_weak = Rc::downgrade(&self.0);
        let wrapped = async move {
            fut.await;
            // Remove from parent's children on completion
            if let Some(parent_rc) = parent_weak.upgrade() {
                let mut parent = parent_rc.borrow_mut();
                parent.children.retain(|c| {
                    c.upgrade()
                        .map(|rc| rc.borrow().id != cleanup_child.id())
                        .unwrap_or(false)
                });
            }
        };

        // Spawn the child on the executor so it runs in parallel
        executor.spawn(wrapped);

        BranchHandle {
            ctx: child_clone,
            future: None, // Future is already spawned
        }
    }

    /// BranchWait: spawn a child task that DOES update parent time on completion.
    /// The child is automatically spawned on the executor.
    pub fn branch_wait<F, Fut>(&self, f: F, opts: BranchOptions) -> SpawnedBranchWaitFuture
    where
        F: FnOnce(Ctx) -> Fut,
        Fut: Future<Output = ()> + 'static,
    {
        let parent_time = self.0.borrow().time;
        let child = self.create_child(parent_time, opts);
        let child_clone = child.clone();
        let executor = self.0.borrow().executor.clone();

        // Shared state for completion signaling
        let completion = Rc::new(RefCell::new(CompletionState {
            done: false,
            final_time: 0.0,
            waker: None,
        }));
        let completion_clone = completion.clone();

        // Create the child future
        let fut = f(child);

        // Wrap in a completion-signaling future
        let parent_weak = Rc::downgrade(&self.0);
        let cleanup_child = child_clone.clone();
        let wrapped = async move {
            fut.await;

            // Signal completion with final time
            let final_time = cleanup_child.time();
            {
                let mut state = completion_clone.borrow_mut();
                state.done = true;
                state.final_time = final_time;
                if let Some(waker) = state.waker.take() {
                    waker.wake();
                }
            }

            // Remove from parent's children
            if let Some(parent_rc) = parent_weak.upgrade() {
                let child_id = cleanup_child.id();
                let mut parent = parent_rc.borrow_mut();
                parent.children.retain(|c| {
                    c.upgrade()
                        .map(|rc| rc.borrow().id != child_id)
                        .unwrap_or(false)
                });
            }
        };

        // Spawn the child on the executor
        executor.spawn(wrapped);

        SpawnedBranchWaitFuture {
            parent: self.clone(),
            child: child_clone,
            completion,
        }
    }
}

/// Shared state for spawned branch_wait completion
struct CompletionState {
    done: bool,
    final_time: f64,
    waker: Option<Waker>,
}

/// Handle returned by branch() for cancellation.
pub struct BranchHandle {
    ctx: Ctx,
    future: Option<Pin<Box<dyn Future<Output = ()>>>>,
}

impl BranchHandle {
    /// Cancel the branch and all its children.
    pub fn cancel(&self) {
        self.ctx.cancel();
    }

    /// Get the future to spawn on the executor.
    pub fn take_future(&mut self) -> Option<Pin<Box<dyn Future<Output = ()>>>> {
        self.future.take()
    }
}

/// Future for spawned branch_wait that waits for the child to complete.
/// The child is already spawned on the executor when this future is created.
pub struct SpawnedBranchWaitFuture {
    parent: Ctx,
    child: Ctx,
    completion: Rc<RefCell<CompletionState>>,
}

impl Future for SpawnedBranchWaitFuture {
    type Output = Result<(), WaitError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        // Check if child is cancelled
        if this.child.is_canceled() {
            return Poll::Ready(Err(WaitError {
                message: "context cancelled".to_string(),
            }));
        }

        let mut state = this.completion.borrow_mut();

        if state.done {
            // Child completed - update parent time
            let parent_time = this.parent.time();
            this.parent.set_time(parent_time.max(state.final_time));
            Poll::Ready(Ok(()))
        } else {
            // Not done yet - register waker
            state.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

impl Unpin for SpawnedBranchWaitFuture {}

/// Enum to hold either time or beat wait future.
pub enum WaitFuture {
    Time(TimeWaitFuture),
    Beat(BeatWaitFuture),
}

impl Future for WaitFuture {
    type Output = Result<(), WaitError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match unsafe { self.get_unchecked_mut() } {
            WaitFuture::Time(f) => unsafe { Pin::new_unchecked(f) }.poll(cx),
            WaitFuture::Beat(f) => unsafe { Pin::new_unchecked(f) }.poll(cx),
        }
    }
}

/// Future for time-based waits.
pub struct TimeWaitFuture {
    ctx: Ctx,
    sched: Rc<RefCell<TimeScheduler>>,
    target_time: f64,
    state: WaitState,
    registered: Cell<bool>,
    waiter_id: Cell<u64>,
}

impl TimeWaitFuture {
    pub fn new(ctx: Ctx, sched: Rc<RefCell<TimeScheduler>>, target_time: f64) -> Self {
        Self {
            ctx,
            sched,
            target_time,
            state: WaitState::new(),
            registered: Cell::new(false),
            waiter_id: Cell::new(0),
        }
    }
}

impl Future for TimeWaitFuture {
    type Output = Result<(), WaitError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        // Check if already complete
        if this.state.is_cancelled() {
            return Poll::Ready(Err(WaitError {
                message: "cancelled".to_string(),
            }));
        }
        if this.state.is_done() {
            // Update context time
            let new_time = this.ctx.time().max(this.target_time);
            this.ctx.set_time(new_time);
            this.ctx.update_most_recent_desc_time(new_time);

            // Remove from pending waiters
            {
                let mut inner = this.ctx.0.borrow_mut();
                inner.pending_waiters.remove(&this.waiter_id.get());
            }

            return Poll::Ready(Ok(()));
        }

        this.state.set_waker(cx.waker());

        if !this.registered.get() {
            // Check cancellation
            if this.ctx.is_canceled() {
                this.state.complete_cancelled();
                return Poll::Ready(Err(WaitError {
                    message: "context cancelled".to_string(),
                }));
            }

            let mut sched = this.sched.borrow_mut();
            let seq = sched.alloc_seq();
            let waiter_id = seq;
            this.waiter_id.set(waiter_id);

            // Register with context's pending waiters
            {
                let mut ctx_inner = this.ctx.0.borrow_mut();
                ctx_inner
                    .pending_waiters
                    .insert(waiter_id, WaiterKind::Time);
            }

            let meta = TimeWaitMeta {
                seq,
                target_time: this.target_time.max(0.0),
                state: this.state.clone(),
                waiter_id,
            };

            sched.add_time_wait(meta);
            this.registered.set(true);
        }

        Poll::Pending
    }
}

/// Future for beat-based waits.
pub struct BeatWaitFuture {
    ctx: Ctx,
    sched: Rc<RefCell<TimeScheduler>>,
    tempo: Rc<RefCell<TempoMap>>,
    tempo_id: u64,
    target_beat: f64,
    state: WaitState,
    registered: Cell<bool>,
    waiter_id: Cell<u64>,
}

impl BeatWaitFuture {
    pub fn new(
        ctx: Ctx,
        sched: Rc<RefCell<TimeScheduler>>,
        tempo: Rc<RefCell<TempoMap>>,
        tempo_id: u64,
        target_beat: f64,
    ) -> Self {
        Self {
            ctx,
            sched,
            tempo,
            tempo_id,
            target_beat,
            state: WaitState::new(),
            registered: Cell::new(false),
            waiter_id: Cell::new(0),
        }
    }
}

impl Future for BeatWaitFuture {
    type Output = Result<(), WaitError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        if this.state.is_cancelled() {
            return Poll::Ready(Err(WaitError {
                message: "cancelled".to_string(),
            }));
        }
        if this.state.is_done() {
            // Calculate due time and update context
            let due_time = this.tempo.borrow().time_at_beats(this.target_beat);
            let new_time = this.ctx.time().max(due_time);
            this.ctx.set_time(new_time);
            this.ctx.update_most_recent_desc_time(new_time);

            // Remove from pending waiters
            {
                let mut inner = this.ctx.0.borrow_mut();
                inner.pending_waiters.remove(&this.waiter_id.get());
            }

            return Poll::Ready(Ok(()));
        }

        this.state.set_waker(cx.waker());

        if !this.registered.get() {
            if this.ctx.is_canceled() {
                this.state.complete_cancelled();
                return Poll::Ready(Err(WaitError {
                    message: "context cancelled".to_string(),
                }));
            }

            let mut sched = this.sched.borrow_mut();
            let seq = sched.alloc_seq();
            let waiter_id = seq;
            this.waiter_id.set(waiter_id);

            {
                let mut ctx_inner = this.ctx.0.borrow_mut();
                ctx_inner.pending_waiters.insert(
                    waiter_id,
                    WaiterKind::Beat {
                        tempo_id: this.tempo_id,
                    },
                );
            }

            let meta = BeatWaitMeta {
                seq,
                tempo: this.tempo.clone(),
                target_beat: this.target_beat,
                state: this.state.clone(),
                waiter_id,
                tempo_id: this.tempo_id,
            };

            sched.add_beat_wait(meta);
            this.registered.set(true);
        }

        Poll::Pending
    }
}

/// Future for frame waits.
pub struct FrameWaitFuture {
    ctx: Ctx,
    sched: Rc<RefCell<TimeScheduler>>,
    state: WaitState,
    registered: Cell<bool>,
    waiter_id: Cell<u64>,
}

impl FrameWaitFuture {
    pub fn new(ctx: Ctx, sched: Rc<RefCell<TimeScheduler>>) -> Self {
        Self {
            ctx,
            sched,
            state: WaitState::new(),
            registered: Cell::new(false),
            waiter_id: Cell::new(0),
        }
    }
}

impl Future for FrameWaitFuture {
    type Output = Result<(), WaitError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        if this.state.is_cancelled() {
            return Poll::Ready(Err(WaitError {
                message: "cancelled".to_string(),
            }));
        }
        if this.state.is_done() {
            let (t, root_time) = {
                let sched = this.sched.borrow();
                (sched.now(), sched.most_recent_desc_time)
            };
            let new_time = this.ctx.time().max(t).max(root_time);
            this.ctx.set_time(new_time);
            this.ctx.update_most_recent_desc_time(new_time);

            // Remove from pending waiters
            {
                let mut inner = this.ctx.0.borrow_mut();
                inner.pending_waiters.remove(&this.waiter_id.get());
            }

            return Poll::Ready(Ok(()));
        }

        this.state.set_waker(cx.waker());

        if !this.registered.get() {
            if this.ctx.is_canceled() {
                this.state.complete_cancelled();
                return Poll::Ready(Err(WaitError {
                    message: "context cancelled".to_string(),
                }));
            }

            let mut sched = this.sched.borrow_mut();
            let seq = sched.alloc_seq();
            let waiter_id = seq;
            this.waiter_id.set(waiter_id);

            {
                let mut ctx_inner = this.ctx.0.borrow_mut();
                ctx_inner
                    .pending_waiters
                    .insert(waiter_id, WaiterKind::Frame);
            }

            let meta = FrameWaitMeta {
                seq,
                state: this.state.clone(),
                waiter_id,
            };

            sched.add_frame_wait(meta);
            this.registered.set(true);
        }

        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::SchedulerMode;

    #[test]
    fn test_ctx_creation() {
        let sched = Rc::new(RefCell::new(TimeScheduler::new(SchedulerMode::Offline)));
        let executor = Rc::new(crate::executor::Executor::new());
        let tempo = TempoMap::new(120.0);
        let ctx = Ctx::new_root(sched, executor, tempo, "test_seed");

        assert_eq!(ctx.time(), 0.0);
        assert!(!ctx.is_canceled());
    }

    #[test]
    fn test_ctx_cancel() {
        let sched = Rc::new(RefCell::new(TimeScheduler::new(SchedulerMode::Offline)));
        let executor = Rc::new(crate::executor::Executor::new());
        let tempo = TempoMap::new(120.0);
        let ctx = Ctx::new_root(sched, executor, tempo, "test_seed");

        assert!(!ctx.is_canceled());
        ctx.cancel();
        assert!(ctx.is_canceled());
    }

    #[test]
    fn test_random_deterministic() {
        let sched = Rc::new(RefCell::new(TimeScheduler::new(SchedulerMode::Offline)));
        let executor = Rc::new(crate::executor::Executor::new());
        let tempo = TempoMap::new(120.0);
        let ctx1 = Ctx::new_root(sched.clone(), executor.clone(), tempo.clone(), "same_seed");
        let ctx2 = Ctx::new_root(sched, executor, TempoMap::new(120.0), "same_seed");

        for _ in 0..100 {
            assert_eq!(ctx1.random(), ctx2.random());
        }
    }
}
