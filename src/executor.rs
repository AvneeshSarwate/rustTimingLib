//! Single-threaded deterministic executor
//!
//! A custom async executor that provides deterministic FIFO polling order.
//! Tasks are woken in the order they were scheduled, ensuring reproducible
//! behavior across runs.

use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    future::Future,
    pin::Pin,
    rc::{Rc, Weak},
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
};

/// A task in the executor.
pub struct Task {
    /// The future being executed.
    fut: RefCell<Pin<Box<dyn Future<Output = ()>>>>,
    /// Whether this task is currently scheduled in the ready queue.
    scheduled: Cell<bool>,
    /// Whether this task has completed and should not be polled again.
    completed: Cell<bool>,
    /// Weak reference to the executor.
    exec: Weak<RefCell<ExecutorInner>>,
}

struct ExecutorInner {
    /// FIFO queue of ready tasks.
    ready: VecDeque<Rc<Task>>,
}

/// Single-threaded deterministic executor.
pub struct Executor {
    inner: Rc<RefCell<ExecutorInner>>,
}

impl Default for Executor {
    fn default() -> Self {
        Self::new()
    }
}

impl Executor {
    /// Create a new executor.
    pub fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(ExecutorInner {
                ready: VecDeque::new(),
            })),
        }
    }

    /// Spawn a new task.
    pub fn spawn(&self, fut: impl Future<Output = ()> + 'static) -> Rc<Task> {
        let task = Rc::new(Task {
            fut: RefCell::new(Box::pin(fut)),
            scheduled: Cell::new(false),
            completed: Cell::new(false),
            exec: Rc::downgrade(&self.inner),
        });
        self.enqueue(&task);
        task
    }

    /// Enqueue a task if not already scheduled and not completed.
    fn enqueue(&self, task: &Rc<Task>) {
        // Don't enqueue completed tasks
        if task.completed.get() {
            return;
        }
        if task.scheduled.replace(true) {
            return; // Already queued
        }
        self.inner.borrow_mut().ready.push_back(task.clone());
    }

    /// Run all ready tasks until no more are ready.
    /// Returns the number of polls performed.
    pub fn run_until_stalled(&self) -> usize {
        let mut polls = 0;
        loop {
            let task = {
                let mut inner = self.inner.borrow_mut();
                inner.ready.pop_front()
            };

            let Some(task) = task else {
                break;
            };

            task.scheduled.set(false);

            // Skip completed tasks (stale wakers might have re-added them)
            if task.completed.get() {
                continue;
            }

            polls += 1;

            let waker = task_waker(&task);
            let mut cx = Context::from_waker(&waker);

            let poll = task.fut.borrow_mut().as_mut().poll(&mut cx);
            if let Poll::Ready(()) = poll {
                // Mark task as completed so it's never polled again
                task.completed.set(true);
            }
        }
        polls
    }

    /// Check if there are any ready tasks.
    pub fn has_ready_tasks(&self) -> bool {
        !self.inner.borrow().ready.is_empty()
    }
}

// --- Waker implementation ---

fn task_waker(task: &Rc<Task>) -> Waker {
    /// Clone the waker data.
    unsafe fn clone_fn(data: *const ()) -> RawWaker {
        let task = Rc::<Task>::from_raw(data as *const Task);
        let cloned = task.clone();
        std::mem::forget(task);
        RawWaker::new(Rc::into_raw(cloned) as *const (), &VTABLE)
    }

    /// Wake the task and consume the waker.
    unsafe fn wake_fn(data: *const ()) {
        wake_by_ref_fn(data);
        drop_fn(data);
    }

    /// Wake the task without consuming the waker.
    unsafe fn wake_by_ref_fn(data: *const ()) {
        let task = Rc::<Task>::from_raw(data as *const Task);
        // Don't wake completed tasks
        if task.completed.get() {
            std::mem::forget(task);
            return;
        }
        if let Some(exec) = task.exec.upgrade() {
            // Enqueue deterministically in FIFO order
            if !task.scheduled.replace(true) {
                exec.borrow_mut().ready.push_back(task.clone());
            }
        }
        std::mem::forget(task);
    }

    /// Drop the waker.
    unsafe fn drop_fn(data: *const ()) {
        drop(Rc::<Task>::from_raw(data as *const Task));
    }

    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone_fn, wake_fn, wake_by_ref_fn, drop_fn);

    let raw = RawWaker::new(Rc::into_raw(task.clone()) as *const (), &VTABLE);
    unsafe { Waker::from_raw(raw) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn test_basic_spawn() {
        let exec = Executor::new();
        let counter = Rc::new(Cell::new(0));

        let c = counter.clone();
        exec.spawn(async move {
            c.set(c.get() + 1);
        });

        exec.run_until_stalled();
        assert_eq!(counter.get(), 1);
    }

    #[test]
    fn test_ordering() {
        let exec = Executor::new();
        let order = Rc::new(RefCell::new(Vec::new()));

        let o1 = order.clone();
        exec.spawn(async move {
            o1.borrow_mut().push(1);
        });

        let o2 = order.clone();
        exec.spawn(async move {
            o2.borrow_mut().push(2);
        });

        let o3 = order.clone();
        exec.spawn(async move {
            o3.borrow_mut().push(3);
        });

        exec.run_until_stalled();
        assert_eq!(*order.borrow(), vec![1, 2, 3]);
    }

    #[test]
    fn test_pending_then_wake() {
        let exec = Executor::new();
        let counter = Rc::new(Cell::new(0));
        let waker_holder: Rc<RefCell<Option<Waker>>> = Rc::new(RefCell::new(None));

        let c = counter.clone();
        let wh = waker_holder.clone();

        exec.spawn(async move {
            // First poll: store waker and return pending
            std::future::poll_fn(|cx| {
                let current = c.get();
                if current == 0 {
                    *wh.borrow_mut() = Some(cx.waker().clone());
                    c.set(1);
                    Poll::Pending
                } else {
                    c.set(current + 1);
                    Poll::Ready(())
                }
            })
            .await;
        });

        // First run: task goes pending
        exec.run_until_stalled();
        assert_eq!(counter.get(), 1);

        // Wake the task
        if let Some(waker) = waker_holder.borrow_mut().take() {
            waker.wake();
        }

        // Second run: task completes
        exec.run_until_stalled();
        assert_eq!(counter.get(), 2);
    }
}
