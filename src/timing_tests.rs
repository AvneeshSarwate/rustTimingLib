//! Timing Engine Determinism Test Suite
//!
//! This module is a direct port of the TypeScript timing_tests.ts test suite.
//! It tests deterministic logical-time scheduling across both offline and realtime modes.
//!
//! The suite assumes the engine provides these guarantees:
//! 1) Deterministic logical-time ordering across realtime vs offline
//!    - same initial state
//!    - no shared-state mutation outside scheduler-driven continuations
//! 2) Deterministic tie-breaking for events at the same logical timestamp
//!    - ordering is arbitrary but stable (defined by scheduler sequence numbers)
//! 3) Seeded randomness:
//!    - RNG is forked per context by default (unless explicitly shared)
//!
//! Therefore, we can compare event ORDER strictly between offline and realtime modes.
//! We keep a small time epsilon for floating-point tolerance.
//!
//! ## Structure (mirrors TypeScript timing_tests.ts)
//!
//! Test cases are defined using the `dual_mode_test!` macro which:
//! 1. Runs the scenario in offline mode
//! 2. Runs the scenario in realtime mode (at 100x speed)
//! 3. Compares the event logs for deterministic equivalence

#[cfg(test)]
mod tests {
    use crate::barrier::{await_barrier, clear_all_barriers, resolve_barrier, start_barrier};
    use crate::context::{BranchOptions, Ctx, RngMode, TempoMode};
    use crate::engine::{EngineConfig, OfflineRunner, RealtimeRunner};
    use std::cell::RefCell;
    use std::future::Future;
    use std::rc::Rc;

    // ============================================================================
    // CONFIGURATION
    // ============================================================================

    /// Speed multiplier for realtime tests to make them fast.
    /// At 100x, a 1-second logical duration only takes 10ms wall time.
    const REALTIME_RATE: f64 = 100.0;

    // ============================================================================
    // TYPES
    // ============================================================================

    /// A logged event for comparison between offline and realtime runs.
    #[derive(Clone, Debug)]
    struct LoggedEvent {
        id: String,
        t: f64,
        ctx_id: u64,
        note: Option<String>,
        value: Option<f64>,
    }

    /// Test tolerances for comparison.
    #[derive(Clone)]
    struct TestTolerances {
        time_eps_sec: f64,
        value_eps: f64,
    }

    impl Default for TestTolerances {
        fn default() -> Self {
            Self {
                time_eps_sec: 1e-6,  // 1 microsecond
                value_eps: 1e-12,    // very tight for RNG values
            }
        }
    }

    /// Configuration for a test case.
    #[derive(Clone)]
    struct TestConfig {
        name: &'static str,
        bpm: f64,
        seed: &'static str,
        logical_duration_sec: f64,
        tolerances: TestTolerances,
    }

    impl TestConfig {
        fn new(name: &'static str) -> Self {
            Self {
                name,
                bpm: 60.0,
                seed: name,
                logical_duration_sec: 0.5,
                tolerances: TestTolerances::default(),
            }
        }

        fn with_bpm(mut self, bpm: f64) -> Self {
            self.bpm = bpm;
            self
        }

        fn with_duration(mut self, dur: f64) -> Self {
            self.logical_duration_sec = dur;
            self
        }

        #[allow(dead_code)]
        fn with_tolerances(mut self, tol: TestTolerances) -> Self {
            self.tolerances = tol;
            self
        }
    }

    /// Type alias for the event log.
    type EventLog = Rc<RefCell<Vec<LoggedEvent>>>;

    /// Logger function type.
    type Logger = Box<dyn Fn(&Ctx, &str, Option<&str>, Option<f64>)>;

    // ============================================================================
    // HELPERS
    // ============================================================================

    /// Create a logger that appends events to the log.
    fn make_logger(events: EventLog) -> Logger {
        Box::new(move |ctx: &Ctx, id: &str, note: Option<&str>, value: Option<f64>| {
            events.borrow_mut().push(LoggedEvent {
                id: id.to_string(),
                t: ctx.time(),
                ctx_id: ctx.id(),
                note: note.map(|s| s.to_string()),
                value,
            });
        })
    }

    /// Run a scenario in offline mode.
    fn run_offline<F, Fut>(config: &TestConfig, scenario: F) -> Vec<LoggedEvent>
    where
        F: FnOnce(Ctx, EventLog) -> Fut,
        Fut: Future<Output = ()> + 'static,
    {
        clear_all_barriers();
        let events: EventLog = Rc::new(RefCell::new(Vec::new()));
        let events_clone = events.clone();

        let mut runner = OfflineRunner::new(
            move |ctx| scenario(ctx, events_clone),
            EngineConfig {
                bpm: config.bpm,
                seed: config.seed.to_string(),
                ..Default::default()
            },
        );

        runner.step_sec(config.logical_duration_sec);
        let result = events.borrow().clone();
        result
    }

    /// Run a scenario in realtime mode.
    fn run_realtime<F, Fut>(config: &TestConfig, scenario: F) -> Vec<LoggedEvent>
    where
        F: FnOnce(Ctx, EventLog) -> Fut,
        Fut: Future<Output = ()> + 'static,
    {
        clear_all_barriers();
        let events: EventLog = Rc::new(RefCell::new(Vec::new()));
        let events_clone = events.clone();

        let mut runner = RealtimeRunner::new(
            move |ctx| scenario(ctx, events_clone),
            EngineConfig {
                bpm: config.bpm,
                seed: config.seed.to_string(),
                rate: REALTIME_RATE,
                ..Default::default()
            },
        );

        runner.run_until_complete();
        let result = events.borrow().clone();
        result
    }

    /// Compare two runs (offline vs realtime) for deterministic equivalence.
    fn compare_deterministic_runs(
        label: &str,
        offline_events: &[LoggedEvent],
        realtime_events: &[LoggedEvent],
        tolerances: &TestTolerances,
    ) {
        // Check counts match
        assert_eq!(
            offline_events.len(),
            realtime_events.len(),
            "[{}] Event count mismatch: offline={}, realtime={}",
            label,
            offline_events.len(),
            realtime_events.len()
        );

        // Check each event matches
        for (i, (o, r)) in offline_events.iter().zip(realtime_events.iter()).enumerate() {
            // Check IDs match (strict order)
            assert_eq!(
                o.id, r.id,
                "[{}] Event order mismatch at index {}.\nOffline: {:?}\nRealtime: {:?}",
                label, i, o.id, r.id
            );

            // Check times match within tolerance
            assert!(
                (o.t - r.t).abs() <= tolerances.time_eps_sec,
                "[{}] Time mismatch for '{}': offline={:.9}, realtime={:.9}, eps={}",
                label,
                o.id,
                o.t,
                r.t,
                tolerances.time_eps_sec
            );

            // Check values match if present
            match (&o.value, &r.value) {
                (Some(ov), Some(rv)) => {
                    assert!(
                        (ov - rv).abs() <= tolerances.value_eps,
                        "[{}] Value mismatch for '{}': offline={}, realtime={}, eps={}",
                        label,
                        o.id,
                        ov,
                        rv,
                        tolerances.value_eps
                    );
                }
                (None, None) => {}
                _ => panic!(
                    "[{}] Value presence mismatch at '{}': offline={:?}, realtime={:?}",
                    label, o.id, o.value, r.value
                ),
            }

            // Check notes match if present
            assert_eq!(
                o.note, r.note,
                "[{}] Note mismatch for '{}': offline={:?}, realtime={:?}",
                label, o.id, o.note, r.note
            );
        }
    }

    // ============================================================================
    // MACRO FOR DUAL-MODE TESTS
    // ============================================================================

    /// Macro to define a dual-mode test that runs in both offline and realtime modes.
    ///
    /// This mirrors the TypeScript pattern where each test case is defined once
    /// and automatically run in both modes with results compared.
    macro_rules! dual_mode_test {
        (
            name: $name:ident,
            config: $config:expr,
            scenario: $scenario:expr
        ) => {
            #[test]
            fn $name() {
                let config: TestConfig = $config;
                let offline_events = run_offline(&config, $scenario);
                let realtime_events = run_realtime(&config, $scenario);
                compare_deterministic_runs(
                    config.name,
                    &offline_events,
                    &realtime_events,
                    &config.tolerances,
                );
            }
        };
    }

    // ============================================================================
    // TEST SCENARIOS (mirrors TypeScript makeTimingTestCases)
    // ============================================================================

    // Test 1: sequential_waitSec_basic
    dual_mode_test! {
        name: test_sequential_wait_sec_basic,
        config: TestConfig::new("sequential_waitSec_basic"),
        scenario: |ctx: Ctx, events: EventLog| async move {
            let log = make_logger(events);
            log(&ctx, "start", None, None);
            let _ = ctx.wait_sec(0.05).await;
            log(&ctx, "t=0.05", None, None);
            let _ = ctx.wait_sec(0.10).await;
            log(&ctx, "t=0.15", None, None);
            let _ = ctx.wait_sec(0.20).await;
            log(&ctx, "t=0.35", None, None);
        }
    }

    // Test 2: parallel_branchWait_ordering
    dual_mode_test! {
        name: test_parallel_branch_wait_ordering,
        config: TestConfig::new("parallel_branchWait_ordering").with_duration(0.4),
        scenario: |ctx: Ctx, events: EventLog| async move {
            let log = make_logger(events.clone());
            log(&ctx, "start", None, None);

            let events_a = events.clone();
            let a = ctx.branch_wait(
                move |c| {
                    let log = make_logger(events_a);
                    async move {
                        log(&c, "A_start", None, None);
                        let _ = c.wait_sec(0.20).await;
                        log(&c, "A_end", None, None);
                    }
                },
                BranchOptions::default(),
            );

            let events_b = events.clone();
            let b = ctx.branch_wait(
                move |c| {
                    let log = make_logger(events_b);
                    async move {
                        log(&c, "B_start", None, None);
                        let _ = c.wait_sec(0.10).await;
                        log(&c, "B_end", None, None);
                    }
                },
                BranchOptions::default(),
            );

            let _ = a.await;
            let _ = b.await;
            let _ = ctx.wait(0.0).await;
            log(&ctx, "joined", None, None);
        }
    }

    // Test 3: deterministic_tie_break_same_deadline_is_stable
    dual_mode_test! {
        name: test_deterministic_tie_break_same_deadline_is_stable,
        config: TestConfig::new("deterministic_tie_break_same_deadline_is_stable").with_duration(0.25),
        scenario: |ctx: Ctx, events: EventLog| async move {
            let log = make_logger(events.clone());
            let shared: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));

            log(&ctx, "start", None, None);

            let events_a = events.clone();
            let shared_a = shared.clone();
            let a = ctx.branch_wait(
                move |c| {
                    let log = make_logger(events_a);
                    async move {
                        log(&c, "A_start", None, None);
                        let _ = c.wait_sec(0.10).await;
                        shared_a.borrow_mut().push("A".to_string());
                        log(&c, "A_end", None, None);
                    }
                },
                BranchOptions::default(),
            );

            let events_b = events.clone();
            let shared_b = shared.clone();
            let b = ctx.branch_wait(
                move |c| {
                    let log = make_logger(events_b);
                    async move {
                        log(&c, "B_start", None, None);
                        let _ = c.wait_sec(0.10).await;
                        shared_b.borrow_mut().push("B".to_string());
                        log(&c, "B_end", None, None);
                    }
                },
                BranchOptions::default(),
            );

            let _ = a.await;
            let _ = b.await;
            let _ = ctx.wait(0.0).await;

            let order = shared.borrow().join("");
            log(&ctx, "joined", Some(&order), None);

            // Explicit guarantee check
            assert_eq!(order, "AB", "Expected shared order 'AB' but got '{}'", order);
        }
    }

    // Test 4: many_branchWaits_stress_join
    dual_mode_test! {
        name: test_many_branch_waits_stress_join,
        config: TestConfig::new("many_branchWaits_stress_join").with_duration(1.0),
        scenario: |ctx: Ctx, events: EventLog| async move {
            let log = make_logger(events.clone());
            log(&ctx, "start", None, None);

            let mut handles = Vec::new();
            for i in 0..20 {
                let d = 0.02 * (i as f64 + 1.0);
                let events_i = events.clone();
                let task_name = format!("task{}", i);

                let handle = ctx.branch_wait(
                    move |c| {
                        let log = make_logger(events_i);
                        let name = task_name;
                        async move {
                            log(&c, &format!("{}_start", name), None, None);
                            let _ = c.wait_sec(d).await;
                            log(&c, &format!("{}_end", name), None, None);
                        }
                    },
                    BranchOptions::default(),
                );
                handles.push(handle);
            }

            for h in handles {
                let _ = h.await;
            }
            let _ = ctx.wait(0.0).await;
            log(&ctx, "joined", None, None);
        }
    }

    // Test 5: microtask_yield_intermediate_scheduling
    dual_mode_test! {
        name: test_microtask_yield_intermediate_scheduling,
        config: TestConfig::new("microtask_yield_intermediate_scheduling").with_duration(0.4),
        scenario: |ctx: Ctx, events: EventLog| async move {
            let log = make_logger(events.clone());

            let events_a = events.clone();
            let a = ctx.branch_wait(
                move |c| {
                    let log = make_logger(events_a);
                    async move {
                        let _ = c.wait_sec(0.10).await;
                        log(&c, "A@0.10", None, None);
                        let _ = c.wait_sec(0.05).await;
                        log(&c, "A@0.15", None, None);
                    }
                },
                BranchOptions::default(),
            );

            let events_b = events.clone();
            let b = ctx.branch_wait(
                move |c| {
                    let log = make_logger(events_b);
                    async move {
                        let _ = c.wait_sec(0.12).await;
                        log(&c, "B@0.12", None, None);
                    }
                },
                BranchOptions::default(),
            );

            let _ = a.await;
            let _ = b.await;
            let _ = ctx.wait(0.0).await;
            log(&ctx, "done", None, None);
        }
    }

    // Test 6: cancel_cascades_to_children_and_stops_ticks
    dual_mode_test! {
        name: test_cancel_cascades_to_children_and_stops_ticks,
        config: TestConfig::new("cancel_cascades_to_children_and_stops_ticks").with_duration(0.8),
        scenario: |ctx: Ctx, events: EventLog| async move {
            let log = make_logger(events.clone());
            log(&ctx, "start", None, None);

            let events_child = events.clone();
            let events_grand = events.clone();

            let mut handle = ctx.branch(
                move |child| {
                    let events_c = events_child;
                    let events_g = events_grand;
                    async move {
                        let log_c = make_logger(events_c.clone());
                        let events_grand_inner = events_g.clone();

                        child.branch(
                            move |grand| {
                                let log_g = make_logger(events_grand_inner);
                                async move {
                                    for i in 0..100 {
                                        if grand.is_canceled() {
                                            break;
                                        }
                                        log_g(&grand, &format!("grand_tick_{}", i), None, None);
                                        if grand.wait_sec(0.05).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            },
                            BranchOptions::default(),
                        );

                        for i in 0..100 {
                            if child.is_canceled() {
                                break;
                            }
                            log_c(&child, &format!("child_tick_{}", i), None, None);
                            if child.wait_sec(0.05).await.is_err() {
                                break;
                            }
                        }
                    }
                },
                BranchOptions::default(),
            );

            // Take and drop the future (it's already spawned)
            if let Some(fut) = handle.take_future() {
                std::mem::drop(fut);
            }

            let _ = ctx.wait_sec(0.23).await;
            log(&ctx, "cancel", None, None);
            handle.cancel();

            let _ = ctx.wait_sec(0.25).await;
            log(&ctx, "after_cancel_wait", None, None);
        }
    }

    // Test 7: barrier_loop_sync_melodyA_waits_for_melodyB
    dual_mode_test! {
        name: test_barrier_loop_sync_melody_a_waits_for_melody_b,
        config: TestConfig::new("barrier_loop_sync_melodyA_waits_for_melodyB").with_duration(1.2),
        scenario: |ctx: Ctx, events: EventLog| async move {
            let key = "barrier_loop_sync_melodyA_waits_for_melodyB";

            // Melody B - producer
            let events_b = events.clone();
            ctx.branch(
                move |b| {
                    let log = make_logger(events_b);
                    async move {
                        for cycle in 0..3 {
                            start_barrier(key, &b);
                            log(&b, &format!("B_start_{}", cycle), None, None);
                            let _ = b.wait_sec(0.30).await;
                            log(&b, &format!("B_end_{}", cycle), None, None);
                            resolve_barrier(key, &b);
                        }
                    }
                },
                BranchOptions::default(),
            );

            // Melody A - consumer
            let events_a = events.clone();
            let _ = ctx.branch_wait(
                move |a| {
                    let log = make_logger(events_a);
                    async move {
                        for cycle in 0..3 {
                            log(&a, &format!("A_start_{}", cycle), None, None);
                            let _ = a.wait_sec(0.20).await;
                            log(&a, &format!("A_end_{}", cycle), None, None);
                            let _ = await_barrier(key, &a).await;
                            log(&a, &format!("A_synced_{}", cycle), None, None);
                        }
                    }
                },
                BranchOptions::default(),
            ).await;
        }
    }

    // Test 8: barrier_race_resolve_then_immediate_restart
    dual_mode_test! {
        name: test_barrier_race_resolve_then_immediate_restart,
        config: TestConfig::new("barrier_race_resolve_then_immediate_restart").with_duration(0.6),
        scenario: |ctx: Ctx, events: EventLog| async move {
            let key = "barrier_race_resolve_then_immediate_restart";

            // Producer
            let events_b = events.clone();
            ctx.branch(
                move |b| {
                    let log = make_logger(events_b);
                    async move {
                        start_barrier(key, &b);
                        let _ = b.wait_sec(0.10).await;
                        resolve_barrier(key, &b);
                        log(&b, "B_resolved_0.10", None, None);

                        start_barrier(key, &b);
                        log(&b, "B_restarted_0.10", None, None);

                        let _ = b.wait_sec(0.10).await;
                        resolve_barrier(key, &b);
                        log(&b, "B_resolved_0.20", None, None);
                    }
                },
                BranchOptions::default(),
            );

            // Consumer
            let events_a = events.clone();
            let _ = ctx.branch_wait(
                move |a| {
                    let log = make_logger(events_a);
                    async move {
                        let _ = a.wait_sec(0.10).await;
                        log(&a, "A_before_await_0.10", None, None);
                        let _ = await_barrier(key, &a).await;
                        log(&a, "A_after_await_0.10", None, None);

                        let _ = a.wait_sec(0.10).await;
                        log(&a, "A_before_await_0.20", None, None);
                        let _ = await_barrier(key, &a).await;
                        log(&a, "A_after_await_0.20", None, None);
                    }
                },
                BranchOptions::default(),
            ).await;
        }
    }

    // Test 9: tempo_change_shared_retimes_beat_wait
    dual_mode_test! {
        name: test_tempo_change_shared_retimes_beat_wait,
        config: TestConfig::new("tempo_change_shared_retimes_beat_wait")
            .with_bpm(240.0)
            .with_duration(1.2),
        scenario: |ctx: Ctx, events: EventLog| async move {
            let log = make_logger(events.clone());

            // Branch to change tempo at t=0.50
            let ctx_clone = ctx.clone();
            let events_ctl = events.clone();
            ctx.branch(
                move |ctl| {
                    let log = make_logger(events_ctl);
                    async move {
                        let _ = ctl.wait_sec(0.50).await;
                        ctx_clone.set_bpm(480.0);
                        log(&ctl, "tempo_set_480@0.50", None, None);
                    }
                },
                BranchOptions::default(),
            );

            // Wait for 4 beats at 240 BPM
            // At 240 BPM: 4 beats/sec, so 4 beats = 1.0 sec at constant tempo
            // But tempo changes to 480 BPM at t=0.50
            // At t=0.50, we're at 2 beats. Need 2 more at 480 BPM = 0.25 sec
            // Total: 0.50 + 0.25 = 0.75 sec
            let _ = ctx.wait(4.0).await;
            log(&ctx, "wait4beats_done", None, None);
        }
    }

    // Test 10: tempo_clone_child_isolated_from_root_changes
    dual_mode_test! {
        name: test_tempo_clone_child_isolated_from_root_changes,
        config: TestConfig::new("tempo_clone_child_isolated_from_root_changes")
            .with_bpm(240.0)
            .with_duration(1.5),
        scenario: |ctx: Ctx, events: EventLog| async move {
            let log = make_logger(events.clone());

            // Branch to change root tempo at t=0.50
            let ctx_clone = ctx.clone();
            let events_ctl = events.clone();
            ctx.branch(
                move |ctl| {
                    let log = make_logger(events_ctl);
                    async move {
                        let _ = ctl.wait_sec(0.50).await;
                        ctx_clone.set_bpm(480.0);
                        log(&ctl, "root_tempo_set_480@0.50", None, None);
                    }
                },
                BranchOptions::default(),
            );

            // Child with cloned tempo - should NOT be affected by root tempo change
            let events_child = events.clone();
            let _ = ctx.branch_wait(
                move |child| {
                    let log = make_logger(events_child);
                    async move {
                        log(&child, "child_start", None, None);
                        // 4 beats at 240 BPM = 1.0 sec (unchanged because cloned)
                        let _ = child.wait(4.0).await;
                        log(&child, "child_done", None, None);
                    }
                },
                BranchOptions {
                    tempo: TempoMode::Cloned,
                    ..Default::default()
                },
            ).await;
        }
    }

    // Test 11: wait0_is_valid_sync_point
    dual_mode_test! {
        name: test_wait0_is_valid_sync_point,
        config: TestConfig::new("wait0_is_valid_sync_point").with_duration(0.6),
        scenario: |ctx: Ctx, events: EventLog| async move {
            let log = make_logger(events.clone());

            let events_p1 = events.clone();
            let p1 = ctx.branch_wait(
                move |c| {
                    let log = make_logger(events_p1);
                    async move {
                        let _ = c.wait_sec(0.10).await;
                        log(&c, "p1_done", None, None);
                    }
                },
                BranchOptions::default(),
            );

            let events_p2 = events.clone();
            let p2 = ctx.branch_wait(
                move |c| {
                    let log = make_logger(events_p2);
                    async move {
                        let _ = c.wait_sec(0.20).await;
                        log(&c, "p2_done", None, None);
                    }
                },
                BranchOptions::default(),
            );

            let _ = p1.await;
            let _ = p2.await;
            log(&ctx, "after_all_immediate", None, None);
            let _ = ctx.wait(0.0).await;
            log(&ctx, "after_all_after_wait0", None, None);
        }
    }

    // Test 12: frame_like_loop_waitSec_60fpsish
    dual_mode_test! {
        name: test_frame_like_loop_wait_sec_60fpsish,
        config: TestConfig::new("frame_like_loop_waitSec_60fpsish").with_duration(1.0),
        scenario: |ctx: Ctx, events: EventLog| async move {
            let log = make_logger(events);
            log(&ctx, "start", None, None);
            let dt = 1.0 / 60.0;

            for i in 0..30 {
                let _ = ctx.wait_sec(dt).await;
                log(&ctx, &format!("frame_{}", i), None, None);
            }
            log(&ctx, "done", None, None);
        }
    }

    // Test 13: noteoff_handleCancel_guaranteed_on_cancel
    // Tests that handleCancel is called when a branch is canceled, even if the branch
    // has already completed. This is crucial for "note off" cleanup in musical contexts.
    dual_mode_test! {
        name: test_noteoff_handle_cancel_guaranteed_on_cancel,
        config: TestConfig::new("noteoff_handleCancel_guaranteed_on_cancel").with_duration(1.2),
        scenario: |ctx: Ctx, events: EventLog| async move {
            let log = make_logger(events.clone());

            log(&ctx, "note1_on", None, None);

            // Start note1 branch - will complete naturally at t=0.30
            let events_note1 = events.clone();
            let note1 = ctx.branch(
                move |c| {
                    let log = make_logger(events_note1);
                    async move {
                        let _ = c.wait_sec(0.30).await;
                        log(&c, "note1_off_in_branch", None, None);
                    }
                },
                BranchOptions::default(),
            );

            // Register handleCancel for note1 - this should fire when we cancel() later
            let events_cancel1 = events.clone();
            note1.handle_cancel({
                let log = make_logger(events_cancel1);
                let ctx_clone = ctx.clone();
                move || {
                    log(&ctx_clone, "note1_off_handleCancel", None, None);
                }
            });

            let _ = ctx.wait_sec(0.05).await;
            log(&ctx, "note2_on", None, None);

            // Start note2 branch - will be canceled before completion
            let events_note2 = events.clone();
            let note2 = ctx.branch(
                move |c| {
                    let log = make_logger(events_note2);
                    async move {
                        let _ = c.wait_sec(0.30).await;
                        log(&c, "note2_off_in_branch", None, None);
                    }
                },
                BranchOptions::default(),
            );

            // Register handleCancel for note2
            let events_cancel2 = events.clone();
            note2.handle_cancel({
                let log = make_logger(events_cancel2);
                let ctx_clone = ctx.clone();
                move || {
                    log(&ctx_clone, "note2_off_handleCancel", None, None);
                }
            });

            // At t=0.20, cancel note2 (before it completes at t=0.35)
            let _ = ctx.wait_sec(0.15).await;
            log(&ctx, "cancel_note2", None, None);
            note2.cancel();

            // At t=0.20 after cancel: handleCancel for note2 should have fired

            // At t=0.40, cancel note1 (after it completed at t=0.30)
            let _ = ctx.wait_sec(0.20).await;
            log(&ctx, "cancel_note1", None, None);
            note1.cancel();

            // Continue to verify no issues
            let _ = ctx.wait_sec(0.50).await;
            log(&ctx, "done", None, None);
        }
    }

    // Test 14: waitSec_negative_and_NaN_are_safe
    dual_mode_test! {
        name: test_wait_sec_negative_and_nan_are_safe,
        config: TestConfig::new("waitSec_negative_and_NaN_are_safe").with_duration(0.4),
        scenario: |ctx: Ctx, events: EventLog| async move {
            let log = make_logger(events);
            log(&ctx, "start", None, None);

            let _ = ctx.wait_sec(-0.10).await;
            log(&ctx, "after_neg_wait", None, None);

            let _ = ctx.wait_sec(f64::NAN).await;
            log(&ctx, "after_nan_wait", None, None);

            let _ = ctx.wait_sec(0.10).await;
            log(&ctx, "after_0.10", None, None);
        }
    }

    // Test 15: seeded_rng_forked_branches_repeatable
    dual_mode_test! {
        name: test_seeded_rng_forked_branches_repeatable,
        config: TestConfig::new("seeded_rng_forked_branches_repeatable").with_duration(0.4),
        scenario: |ctx: Ctx, events: EventLog| async move {
            let log = make_logger(events.clone());
            log(&ctx, "start", None, None);

            let events_a = events.clone();
            let a = ctx.branch_wait(
                move |c| {
                    let log = make_logger(events_a);
                    async move {
                        let _ = c.wait_sec(0.05).await;
                        log(&c, "A_r0", None, Some(c.random()));
                        let _ = c.wait_sec(0.05).await;
                        log(&c, "A_r1", None, Some(c.random()));
                        let _ = c.wait_sec(0.05).await;
                        log(&c, "A_r2", None, Some(c.random()));
                    }
                },
                BranchOptions::default(),
            );

            let events_b = events.clone();
            let b = ctx.branch_wait(
                move |c| {
                    let log = make_logger(events_b);
                    async move {
                        let _ = c.wait_sec(0.10).await;
                        log(&c, "B_r0", None, Some(c.random()));
                        let _ = c.wait_sec(0.05).await;
                        log(&c, "B_r1", None, Some(c.random()));
                    }
                },
                BranchOptions::default(),
            );

            let _ = a.await;
            let _ = b.await;
            let _ = ctx.wait(0.0).await;
            log(&ctx, "joined", None, None);
        }
    }

    // Test 16: seeded_rng_shared_stream_deterministic_under_tie_break
    dual_mode_test! {
        name: test_seeded_rng_shared_stream_deterministic_under_tie_break,
        config: TestConfig::new("seeded_rng_shared_stream_deterministic_under_tie_break").with_duration(0.3),
        scenario: |ctx: Ctx, events: EventLog| async move {
            let log = make_logger(events.clone());
            log(&ctx, "start", None, None);

            let events_a = events.clone();
            let a = ctx.branch_wait(
                move |c| {
                    let log = make_logger(events_a);
                    async move {
                        let _ = c.wait_sec(0.05).await;
                        log(&c, "A_draw", None, Some(c.random()));
                    }
                },
                BranchOptions {
                    rng: RngMode::Shared,
                    ..Default::default()
                },
            );

            let events_b = events.clone();
            let b = ctx.branch_wait(
                move |c| {
                    let log = make_logger(events_b);
                    async move {
                        let _ = c.wait_sec(0.05).await;
                        log(&c, "B_draw", None, Some(c.random()));
                    }
                },
                BranchOptions {
                    rng: RngMode::Shared,
                    ..Default::default()
                },
            );

            let _ = a.await;
            let _ = b.await;
            let _ = ctx.wait(0.0).await;
            log(&ctx, "joined", None, None);
        }
    }

    // ============================================================================
    // ADDITIONAL TESTS: Beat wait basic
    // ============================================================================

    dual_mode_test! {
        name: test_beat_wait_basic,
        config: TestConfig::new("beat_wait_basic").with_bpm(120.0).with_duration(3.0),
        scenario: |ctx: Ctx, events: EventLog| async move {
            let log = make_logger(events);
            log(&ctx, "start", None, None);

            // At 120 BPM, 2 beats = 1 second
            let _ = ctx.wait(2.0).await;
            log(&ctx, "after_2_beats", None, None);

            let _ = ctx.wait(2.0).await;
            log(&ctx, "after_4_beats", None, None);
        }
    }

    // ============================================================================
    // UNIT TESTS (offline-only, testing specific API behaviors)
    // ============================================================================

    /// Test: prog_time and prog_beats
    /// Tests program time and beats tracking relative to context start
    #[test]
    fn test_prog_time_and_beats() {
        clear_all_barriers();

        let mut runner = OfflineRunner::new(
            |ctx| async move {
                assert!((ctx.time() - 0.0).abs() < 1e-10);
                assert!((ctx.prog_time() - 0.0).abs() < 1e-10);

                let _ = ctx.wait_sec(0.5).await;

                assert!((ctx.time() - 0.5).abs() < 1e-10);
                assert!((ctx.prog_time() - 0.5).abs() < 1e-10);

                // At 60 BPM, 1 beat = 1 second
                // After 0.5 seconds, we have 0.5 beats
                assert!((ctx.beats() - 0.5).abs() < 1e-10);
                assert!((ctx.prog_beats() - 0.5).abs() < 1e-10);
            },
            EngineConfig {
                bpm: 60.0,
                seed: "prog_time_test".to_string(),
                ..Default::default()
            },
        );

        runner.step_sec(1.0);
    }

    /// Test: branch context inherits correct time
    /// Tests that branch contexts start at the correct time
    #[test]
    fn test_branch_context_inherits_time() {
        clear_all_barriers();
        let events: EventLog = Rc::new(RefCell::new(Vec::new()));

        let events_clone = events.clone();
        let mut runner = OfflineRunner::new(
            move |ctx| {
                let events = events_clone.clone();
                async move {
                    let _ = ctx.wait_sec(0.5).await;

                    let events_branch = events.clone();
                    let _ = ctx
                        .branch_wait(
                            move |c| {
                                let events = events_branch;
                                async move {
                                    // Branch should start at parent time (0.5)
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "branch_start".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                    assert!(
                                        (c.time() - 0.5).abs() < 1e-10,
                                        "Branch should start at parent time"
                                    );

                                    let _ = c.wait_sec(0.3).await;

                                    events.borrow_mut().push(LoggedEvent {
                                        id: "branch_end".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                }
                            },
                            BranchOptions::default(),
                        )
                        .await;

                    // Parent time should be updated after branch_wait
                    events.borrow_mut().push(LoggedEvent {
                        id: "parent_after".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });
                }
            },
            EngineConfig {
                bpm: 60.0,
                seed: "branch_time_test".to_string(),
                ..Default::default()
            },
        );

        runner.step_sec(2.0);

        let events = events.borrow();

        let branch_start = events.iter().find(|e| e.id == "branch_start").unwrap();
        let branch_end = events.iter().find(|e| e.id == "branch_end").unwrap();
        let parent_after = events.iter().find(|e| e.id == "parent_after").unwrap();

        assert!((branch_start.t - 0.5).abs() < 1e-6);
        assert!((branch_end.t - 0.8).abs() < 1e-6);
        assert!((parent_after.t - 0.8).abs() < 1e-6);
    }
}
