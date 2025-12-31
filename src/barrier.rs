//! Barriers - synchronization points across coroutines
//!
//! Barriers allow coroutines within the same root context to synchronize.
//! One coroutine "starts" a barrier, others can "await" it, and the barrier
//! is "resolved" when the producer finishes its work.

use crate::context::Ctx;
use crate::scheduler::WaitState;
use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// State of a barrier.
struct BarrierState {
    /// The barrier key.
    #[allow(dead_code)]
    key: String,
    /// Root context ID.
    #[allow(dead_code)]
    root_id: u64,
    /// Time at which the barrier was last resolved.
    last_resolved_time: f64,
    /// Whether a barrier cycle is in progress.
    in_progress: bool,
    /// Time at which the current cycle started.
    #[allow(dead_code)]
    start_time: f64,
    /// Waiters for this barrier.
    waiters: Vec<BarrierWaiter>,
}

struct BarrierWaiter {
    ctx: Ctx,
    state: WaitState,
}

// Thread-local barrier storage.
thread_local! {
    static BARRIERS: RefCell<HashMap<String, BarrierState>> = RefCell::new(HashMap::new());
}

/// Create a scoped storage key for a barrier.
fn barrier_store_key(root_id: u64, key: &str) -> String {
    format!("{}\0{}", root_id, key)
}

/// Get or create a barrier state.
fn get_or_create_barrier(key: &str, root_id: u64) -> String {
    let store_key = barrier_store_key(root_id, key);

    BARRIERS.with(|barriers| {
        let mut map = barriers.borrow_mut();
        if !map.contains_key(&store_key) {
            map.insert(
                store_key.clone(),
                BarrierState {
                    key: key.to_string(),
                    root_id,
                    last_resolved_time: f64::NEG_INFINITY,
                    in_progress: false,
                    start_time: f64::NEG_INFINITY,
                    waiters: Vec::new(),
                },
            );
        }
    });

    store_key
}

/// Start a new barrier cycle.
/// If a cycle is already in progress, it is resolved first.
pub fn start_barrier(key: &str, ctx: &Ctx) {
    let root_id = {
        let inner = ctx.0.borrow();
        inner
            .root
            .as_ref()
            .and_then(|w| w.upgrade())
            .map(|r| r.borrow().id)
            .unwrap_or(inner.id)
    };

    let store_key = get_or_create_barrier(key, root_id);

    BARRIERS.with(|barriers| {
        let mut map = barriers.borrow_mut();
        if let Some(state) = map.get_mut(&store_key) {
            // Resolve any stale in-progress cycle
            if state.in_progress {
                let t = state.last_resolved_time;
                for waiter in state.waiters.drain(..) {
                    if waiter.ctx.is_canceled() {
                        waiter.state.complete_cancelled();
                    } else {
                        let new_time = waiter.ctx.time().max(t);
                        waiter.ctx.set_time(new_time);
                        waiter.state.complete_ok();
                    }
                }
            }

            state.in_progress = true;
            state.start_time = ctx.time();
        }
    });
}

/// Resolve the current barrier cycle, waking all waiters.
pub fn resolve_barrier(key: &str, ctx: &Ctx) {
    let root_id = {
        let inner = ctx.0.borrow();
        inner
            .root
            .as_ref()
            .and_then(|w| w.upgrade())
            .map(|r| r.borrow().id)
            .unwrap_or(inner.id)
    };

    let store_key = barrier_store_key(root_id, key);

    BARRIERS.with(|barriers| {
        let mut map = barriers.borrow_mut();
        if let Some(state) = map.get_mut(&store_key) {
            state.in_progress = false;
            state.last_resolved_time = ctx.time();

            let t = state.last_resolved_time;
            for waiter in state.waiters.drain(..) {
                if waiter.ctx.is_canceled() {
                    waiter.state.complete_cancelled();
                } else {
                    let new_time = waiter.ctx.time().max(t);
                    waiter.ctx.set_time(new_time);
                    waiter.ctx.update_most_recent_desc_time(new_time);
                    waiter.state.complete_ok();
                }
            }
        }
    });
}

/// Await a barrier. Returns a future that resolves when the barrier is resolved.
/// If the barrier was already resolved at or after the context's current time,
/// returns immediately.
pub fn await_barrier(key: &str, ctx: &Ctx) -> BarrierWaitFuture {
    let root_id = {
        let inner = ctx.0.borrow();
        inner
            .root
            .as_ref()
            .and_then(|w| w.upgrade())
            .map(|r| r.borrow().id)
            .unwrap_or(inner.id)
    };

    let store_key = get_or_create_barrier(key, root_id);

    // Check if already resolved
    let already_resolved = BARRIERS.with(|barriers| {
        let map = barriers.borrow();
        if let Some(state) = map.get(&store_key) {
            if state.last_resolved_time >= ctx.time() {
                let new_time = ctx.time().max(state.last_resolved_time);
                ctx.set_time(new_time);
                ctx.update_most_recent_desc_time(new_time);
                return true;
            }
        }
        false
    });

    BarrierWaitFuture {
        ctx: ctx.clone(),
        store_key,
        already_resolved,
        state: WaitState::new(),
        registered: false,
    }
}

/// Future for awaiting a barrier.
pub struct BarrierWaitFuture {
    ctx: Ctx,
    store_key: String,
    already_resolved: bool,
    state: WaitState,
    registered: bool,
}

impl Future for BarrierWaitFuture {
    type Output = Result<(), crate::context::WaitError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        if this.already_resolved {
            return Poll::Ready(Ok(()));
        }

        if this.state.is_cancelled() {
            return Poll::Ready(Err(crate::context::WaitError {
                message: "cancelled".to_string(),
            }));
        }

        if this.state.is_done() {
            return Poll::Ready(Ok(()));
        }

        this.state.set_waker(cx.waker());

        if !this.registered {
            if this.ctx.is_canceled() {
                this.state.complete_cancelled();
                return Poll::Ready(Err(crate::context::WaitError {
                    message: "context cancelled".to_string(),
                }));
            }

            // Register as a waiter
            BARRIERS.with(|barriers| {
                let mut map = barriers.borrow_mut();
                if let Some(state) = map.get_mut(&this.store_key) {
                    state.waiters.push(BarrierWaiter {
                        ctx: this.ctx.clone(),
                        state: this.state.clone(),
                    });
                }
            });

            this.registered = true;
        }

        Poll::Pending
    }
}

/// Clear all barriers (useful for testing).
pub fn clear_all_barriers() {
    BARRIERS.with(|barriers| {
        barriers.borrow_mut().clear();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::{SchedulerMode, TimeScheduler};
    use crate::tempo::TempoMap;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn test_barrier_basic() {
        clear_all_barriers();

        let sched = Rc::new(RefCell::new(TimeScheduler::new(SchedulerMode::Offline)));
        let executor = Rc::new(crate::executor::Executor::new());
        let tempo = TempoMap::new(120.0);
        let ctx = Ctx::new_root(sched, executor, tempo, "test");

        start_barrier("test_barrier", &ctx);

        // Simulate some time passing
        ctx.set_time(1.0);

        resolve_barrier("test_barrier", &ctx);

        // Check that last_resolved_time is updated
        let store_key = barrier_store_key(ctx.id(), "test_barrier");
        BARRIERS.with(|barriers| {
            let map = barriers.borrow();
            let state = map.get(&store_key).unwrap();
            assert!((state.last_resolved_time - 1.0).abs() < 1e-10);
        });
    }
}
