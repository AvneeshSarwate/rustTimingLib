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
//! Therefore, we can compare event ORDER strictly between offline and realtime modes
//! (no windowed order tolerance). We still keep a small time epsilon for safety.

#[cfg(test)]
mod tests {
    use crate::barrier::{await_barrier, clear_all_barriers, resolve_barrier, start_barrier};
    use crate::context::{BranchOptions, Ctx, RngMode, TempoMode};
    use crate::engine::{EngineConfig, OfflineRunner, RealtimeRunner};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Speed multiplier for realtime tests to make them fast.
    /// At 100x, a 1-second logical duration only takes 10ms wall time.
    const REALTIME_RATE: f64 = 100.0;

    /// A logged event for comparison
    #[derive(Clone, Debug)]
    #[allow(dead_code)]
    struct LoggedEvent {
        id: String,
        t: f64,
        ctx_id: u64,
        note: Option<String>,
        value: Option<f64>,
    }

    /// Test tolerances for comparison
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

    /// Assert that events are as expected
    fn assert_events_match(events: &[LoggedEvent], expected_ids: &[&str], test_name: &str) {
        let actual_ids: Vec<&str> = events.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            actual_ids, expected_ids,
            "[{}] Event order mismatch.\nActual: {:?}\nExpected: {:?}",
            test_name, actual_ids, expected_ids
        );
    }

    /// Assert events have monotonically non-decreasing times
    fn assert_monotonic_times(events: &[LoggedEvent], test_name: &str) {
        for i in 1..events.len() {
            assert!(
                events[i].t >= events[i - 1].t - 1e-10,
                "[{}] Non-monotonic time at index {}: {} -> {}",
                test_name,
                i,
                events[i - 1].t,
                events[i].t
            );
        }
    }

    /// Assert event time is approximately equal to expected
    fn assert_time_approx(events: &[LoggedEvent], idx: usize, expected: f64, eps: f64, test_name: &str) {
        let actual = events[idx].t;
        assert!(
            (actual - expected).abs() <= eps,
            "[{}] Time mismatch at '{}': expected {}, got {} (eps={})",
            test_name,
            events[idx].id,
            expected,
            actual,
            eps
        );
    }

    // ==================== TEST CASES ====================

    /// Test: sequential_waitSec_basic
    /// Basic sequential waits in seconds
    #[test]
    fn test_sequential_wait_sec_basic() {
        clear_all_barriers();
        let events = Rc::new(RefCell::new(Vec::new()));

        let events_clone = events.clone();
        let mut runner = OfflineRunner::new(
            move |ctx| {
                let events = events_clone.clone();
                async move {
                    let log = |ctx: &Ctx, id: &str| {
                        events.borrow_mut().push(LoggedEvent {
                            id: id.to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                    };

                    log(&ctx, "start");
                    let _ = ctx.wait_sec(0.05).await;
                    log(&ctx, "t=0.05");
                    let _ = ctx.wait_sec(0.10).await;
                    log(&ctx, "t=0.15");
                    let _ = ctx.wait_sec(0.20).await;
                    log(&ctx, "t=0.35");
                }
            },
            EngineConfig {
                bpm: 60.0,
                seed: "sequential_waitSec_basic".to_string(),
                ..Default::default()
            },
        );

        runner.step_sec(0.5);

        let events = events.borrow();
        assert_events_match(&events, &["start", "t=0.05", "t=0.15", "t=0.35"], "sequential_waitSec_basic");
        assert_monotonic_times(&events, "sequential_waitSec_basic");

        // Check specific times
        assert_time_approx(&events, 0, 0.0, 1e-6, "sequential_waitSec_basic");
        assert_time_approx(&events, 1, 0.05, 1e-6, "sequential_waitSec_basic");
        assert_time_approx(&events, 2, 0.15, 1e-6, "sequential_waitSec_basic");
        assert_time_approx(&events, 3, 0.35, 1e-6, "sequential_waitSec_basic");
    }

    /// Test: parallel_branchWait_ordering
    /// Parallel branches with branchWait
    #[test]
    fn test_parallel_branch_wait_ordering() {
        clear_all_barriers();
        let events = Rc::new(RefCell::new(Vec::new()));

        let events_clone = events.clone();
        let mut runner = OfflineRunner::new(
            move |ctx| {
                let events = events_clone.clone();
                async move {
                    let log = |ctx: &Ctx, id: &str| {
                        events.borrow_mut().push(LoggedEvent {
                            id: id.to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                    };

                    log(&ctx, "start");

                    let events_a = events.clone();
                    let a = ctx.branch_wait(
                        move |c| {
                            let events = events_a;
                            async move {
                                events.borrow_mut().push(LoggedEvent {
                                    id: "A_start".to_string(),
                                    t: c.time(),
                                    ctx_id: c.id(),
                                    note: None,
                                    value: None,
                                });
                                let _ = c.wait_sec(0.20).await;
                                events.borrow_mut().push(LoggedEvent {
                                    id: "A_end".to_string(),
                                    t: c.time(),
                                    ctx_id: c.id(),
                                    note: None,
                                    value: None,
                                });
                            }
                        },
                        BranchOptions::default(),
                    );

                    let events_b = events.clone();
                    let b = ctx.branch_wait(
                        move |c| {
                            let events = events_b;
                            async move {
                                events.borrow_mut().push(LoggedEvent {
                                    id: "B_start".to_string(),
                                    t: c.time(),
                                    ctx_id: c.id(),
                                    note: None,
                                    value: None,
                                });
                                let _ = c.wait_sec(0.10).await;
                                events.borrow_mut().push(LoggedEvent {
                                    id: "B_end".to_string(),
                                    t: c.time(),
                                    ctx_id: c.id(),
                                    note: None,
                                    value: None,
                                });
                            }
                        },
                        BranchOptions::default(),
                    );

                    // Wait for both branches
                    let _ = a.await;
                    let _ = b.await;
                    let _ = ctx.wait(0.0).await;
                    log(&ctx, "joined");
                }
            },
            EngineConfig {
                bpm: 60.0,
                seed: "parallel_branchWait_ordering".to_string(),
                ..Default::default()
            },
        );

        runner.step_sec(0.5);

        let events = events.borrow();

        // Check that B_end comes before A_end (B waits 0.10, A waits 0.20)
        let b_end_idx = events.iter().position(|e| e.id == "B_end").unwrap();
        let a_end_idx = events.iter().position(|e| e.id == "A_end").unwrap();
        assert!(
            b_end_idx < a_end_idx,
            "B_end should come before A_end"
        );

        // Check times
        let b_end = events.iter().find(|e| e.id == "B_end").unwrap();
        let a_end = events.iter().find(|e| e.id == "A_end").unwrap();
        assert!((b_end.t - 0.10).abs() < 1e-6, "B_end should be at t=0.10");
        assert!((a_end.t - 0.20).abs() < 1e-6, "A_end should be at t=0.20");
    }

    /// Test: deterministic_tie_break_same_deadline_is_stable
    /// Tests that waiters at the same deadline resolve in deterministic order
    #[test]
    fn test_deterministic_tie_break_same_deadline() {
        clear_all_barriers();
        let events = Rc::new(RefCell::new(Vec::new()));
        let shared = Rc::new(RefCell::new(Vec::<String>::new()));

        let events_clone = events.clone();
        let shared_clone = shared.clone();
        let mut runner = OfflineRunner::new(
            move |ctx| {
                let events = events_clone.clone();
                let shared = shared_clone.clone();
                async move {
                    let log = |ctx: &Ctx, id: &str, note: Option<&str>| {
                        events.borrow_mut().push(LoggedEvent {
                            id: id.to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: note.map(|s| s.to_string()),
                            value: None,
                        });
                    };

                    log(&ctx, "start", None);

                    let events_a = events.clone();
                    let shared_a = shared.clone();
                    let a = ctx.branch_wait(
                        move |c| {
                            let events = events_a;
                            let shared = shared_a;
                            async move {
                                events.borrow_mut().push(LoggedEvent {
                                    id: "A_start".to_string(),
                                    t: c.time(),
                                    ctx_id: c.id(),
                                    note: None,
                                    value: None,
                                });
                                let _ = c.wait_sec(0.10).await;
                                shared.borrow_mut().push("A".to_string());
                                events.borrow_mut().push(LoggedEvent {
                                    id: "A_end".to_string(),
                                    t: c.time(),
                                    ctx_id: c.id(),
                                    note: None,
                                    value: None,
                                });
                            }
                        },
                        BranchOptions::default(),
                    );

                    let events_b = events.clone();
                    let shared_b = shared.clone();
                    let b = ctx.branch_wait(
                        move |c| {
                            let events = events_b;
                            let shared = shared_b;
                            async move {
                                events.borrow_mut().push(LoggedEvent {
                                    id: "B_start".to_string(),
                                    t: c.time(),
                                    ctx_id: c.id(),
                                    note: None,
                                    value: None,
                                });
                                let _ = c.wait_sec(0.10).await;
                                shared.borrow_mut().push("B".to_string());
                                events.borrow_mut().push(LoggedEvent {
                                    id: "B_end".to_string(),
                                    t: c.time(),
                                    ctx_id: c.id(),
                                    note: None,
                                    value: None,
                                });
                            }
                        },
                        BranchOptions::default(),
                    );

                    let _ = a.await;
                    let _ = b.await;
                    let _ = ctx.wait(0.0).await;

                    let order = shared.borrow().join("");
                    log(&ctx, "joined", Some(&order));

                    // Explicit check: A should be scheduled first, so it should resolve first
                    assert_eq!(
                        order, "AB",
                        "Expected shared order 'AB' but got '{}'",
                        order
                    );
                }
            },
            EngineConfig {
                bpm: 60.0,
                seed: "deterministic_tie_break".to_string(),
                ..Default::default()
            },
        );

        runner.step_sec(0.3);

        let events = events.borrow();

        // A_end should come before B_end due to deterministic tie-breaking
        let a_end_idx = events.iter().position(|e| e.id == "A_end").unwrap();
        let b_end_idx = events.iter().position(|e| e.id == "B_end").unwrap();
        assert!(
            a_end_idx < b_end_idx,
            "A_end (idx {}) should come before B_end (idx {}) due to deterministic tie-breaking",
            a_end_idx,
            b_end_idx
        );
    }

    /// Test: many_branchWaits_stress_join
    /// Stress test with many parallel branches
    #[test]
    fn test_many_branch_waits_stress_join() {
        clear_all_barriers();
        let events = Rc::new(RefCell::new(Vec::new()));

        let events_clone = events.clone();
        let mut runner = OfflineRunner::new(
            move |ctx| {
                let events = events_clone.clone();
                async move {
                    events.borrow_mut().push(LoggedEvent {
                        id: "start".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });

                    let mut handles = Vec::new();

                    // Create 20 branches with durations 0.02, 0.04, ..., 0.40
                    for i in 0..20 {
                        let d = 0.02 * (i as f64 + 1.0);
                        let events_i = events.clone();
                        let task_name = format!("task{}", i);

                        let handle = ctx.branch_wait(
                            move |c| {
                                let events = events_i;
                                let name = task_name;
                                async move {
                                    events.borrow_mut().push(LoggedEvent {
                                        id: format!("{}_start", name),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                    let _ = c.wait_sec(d).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: format!("{}_end", name),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );
                        handles.push(handle);
                    }

                    // Wait for all
                    for h in handles {
                        let _ = h.await;
                    }
                    let _ = ctx.wait(0.0).await;

                    events.borrow_mut().push(LoggedEvent {
                        id: "joined".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });
                }
            },
            EngineConfig {
                bpm: 60.0,
                seed: "many_branchWaits_stress".to_string(),
                ..Default::default()
            },
        );

        runner.step_sec(1.0);

        let events = events.borrow();

        // Check we have start, 20 task_start, 20 task_end, joined
        let start_count = events.iter().filter(|e| e.id.ends_with("_start")).count();
        let end_count = events.iter().filter(|e| e.id.ends_with("_end")).count();
        assert_eq!(start_count, 20, "Should have 20 start events");
        assert_eq!(end_count, 20, "Should have 20 end events");

        // Check that joined is last
        let last = events.last().unwrap();
        assert_eq!(last.id, "joined", "Last event should be 'joined'");

        // Check that task ends are in order of their duration
        let end_events: Vec<&LoggedEvent> = events.iter().filter(|e| e.id.ends_with("_end") && e.id != "joined").collect();
        for i in 1..end_events.len() {
            assert!(
                end_events[i].t >= end_events[i - 1].t - 1e-6,
                "End events should be in non-decreasing time order"
            );
        }
    }

    /// Test: microtask_yield_intermediate_scheduling
    /// Tests proper interleaving of waits
    #[test]
    fn test_microtask_yield_intermediate_scheduling() {
        clear_all_barriers();
        let events = Rc::new(RefCell::new(Vec::new()));

        let events_clone = events.clone();
        let mut runner = OfflineRunner::new(
            move |ctx| {
                let events = events_clone.clone();
                async move {
                    let events_a = events.clone();
                    let a = ctx.branch_wait(
                        move |c| {
                            let events = events_a;
                            async move {
                                let _ = c.wait_sec(0.10).await;
                                events.borrow_mut().push(LoggedEvent {
                                    id: "A@0.10".to_string(),
                                    t: c.time(),
                                    ctx_id: c.id(),
                                    note: None,
                                    value: None,
                                });
                                let _ = c.wait_sec(0.05).await;
                                events.borrow_mut().push(LoggedEvent {
                                    id: "A@0.15".to_string(),
                                    t: c.time(),
                                    ctx_id: c.id(),
                                    note: None,
                                    value: None,
                                });
                            }
                        },
                        BranchOptions::default(),
                    );

                    let events_b = events.clone();
                    let b = ctx.branch_wait(
                        move |c| {
                            let events = events_b;
                            async move {
                                let _ = c.wait_sec(0.12).await;
                                events.borrow_mut().push(LoggedEvent {
                                    id: "B@0.12".to_string(),
                                    t: c.time(),
                                    ctx_id: c.id(),
                                    note: None,
                                    value: None,
                                });
                            }
                        },
                        BranchOptions::default(),
                    );

                    let _ = a.await;
                    let _ = b.await;
                    let _ = ctx.wait(0.0).await;

                    events.borrow_mut().push(LoggedEvent {
                        id: "done".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });
                }
            },
            EngineConfig {
                bpm: 60.0,
                seed: "microtask_yield".to_string(),
                ..Default::default()
            },
        );

        runner.step_sec(0.5);

        let events = events.borrow();

        // Expected order: A@0.10, B@0.12, A@0.15, done
        assert_events_match(
            &events,
            &["A@0.10", "B@0.12", "A@0.15", "done"],
            "microtask_yield_intermediate_scheduling",
        );
    }

    /// Test: cancel_cascades_to_children_and_stops_ticks
    /// Tests that cancellation cascades properly
    #[test]
    fn test_cancel_cascades_to_children() {
        clear_all_barriers();
        let events = Rc::new(RefCell::new(Vec::new()));

        let events_clone = events.clone();
        let mut runner = OfflineRunner::new(
            move |ctx| {
                let events = events_clone.clone();
                async move {
                    events.borrow_mut().push(LoggedEvent {
                        id: "start".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });

                    let events_child = events.clone();
                    let events_grand = events.clone();

                    let mut handle = ctx.branch(
                        move |child| {
                            let events_c = events_child;
                            let events_g = events_grand;
                            async move {
                                // Grandchild
                                let events_grand_inner = events_g.clone();
                                child.branch(
                                    move |grand| {
                                        let events = events_grand_inner;
                                        async move {
                                            for i in 0..100 {
                                                // Stop if cancelled
                                                if grand.is_canceled() {
                                                    break;
                                                }
                                                events.borrow_mut().push(LoggedEvent {
                                                    id: format!("grand_tick_{}", i),
                                                    t: grand.time(),
                                                    ctx_id: grand.id(),
                                                    note: None,
                                                    value: None,
                                                });
                                                if grand.wait_sec(0.05).await.is_err() {
                                                    break;
                                                }
                                            }
                                        }
                                    },
                                    BranchOptions::default(),
                                );

                                // Child loop
                                for i in 0..100 {
                                    // Stop if cancelled
                                    if child.is_canceled() {
                                        break;
                                    }
                                    events_c.borrow_mut().push(LoggedEvent {
                                        id: format!("child_tick_{}", i),
                                        t: child.time(),
                                        ctx_id: child.id(),
                                        note: None,
                                        value: None,
                                    });
                                    if child.wait_sec(0.05).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        },
                        BranchOptions::default(),
                    );

                    // Take the future out and spawn it
                    if let Some(fut) = handle.take_future() {
                        // The future is spawned internally
                        std::mem::drop(fut);
                    }

                    let _ = ctx.wait_sec(0.23).await;
                    events.borrow_mut().push(LoggedEvent {
                        id: "cancel".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });
                    handle.cancel();

                    let _ = ctx.wait_sec(0.25).await;
                    events.borrow_mut().push(LoggedEvent {
                        id: "after_cancel_wait".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });
                }
            },
            EngineConfig {
                bpm: 60.0,
                seed: "cancel_cascades".to_string(),
                ..Default::default()
            },
        );

        runner.step_sec(1.0);

        let events = events.borrow();

        // Count how many child and grand ticks we got
        let child_ticks = events.iter().filter(|e| e.id.starts_with("child_tick_")).count();
        let grand_ticks = events.iter().filter(|e| e.id.starts_with("grand_tick_")).count();

        // Should have limited ticks (cancel at 0.23, each tick is 0.05)
        // That's about 4-5 ticks max
        assert!(
            child_ticks < 10,
            "Child should have stopped after cancel, got {} ticks",
            child_ticks
        );
        assert!(
            grand_ticks < 10,
            "Grand should have stopped after cancel, got {} ticks",
            grand_ticks
        );

        // Check cancel and after_cancel_wait are present
        assert!(
            events.iter().any(|e| e.id == "cancel"),
            "Should have cancel event"
        );
        assert!(
            events.iter().any(|e| e.id == "after_cancel_wait"),
            "Should have after_cancel_wait event"
        );
    }

    /// Test: barrier_loop_sync_melodyA_waits_for_melodyB
    /// Tests barrier synchronization
    #[test]
    fn test_barrier_loop_sync() {
        clear_all_barriers();
        let events = Rc::new(RefCell::new(Vec::new()));

        let events_clone = events.clone();
        let mut runner = OfflineRunner::new(
            move |ctx| {
                let events = events_clone.clone();
                async move {
                    let key = "barrier_loop_sync_melodyA_waits_for_melodyB";

                    // Melody B - producer
                    let events_b = events.clone();
                    ctx.branch(
                        move |b| {
                            let events = events_b;
                            async move {
                                for cycle in 0..3 {
                                    start_barrier(key, &b);
                                    events.borrow_mut().push(LoggedEvent {
                                        id: format!("B_start_{}", cycle),
                                        t: b.time(),
                                        ctx_id: b.id(),
                                        note: None,
                                        value: None,
                                    });
                                    let _ = b.wait_sec(0.30).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: format!("B_end_{}", cycle),
                                        t: b.time(),
                                        ctx_id: b.id(),
                                        note: None,
                                        value: None,
                                    });
                                    resolve_barrier(key, &b);
                                }
                            }
                        },
                        BranchOptions::default(),
                    );

                    // Melody A - consumer
                    let events_a = events.clone();
                    let _ = ctx
                        .branch_wait(
                            move |a| {
                                let events = events_a;
                                async move {
                                    for cycle in 0..3 {
                                        events.borrow_mut().push(LoggedEvent {
                                            id: format!("A_start_{}", cycle),
                                            t: a.time(),
                                            ctx_id: a.id(),
                                            note: None,
                                            value: None,
                                        });
                                        let _ = a.wait_sec(0.20).await;
                                        events.borrow_mut().push(LoggedEvent {
                                            id: format!("A_end_{}", cycle),
                                            t: a.time(),
                                            ctx_id: a.id(),
                                            note: None,
                                            value: None,
                                        });
                                        let _ = await_barrier(key, &a).await;
                                        events.borrow_mut().push(LoggedEvent {
                                            id: format!("A_synced_{}", cycle),
                                            t: a.time(),
                                            ctx_id: a.id(),
                                            note: None,
                                            value: None,
                                        });
                                    }
                                }
                            },
                            BranchOptions::default(),
                        )
                        .await;
                }
            },
            EngineConfig {
                bpm: 60.0,
                seed: "barrier_loop_sync".to_string(),
                ..Default::default()
            },
        );

        runner.step_sec(2.0);

        let events = events.borrow();

        // Check that A_synced events occur after corresponding B_end events
        for cycle in 0..3 {
            let b_end = events
                .iter()
                .find(|e| e.id == format!("B_end_{}", cycle))
                .expect(&format!("Should have B_end_{}", cycle));
            let a_synced = events
                .iter()
                .find(|e| e.id == format!("A_synced_{}", cycle))
                .expect(&format!("Should have A_synced_{}", cycle));

            assert!(
                a_synced.t >= b_end.t - 1e-6,
                "A_synced_{} (t={}) should be >= B_end_{} (t={})",
                cycle,
                a_synced.t,
                cycle,
                b_end.t
            );
        }
    }

    /// Test: barrier_race_resolve_then_immediate_restart
    /// Tests barrier restart behavior
    #[test]
    fn test_barrier_race_resolve_restart() {
        clear_all_barriers();
        let events = Rc::new(RefCell::new(Vec::new()));

        let events_clone = events.clone();
        let mut runner = OfflineRunner::new(
            move |ctx| {
                let events = events_clone.clone();
                async move {
                    let key = "barrier_race_resolve_then_immediate_restart";

                    // Producer
                    let events_b = events.clone();
                    ctx.branch(
                        move |b| {
                            let events = events_b;
                            async move {
                                start_barrier(key, &b);
                                let _ = b.wait_sec(0.10).await;
                                resolve_barrier(key, &b);
                                events.borrow_mut().push(LoggedEvent {
                                    id: "B_resolved_0.10".to_string(),
                                    t: b.time(),
                                    ctx_id: b.id(),
                                    note: None,
                                    value: None,
                                });

                                start_barrier(key, &b);
                                events.borrow_mut().push(LoggedEvent {
                                    id: "B_restarted_0.10".to_string(),
                                    t: b.time(),
                                    ctx_id: b.id(),
                                    note: None,
                                    value: None,
                                });

                                let _ = b.wait_sec(0.10).await;
                                resolve_barrier(key, &b);
                                events.borrow_mut().push(LoggedEvent {
                                    id: "B_resolved_0.20".to_string(),
                                    t: b.time(),
                                    ctx_id: b.id(),
                                    note: None,
                                    value: None,
                                });
                            }
                        },
                        BranchOptions::default(),
                    );

                    // Consumer
                    let events_a = events.clone();
                    let _ = ctx
                        .branch_wait(
                            move |a| {
                                let events = events_a;
                                async move {
                                    let _ = a.wait_sec(0.10).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A_before_await_0.10".to_string(),
                                        t: a.time(),
                                        ctx_id: a.id(),
                                        note: None,
                                        value: None,
                                    });
                                    let _ = await_barrier(key, &a).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A_after_await_0.10".to_string(),
                                        t: a.time(),
                                        ctx_id: a.id(),
                                        note: None,
                                        value: None,
                                    });

                                    let _ = a.wait_sec(0.10).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A_before_await_0.20".to_string(),
                                        t: a.time(),
                                        ctx_id: a.id(),
                                        note: None,
                                        value: None,
                                    });
                                    let _ = await_barrier(key, &a).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A_after_await_0.20".to_string(),
                                        t: a.time(),
                                        ctx_id: a.id(),
                                        note: None,
                                        value: None,
                                    });
                                }
                            },
                            BranchOptions::default(),
                        )
                        .await;
                }
            },
            EngineConfig {
                bpm: 60.0,
                seed: "barrier_race".to_string(),
                ..Default::default()
            },
        );

        runner.step_sec(1.0);

        let events = events.borrow();

        // Check that both barrier cycles completed
        assert!(
            events.iter().any(|e| e.id == "A_after_await_0.10"),
            "Should have A_after_await_0.10"
        );
        assert!(
            events.iter().any(|e| e.id == "A_after_await_0.20"),
            "Should have A_after_await_0.20"
        );
    }

    /// Test: tempo_change_shared_retimes_beat_wait
    /// Tests that tempo changes affect beat waits
    #[test]
    fn test_tempo_change_shared_retimes() {
        clear_all_barriers();
        let events = Rc::new(RefCell::new(Vec::new()));

        let events_clone = events.clone();
        let mut runner = OfflineRunner::new(
            move |ctx| {
                let events = events_clone.clone();
                async move {
                    // Branch to change tempo at t=0.50
                    let ctx_clone = ctx.clone();
                    let events_ctl = events.clone();
                    ctx.branch(
                        move |ctl| {
                            let ctx = ctx_clone;
                            let events = events_ctl;
                            async move {
                                let _ = ctl.wait_sec(0.50).await;
                                ctx.set_bpm(480.0);
                                events.borrow_mut().push(LoggedEvent {
                                    id: "tempo_set_480@0.50".to_string(),
                                    t: ctl.time(),
                                    ctx_id: ctl.id(),
                                    note: None,
                                    value: None,
                                });
                            }
                        },
                        BranchOptions::default(),
                    );

                    // Wait for 4 beats at 240 BPM
                    // At 240 BPM: 4 beats/sec, so 4 beats = 1.0 sec
                    // But tempo changes to 480 BPM at t=0.50
                    // At t=0.50, we're at 240 BPM * 0.50 sec / 60 = 2 beats
                    // Need 2 more beats at 480 BPM = 2 * 60/480 = 0.25 sec
                    // Total: 0.50 + 0.25 = 0.75 sec
                    let _ = ctx.wait(4.0).await;
                    events.borrow_mut().push(LoggedEvent {
                        id: "wait4beats_done".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });
                }
            },
            EngineConfig {
                bpm: 240.0,
                seed: "tempo_change_shared".to_string(),
                ..Default::default()
            },
        );

        runner.step_sec(2.0);

        let events = events.borrow();

        let done = events.iter().find(|e| e.id == "wait4beats_done").unwrap();
        // Should complete at approximately t=0.75
        assert!(
            (done.t - 0.75).abs() < 0.01,
            "wait4beats_done should be at ~0.75 sec, got {}",
            done.t
        );
    }

    /// Test: tempo_clone_child_isolated_from_root_changes
    /// Tests that cloned tempo is isolated
    #[test]
    fn test_tempo_clone_isolated() {
        clear_all_barriers();
        let events = Rc::new(RefCell::new(Vec::new()));

        let events_clone = events.clone();
        let mut runner = OfflineRunner::new(
            move |ctx| {
                let events = events_clone.clone();
                async move {
                    // Branch to change root tempo at t=0.50
                    let ctx_clone = ctx.clone();
                    let events_ctl = events.clone();
                    ctx.branch(
                        move |ctl| {
                            let ctx = ctx_clone;
                            let events = events_ctl;
                            async move {
                                let _ = ctl.wait_sec(0.50).await;
                                ctx.set_bpm(480.0);
                                events.borrow_mut().push(LoggedEvent {
                                    id: "root_tempo_set_480@0.50".to_string(),
                                    t: ctl.time(),
                                    ctx_id: ctl.id(),
                                    note: None,
                                    value: None,
                                });
                            }
                        },
                        BranchOptions::default(),
                    );

                    // Child with cloned tempo - should NOT be affected by root tempo change
                    let events_child = events.clone();
                    let _ = ctx
                        .branch_wait(
                            move |child| {
                                let events = events_child;
                                async move {
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "child_start".to_string(),
                                        t: child.time(),
                                        ctx_id: child.id(),
                                        note: None,
                                        value: None,
                                    });
                                    // 4 beats at 240 BPM = 1.0 sec (unchanged because cloned)
                                    let _ = child.wait(4.0).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "child_done".to_string(),
                                        t: child.time(),
                                        ctx_id: child.id(),
                                        note: None,
                                        value: None,
                                    });
                                }
                            },
                            BranchOptions {
                                tempo: TempoMode::Cloned,
                                ..Default::default()
                            },
                        )
                        .await;
                }
            },
            EngineConfig {
                bpm: 240.0,
                seed: "tempo_clone_isolated".to_string(),
                ..Default::default()
            },
        );

        runner.step_sec(2.0);

        let events = events.borrow();

        let child_done = events.iter().find(|e| e.id == "child_done").unwrap();
        // Should complete at t=1.0 (4 beats at 240 BPM, unaffected by root change)
        assert!(
            (child_done.t - 1.0).abs() < 0.01,
            "child_done should be at ~1.0 sec (isolated tempo), got {}",
            child_done.t
        );
    }

    /// Test: wait0_is_valid_sync_point
    /// Tests that wait(0) works as a sync point
    #[test]
    fn test_wait0_sync_point() {
        clear_all_barriers();
        let events = Rc::new(RefCell::new(Vec::new()));

        let events_clone = events.clone();
        let mut runner = OfflineRunner::new(
            move |ctx| {
                let events = events_clone.clone();
                async move {
                    let events_p1 = events.clone();
                    let p1 = ctx.branch_wait(
                        move |c| {
                            let events = events_p1;
                            async move {
                                let _ = c.wait_sec(0.10).await;
                                events.borrow_mut().push(LoggedEvent {
                                    id: "p1_done".to_string(),
                                    t: c.time(),
                                    ctx_id: c.id(),
                                    note: None,
                                    value: None,
                                });
                            }
                        },
                        BranchOptions::default(),
                    );

                    let events_p2 = events.clone();
                    let p2 = ctx.branch_wait(
                        move |c| {
                            let events = events_p2;
                            async move {
                                let _ = c.wait_sec(0.20).await;
                                events.borrow_mut().push(LoggedEvent {
                                    id: "p2_done".to_string(),
                                    t: c.time(),
                                    ctx_id: c.id(),
                                    note: None,
                                    value: None,
                                });
                            }
                        },
                        BranchOptions::default(),
                    );

                    let _ = p1.await;
                    let _ = p2.await;

                    events.borrow_mut().push(LoggedEvent {
                        id: "after_all_immediate".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });

                    let _ = ctx.wait(0.0).await;

                    events.borrow_mut().push(LoggedEvent {
                        id: "after_all_after_wait0".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });
                }
            },
            EngineConfig {
                bpm: 60.0,
                seed: "wait0_sync".to_string(),
                ..Default::default()
            },
        );

        runner.step_sec(1.0);

        let events = events.borrow();

        // Check both after_all events are present
        assert!(
            events.iter().any(|e| e.id == "after_all_immediate"),
            "Should have after_all_immediate"
        );
        assert!(
            events.iter().any(|e| e.id == "after_all_after_wait0"),
            "Should have after_all_after_wait0"
        );

        // Both should be at t=0.20 (max of p1 and p2)
        let immediate = events
            .iter()
            .find(|e| e.id == "after_all_immediate")
            .unwrap();
        let after_wait0 = events
            .iter()
            .find(|e| e.id == "after_all_after_wait0")
            .unwrap();

        assert!(
            (immediate.t - 0.20).abs() < 1e-6,
            "after_all_immediate should be at t=0.20"
        );
        assert!(
            (after_wait0.t - 0.20).abs() < 1e-6,
            "after_all_after_wait0 should be at t=0.20"
        );
    }

    /// Test: frame_like_loop_waitSec_60fpsish
    /// Tests a frame-like loop
    #[test]
    fn test_frame_like_loop() {
        clear_all_barriers();
        let events = Rc::new(RefCell::new(Vec::new()));

        let events_clone = events.clone();
        let mut runner = OfflineRunner::new(
            move |ctx| {
                let events = events_clone.clone();
                async move {
                    events.borrow_mut().push(LoggedEvent {
                        id: "start".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });

                    let dt = 1.0 / 60.0;

                    for i in 0..30 {
                        let _ = ctx.wait_sec(dt).await;
                        events.borrow_mut().push(LoggedEvent {
                            id: format!("frame_{}", i),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                    }

                    events.borrow_mut().push(LoggedEvent {
                        id: "done".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });
                }
            },
            EngineConfig {
                bpm: 60.0,
                seed: "frame_like_loop".to_string(),
                ..Default::default()
            },
        );

        runner.step_sec(1.0);

        let events = events.borrow();

        // Should have start, 30 frames, done = 32 events
        assert_eq!(events.len(), 32, "Should have 32 events");
        assert_eq!(events.first().unwrap().id, "start");
        assert_eq!(events.last().unwrap().id, "done");

        // Check times are roughly correct
        let done = events.last().unwrap();
        let expected_time = 30.0 / 60.0;
        assert!(
            (done.t - expected_time).abs() < 1e-6,
            "done should be at ~{} sec, got {}",
            expected_time,
            done.t
        );
    }

    /// Test: waitSec_negative_and_NaN_are_safe
    /// Tests that negative and NaN waits are handled safely
    #[test]
    fn test_wait_sec_negative_and_nan_safe() {
        clear_all_barriers();
        let events = Rc::new(RefCell::new(Vec::new()));

        let events_clone = events.clone();
        let mut runner = OfflineRunner::new(
            move |ctx| {
                let events = events_clone.clone();
                async move {
                    events.borrow_mut().push(LoggedEvent {
                        id: "start".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });

                    let _ = ctx.wait_sec(-0.10).await;
                    events.borrow_mut().push(LoggedEvent {
                        id: "after_neg_wait".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });

                    let _ = ctx.wait_sec(f64::NAN).await;
                    events.borrow_mut().push(LoggedEvent {
                        id: "after_nan_wait".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });

                    let _ = ctx.wait_sec(0.10).await;
                    events.borrow_mut().push(LoggedEvent {
                        id: "after_0.10".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });
                }
            },
            EngineConfig {
                bpm: 60.0,
                seed: "wait_sec_edge_cases".to_string(),
                ..Default::default()
            },
        );

        runner.step_sec(0.5);

        let events = events.borrow();

        assert_events_match(
            &events,
            &["start", "after_neg_wait", "after_nan_wait", "after_0.10"],
            "waitSec_negative_and_NaN_are_safe",
        );

        // All should have valid times
        for e in events.iter() {
            assert!(e.t.is_finite(), "Event {} should have finite time", e.id);
        }
    }

    /// Test: seeded_rng_forked_branches_repeatable
    /// Tests deterministic RNG with forked branches
    #[test]
    fn test_seeded_rng_forked_branches() {
        clear_all_barriers();

        // Run twice and compare
        fn run_once() -> Vec<(String, Option<f64>)> {
            let events = Rc::new(RefCell::new(Vec::new()));

            let events_clone = events.clone();
            let mut runner = OfflineRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    async move {
                        events.borrow_mut().push(LoggedEvent {
                            id: "start".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });

                        let events_a = events.clone();
                        let a = ctx.branch_wait(
                            move |c| {
                                let events = events_a;
                                async move {
                                    let _ = c.wait_sec(0.05).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A_r0".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: Some(c.random()),
                                    });
                                    let _ = c.wait_sec(0.05).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A_r1".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: Some(c.random()),
                                    });
                                    let _ = c.wait_sec(0.05).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A_r2".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: Some(c.random()),
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        let events_b = events.clone();
                        let b = ctx.branch_wait(
                            move |c| {
                                let events = events_b;
                                async move {
                                    let _ = c.wait_sec(0.10).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "B_r0".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: Some(c.random()),
                                    });
                                    let _ = c.wait_sec(0.05).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "B_r1".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: Some(c.random()),
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        let _ = a.await;
                        let _ = b.await;
                        let _ = ctx.wait(0.0).await;

                        events.borrow_mut().push(LoggedEvent {
                            id: "joined".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                    }
                },
                EngineConfig {
                    bpm: 60.0,
                    seed: "rng_forked_demo_seed".to_string(),
                    ..Default::default()
                },
            );

            runner.step_sec(0.5);

            let result: Vec<(String, Option<f64>)> = events
                .borrow()
                .iter()
                .map(|e| (e.id.clone(), e.value))
                .collect();
            result
        }

        let run1 = run_once();
        let run2 = run_once();

        // Compare runs
        assert_eq!(run1.len(), run2.len(), "Runs should have same number of events");
        for (i, ((id1, v1), (id2, v2))) in run1.iter().zip(run2.iter()).enumerate() {
            assert_eq!(id1, id2, "Event {} ID mismatch", i);
            match (v1, v2) {
                (Some(a), Some(b)) => {
                    assert!(
                        (a - b).abs() < 1e-15,
                        "Event {} value mismatch: {} vs {}",
                        i,
                        a,
                        b
                    );
                }
                (None, None) => {}
                _ => panic!("Event {} value presence mismatch", i),
            }
        }
    }

    /// Test: seeded_rng_shared_stream_deterministic_under_tie_break
    /// Tests deterministic RNG with shared stream
    #[test]
    fn test_seeded_rng_shared_stream() {
        clear_all_barriers();

        fn run_once() -> Vec<(String, Option<f64>)> {
            let events = Rc::new(RefCell::new(Vec::new()));

            let events_clone = events.clone();
            let mut runner = OfflineRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    async move {
                        events.borrow_mut().push(LoggedEvent {
                            id: "start".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });

                        let events_a = events.clone();
                        let a = ctx.branch_wait(
                            move |c| {
                                let events = events_a;
                                async move {
                                    let _ = c.wait_sec(0.05).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A_draw".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: Some(c.random()),
                                    });
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
                                let events = events_b;
                                async move {
                                    let _ = c.wait_sec(0.05).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "B_draw".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: Some(c.random()),
                                    });
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

                        events.borrow_mut().push(LoggedEvent {
                            id: "joined".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                    }
                },
                EngineConfig {
                    bpm: 60.0,
                    seed: "rng_shared_demo_seed".to_string(),
                    ..Default::default()
                },
            );

            runner.step_sec(0.5);

            let result: Vec<(String, Option<f64>)> = events
                .borrow()
                .iter()
                .map(|e| (e.id.clone(), e.value))
                .collect();
            result
        }

        let run1 = run_once();
        let run2 = run_once();

        // Compare runs
        assert_eq!(run1.len(), run2.len(), "Runs should have same number of events");
        for (i, ((id1, v1), (id2, v2))) in run1.iter().zip(run2.iter()).enumerate() {
            assert_eq!(id1, id2, "Event {} ID mismatch", i);
            match (v1, v2) {
                (Some(a), Some(b)) => {
                    assert!(
                        (a - b).abs() < 1e-15,
                        "Event {} value mismatch: {} vs {}",
                        i,
                        a,
                        b
                    );
                }
                (None, None) => {}
                _ => panic!("Event {} value presence mismatch", i),
            }
        }

        // A_draw should come before B_draw due to deterministic tie-breaking
        let a_idx = run1.iter().position(|(id, _)| id == "A_draw").unwrap();
        let b_idx = run1.iter().position(|(id, _)| id == "B_draw").unwrap();
        assert!(
            a_idx < b_idx,
            "A_draw should come before B_draw due to deterministic tie-breaking"
        );
    }

    /// Test: beat wait basic
    /// Tests basic beat-based waiting
    #[test]
    fn test_beat_wait_basic() {
        clear_all_barriers();
        let events = Rc::new(RefCell::new(Vec::new()));

        let events_clone = events.clone();
        let mut runner = OfflineRunner::new(
            move |ctx| {
                let events = events_clone.clone();
                async move {
                    events.borrow_mut().push(LoggedEvent {
                        id: "start".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });

                    // At 120 BPM, 2 beats = 1 second
                    let _ = ctx.wait(2.0).await;
                    events.borrow_mut().push(LoggedEvent {
                        id: "after_2_beats".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });

                    let _ = ctx.wait(2.0).await;
                    events.borrow_mut().push(LoggedEvent {
                        id: "after_4_beats".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });
                }
            },
            EngineConfig {
                bpm: 120.0,
                seed: "beat_wait_basic".to_string(),
                ..Default::default()
            },
        );

        runner.step_sec(3.0);

        let events = events.borrow();

        assert_events_match(
            &events,
            &["start", "after_2_beats", "after_4_beats"],
            "beat_wait_basic",
        );

        // Check times
        assert_time_approx(&events, 0, 0.0, 1e-6, "beat_wait_basic");
        assert_time_approx(&events, 1, 1.0, 1e-6, "beat_wait_basic"); // 2 beats at 120 BPM = 1 sec
        assert_time_approx(&events, 2, 2.0, 1e-6, "beat_wait_basic"); // 4 beats at 120 BPM = 2 sec
    }

    /// Test: prog_time and prog_beats
    /// Tests program time and beats tracking
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
        let events = Rc::new(RefCell::new(Vec::new()));

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

    // ==================== DUAL-MODE TESTS (Offline vs Realtime) ====================
    // These tests run the same scenario in both offline and realtime modes,
    // then compare the event logs to verify deterministic equivalence.

    /// Test: dual_mode_sequential_waitSec
    /// Verifies that sequential waits produce identical events in both modes
    #[test]
    fn test_dual_mode_sequential_wait_sec() {
        clear_all_barriers();

        let test_name = "dual_mode_sequential_waitSec";

        // Run offline
        let offline_events = {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_clone = events.clone();
            let mut runner = OfflineRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    async move {
                        events.borrow_mut().push(LoggedEvent {
                            id: "start".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                        let _ = ctx.wait_sec(0.05).await;
                        events.borrow_mut().push(LoggedEvent {
                            id: "t=0.05".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                        let _ = ctx.wait_sec(0.10).await;
                        events.borrow_mut().push(LoggedEvent {
                            id: "t=0.15".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                        let _ = ctx.wait_sec(0.20).await;
                        events.borrow_mut().push(LoggedEvent {
                            id: "t=0.35".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                    }
                },
                EngineConfig {
                    bpm: 60.0,
                    seed: test_name.to_string(),
                    ..Default::default()
                },
            );
            runner.step_sec(0.5);
            { let v = events.borrow().clone(); v }
        };

        clear_all_barriers();

        // Run realtime (with high rate for speed)
        let realtime_events = {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_clone = events.clone();
            let mut runner = RealtimeRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    async move {
                        events.borrow_mut().push(LoggedEvent {
                            id: "start".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                        let _ = ctx.wait_sec(0.05).await;
                        events.borrow_mut().push(LoggedEvent {
                            id: "t=0.05".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                        let _ = ctx.wait_sec(0.10).await;
                        events.borrow_mut().push(LoggedEvent {
                            id: "t=0.15".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                        let _ = ctx.wait_sec(0.20).await;
                        events.borrow_mut().push(LoggedEvent {
                            id: "t=0.35".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                    }
                },
                EngineConfig {
                    bpm: 60.0,
                    seed: test_name.to_string(),
                    rate: REALTIME_RATE,
                    ..Default::default()
                },
            );
            runner.run_until_complete();
            { let v = events.borrow().clone(); v }
        };

        compare_deterministic_runs(
            test_name,
            &offline_events,
            &realtime_events,
            &TestTolerances::default(),
        );
    }

    /// Test: dual_mode_parallel_branchWait
    /// Verifies that parallel branches produce identical events in both modes
    #[test]
    fn test_dual_mode_parallel_branch_wait() {
        clear_all_barriers();

        let test_name = "dual_mode_parallel_branchWait";

        // Run offline
        let offline_events = {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_clone = events.clone();
            let mut runner = OfflineRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    async move {
                        events.borrow_mut().push(LoggedEvent {
                            id: "start".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });

                        let events_a = events.clone();
                        let a = ctx.branch_wait(
                            move |c| {
                                let events = events_a;
                                async move {
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A_start".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                    let _ = c.wait_sec(0.20).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A_end".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        let events_b = events.clone();
                        let b = ctx.branch_wait(
                            move |c| {
                                let events = events_b;
                                async move {
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "B_start".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                    let _ = c.wait_sec(0.10).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "B_end".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        let _ = a.await;
                        let _ = b.await;
                        let _ = ctx.wait(0.0).await;
                        events.borrow_mut().push(LoggedEvent {
                            id: "joined".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                    }
                },
                EngineConfig {
                    bpm: 60.0,
                    seed: test_name.to_string(),
                    ..Default::default()
                },
            );
            runner.step_sec(0.5);
            { let v = events.borrow().clone(); v }
        };

        clear_all_barriers();

        // Run realtime
        let realtime_events = {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_clone = events.clone();
            let mut runner = RealtimeRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    async move {
                        events.borrow_mut().push(LoggedEvent {
                            id: "start".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });

                        let events_a = events.clone();
                        let a = ctx.branch_wait(
                            move |c| {
                                let events = events_a;
                                async move {
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A_start".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                    let _ = c.wait_sec(0.20).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A_end".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        let events_b = events.clone();
                        let b = ctx.branch_wait(
                            move |c| {
                                let events = events_b;
                                async move {
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "B_start".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                    let _ = c.wait_sec(0.10).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "B_end".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        let _ = a.await;
                        let _ = b.await;
                        let _ = ctx.wait(0.0).await;
                        events.borrow_mut().push(LoggedEvent {
                            id: "joined".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                    }
                },
                EngineConfig {
                    bpm: 60.0,
                    seed: test_name.to_string(),
                    rate: REALTIME_RATE,
                    ..Default::default()
                },
            );
            runner.run_until_complete();
            { let v = events.borrow().clone(); v }
        };

        compare_deterministic_runs(
            test_name,
            &offline_events,
            &realtime_events,
            &TestTolerances::default(),
        );
    }

    /// Test: dual_mode_deterministic_tie_break
    /// Verifies that tie-breaking is identical in both modes
    #[test]
    fn test_dual_mode_deterministic_tie_break() {
        clear_all_barriers();

        let test_name = "dual_mode_deterministic_tie_break";

        // Run offline
        let (offline_events, offline_shared) = {
            let events = Rc::new(RefCell::new(Vec::new()));
            let shared = Rc::new(RefCell::new(Vec::<String>::new()));
            let events_clone = events.clone();
            let shared_clone = shared.clone();
            let mut runner = OfflineRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    let shared = shared_clone.clone();
                    async move {
                        events.borrow_mut().push(LoggedEvent {
                            id: "start".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });

                        let events_a = events.clone();
                        let shared_a = shared.clone();
                        let a = ctx.branch_wait(
                            move |c| {
                                let events = events_a;
                                let shared = shared_a;
                                async move {
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A_start".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                    let _ = c.wait_sec(0.10).await;
                                    shared.borrow_mut().push("A".to_string());
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A_end".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        let events_b = events.clone();
                        let shared_b = shared.clone();
                        let b = ctx.branch_wait(
                            move |c| {
                                let events = events_b;
                                let shared = shared_b;
                                async move {
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "B_start".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                    let _ = c.wait_sec(0.10).await;
                                    shared.borrow_mut().push("B".to_string());
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "B_end".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        let _ = a.await;
                        let _ = b.await;
                        let _ = ctx.wait(0.0).await;

                        let order = shared.borrow().join("");
                        events.borrow_mut().push(LoggedEvent {
                            id: "joined".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: Some(order),
                            value: None,
                        });
                    }
                },
                EngineConfig {
                    bpm: 60.0,
                    seed: test_name.to_string(),
                    ..Default::default()
                },
            );
            runner.step_sec(0.3);
            let v = events.borrow().clone();
            let s = shared.borrow().join("");
            (v, s)
        };

        clear_all_barriers();

        // Run realtime
        let (realtime_events, realtime_shared) = {
            let events = Rc::new(RefCell::new(Vec::new()));
            let shared = Rc::new(RefCell::new(Vec::<String>::new()));
            let events_clone = events.clone();
            let shared_clone = shared.clone();
            let mut runner = RealtimeRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    let shared = shared_clone.clone();
                    async move {
                        events.borrow_mut().push(LoggedEvent {
                            id: "start".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });

                        let events_a = events.clone();
                        let shared_a = shared.clone();
                        let a = ctx.branch_wait(
                            move |c| {
                                let events = events_a;
                                let shared = shared_a;
                                async move {
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A_start".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                    let _ = c.wait_sec(0.10).await;
                                    shared.borrow_mut().push("A".to_string());
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A_end".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        let events_b = events.clone();
                        let shared_b = shared.clone();
                        let b = ctx.branch_wait(
                            move |c| {
                                let events = events_b;
                                let shared = shared_b;
                                async move {
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "B_start".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                    let _ = c.wait_sec(0.10).await;
                                    shared.borrow_mut().push("B".to_string());
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "B_end".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        let _ = a.await;
                        let _ = b.await;
                        let _ = ctx.wait(0.0).await;

                        let order = shared.borrow().join("");
                        events.borrow_mut().push(LoggedEvent {
                            id: "joined".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: Some(order),
                            value: None,
                        });
                    }
                },
                EngineConfig {
                    bpm: 60.0,
                    seed: test_name.to_string(),
                    rate: REALTIME_RATE,
                    ..Default::default()
                },
            );
            runner.run_until_complete();
            let v = events.borrow().clone();
            let s = shared.borrow().join("");
            (v, s)
        };

        // Both modes should produce "AB" (A scheduled first)
        assert_eq!(offline_shared, "AB", "Offline should produce AB ordering");
        assert_eq!(realtime_shared, "AB", "Realtime should produce AB ordering");

        compare_deterministic_runs(
            test_name,
            &offline_events,
            &realtime_events,
            &TestTolerances::default(),
        );
    }

    /// Test: dual_mode_microtask_yield
    /// Verifies proper interleaving of waits in both modes
    #[test]
    fn test_dual_mode_microtask_yield() {
        clear_all_barriers();

        let test_name = "dual_mode_microtask_yield";

        // Run offline
        let offline_events = {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_clone = events.clone();
            let mut runner = OfflineRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    async move {
                        let events_a = events.clone();
                        let a = ctx.branch_wait(
                            move |c| {
                                let events = events_a;
                                async move {
                                    let _ = c.wait_sec(0.10).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A@0.10".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                    let _ = c.wait_sec(0.05).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A@0.15".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        let events_b = events.clone();
                        let b = ctx.branch_wait(
                            move |c| {
                                let events = events_b;
                                async move {
                                    let _ = c.wait_sec(0.12).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "B@0.12".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        let _ = a.await;
                        let _ = b.await;
                        let _ = ctx.wait(0.0).await;

                        events.borrow_mut().push(LoggedEvent {
                            id: "done".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                    }
                },
                EngineConfig {
                    bpm: 60.0,
                    seed: test_name.to_string(),
                    ..Default::default()
                },
            );
            runner.step_sec(0.5);
            { let v = events.borrow().clone(); v }
        };

        clear_all_barriers();

        // Run realtime
        let realtime_events = {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_clone = events.clone();
            let mut runner = RealtimeRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    async move {
                        let events_a = events.clone();
                        let a = ctx.branch_wait(
                            move |c| {
                                let events = events_a;
                                async move {
                                    let _ = c.wait_sec(0.10).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A@0.10".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                    let _ = c.wait_sec(0.05).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A@0.15".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        let events_b = events.clone();
                        let b = ctx.branch_wait(
                            move |c| {
                                let events = events_b;
                                async move {
                                    let _ = c.wait_sec(0.12).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "B@0.12".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        let _ = a.await;
                        let _ = b.await;
                        let _ = ctx.wait(0.0).await;

                        events.borrow_mut().push(LoggedEvent {
                            id: "done".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                    }
                },
                EngineConfig {
                    bpm: 60.0,
                    seed: test_name.to_string(),
                    rate: REALTIME_RATE,
                    ..Default::default()
                },
            );
            runner.run_until_complete();
            { let v = events.borrow().clone(); v }
        };

        compare_deterministic_runs(
            test_name,
            &offline_events,
            &realtime_events,
            &TestTolerances::default(),
        );
    }

    /// Test: dual_mode_beat_wait
    /// Verifies that beat-based waits work identically in both modes
    #[test]
    fn test_dual_mode_beat_wait() {
        clear_all_barriers();

        let test_name = "dual_mode_beat_wait";

        // Run offline
        let offline_events = {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_clone = events.clone();
            let mut runner = OfflineRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    async move {
                        events.borrow_mut().push(LoggedEvent {
                            id: "start".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });

                        // At 120 BPM, 2 beats = 1 second
                        let _ = ctx.wait(2.0).await;
                        events.borrow_mut().push(LoggedEvent {
                            id: "after_2_beats".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });

                        let _ = ctx.wait(2.0).await;
                        events.borrow_mut().push(LoggedEvent {
                            id: "after_4_beats".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                    }
                },
                EngineConfig {
                    bpm: 120.0,
                    seed: test_name.to_string(),
                    ..Default::default()
                },
            );
            runner.step_sec(3.0);
            { let v = events.borrow().clone(); v }
        };

        clear_all_barriers();

        // Run realtime
        let realtime_events = {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_clone = events.clone();
            let mut runner = RealtimeRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    async move {
                        events.borrow_mut().push(LoggedEvent {
                            id: "start".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });

                        // At 120 BPM, 2 beats = 1 second
                        let _ = ctx.wait(2.0).await;
                        events.borrow_mut().push(LoggedEvent {
                            id: "after_2_beats".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });

                        let _ = ctx.wait(2.0).await;
                        events.borrow_mut().push(LoggedEvent {
                            id: "after_4_beats".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                    }
                },
                EngineConfig {
                    bpm: 120.0,
                    seed: test_name.to_string(),
                    rate: REALTIME_RATE,
                    ..Default::default()
                },
            );
            runner.run_until_complete();
            { let v = events.borrow().clone(); v }
        };

        compare_deterministic_runs(
            test_name,
            &offline_events,
            &realtime_events,
            &TestTolerances::default(),
        );
    }

    /// Test: dual_mode_tempo_change_retimes
    /// Verifies that tempo changes work identically in both modes
    #[test]
    fn test_dual_mode_tempo_change_retimes() {
        clear_all_barriers();

        let test_name = "dual_mode_tempo_change_retimes";

        // Run offline
        let offline_events = {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_clone = events.clone();
            let mut runner = OfflineRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    async move {
                        // Branch to change tempo at t=0.50
                        let ctx_clone = ctx.clone();
                        let events_ctl = events.clone();
                        ctx.branch(
                            move |ctl| {
                                let ctx = ctx_clone;
                                let events = events_ctl;
                                async move {
                                    let _ = ctl.wait_sec(0.50).await;
                                    ctx.set_bpm(480.0);
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "tempo_set_480@0.50".to_string(),
                                        t: ctl.time(),
                                        ctx_id: ctl.id(),
                                        note: None,
                                        value: None,
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        // Wait for 4 beats at 240 BPM
                        // At 240 BPM: 4 beats/sec, so 4 beats = 1.0 sec at constant tempo
                        // But tempo changes to 480 BPM at t=0.50
                        let _ = ctx.wait(4.0).await;
                        events.borrow_mut().push(LoggedEvent {
                            id: "wait4beats_done".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                    }
                },
                EngineConfig {
                    bpm: 240.0,
                    seed: test_name.to_string(),
                    ..Default::default()
                },
            );
            runner.step_sec(2.0);
            { let v = events.borrow().clone(); v }
        };

        clear_all_barriers();

        // Run realtime
        let realtime_events = {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_clone = events.clone();
            let mut runner = RealtimeRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    async move {
                        // Branch to change tempo at t=0.50
                        let ctx_clone = ctx.clone();
                        let events_ctl = events.clone();
                        ctx.branch(
                            move |ctl| {
                                let ctx = ctx_clone;
                                let events = events_ctl;
                                async move {
                                    let _ = ctl.wait_sec(0.50).await;
                                    ctx.set_bpm(480.0);
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "tempo_set_480@0.50".to_string(),
                                        t: ctl.time(),
                                        ctx_id: ctl.id(),
                                        note: None,
                                        value: None,
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        let _ = ctx.wait(4.0).await;
                        events.borrow_mut().push(LoggedEvent {
                            id: "wait4beats_done".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                    }
                },
                EngineConfig {
                    bpm: 240.0,
                    seed: test_name.to_string(),
                    rate: REALTIME_RATE,
                    ..Default::default()
                },
            );
            runner.run_until_complete();
            { let v = events.borrow().clone(); v }
        };

        compare_deterministic_runs(
            test_name,
            &offline_events,
            &realtime_events,
            &TestTolerances::default(),
        );
    }

    /// Test: dual_mode_rng_forked
    /// Verifies that forked RNG produces identical values in both modes
    #[test]
    fn test_dual_mode_rng_forked() {
        clear_all_barriers();

        let test_name = "dual_mode_rng_forked";

        // Run offline
        let offline_events = {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_clone = events.clone();
            let mut runner = OfflineRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    async move {
                        events.borrow_mut().push(LoggedEvent {
                            id: "start".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });

                        let events_a = events.clone();
                        let a = ctx.branch_wait(
                            move |c| {
                                let events = events_a;
                                async move {
                                    let _ = c.wait_sec(0.05).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A_r0".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: Some(c.random()),
                                    });
                                    let _ = c.wait_sec(0.05).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A_r1".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: Some(c.random()),
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        let events_b = events.clone();
                        let b = ctx.branch_wait(
                            move |c| {
                                let events = events_b;
                                async move {
                                    let _ = c.wait_sec(0.10).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "B_r0".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: Some(c.random()),
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        let _ = a.await;
                        let _ = b.await;
                        let _ = ctx.wait(0.0).await;

                        events.borrow_mut().push(LoggedEvent {
                            id: "joined".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                    }
                },
                EngineConfig {
                    bpm: 60.0,
                    seed: test_name.to_string(),
                    ..Default::default()
                },
            );
            runner.step_sec(0.5);
            { let v = events.borrow().clone(); v }
        };

        clear_all_barriers();

        // Run realtime
        let realtime_events = {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_clone = events.clone();
            let mut runner = RealtimeRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    async move {
                        events.borrow_mut().push(LoggedEvent {
                            id: "start".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });

                        let events_a = events.clone();
                        let a = ctx.branch_wait(
                            move |c| {
                                let events = events_a;
                                async move {
                                    let _ = c.wait_sec(0.05).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A_r0".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: Some(c.random()),
                                    });
                                    let _ = c.wait_sec(0.05).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A_r1".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: Some(c.random()),
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        let events_b = events.clone();
                        let b = ctx.branch_wait(
                            move |c| {
                                let events = events_b;
                                async move {
                                    let _ = c.wait_sec(0.10).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "B_r0".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: Some(c.random()),
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        let _ = a.await;
                        let _ = b.await;
                        let _ = ctx.wait(0.0).await;

                        events.borrow_mut().push(LoggedEvent {
                            id: "joined".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                    }
                },
                EngineConfig {
                    bpm: 60.0,
                    seed: test_name.to_string(),
                    rate: REALTIME_RATE,
                    ..Default::default()
                },
            );
            runner.run_until_complete();
            { let v = events.borrow().clone(); v }
        };

        compare_deterministic_runs(
            test_name,
            &offline_events,
            &realtime_events,
            &TestTolerances::default(),
        );
    }

    /// Test: dual_mode_rng_shared
    /// Verifies that shared RNG produces identical values in both modes
    #[test]
    fn test_dual_mode_rng_shared() {
        clear_all_barriers();

        let test_name = "dual_mode_rng_shared";

        // Run offline
        let offline_events = {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_clone = events.clone();
            let mut runner = OfflineRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    async move {
                        events.borrow_mut().push(LoggedEvent {
                            id: "start".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });

                        let events_a = events.clone();
                        let a = ctx.branch_wait(
                            move |c| {
                                let events = events_a;
                                async move {
                                    let _ = c.wait_sec(0.05).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A_draw".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: Some(c.random()),
                                    });
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
                                let events = events_b;
                                async move {
                                    let _ = c.wait_sec(0.05).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "B_draw".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: Some(c.random()),
                                    });
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

                        events.borrow_mut().push(LoggedEvent {
                            id: "joined".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                    }
                },
                EngineConfig {
                    bpm: 60.0,
                    seed: test_name.to_string(),
                    ..Default::default()
                },
            );
            runner.step_sec(0.5);
            { let v = events.borrow().clone(); v }
        };

        clear_all_barriers();

        // Run realtime
        let realtime_events = {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_clone = events.clone();
            let mut runner = RealtimeRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    async move {
                        events.borrow_mut().push(LoggedEvent {
                            id: "start".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });

                        let events_a = events.clone();
                        let a = ctx.branch_wait(
                            move |c| {
                                let events = events_a;
                                async move {
                                    let _ = c.wait_sec(0.05).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "A_draw".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: Some(c.random()),
                                    });
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
                                let events = events_b;
                                async move {
                                    let _ = c.wait_sec(0.05).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "B_draw".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: Some(c.random()),
                                    });
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

                        events.borrow_mut().push(LoggedEvent {
                            id: "joined".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                    }
                },
                EngineConfig {
                    bpm: 60.0,
                    seed: test_name.to_string(),
                    rate: REALTIME_RATE,
                    ..Default::default()
                },
            );
            runner.run_until_complete();
            { let v = events.borrow().clone(); v }
        };

        compare_deterministic_runs(
            test_name,
            &offline_events,
            &realtime_events,
            &TestTolerances::default(),
        );
    }

    /// Test: dual_mode_barrier_sync
    /// Verifies that barrier synchronization works identically in both modes
    #[test]
    fn test_dual_mode_barrier_sync() {
        clear_all_barriers();

        let test_name = "dual_mode_barrier_sync";
        let barrier_key = "dual_mode_barrier_sync_key";

        // Run offline
        let offline_events = {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_clone = events.clone();
            let mut runner = OfflineRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    async move {
                        // Producer
                        let events_b = events.clone();
                        ctx.branch(
                            move |b| {
                                let events = events_b;
                                async move {
                                    for cycle in 0..2 {
                                        start_barrier(barrier_key, &b);
                                        events.borrow_mut().push(LoggedEvent {
                                            id: format!("B_start_{}", cycle),
                                            t: b.time(),
                                            ctx_id: b.id(),
                                            note: None,
                                            value: None,
                                        });
                                        let _ = b.wait_sec(0.20).await;
                                        events.borrow_mut().push(LoggedEvent {
                                            id: format!("B_end_{}", cycle),
                                            t: b.time(),
                                            ctx_id: b.id(),
                                            note: None,
                                            value: None,
                                        });
                                        resolve_barrier(barrier_key, &b);
                                    }
                                }
                            },
                            BranchOptions::default(),
                        );

                        // Consumer
                        let events_a = events.clone();
                        let _ = ctx
                            .branch_wait(
                                move |a| {
                                    let events = events_a;
                                    async move {
                                        for cycle in 0..2 {
                                            events.borrow_mut().push(LoggedEvent {
                                                id: format!("A_start_{}", cycle),
                                                t: a.time(),
                                                ctx_id: a.id(),
                                                note: None,
                                                value: None,
                                            });
                                            let _ = a.wait_sec(0.15).await;
                                            events.borrow_mut().push(LoggedEvent {
                                                id: format!("A_end_{}", cycle),
                                                t: a.time(),
                                                ctx_id: a.id(),
                                                note: None,
                                                value: None,
                                            });
                                            let _ = await_barrier(barrier_key, &a).await;
                                            events.borrow_mut().push(LoggedEvent {
                                                id: format!("A_synced_{}", cycle),
                                                t: a.time(),
                                                ctx_id: a.id(),
                                                note: None,
                                                value: None,
                                            });
                                        }
                                    }
                                },
                                BranchOptions::default(),
                            )
                            .await;
                    }
                },
                EngineConfig {
                    bpm: 60.0,
                    seed: test_name.to_string(),
                    ..Default::default()
                },
            );
            runner.step_sec(1.0);
            { let v = events.borrow().clone(); v }
        };

        clear_all_barriers();

        // Run realtime
        let realtime_events = {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_clone = events.clone();
            let mut runner = RealtimeRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    async move {
                        // Producer
                        let events_b = events.clone();
                        ctx.branch(
                            move |b| {
                                let events = events_b;
                                async move {
                                    for cycle in 0..2 {
                                        start_barrier(barrier_key, &b);
                                        events.borrow_mut().push(LoggedEvent {
                                            id: format!("B_start_{}", cycle),
                                            t: b.time(),
                                            ctx_id: b.id(),
                                            note: None,
                                            value: None,
                                        });
                                        let _ = b.wait_sec(0.20).await;
                                        events.borrow_mut().push(LoggedEvent {
                                            id: format!("B_end_{}", cycle),
                                            t: b.time(),
                                            ctx_id: b.id(),
                                            note: None,
                                            value: None,
                                        });
                                        resolve_barrier(barrier_key, &b);
                                    }
                                }
                            },
                            BranchOptions::default(),
                        );

                        // Consumer
                        let events_a = events.clone();
                        let _ = ctx
                            .branch_wait(
                                move |a| {
                                    let events = events_a;
                                    async move {
                                        for cycle in 0..2 {
                                            events.borrow_mut().push(LoggedEvent {
                                                id: format!("A_start_{}", cycle),
                                                t: a.time(),
                                                ctx_id: a.id(),
                                                note: None,
                                                value: None,
                                            });
                                            let _ = a.wait_sec(0.15).await;
                                            events.borrow_mut().push(LoggedEvent {
                                                id: format!("A_end_{}", cycle),
                                                t: a.time(),
                                                ctx_id: a.id(),
                                                note: None,
                                                value: None,
                                            });
                                            let _ = await_barrier(barrier_key, &a).await;
                                            events.borrow_mut().push(LoggedEvent {
                                                id: format!("A_synced_{}", cycle),
                                                t: a.time(),
                                                ctx_id: a.id(),
                                                note: None,
                                                value: None,
                                            });
                                        }
                                    }
                                },
                                BranchOptions::default(),
                            )
                            .await;
                    }
                },
                EngineConfig {
                    bpm: 60.0,
                    seed: test_name.to_string(),
                    rate: REALTIME_RATE,
                    ..Default::default()
                },
            );
            runner.run_until_complete();
            { let v = events.borrow().clone(); v }
        };

        compare_deterministic_runs(
            test_name,
            &offline_events,
            &realtime_events,
            &TestTolerances::default(),
        );
    }

    /// Test: dual_mode_many_branches_stress
    /// Stress test with many parallel branches in both modes
    #[test]
    fn test_dual_mode_many_branches_stress() {
        clear_all_barriers();

        let test_name = "dual_mode_many_branches_stress";

        // Run offline
        let offline_events = {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_clone = events.clone();
            let mut runner = OfflineRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    async move {
                        events.borrow_mut().push(LoggedEvent {
                            id: "start".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });

                        let mut handles = Vec::new();

                        for i in 0..10 {
                            let d = 0.02 * (i as f64 + 1.0);
                            let events_i = events.clone();
                            let task_name = format!("task{}", i);

                            let handle = ctx.branch_wait(
                                move |c| {
                                    let events = events_i;
                                    let name = task_name;
                                    async move {
                                        events.borrow_mut().push(LoggedEvent {
                                            id: format!("{}_start", name),
                                            t: c.time(),
                                            ctx_id: c.id(),
                                            note: None,
                                            value: None,
                                        });
                                        let _ = c.wait_sec(d).await;
                                        events.borrow_mut().push(LoggedEvent {
                                            id: format!("{}_end", name),
                                            t: c.time(),
                                            ctx_id: c.id(),
                                            note: None,
                                            value: None,
                                        });
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

                        events.borrow_mut().push(LoggedEvent {
                            id: "joined".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                    }
                },
                EngineConfig {
                    bpm: 60.0,
                    seed: test_name.to_string(),
                    ..Default::default()
                },
            );
            runner.step_sec(0.5);
            { let v = events.borrow().clone(); v }
        };

        clear_all_barriers();

        // Run realtime
        let realtime_events = {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_clone = events.clone();
            let mut runner = RealtimeRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    async move {
                        events.borrow_mut().push(LoggedEvent {
                            id: "start".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });

                        let mut handles = Vec::new();

                        for i in 0..10 {
                            let d = 0.02 * (i as f64 + 1.0);
                            let events_i = events.clone();
                            let task_name = format!("task{}", i);

                            let handle = ctx.branch_wait(
                                move |c| {
                                    let events = events_i;
                                    let name = task_name;
                                    async move {
                                        events.borrow_mut().push(LoggedEvent {
                                            id: format!("{}_start", name),
                                            t: c.time(),
                                            ctx_id: c.id(),
                                            note: None,
                                            value: None,
                                        });
                                        let _ = c.wait_sec(d).await;
                                        events.borrow_mut().push(LoggedEvent {
                                            id: format!("{}_end", name),
                                            t: c.time(),
                                            ctx_id: c.id(),
                                            note: None,
                                            value: None,
                                        });
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

                        events.borrow_mut().push(LoggedEvent {
                            id: "joined".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                    }
                },
                EngineConfig {
                    bpm: 60.0,
                    seed: test_name.to_string(),
                    rate: REALTIME_RATE,
                    ..Default::default()
                },
            );
            runner.run_until_complete();
            { let v = events.borrow().clone(); v }
        };

        compare_deterministic_runs(
            test_name,
            &offline_events,
            &realtime_events,
            &TestTolerances::default(),
        );
    }

    /// Test: dual_mode_wait0_sync
    /// Verifies that wait(0) works as a sync point in both modes
    #[test]
    fn test_dual_mode_wait0_sync() {
        clear_all_barriers();

        let test_name = "dual_mode_wait0_sync";

        // Run offline
        let offline_events = {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_clone = events.clone();
            let mut runner = OfflineRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    async move {
                        let events_p1 = events.clone();
                        let p1 = ctx.branch_wait(
                            move |c| {
                                let events = events_p1;
                                async move {
                                    let _ = c.wait_sec(0.10).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "p1_done".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        let events_p2 = events.clone();
                        let p2 = ctx.branch_wait(
                            move |c| {
                                let events = events_p2;
                                async move {
                                    let _ = c.wait_sec(0.20).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "p2_done".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        let _ = p1.await;
                        let _ = p2.await;

                        events.borrow_mut().push(LoggedEvent {
                            id: "after_all_immediate".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });

                        let _ = ctx.wait(0.0).await;

                        events.borrow_mut().push(LoggedEvent {
                            id: "after_all_after_wait0".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                    }
                },
                EngineConfig {
                    bpm: 60.0,
                    seed: test_name.to_string(),
                    ..Default::default()
                },
            );
            runner.step_sec(1.0);
            { let v = events.borrow().clone(); v }
        };

        clear_all_barriers();

        // Run realtime
        let realtime_events = {
            let events = Rc::new(RefCell::new(Vec::new()));
            let events_clone = events.clone();
            let mut runner = RealtimeRunner::new(
                move |ctx| {
                    let events = events_clone.clone();
                    async move {
                        let events_p1 = events.clone();
                        let p1 = ctx.branch_wait(
                            move |c| {
                                let events = events_p1;
                                async move {
                                    let _ = c.wait_sec(0.10).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "p1_done".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        let events_p2 = events.clone();
                        let p2 = ctx.branch_wait(
                            move |c| {
                                let events = events_p2;
                                async move {
                                    let _ = c.wait_sec(0.20).await;
                                    events.borrow_mut().push(LoggedEvent {
                                        id: "p2_done".to_string(),
                                        t: c.time(),
                                        ctx_id: c.id(),
                                        note: None,
                                        value: None,
                                    });
                                }
                            },
                            BranchOptions::default(),
                        );

                        let _ = p1.await;
                        let _ = p2.await;

                        events.borrow_mut().push(LoggedEvent {
                            id: "after_all_immediate".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });

                        let _ = ctx.wait(0.0).await;

                        events.borrow_mut().push(LoggedEvent {
                            id: "after_all_after_wait0".to_string(),
                            t: ctx.time(),
                            ctx_id: ctx.id(),
                            note: None,
                            value: None,
                        });
                    }
                },
                EngineConfig {
                    bpm: 60.0,
                    seed: test_name.to_string(),
                    rate: REALTIME_RATE,
                    ..Default::default()
                },
            );
            runner.run_until_complete();
            { let v = events.borrow().clone(); v }
        };

        compare_deterministic_runs(
            test_name,
            &offline_events,
            &realtime_events,
            &TestTolerances::default(),
        );
    }

    /// Test: realtime_basic_wait
    /// Basic realtime test without comparison (standalone validation)
    #[test]
    fn test_realtime_basic_wait() {
        clear_all_barriers();

        let events = Rc::new(RefCell::new(Vec::new()));
        let events_clone = events.clone();

        let mut runner = RealtimeRunner::new(
            move |ctx| {
                let events = events_clone.clone();
                async move {
                    events.borrow_mut().push(LoggedEvent {
                        id: "start".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });
                    let _ = ctx.wait_sec(0.05).await;
                    events.borrow_mut().push(LoggedEvent {
                        id: "mid".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });
                    let _ = ctx.wait_sec(0.05).await;
                    events.borrow_mut().push(LoggedEvent {
                        id: "end".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });
                }
            },
            EngineConfig {
                bpm: 60.0,
                seed: "realtime_basic_wait".to_string(),
                rate: REALTIME_RATE,
                ..Default::default()
            },
        );

        runner.run_until_complete();

        let events = events.borrow();
        assert_events_match(&events, &["start", "mid", "end"], "realtime_basic_wait");
        assert_time_approx(&events, 0, 0.0, 1e-6, "realtime_basic_wait");
        assert_time_approx(&events, 1, 0.05, 1e-6, "realtime_basic_wait");
        assert_time_approx(&events, 2, 0.10, 1e-6, "realtime_basic_wait");
    }

    /// Test: realtime_beat_wait_basic
    /// Basic realtime beat wait test
    #[test]
    fn test_realtime_beat_wait_basic() {
        clear_all_barriers();

        let events = Rc::new(RefCell::new(Vec::new()));
        let events_clone = events.clone();

        let mut runner = RealtimeRunner::new(
            move |ctx| {
                let events = events_clone.clone();
                async move {
                    events.borrow_mut().push(LoggedEvent {
                        id: "start".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });
                    // At 120 BPM, 2 beats = 1 second
                    let _ = ctx.wait(2.0).await;
                    events.borrow_mut().push(LoggedEvent {
                        id: "after_2_beats".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });
                }
            },
            EngineConfig {
                bpm: 120.0,
                seed: "realtime_beat_wait_basic".to_string(),
                rate: REALTIME_RATE,
                ..Default::default()
            },
        );

        runner.run_until_complete();

        let events = events.borrow();
        assert_events_match(&events, &["start", "after_2_beats"], "realtime_beat_wait_basic");
        assert_time_approx(&events, 0, 0.0, 1e-6, "realtime_beat_wait_basic");
        assert_time_approx(&events, 1, 1.0, 1e-6, "realtime_beat_wait_basic"); // 2 beats at 120 BPM = 1 sec
    }

    /// Test: realtime_branch_parallel
    /// Test parallel branches in realtime mode
    #[test]
    fn test_realtime_branch_parallel() {
        clear_all_barriers();

        let events = Rc::new(RefCell::new(Vec::new()));
        let events_clone = events.clone();

        let mut runner = RealtimeRunner::new(
            move |ctx| {
                let events = events_clone.clone();
                async move {
                    events.borrow_mut().push(LoggedEvent {
                        id: "start".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });

                    let events_a = events.clone();
                    let a = ctx.branch_wait(
                        move |c| {
                            let events = events_a;
                            async move {
                                let _ = c.wait_sec(0.10).await;
                                events.borrow_mut().push(LoggedEvent {
                                    id: "A_done".to_string(),
                                    t: c.time(),
                                    ctx_id: c.id(),
                                    note: None,
                                    value: None,
                                });
                            }
                        },
                        BranchOptions::default(),
                    );

                    let events_b = events.clone();
                    let b = ctx.branch_wait(
                        move |c| {
                            let events = events_b;
                            async move {
                                let _ = c.wait_sec(0.05).await;
                                events.borrow_mut().push(LoggedEvent {
                                    id: "B_done".to_string(),
                                    t: c.time(),
                                    ctx_id: c.id(),
                                    note: None,
                                    value: None,
                                });
                            }
                        },
                        BranchOptions::default(),
                    );

                    let _ = a.await;
                    let _ = b.await;
                    let _ = ctx.wait(0.0).await;

                    events.borrow_mut().push(LoggedEvent {
                        id: "joined".to_string(),
                        t: ctx.time(),
                        ctx_id: ctx.id(),
                        note: None,
                        value: None,
                    });
                }
            },
            EngineConfig {
                bpm: 60.0,
                seed: "realtime_branch_parallel".to_string(),
                rate: REALTIME_RATE,
                ..Default::default()
            },
        );

        runner.run_until_complete();

        let events = events.borrow();

        // B finishes before A (0.05 < 0.10)
        let b_idx = events.iter().position(|e| e.id == "B_done").unwrap();
        let a_idx = events.iter().position(|e| e.id == "A_done").unwrap();
        assert!(b_idx < a_idx, "B_done should come before A_done");

        // Check joined is last
        assert_eq!(events.last().unwrap().id, "joined");
    }
}
