Below is a Rust implementation plan that keeps the **same semantics** as your TypeScript engine (deterministic logical-time scheduling, drift-free waits, tempo-aware beat waits, structured concurrency, offline stepping), but adapted to the realities of Rust:

* **No Tokio**
* **Single-threaded custom executor** (deterministic polling order)
* **Realtime sleeping using `spin_sleep`** for better timing precision than `std::thread::sleep` (and because `std::thread::sleep_until` is nightly-only). ([Docs.rs][1])
* **Heap using std** (`BinaryHeap`) + a deterministic “lazy deletion” index map to support `remove()` / `adjust_deadline()` (because `BinaryHeap` alone can’t do that)

I’ll give you:

1. a clean architecture mapping,
2. the core tricky functions in Rust with code snippets,
3. explicit divergence points from the TS version (borrow checker, futures, cancellation, ordering),
4. “gotchas” a new Rust dev will hit and how to avoid them.

---

## 1. Map the TypeScript concepts to Rust concepts

### What TS has

* **Promises + microtasks/macrotasks**: the engine relies on “after one timeslice resolves, promise continuations run before the next timeslice”.
* **Scheduler** owns PQs and resolves waiters by calling `resolve()`.
* **Contexts** hold state (`time`, `tempo`, `rng`, cancellation, children).
* **OfflineRunner** calls `advanceTo()` and yields to macrotasks between slices.

### What Rust needs instead

Rust async is *pull-based*: futures do nothing until an executor polls them.
So we’ll implement:

* A **single-threaded executor** with:

  * a deterministic FIFO “ready queue”
  * custom `Waker`s that re-enqueue tasks
* Waits (`wait_sec`, `wait(beats)`, `wait_frame`, barrier waits) are implemented as **Futures** that:

  * register themselves with the scheduler (store a `Waker`)
  * return `Poll::Pending`
  * later the scheduler marks them ready and calls `waker.wake()`

**Important difference**: In JS, `resolve()` *pushes* work into the microtask queue.
In Rust, `wake()` just makes the executor poll the task again.

The key invariant (“one timeslice, then drain continuations”) becomes:

> **Process exactly one logical timeslice, wake tasks, then fully drain the executor’s ready queue before processing the next timeslice.**

That is the Rust equivalent of your “macrotask yield” requirement.

---

## 2. Recommended Rust module layout

A practical crate layout:

```
src/
  lib.rs
  engine.rs        // Engine loop: realtime + offline stepping
  executor.rs      // single-thread executor
  scheduler.rs     // PQs + process_one_timeslice
  pq.rs            // MinPQ with remove/adjust using BinaryHeap + HashMap
  tempo.rs         // TempoMap
  context.rs       // TimeContext handle, branch/branch_wait, cancel cascade
  barrier.rs       // barrier registry + barrier wait future
  rng.rs           // deterministic RNG + seed derivation
```

---

## 3. Core data model

### 3.1 Scheduler mode

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchedulerMode {
    Realtime,
    Offline,
}
```

### 3.2 Context handle needs interior mutability (diverges from TS)

In TS you can pass `ctx` around and mutate freely.
In Rust, you cannot hold `&mut ctx` across `.await`. So you must use a **shared handle**:

```rust
use std::{cell::RefCell, rc::Rc};

#[derive(Clone)]
pub struct Ctx(Rc<RefCell<CtxInner>>);

struct CtxInner {
    id: u64,
    debug_name: String,
    time: f64,
    start_time: f64,
    canceled: bool,

    // deterministic cancellation + waiter cleanup
    pending_waiters: std::collections::HashMap<u64, WaiterKind>,

    // tree
    children: Vec<std::rc::Weak<RefCell<CtxInner>>>,

    // shared root scheduler
    scheduler: Rc<RefCell<TimeScheduler>>,

    // tempo: shared or cloned depending on branch options
    tempo: Rc<RefCell<TempoMap>>,

    // rng: shared or forked
    rng: Rc<RefCell<DetRng>>,
    rng_seed: String,
    rng_fork_counter: u64,
}
```

**Gotcha**: You *must not* hold a `RefMut` across `.await`. Always do:

```rust
let base_time = {
    let mut inner = ctx.0.borrow_mut();
    // compute needed values
    inner.time
}; // borrow ends here
await something;
```

---

## 4. The custom executor (single-thread deterministic)

You need:

* FIFO ready queue
* each task schedules itself at most once (“scheduled flag”) to avoid duplicates
* a `RawWaker` that pushes the task into the ready queue

### 4.1 Executor core (snippet)

```rust
// executor.rs
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    future::Future,
    pin::Pin,
    rc::{Rc, Weak},
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

pub struct Executor {
    inner: Rc<RefCell<ExecutorInner>>,
}

struct ExecutorInner {
    ready: VecDeque<Rc<Task>>,
}

struct Task {
    // The future is polled until completion.
    fut: RefCell<Pin<Box<dyn Future<Output = ()>>>>,
    scheduled: Cell<bool>,
    exec: Weak<RefCell<ExecutorInner>>,
}

impl Executor {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(ExecutorInner { ready: VecDeque::new() })),
        }
    }

    pub fn spawn(&self, fut: impl Future<Output = ()> + 'static) -> Rc<Task> {
        let task = Rc::new(Task {
            fut: RefCell::new(Box::pin(fut)),
            scheduled: Cell::new(false),
            exec: Rc::downgrade(&self.inner),
        });
        self.enqueue(&task);
        task
    }

    fn enqueue(&self, task: &Rc<Task>) {
        if task.scheduled.replace(true) {
            return; // already queued
        }
        self.inner.borrow_mut().ready.push_back(task.clone());
    }

    pub fn run_until_stalled(&self) {
        loop {
            let task = {
                let mut inner = self.inner.borrow_mut();
                inner.ready.pop_front()
            };
            let Some(task) = task else { break };

            task.scheduled.set(false);

            let waker = task_waker(&task);
            let mut cx = Context::from_waker(&waker);

            let poll = task.fut.borrow_mut().as_mut().poll(&mut cx);
            if let Poll::Ready(()) = poll {
                // task completes; drop future
            }
        }
    }
}

// ---- Waker glue ----

fn task_waker(task: &Rc<Task>) -> Waker {
    unsafe fn clone(data: *const ()) -> RawWaker {
        let task = Rc::<Task>::from_raw(data as *const Task);
        let cloned = task.clone();
        std::mem::forget(task);
        RawWaker::new(Rc::into_raw(cloned) as *const (), &VTABLE)
    }

    unsafe fn wake(data: *const ()) {
        wake_by_ref(data);
        drop_raw(data);
    }

    unsafe fn wake_by_ref(data: *const ()) {
        let task = Rc::<Task>::from_raw(data as *const Task);
        if let Some(exec) = task.exec.upgrade() {
            // enqueue deterministically FIFO
            if !task.scheduled.replace(true) {
                exec.borrow_mut().ready.push_back(task.clone());
            }
        }
        std::mem::forget(task);
    }

    unsafe fn drop_raw(data: *const ()) {
        drop(Rc::<Task>::from_raw(data as *const Task));
    }

    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop_raw);

    let raw = RawWaker::new(Rc::into_raw(task.clone()) as *const (), &VTABLE);
    unsafe { Waker::from_raw(raw) }
}
```

### Why this preserves your TS invariants

* Scheduler resolves waiters → calls `wake()` in deterministic sequence order
* Executor drains the ready queue fully before scheduler processes another timeslice
* That’s your “macrotask boundary” analog in Rust

---

## 5. Priority queues using std `BinaryHeap` + HashMap (remove/adjust)

### TS divergence

Your TS PQ supports `remove(id)` and `adjustDeadline(id, newDeadline)` efficiently.
Rust’s `BinaryHeap` cannot remove arbitrary elements or adjust keys.

### Solution: “lazy deletion”

* Keep a `HashMap<Id, Entry>` as the **truth**
* Heap holds `(deadline, tie, id)` keys, but may contain stale keys
* On `peek/pop`, discard heap tops that don’t match the map anymore

This is deterministic and uses std structures.

### 5.1 A minimal MinPQ (snippet)

```rust
// pq.rs
use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashMap},
};

#[derive(Clone)]
struct Key {
    deadline: f64,
    tie: u64, // seq for deterministic ordering
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

// IMPORTANT: Use total_cmp to order floats deterministically (handles -0, etc).
impl Ord for Key {
    fn cmp(&self, other: &Self) -> Ordering {
        // normal ascending (deadline, tie), but BinaryHeap is max-heap.
        // We'll invert by reversing the Ordering here.
        match self.deadline.total_cmp(&other.deadline) {
            Ordering::Equal => match self.tie.cmp(&other.tie) {
                Ordering::Equal => self.id.cmp(&other.id),
                o => o,
            },
            o => o,
        }
        .reverse()
    }
}

pub struct MinPq<M> {
    heap: BinaryHeap<Key>,
    live: HashMap<u64, (f64, u64, M)>, // id -> (deadline, tie, meta)
}

impl<M> MinPq<M> {
    pub fn new() -> Self {
        Self { heap: BinaryHeap::new(), live: HashMap::new() }
    }

    pub fn add(&mut self, id: u64, deadline: f64, tie: u64, meta: M) -> bool {
        if self.live.contains_key(&id) { return false; }
        self.live.insert(id, (deadline, tie, meta));
        self.heap.push(Key { deadline, tie, id });
        true
    }

    pub fn remove(&mut self, id: u64) -> Option<M> {
        self.live.remove(&id).map(|(_, _, m)| m)
    }

    pub fn adjust_deadline(&mut self, id: u64, new_deadline: f64) -> bool {
        let Some((deadline, tie, _)) = self.live.get_mut(&id) else { return false };
        *deadline = new_deadline;
        // push new key; old one becomes stale
        self.heap.push(Key { deadline: new_deadline, tie: *tie, id });
        true
    }

    pub fn peek_deadline(&mut self) -> Option<f64> {
        self.clean_top();
        self.heap.peek().map(|k| k.deadline)
    }

    pub fn pop(&mut self) -> Option<(u64, f64, u64, M)> {
        loop {
            let k = self.heap.pop()?;
            let Some((dl, tie, _)) = self.live.get(&k.id) else { continue; }; // stale
            if dl.to_bits() != k.deadline.to_bits() || *tie != k.tie {
                continue; // stale
            }
            let (dl, tie, meta) = self.live.remove(&k.id).unwrap();
            return Some((k.id, dl, tie, meta));
        }
    }

    fn clean_top(&mut self) {
        while let Some(k) = self.heap.peek() {
            let ok = match self.live.get(&k.id) {
                Some((dl, tie, _)) => dl.to_bits() == k.deadline.to_bits() && *tie == k.tie,
                None => false,
            };
            if ok { break; }
            self.heap.pop(); // discard stale
        }
    }

    pub fn is_empty(&self) -> bool { self.live.is_empty() }
}
```

**Gotcha**: You must use `f64::total_cmp()` (stable) or a wrapper crate because `f64` has no `Ord`. Also avoid NaNs entirely (clamp inputs like you do in TS).

---

## 6. The scheduler core (process one timeslice deterministically)

### Scheduler struct outline

```rust
// scheduler.rs
use std::{cell::RefCell, rc::{Rc, Weak}, time::Instant, collections::HashMap};

pub struct TimeScheduler {
    pub mode: SchedulerMode,

    // deterministic sequence
    seq: u64,

    // global alignment (TS root.mostRecentDescendentTime)
    most_recent_desc_time: f64,

    // time dilation anchors (realtime only)
    rate: f64,
    wall_anchor: Instant,
    logical_anchor: f64,

    // offline clock
    offline_now: f64,

    // PQs
    time_pq: MinPq<TimeWaitMeta>,
    beat_pqs: HashMap<u64, MinPq<BeatWaitMeta>>, // tempo_id -> PQ
    tempo_head_pq: MinPq<TempoHeadMeta>,         // id = tempo_id, deadline = dueTime

    // frame waiters (not PQ; resolved only on frame tick)
    frame_waiters: HashMap<u64, FrameWaitMeta>,

    // barriers (scoped per scheduler root)
    barriers: HashMap<String, BarrierState>,
}
```

### 6.1 Metadata types

Instead of JS `resolve/reject`, store a shared “wait state” + waker.

```rust
use std::{rc::{Rc, Weak}, cell::RefCell, task::Waker};

#[derive(Clone)]
struct WaitState {
    inner: Rc<RefCell<WaitStateInner>>,
}
struct WaitStateInner {
    done: bool,
    cancelled: bool,
    waker: Option<Waker>,
}

impl WaitState {
    fn set_waker(&self, w: &Waker) {
        self.inner.borrow_mut().waker = Some(w.clone());
    }
    fn complete_ok(&self) {
        let mut s = self.inner.borrow_mut();
        if s.done || s.cancelled { return; }
        s.done = true;
        if let Some(w) = s.waker.take() { w.wake(); }
    }
    fn complete_cancelled(&self) {
        let mut s = self.inner.borrow_mut();
        if s.done || s.cancelled { return; }
        s.cancelled = true;
        if let Some(w) = s.waker.take() { w.wake(); }
    }
    fn poll_result(&self) -> Option<Result<(), WaitError>> {
        let s = self.inner.borrow();
        if s.cancelled { Some(Err(WaitError::Cancelled)) }
        else if s.done { Some(Ok(())) }
        else { None }
    }
}

#[derive(Debug)]
pub enum WaitError {
    Cancelled,
}
```

Now the scheduler stores meta like:

```rust
#[derive(Clone)]
struct TimeWaitMeta {
    seq: u64,
    ctx: Weak<RefCell<CtxInner>>,
    target_time: f64,
    state: WaitState,
    waiter_id: u64,
}

#[derive(Clone)]
struct BeatWaitMeta {
    seq: u64,
    ctx: Weak<RefCell<CtxInner>>,
    tempo: Rc<RefCell<TempoMap>>,
    target_beat: f64,
    state: WaitState,
    waiter_id: u64,
    tempo_id: u64,
}

#[derive(Clone)]
struct TempoHeadMeta {
    tempo_id: u64,
}

#[derive(Clone)]
struct FrameWaitMeta {
    seq: u64,
    ctx: Weak<RefCell<CtxInner>>,
    state: WaitState,
    waiter_id: u64,
}
```

### 6.2 Scheduler alloc_seq (tie-breaker)

```rust
impl TimeScheduler {
    pub fn alloc_seq(&mut self) -> u64 {
        let s = self.seq;
        self.seq += 1;
        s
    }
}
```

### 6.3 Now() and set_rate() in realtime (like TS)

Use `Instant` for wall time. Keep logical seconds as `f64` to match TS.

```rust
impl TimeScheduler {
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

    pub fn set_rate(&mut self, rate: f64) {
        if self.mode == SchedulerMode::Offline { return; }
        let r = if rate.is_finite() && rate > 0.0 { rate } else { 1.0 };
        let l = self.now();
        self.logical_anchor = l;
        self.wall_anchor = Instant::now();
        self.rate = r;
    }
}
```

### 6.4 Peek next event time (min of timePQ and tempoHeadPQ)

```rust
impl TimeScheduler {
    pub fn peek_next_event_time(&mut self) -> Option<f64> {
        let t1 = self.time_pq.peek_deadline().unwrap_or(f64::INFINITY);
        let t2 = self.tempo_head_pq.peek_deadline().unwrap_or(f64::INFINITY);
        let next = t1.min(t2);
        if next.is_finite() { Some(next) } else { None }
    }
}
```

### 6.5 refresh_tempo_head (head-only retiming)

```rust
impl TimeScheduler {
    fn refresh_tempo_head(&mut self, tempo_id: u64) {
        let head = self.beat_pqs
            .get_mut(&tempo_id)
            .and_then(|pq| {
                // We need the *current* head’s targetBeat+tempo; easiest: pop/peek not ideal.
                // Plan: add a peek_meta() that clones meta, OR store head info separately.
                // For brevity, assume MinPq has a peek_clone() method:
                pq.peek_clone()
            });

        if let Some(meta) = head {
            let due_time = meta.tempo.borrow().time_at_beats(meta.target_beat);
            if due_time.is_finite() {
                // tempo_head id is tempo_id itself
                if !self.tempo_head_pq.adjust_deadline(tempo_id, due_time) {
                    let tie = 0; // tie doesn't matter much; can use alloc_seq too
                    self.tempo_head_pq.add(
                        tempo_id,
                        due_time,
                        tie,
                        TempoHeadMeta { tempo_id },
                    );
                }
            }
        } else {
            self.tempo_head_pq.remove(tempo_id);
        }
    }

    pub fn on_tempo_changed(&mut self, tempo_id: u64) {
        self.refresh_tempo_head(tempo_id);
    }
}
```

**TS divergence**: in TS you store one tempo head entry as `tempohead:${tempoId}` string.
In Rust, use `tempo_id: u64` directly as the PQ id.

### 6.6 process_one_timeslice (heart of engine)

This is the function you must get exactly right.

```rust
impl TimeScheduler {
    pub fn process_one_timeslice(&mut self, t_slice: f64) {
        // Determine whether time or beat slice is next.
        let t_time = self.time_pq.peek_deadline().unwrap_or(f64::INFINITY);
        let t_beat = self.tempo_head_pq.peek_deadline().unwrap_or(f64::INFINITY);

        if t_time <= t_beat {
            self.process_time_waiters_at(t_time);
        } else {
            self.process_beat_waiters_for_earliest_tempo();
        }

        // Note: In TS you queueMicrotask to exit “microtask phase”.
        // In Rust, the Engine loop will immediately drain ready tasks after this call,
        // which is the equivalent boundary.
    }

    fn process_time_waiters_at(&mut self, t: f64) {
        // Pop all waiters with exact deadline == t
        let mut batch: Vec<TimeWaitMeta> = Vec::new();
        while let Some(dl) = self.time_pq.peek_deadline() {
            if dl.to_bits() != t.to_bits() { break; }
            let (_id, _dl, _tie, meta) = self.time_pq.pop().unwrap();
            batch.push(meta);
        }

        // They are already ordered by (deadline, seq) in PQ key,
        // but sorting by seq is safe & matches TS.
        batch.sort_by_key(|m| m.seq);

        for w in batch {
            // Remove from ctx pending list, check cancel
            let Some(ctx_rc) = w.ctx.upgrade() else {
                w.state.complete_cancelled();
                continue;
            };
            let mut ctx = ctx_rc.borrow_mut();
            ctx.pending_waiters.remove(&w.waiter_id);

            if ctx.canceled {
                w.state.complete_cancelled();
                continue;
            }

            ctx.time = ctx.time.max(w.target_time);
            self.most_recent_desc_time = self.most_recent_desc_time.max(ctx.time);

            w.state.complete_ok();
        }
    }

    fn process_beat_waiters_for_earliest_tempo(&mut self) {
        let Some((_tempo_id, _dl, _tie, head_meta)) = self.tempo_head_pq.pop() else { return; };
        let tempo_id = head_meta.tempo_id;

        let beat_pq = match self.beat_pqs.get_mut(&tempo_id) {
            Some(pq) => pq,
            None => return,
        };

        // Peek head beat waiter to get target beat + tempo
        let head = match beat_pq.peek_clone() {
            Some(h) => h,
            None => return,
        };

        let due_time = head.tempo.borrow().time_at_beats(head.target_beat);
        if !due_time.is_finite() {
            // discard head (broken)
            let _ = beat_pq.pop();
            self.refresh_tempo_head(tempo_id);
            return;
        }

        let target_beat = head.target_beat;

        // Pop all waiters with same target beat
        let mut batch: Vec<BeatWaitMeta> = Vec::new();
        while let Some(h) = beat_pq.peek_clone() {
            if h.target_beat.to_bits() != target_beat.to_bits() { break; }
            let (_id, _dl, _tie, meta) = beat_pq.pop().unwrap();
            batch.push(meta);
        }
        batch.sort_by_key(|m| m.seq);

        for w in batch {
            let Some(ctx_rc) = w.ctx.upgrade() else {
                w.state.complete_cancelled();
                continue;
            };
            let mut ctx = ctx_rc.borrow_mut();
            ctx.pending_waiters.remove(&w.waiter_id);

            if ctx.canceled {
                w.state.complete_cancelled();
                continue;
            }

            ctx.time = ctx.time.max(due_time);
            self.most_recent_desc_time = self.most_recent_desc_time.max(ctx.time);

            w.state.complete_ok();
        }

        // recompute tempo head from new beatPQ head
        self.refresh_tempo_head(tempo_id);
    }
}
```

**Important**: you need `peek_clone()` on `MinPq`.
Implement it like TS `peek()`, but returning a clone of meta; that requires `M: Clone`.

---

## 7. Wait futures (time + beat + wait(0) behavior)

### 7.1 `wait_sec` in the context (computes absolute target)

This matches TS:
`base = max(rootMostRecent, ctx.time)` → `target = base + clamp(sec)`.

```rust
impl Ctx {
    pub fn time(&self) -> f64 { self.0.borrow().time }

    pub async fn wait_sec(&self, sec: f64) -> Result<(), WaitError> {
        let s = if sec.is_finite() && sec > 0.0 { sec } else { 0.0 };

        let (target, scheduler_rc) = {
            let inner = self.0.borrow();
            let mut sched = inner.scheduler.borrow_mut();
            let base = sched.most_recent_desc_time.max(inner.time);
            (base + s, inner.scheduler.clone())
        };

        TimeWaitFuture::new(self.clone(), scheduler_rc, target).await
    }
}
```

### 7.2 The time-wait future (tricky part)

```rust
use std::{future::Future, pin::Pin, task::{Context, Poll}};
use std::{cell::Cell, rc::Rc};

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
            state: WaitState { inner: Rc::new(RefCell::new(WaitStateInner { done: false, cancelled: false, waker: None })) },
            registered: Cell::new(false),
            waiter_id: Cell::new(0),
        }
    }
}

impl Future for TimeWaitFuture {
    type Output = Result<(), WaitError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        if let Some(r) = this.state.poll_result() {
            return Poll::Ready(r);
        }

        this.state.set_waker(cx.waker());

        if !this.registered.get() {
            // cancellation check
            {
                let inner = this.ctx.0.borrow();
                if inner.canceled {
                    this.state.complete_cancelled();
                    return Poll::Ready(Err(WaitError::Cancelled));
                }
            }

            let mut sched = this.sched.borrow_mut();
            let seq = sched.alloc_seq();
            let waiter_id = seq; // reuse seq as id for determinism
            this.waiter_id.set(waiter_id);

            // register with ctx pending waiters so cancel() can remove it immediately
            {
                let mut inner = this.ctx.0.borrow_mut();
                inner.pending_waiters.insert(waiter_id, WaiterKind::Time);
            }

            let meta = TimeWaitMeta {
                seq,
                ctx: std::rc::Rc::downgrade(&this.ctx.0),
                target_time: this.target_time.max(0.0),
                state: this.state.clone(),
                waiter_id,
            };

            sched.time_pq.add(waiter_id, meta.target_time, seq, meta);
            this.registered.set(true);
        }

        Poll::Pending
    }
}
```

### 7.3 wait(beats) and wait(0)

Your TS engine has a crucial special case: `wait(0)` must be scheduler-visible.

Rust should do the same: schedule a time-wait at `base_time` rather than a pure executor yield.

```rust
impl Ctx {
    pub async fn wait_beats(&self, beats: f64) -> Result<(), WaitError> {
        if !beats.is_finite() || beats <= 0.0 {
            // wait(0) => schedule a time waiter at base_time (sync point)
            let (base_time, sched) = {
                let inner = self.0.borrow();
                let mut s = inner.scheduler.borrow_mut();
                (s.most_recent_desc_time.max(inner.time), inner.scheduler.clone())
            };
            return TimeWaitFuture::new(self.clone(), sched, base_time).await;
        }

        // Normal beat wait: targetBeat = beatsAtTime(baseTime) + beats
        let (tempo_rc, tempo_id, target_beat, sched_rc) = {
            let inner = self.0.borrow();
            let tempo_rc = inner.tempo.clone();
            let tempo_id = tempo_rc.borrow().id;

            let mut sched = inner.scheduler.borrow_mut();
            let base_time = sched.most_recent_desc_time.max(inner.time);
            let base_beats = tempo_rc.borrow().beats_at_time(base_time);
            (tempo_rc, tempo_id, base_beats + beats, inner.scheduler.clone())
        };

        BeatWaitFuture::new(self.clone(), sched_rc, tempo_rc, tempo_id, target_beat).await
    }
}
```

---

## 8. Cancellation (must remove waiters immediately, not later)

### TS behavior

* AbortListener removes from PQ immediately
* Reject promise immediately
* Prevents canceled waiters from advancing time or becoming the next earliest event

### Rust equivalent plan

* Context keeps a `pending_waiters` map of all active waiters for that context.
* `Ctx::cancel()`:

  * marks canceled
  * asks scheduler to remove each waiter from relevant structures
  * completes their `WaitState` as cancelled (wakes tasks)
  * cascades to children

```rust
#[derive(Clone, Copy)]
enum WaiterKind {
    Time,
    Beat { tempo_id: u64 },
    Frame,
    Barrier { /* if you use barrier waiter ids */ },
}

impl Ctx {
    pub fn cancel(&self) {
        // 1) mark canceled and extract waiters list + children list
        let (waiters, children, sched) = {
            let mut inner = self.0.borrow_mut();
            if inner.canceled { return; }
            inner.canceled = true;

            let waiters: Vec<(u64, WaiterKind)> = inner.pending_waiters
                .iter()
                .map(|(id, k)| (*id, *k))
                .collect();

            inner.pending_waiters.clear();

            let children = inner.children.clone();
            (waiters, children, inner.scheduler.clone())
        };

        // 2) remove from scheduler + wake canceled wait states
        {
            let mut s = sched.borrow_mut();
            for (id, kind) in waiters {
                s.cancel_waiter(id, kind);
            }
        }

        // 3) cascade
        for w in children {
            if let Some(child) = w.upgrade() {
                Ctx(child).cancel();
            }
        }
    }
}

impl TimeScheduler {
    fn cancel_waiter(&mut self, id: u64, kind: WaiterKind) {
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
            WaiterKind::Barrier { .. } => {
                // barrier cancellation depends on how you store barrier waiters
            }
        }
    }
}
```

**Gotcha**: You must not leave canceled waiters in PQ, or they can incorrectly become the next event and advance logical time.

---

## 9. Engine loops: realtime (spin_sleep) and offline stepping

### 9.1 Why `spin_sleep`

* `spin_sleep::sleep_until(Instant)` is stable and designed to be a precise replacement for `thread::sleep` + “spin for last microseconds”. ([Docs.rs][1])
* `std::thread::sleep_until` is nightly-only. ([Rust Documentation][2])

### 9.2 Realtime event loop (single-thread, deterministic)

Core idea:

1. drain executor,
2. if next event is due → process exactly one timeslice,
3. else sleep until it’s due using spin_sleep.

```rust
// engine.rs
use spin_sleep::SpinSleeper;
use std::time::{Duration, Instant};

pub struct Engine {
    exec: Executor,
    sched: Rc<RefCell<TimeScheduler>>,
    sleeper: SpinSleeper,
}

impl Engine {
    pub fn run_until_complete(&mut self, is_done: impl Fn() -> bool) {
        loop {
            self.exec.run_until_stalled();
            if is_done() { break; }

            let next = self.sched.borrow_mut().peek_next_event_time();
            let Some(next_t) = next else {
                // No scheduled events; just stall.
                // You might break if root done is the only completion condition.
                // Or sleep a little to avoid busy loop.
                self.sleeper.sleep(Duration::from_millis(1));
                continue;
            };

            let now = self.sched.borrow().now();
            if next_t <= now {
                self.sched.borrow_mut().process_one_timeslice(next_t);
                continue; // then drain executor again
            }

            // Sleep until due. dtWall = dtLogical / rate.
            let rate = self.sched.borrow().rate;
            let dt_logical = next_t - now;
            let dt_wall = (dt_logical / rate).max(0.0);

            self.sleeper.sleep(Duration::from_secs_f64(dt_wall));
        }
    }
}
```

**TS divergence**: no `queueMicrotask` / `scheduleMacrotask`.
Instead:

* “microtasks” = ready queue draining
* “macrotask” = the outer loop iteration + optional blocking sleep

### 9.3 Offline stepping (advance_to)

Equivalent to your TS `advanceTo()` but using “drain executor between slices” rather than yielding to a macrotask.

```rust
impl Engine {
    pub fn advance_to(&mut self, target: f64) {
        let target = target.max(0.0);

        const MAX_TIMESLICES: usize = 200_000;
        let mut processed = 0usize;

        loop {
            let next = self.sched.borrow_mut().peek_next_event_time();
            if next.is_none() || next.unwrap() > target { break; }

            let next_t = next.unwrap();

            {
                let mut s = self.sched.borrow_mut();
                s.offline_now = next_t;
                s.process_one_timeslice(next_t);
            }

            // Drain continuations (critical semantic boundary)
            self.exec.run_until_stalled();

            processed += 1;
            if processed > MAX_TIMESLICES {
                panic!("advance_to({target}) exceeded MAX_TIMESLICES (likely infinite scheduling)");
            }
        }

        self.sched.borrow_mut().offline_now = target;
        self.exec.run_until_stalled(); // final flush
    }

    pub fn resolve_frame_tick(&mut self) {
        let t = self.sched.borrow().offline_now;
        self.sched.borrow_mut().resolve_all_frame_waiters_at(t);
        self.exec.run_until_stalled();

        // If frame callbacks scheduled waits due immediately, process them
        self.advance_to(t);
    }
}
```

This directly implements your TS requirement:

> “don’t process timeslice T2 until continuations from T1 had a chance to schedule waits”.

---

## 10. TempoMap port (mostly straightforward, but watch float math + binary search)

This ports cleanly. The only big Rust gotcha is ownership/borrowing. Store segments in a `Vec<TempoSegment>`, mutate by splitting/truncating.

Skeleton:

```rust
// tempo.rs
#[derive(Clone)]
pub struct TempoMap {
    pub id: u64,
    pub version: u64,
    segs: Vec<TempoSegment>,
}

#[derive(Clone)]
struct TempoSegment {
    t0: f64,
    t1: f64,
    bpm0: f64,
    bpm1: f64,
    beats0: f64,
    beats1: f64,
}

fn clamp_pos(x: f64) -> f64 {
    if x.is_finite() && x > 0.0 { x } else { 1.0 }
}

impl TempoMap {
    pub fn new(initial_bpm: f64, id: u64) -> Self {
        let bpm = clamp_pos(initial_bpm);
        Self {
            id,
            version: 0,
            segs: vec![TempoSegment {
                t0: 0.0, t1: f64::INFINITY,
                bpm0: bpm, bpm1: bpm,
                beats0: 0.0, beats1: f64::INFINITY,
            }],
        }
    }

    pub fn bpm_at_time(&self, t: f64) -> f64 { /* port TS */ }
    pub fn beats_at_time(&self, t: f64) -> f64 { /* port TS */ }
    pub fn time_at_beats(&self, b: f64) -> f64 { /* port TS quadratic */ }

    pub fn set_bpm_at_time(&mut self, bpm: f64, t: f64) { /* port TS _setBpmAtTime */ }
    pub fn ramp_to_bpm_at_time(&mut self, bpm: f64, dur: f64, t: f64) { /* port TS */ }
}
```

**Gotcha**: The TS code uses exact equality batching of deadlines/targetBeat.
So in Rust, preserve the same calculations and use `to_bits()` comparisons when batching (as shown).

---

## 11. Branching, structured concurrency, and “finally”

### Key divergence: Rust Futures are not Promises

* A JS `Promise` can be awaited by many consumers.
* A Rust `Future` is generally single-consumer.
  So you return a `JoinHandle<T>` that is itself a Future (or wraps one), plus a `cancel()` method.

### Plan for `branch` / `branch_wait`

* Create child context with initial time:

  * branch: `root_align = scheduler.most_recent_desc_time`
  * branch_wait: `child_start = parent.time`
* Inherit or clone tempo based on `BranchOptions`
* Fork or share RNG based on `BranchOptions`
* Spawn child task on executor
* For branch_wait: when child completes, update parent time (`max(parent.time, child.time)`)

### “finally” implementation trick (FnOnce isn’t object-safe)

TS has `.finally(() => ...)`. Rust needs a workaround:

* Store `Vec<Box<dyn FnMut()>>` and wrap FnOnce into FnMut via `Option`.

Example wrapper:

```rust
fn wrap_finally(f: impl FnOnce() + 'static) -> Box<dyn FnMut()> {
    let mut opt = Some(f);
    Box::new(move || {
        if let Some(f) = opt.take() { f(); }
    })
}
```

Then on completion, you run all callbacks.

---

## 12. Deterministic RNG + seed derivation (don’t use std HashMap hasher!)

### TS seed behavior

* parent seed normalized
* child seed derived via string `${parent}::fork:${i}`

### Rust gotcha

Do **not** use `std::collections::hash_map::DefaultHasher` for seeds:

* it is intentionally randomized per-process for DoS resistance → non-deterministic across runs.

Instead:

* implement a tiny stable hash (FNV-1a) or use `sha2/blake3`.

Example FNV-1a → u64 seed:

```rust
pub fn fnv1a64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}
```

Then implement a tiny deterministic RNG (xorshift/splitmix) so you avoid pulling `rand` at all.

---

## 13. What you can simplify vs TS (because Rust gives you control)

### You probably do NOT need these TS mechanisms in Rust

* `yieldToMacrotask` / `scheduleMacrotask`
* `inTimesliceMicrotaskPhase`

Because your single-thread executor loop *is* the event loop, and you can enforce:

* “exactly one timeslice”
* “drain ready tasks”
* then continue

That’s strictly stronger control than JS gives you.

---

## 14. Common Rust traps to explicitly avoid

1. **Holding a borrow across `.await`**

   * Fix: compute needed values in a block, drop borrow, then `.await`.

2. **Reference cycles with `Rc`**

   * Parent → child should usually be `Weak`, or you’ll leak.

3. **Leaving canceled waiters in PQ**

   * Must remove immediately or your scheduler advances incorrectly.

4. **Float ordering**

   * Use `total_cmp`, avoid NaN.

5. **Accidentally making tasks `Send`**

   * Keep everything `Rc`/`RefCell` and run on one thread.

6. **Busy loop**

   * In realtime: sleep until next event using `SpinSleeper`. ([Docs.rs][1])

---

## 15. Implementation order (so you can get it working incrementally)

1. **TempoMap**

   * Unit test beats/time conversions against known values.

2. **MinPq**

   * Unit test add/remove/adjust + deterministic tie ordering.

3. **Executor**

   * Unit test that waking order is FIFO and stable.

4. **Scheduler**

   * Implement time waits only; write a small scenario: two tasks waiting different times.

5. **Contexts**

   * Add `wait_sec` + cancellation + branch spawning.

6. **Beat waits**

   * Add beat PQ map + tempo head PQ + refresh logic + tempo changes.

7. **Offline runner**

   * Implement `advance_to`, ensure “drain between slices” works.

8. **Barrier**

   * Implement barrier registry per scheduler.

9. **Spin-sleep realtime**

   * Implement realtime run loop.

10. Port your TS test suite structure to Rust (same scenarios, compare event ordering/time epsilon).

---

If you want, I can also sketch a “Rust version” of your test harness (the deterministic log + compare logic) to validate offline vs realtime the same way your TS suite does—still on this custom executor, still single-threaded, no Tokio.

[1]: https://docs.rs/spin_sleep "spin_sleep - Rust"
[2]: https://doc.rust-lang.org/std/thread/fn.sleep_until.html?utm_source=chatgpt.com "sleep_until in std::thread"
