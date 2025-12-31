//! Engine - realtime and offline execution loops
//!
//! The engine ties together the executor and scheduler to run timing code.
//! - Realtime: uses spin_sleep for precise timing
//! - Offline: uses stepping API for faster-than-realtime execution

use crate::context::Ctx;
use crate::executor::Executor;
use crate::scheduler::TimeScheduler;
use crate::tempo::TempoMap;
use spin_sleep::SpinSleeper;
use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;
use std::time::Duration;

pub use crate::scheduler::SchedulerMode;

/// Configuration for launching the engine.
#[derive(Clone)]
pub struct EngineConfig {
    pub bpm: f64,
    pub seed: String,
    pub fps: f64,
    pub rate: f64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            bpm: 60.0,
            seed: "default".to_string(),
            fps: 60.0,
            rate: 1.0,
        }
    }
}

/// The main timing engine.
pub struct Engine {
    pub executor: Rc<Executor>,
    pub scheduler: Rc<RefCell<TimeScheduler>>,
    pub root_ctx: Ctx,
    sleeper: SpinSleeper,
    fps: f64,
}

impl Engine {
    /// Create a new engine with the given mode and configuration.
    pub fn new(mode: SchedulerMode, config: EngineConfig) -> Self {
        let scheduler = Rc::new(RefCell::new(TimeScheduler::new(mode)));
        if mode == SchedulerMode::Realtime {
            scheduler.borrow_mut().set_rate(config.rate);
        }

        // Create executor first so it can be shared with contexts
        let executor = Rc::new(Executor::new());

        let tempo = TempoMap::new(config.bpm);
        let root_ctx = Ctx::new_root(scheduler.clone(), executor.clone(), tempo, &config.seed);

        Self {
            executor,
            scheduler,
            root_ctx,
            sleeper: SpinSleeper::default(),
            fps: config.fps,
        }
    }

    /// Launch a task on the engine.
    pub fn spawn<F, Fut>(&self, f: F)
    where
        F: FnOnce(Ctx) -> Fut,
        Fut: Future<Output = ()> + 'static,
    {
        let ctx = self.root_ctx.clone();
        let fut = f(ctx);
        self.executor.spawn(fut);
    }

    /// Run the engine in realtime mode until the given condition is true.
    pub fn run_until<F>(&mut self, is_done: F)
    where
        F: Fn() -> bool,
    {
        loop {
            // Drain all ready tasks
            self.executor.run_until_stalled();

            if is_done() {
                break;
            }

            let next = self.scheduler.borrow_mut().peek_next_event_time();
            let Some(next_t) = next else {
                // No scheduled events, check if there are pending waits
                if !self.scheduler.borrow().has_pending_waits() {
                    // Nothing to do, might be done
                    if is_done() {
                        break;
                    }
                }
                // Sleep a bit to avoid busy loop
                self.sleeper.sleep(Duration::from_millis(1));
                continue;
            };

            let now = self.scheduler.borrow().now();
            if next_t <= now {
                // Process exactly one timeslice
                self.scheduler.borrow_mut().process_one_timeslice(next_t);
                // Continue to drain executor
                continue;
            }

            // Sleep until due
            let rate = self.scheduler.borrow().rate();
            let dt_logical = next_t - now;
            let dt_wall = (dt_logical / rate).max(0.0);

            self.sleeper.sleep(Duration::from_secs_f64(dt_wall));
        }
    }

    /// Run the engine until the root task completes (realtime).
    pub fn run_until_complete(&mut self) {
        loop {
            self.executor.run_until_stalled();

            let is_done = self.root_ctx.is_canceled() && !self.executor.has_ready_tasks();
            if is_done {
                break;
            }

            let next = self.scheduler.borrow_mut().peek_next_event_time();
            let Some(next_t) = next else {
                if !self.scheduler.borrow().has_pending_waits() {
                    break;
                }
                self.sleeper.sleep(Duration::from_millis(1));
                continue;
            };

            let now = self.scheduler.borrow().now();
            if next_t <= now {
                self.scheduler.borrow_mut().process_one_timeslice(next_t);
                continue;
            }

            let rate = self.scheduler.borrow().rate();
            let dt_logical = next_t - now;
            let dt_wall = (dt_logical / rate).max(0.0);
            self.sleeper.sleep(Duration::from_secs_f64(dt_wall));
        }
    }

    /// Advance offline time to the target.
    /// Processes all due timeslices, draining the executor between each.
    pub fn advance_to(&mut self, target: f64) {
        let target = target.max(0.0);

        // IMPORTANT: Run executor first to let initial tasks schedule their waits
        self.executor.run_until_stalled();

        const MAX_TIMESLICES: usize = 200_000;
        let mut processed = 0;

        loop {
            let next = self.scheduler.borrow_mut().peek_next_event_time();
            if next.is_none() || next.unwrap() > target {
                break;
            }

            let next_t = next.unwrap();

            // Set offline time and process
            self.scheduler.borrow_mut().offline_now = next_t;
            self.scheduler.borrow_mut().process_one_timeslice(next_t);

            // Critical: drain executor between timeslices
            self.executor.run_until_stalled();

            processed += 1;
            if processed > MAX_TIMESLICES {
                panic!(
                    "advance_to({}) exceeded MAX_TIMESLICES - likely infinite scheduling",
                    target
                );
            }
        }

        // Set final time
        self.scheduler.borrow_mut().offline_now = target;
        // Final drain
        self.executor.run_until_stalled();
    }

    /// Resolve all frame waiters at the current offline time.
    pub fn resolve_frame_tick(&mut self) {
        let t = self.scheduler.borrow().offline_now;
        self.scheduler.borrow_mut().resolve_all_frame_waiters(t);
        self.executor.run_until_stalled();

        // Process any waits scheduled by frame callbacks
        self.advance_to(t);
    }

    /// Step by a number of seconds (offline mode).
    pub fn step_sec(&mut self, dt: f64) {
        let s = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
        let target = self.scheduler.borrow().now() + s;
        self.advance_to(target);
    }

    /// Step by one frame (offline mode).
    pub fn step_frame(&mut self) {
        let dt = 1.0 / self.fps;
        self.step_sec(dt);
        self.resolve_frame_tick();
    }

    /// Step by N frames (offline mode).
    pub fn step_frames(&mut self, n: usize) {
        for _ in 0..n {
            self.step_frame();
        }
    }
}

/// Builder for creating and running offline simulations.
pub struct OfflineRunner {
    engine: Engine,
}

impl OfflineRunner {
    /// Create a new offline runner.
    pub fn new<F, Fut>(f: F, config: EngineConfig) -> Self
    where
        F: FnOnce(Ctx) -> Fut,
        Fut: Future<Output = ()> + 'static,
    {
        let engine = Engine::new(SchedulerMode::Offline, config);
        engine.spawn(f);
        Self { engine }
    }

    /// Get the root context.
    pub fn ctx(&self) -> Ctx {
        self.engine.root_ctx.clone()
    }

    /// Get the scheduler.
    pub fn scheduler(&self) -> Rc<RefCell<TimeScheduler>> {
        self.engine.scheduler.clone()
    }

    /// Get the current offline time.
    pub fn now(&self) -> f64 {
        self.engine.scheduler.borrow().now()
    }

    /// Step by seconds.
    pub fn step_sec(&mut self, dt: f64) {
        self.engine.step_sec(dt);
    }

    /// Step by one frame.
    pub fn step_frame(&mut self) {
        self.engine.step_frame();
    }

    /// Step by N frames.
    pub fn step_frames(&mut self, n: usize) {
        self.engine.step_frames(n);
    }
}

/// Builder for creating and running realtime simulations.
pub struct RealtimeRunner {
    engine: Engine,
}

impl RealtimeRunner {
    /// Create a new realtime runner.
    pub fn new<F, Fut>(f: F, config: EngineConfig) -> Self
    where
        F: FnOnce(Ctx) -> Fut,
        Fut: Future<Output = ()> + 'static,
    {
        let engine = Engine::new(SchedulerMode::Realtime, config);
        engine.spawn(f);
        Self { engine }
    }

    /// Get the root context.
    pub fn ctx(&self) -> Ctx {
        self.engine.root_ctx.clone()
    }

    /// Run until complete.
    pub fn run_until_complete(&mut self) {
        self.engine.run_until_complete();
    }

    /// Run until the given condition is true.
    pub fn run_until<F>(&mut self, is_done: F)
    where
        F: Fn() -> bool,
    {
        self.engine.run_until(is_done);
    }
}

/// Convenience function to launch an offline simulation.
pub fn launch_offline<F, Fut>(f: F) -> OfflineRunner
where
    F: FnOnce(Ctx) -> Fut,
    Fut: Future<Output = ()> + 'static,
{
    OfflineRunner::new(f, EngineConfig::default())
}

/// Convenience function to launch an offline simulation with config.
pub fn launch_offline_with_config<F, Fut>(f: F, config: EngineConfig) -> OfflineRunner
where
    F: FnOnce(Ctx) -> Fut,
    Fut: Future<Output = ()> + 'static,
{
    OfflineRunner::new(f, config)
}

/// Convenience function to launch a realtime simulation.
pub fn launch_realtime<F, Fut>(f: F) -> RealtimeRunner
where
    F: FnOnce(Ctx) -> Fut,
    Fut: Future<Output = ()> + 'static,
{
    RealtimeRunner::new(f, EngineConfig::default())
}

/// Convenience function to launch a realtime simulation with config.
pub fn launch_realtime_with_config<F, Fut>(f: F, config: EngineConfig) -> RealtimeRunner
where
    F: FnOnce(Ctx) -> Fut,
    Fut: Future<Output = ()> + 'static,
{
    RealtimeRunner::new(f, config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn test_offline_basic() {
        let counter = Rc::new(Cell::new(0));
        let c = counter.clone();

        let mut runner = launch_offline(move |ctx| {
            let c = c.clone();
            async move {
                c.set(c.get() + 1);
                let _ = ctx.wait_sec(0.1).await;
                c.set(c.get() + 1);
                let _ = ctx.wait_sec(0.1).await;
                c.set(c.get() + 1);
            }
        });

        // Initially nothing has run
        assert_eq!(counter.get(), 0);

        // Step to 0 - first task should run
        runner.step_sec(0.0);
        assert_eq!(counter.get(), 1);

        // Step to 0.1 - second wait resolves
        runner.step_sec(0.1);
        assert_eq!(counter.get(), 2);

        // Step to 0.2 - third wait resolves
        runner.step_sec(0.1);
        assert_eq!(counter.get(), 3);
    }

    #[test]
    fn test_offline_beat_wait() {
        let counter = Rc::new(Cell::new(0));
        let c = counter.clone();

        let mut runner = launch_offline_with_config(
            move |ctx| {
                let c = c.clone();
                async move {
                    c.set(c.get() + 1);
                    // At 120 BPM, 2 beats = 1 second
                    let _ = ctx.wait(2.0).await;
                    c.set(c.get() + 1);
                }
            },
            EngineConfig {
                bpm: 120.0,
                ..Default::default()
            },
        );

        runner.step_sec(0.0);
        assert_eq!(counter.get(), 1);

        runner.step_sec(0.5);
        assert_eq!(counter.get(), 1); // Still waiting

        runner.step_sec(0.6);
        assert_eq!(counter.get(), 2); // Beat wait resolved
    }
}
