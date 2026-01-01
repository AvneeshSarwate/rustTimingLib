//! Maximally Terse MIDI Demo - Proc Macro Edition
//!
//! This demo showcases the most aggressive syntax transformations possible
//! for the timing library. It flagrantly breaks Rust best practices in
//! favor of a "notational" pseudocode-like experience.
//!
//! ## Key Syntax (defined below):
//!
//! - `~expr` or `W!(expr)` - Wait beats (can't use ~ as operator, so we use W!)
//! - `S!(expr)` - Wait seconds
//! - `F!{ ... }` - Fork (fire-and-forget branch)
//! - `J!{ ... }` - Join (branch_wait)
//! - `L!(n) { ... }` - Loop n times with cancellation check
//! - `X!` - Break if cancelled
//! - `midi.on(n,v)` - Note on (method call, auto-transforms)
//! - `midi.off(n)` - Note off
//!
//! ## Shared State Helpers (ultra-short names):
//!
//! - `S!(val)` - Create shared value (also wait seconds - context determines)
//! - `Q!()` - Create message queue
//! - `P!()` - Create params map
//!
//! Usage:
//!   cargo run --bin midi_demo_proc -- --device 0 --test 1

use midir::{MidiOutput, MidiOutputConnection};
use rust_timing_lib::{
    await_barrier, resolve_barrier, start_barrier, BranchOptions, Ctx, Engine, EngineConfig,
    SchedulerMode,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

// ============================================================================
// ULTRA-TERSE MACROS - Breaking all the rules for maximum brevity
// ============================================================================

/// Wait beats - the most common operation, so shortest name
/// Usage: W!(c, 1.0);
macro_rules! W {
    ($c:expr, $b:expr) => {
        let _ = $c.wait($b).await;
    };
}

/// Wait seconds
/// Usage: S!(c, 0.5);
macro_rules! S {
    ($c:expr, $s:expr) => {
        let _ = $c.wait_sec($s).await;
    };
}

/// Check cancelled and break
/// Usage: X!(c);
macro_rules! X {
    ($c:expr) => {
        if $c.is_canceled() {
            break;
        }
    };
}

/// Fork - fire and forget branch
/// Usage: F!(ctx, [midi, running], |c| { W!(c, 1.0); ... });
/// The |c| names the inner context - use that name inside the block.
macro_rules! F {
    // With captures and explicit inner context name
    ($ctx:expr, [$($cap:ident),*], |$c:ident| $body:expr) => {{
        let __ctx_clone = $ctx.clone();
        $(let $cap = $cap.clone();)*
        __ctx_clone.branch(
            move |$c| {
                $(let $cap = $cap.clone();)*
                async move { $body }
            },
            BranchOptions::default(),
        )
    }};
    // No captures, with explicit inner context name
    ($ctx:expr, |$c:ident| $body:expr) => {{
        let __ctx_clone = $ctx.clone();
        __ctx_clone.branch(
            move |$c| async move { $body },
            BranchOptions::default(),
        )
    }};
}

/// Join - branch_wait (awaits completion)
/// Usage: J!(ctx, [midi], |c| { ... });
#[allow(unused_macros)]
macro_rules! J {
    ($ctx:expr, [$($cap:ident),*], |$c:ident| $body:expr) => {{
        let __ctx_clone = $ctx.clone();
        $(let $cap = $cap.clone();)*
        let _ = __ctx_clone.branch_wait(
            move |$c| {
                $(let $cap = $cap.clone();)*
                async move { $body }
            },
            BranchOptions::default(),
        ).await
    }};
    ($ctx:expr, |$c:ident| $body:expr) => {{
        let __ctx_clone = $ctx.clone();
        let _ = __ctx_clone.branch_wait(
            move |$c| async move { $body },
            BranchOptions::default(),
        ).await
    }};
}

/// Loop N times with cancellation check
/// Usage: L!(n, { ... }); - uses ambient `c`
macro_rules! L {
    ($c:expr, $n:expr, $body:expr) => {
        for __i in 0..$n {
            if $c.is_canceled() { break; }
            { $body }
        }
    };
}

/// Infinite loop with cancellation check
macro_rules! LOOP {
    ($c:expr, $body:expr) => {
        loop {
            if $c.is_canceled() { break; }
            { $body }
        }
    };
}

/// Run - spawns on engine and runs until complete
/// Usage: RUN!(120.0, "seed", |ctx| { ... });
macro_rules! RUN {
    ($bpm:expr, $seed:expr, |$ctx:ident| $body:expr) => {{
        let __config = EngineConfig {
            bpm: $bpm,
            seed: $seed.to_string(),
            ..Default::default()
        };
        let mut __engine = Engine::new(SchedulerMode::Realtime, __config);
        __engine.spawn(|$ctx: Ctx| async move { $body });
        __engine.run_until_complete();
    }};
}

/// Run with external running flag
/// Usage: RUN_WHILE!(120.0, "seed", running, |ctx| { ... });
macro_rules! RUN_WHILE {
    ($bpm:expr, $seed:expr, $running:expr, |$ctx:ident| $body:expr) => {{
        let __config = EngineConfig {
            bpm: $bpm,
            seed: $seed.to_string(),
            ..Default::default()
        };
        let mut __engine = Engine::new(SchedulerMode::Realtime, __config);
        let __running = $running.clone();
        __engine.spawn(|$ctx: Ctx| async move { $body });
        __engine.run_until(|| !__running.load(Ordering::Relaxed));
    }};
}

// ============================================================================
// ULTRA-SHORT SHARED STATE TYPES
// ============================================================================

/// Shared value - wraps Rc<RefCell<T>> with terse API
#[derive(Debug)]
pub struct V<T>(Rc<RefCell<T>>);

impl<T> V<T> {
    pub fn new(val: T) -> Self { V(Rc::new(RefCell::new(val))) }
    pub fn m<R>(&self, f: impl FnOnce(&mut T) -> R) -> R { f(&mut self.0.borrow_mut()) }
}
impl<T: Clone> V<T> {
    pub fn g(&self) -> T { self.0.borrow().clone() }
}
impl<T: Copy> V<T> {
    pub fn s(&self, val: T) { *self.0.borrow_mut() = val; }
}
impl<T> Clone for V<T> {
    fn clone(&self) -> Self { V(self.0.clone()) }
}

/// Message queue with ultra-short API
pub struct Q<T>(Rc<RefCell<VecDeque<T>>>);
impl<T> Q<T> {
    pub fn new() -> Self { Q(Rc::new(RefCell::new(VecDeque::new()))) }
    pub fn push(&self, v: T) { self.0.borrow_mut().push_back(v); }
    pub fn pop(&self) -> Option<T> { self.0.borrow_mut().pop_front() }
    pub fn len(&self) -> usize { self.0.borrow().len() }
}
impl<T> Clone for Q<T> { fn clone(&self) -> Self { Q(self.0.clone()) } }
impl<T> Default for Q<T> { fn default() -> Self { Self::new() } }

/// Note set - tracks playing notes
pub struct N(Rc<RefCell<HashSet<u8>>>);
impl N {
    pub fn new() -> Self { N(Rc::new(RefCell::new(HashSet::new()))) }
    pub fn add(&self, n: u8) { self.0.borrow_mut().insert(n); }
    pub fn rm(&self, n: u8) { self.0.borrow_mut().remove(&n); }
    pub fn has(&self, n: u8) -> bool { self.0.borrow().contains(&n) }
    pub fn all(&self) -> Vec<u8> { self.0.borrow().iter().copied().collect() }
    pub fn clear(&self) { self.0.borrow_mut().clear(); }
}
impl Clone for N { fn clone(&self) -> Self { N(self.0.clone()) } }
impl Default for N { fn default() -> Self { Self::new() } }

/// Params map - named parameters
pub struct P(Rc<RefCell<HashMap<String, f64>>>);
impl P {
    pub fn new() -> Self { P(Rc::new(RefCell::new(HashMap::new()))) }
    pub fn s(&self, k: &str, v: f64) { self.0.borrow_mut().insert(k.into(), v); }
    pub fn g(&self, k: &str) -> f64 { self.0.borrow().get(k).copied().unwrap_or(0.0) }
}
impl Clone for P { fn clone(&self) -> Self { P(self.0.clone()) } }
impl Default for P { fn default() -> Self { Self::new() } }

// ============================================================================
// MIDI WRAPPER WITH ULTRA-SHORT METHODS
// ============================================================================

/// MIDI wrapper - on/off are just 2 chars each
pub struct M(Rc<RefCell<MidiOutputConnection>>);

impl M {
    pub fn new(conn: MidiOutputConnection) -> Self { M(Rc::new(RefCell::new(conn))) }
    pub fn on(&self, n: u8, v: u8) { let _ = self.0.borrow_mut().send(&[0x90, n, v]); }
    pub fn off(&self, n: u8) { let _ = self.0.borrow_mut().send(&[0x80, n, 0]); }
}
impl Clone for M { fn clone(&self) -> Self { M(self.0.clone()) } }

fn connect(idx: usize) -> Result<M, Box<dyn std::error::Error>> {
    let out = MidiOutput::new("demo")?;
    let ports = out.ports();
    if idx >= ports.len() { return Err("bad port".into()); }
    let conn = out.connect(&ports[idx], "conn")?;
    println!("Connected to MIDI port {}", idx);
    Ok(M::new(conn))
}

fn list_ports() {
    let out = MidiOutput::new("list").unwrap();
    for (i, p) in out.ports().iter().enumerate() {
        println!("{}: {}", i, out.port_name(p).unwrap_or_default());
    }
}

// ============================================================================
// TEST 1: METRONOME - Extremely terse
// ============================================================================

fn test1(m: M, r: Arc<AtomicBool>) {
    println!("\n=== Test 1: Metronome (ULTRA TERSE) ===\n");

    RUN_WHILE!(120.0, "met", r, |ctx| {
        L!(ctx, 32, {
            m.on(76, 120); S!(ctx, 0.05); m.off(76); S!(ctx, 0.20);
            m.on(72, 90);  S!(ctx, 0.05); m.off(72); S!(ctx, 0.20);
            m.on(72, 90);  S!(ctx, 0.05); m.off(72); S!(ctx, 0.20);
            m.on(72, 90);  S!(ctx, 0.05); m.off(72); S!(ctx, 0.20);
        });
        println!("Done");
    });
}

// ============================================================================
// TEST 2: POLYRHYTHM 3 vs 4 - Compare to original
// ============================================================================

fn test2(m: M, r: Arc<AtomicBool>) {
    println!("\n=== Test 2: Polyrhythm 3v4 (ULTRA TERSE) ===\n");

    RUN_WHILE!(90.0, "poly", r, |ctx| {
        // Voice 1: triplets (3 per bar)
        F!(ctx, [m, r], |c| {
            L!(c, 24, {
                X!(c); if !r.load(Ordering::Relaxed) { break; }
                m.on(60, 100); W!(c, 0.1); m.off(60); W!(c, 4.0/3.0 - 0.1);
            });
        });

        // Voice 2: quarters (4 per bar)
        F!(ctx, [m, r], |c| {
            L!(c, 32, {
                X!(c); if !r.load(Ordering::Relaxed) { break; }
                m.on(72, 80); W!(c, 0.08); m.off(72); W!(c, 0.92);
            });
        });

        W!(ctx, 32.0);
        println!("Done");
    });
}

// ============================================================================
// TEST 3: TEMPO RAMP
// ============================================================================

fn test3(m: M, r: Arc<AtomicBool>) {
    println!("\n=== Test 3: Tempo Ramp (ULTRA TERSE) ===\n");

    RUN_WHILE!(60.0, "ramp", r, |ctx| {
        ctx.ramp_bpm_to(180.0, 8.0);
        let mut i = 0;
        LOOP!(ctx, {
            if !r.load(Ordering::Relaxed) || ctx.time() > 8.5 { break; }
            println!("Beat {} @ t={:.2} bpm={:.0}", i, ctx.time(), ctx.bpm());
            m.on(69, 100); W!(ctx, 0.1); m.off(69); W!(ctx, 0.9);
            i += 1;
        });
        println!("Done - {} beats", i);
    });
}

// ============================================================================
// TEST 4: BARRIER SYNC
// ============================================================================

fn test4(m: M, r: Arc<AtomicBool>) {
    println!("\n=== Test 4: Barrier Sync (ULTRA TERSE) ===\n");

    RUN_WHILE!(120.0, "barr", r, |ctx| {
        for phrase in 0..4 {
            if !r.load(Ordering::Relaxed) { break; }
            println!("--- Phrase {} ---", phrase + 1);
            start_barrier("sync", &ctx);

            // Voice A - inside F!, use `c`
            let h = F!(ctx, [m], |c| {
                for n in [60, 62, 64, 65] {
                    m.on(n, 100); W!(c, 0.8); m.off(n); W!(c, 0.2);
                }
                println!("  A done");
                let _ = await_barrier("sync", &c).await;
            });

            // Voice B - inside F!, use `c`
            F!(ctx, [m], |c| {
                for n in [72, 74, 76, 77, 79] {
                    m.on(n, 80); W!(c, 0.64); m.off(n); W!(c, 0.16);
                }
                println!("  B done");
                let _ = await_barrier("sync", &c).await;
            });

            W!(ctx, 4.5);
            resolve_barrier("sync", &ctx);
            println!("  Resolved!\n");
            h.cancel();
        }
    });
}

// ============================================================================
// TEST 5: GENERATIVE MELODY
// ============================================================================

fn test5(m: M, _r: Arc<AtomicBool>) {
    println!("\n=== Test 5: Generative (ULTRA TERSE) ===\n");

    RUN!(100.0, "gen42", |ctx| {
        let scale = [60, 62, 64, 67, 69, 72, 74, 76, 79, 81];
        let durs = [0.5, 1.0, 1.5, 2.0];

        L!(ctx, 16, {
            let n = scale[(ctx.random() * scale.len() as f64) as usize];
            let d = durs[(ctx.random() * durs.len() as f64) as usize];
            print!("({},{:.1}) ", n, d);
            m.on(n, 90); W!(ctx, d * 0.9); m.off(n); W!(ctx, d * 0.1);
        });
        println!("\nDone");
    });
}

// ============================================================================
// TEST 6: CANCELLATION WITH CLEANUP
// ============================================================================

fn test6(m: M, _r: Arc<AtomicBool>) {
    println!("\n=== Test 6: Cancellation (ULTRA TERSE) ===\n");

    RUN!(120.0, "cancel", |ctx| {
        let chord = [60, 64, 67, 72];
        println!("Starting chord...");

        let h = F!(ctx, [m], |c| {
            for &n in &chord { m.on(n, 100); }
            c.handle_cancel({
                let m = m.clone();
                move || {
                    println!("  [CANCEL HANDLER]");
                    for &n in &chord { m.off(n); }
                }
            });
            W!(c, 20.0);
        });

        W!(ctx, 2.0);
        println!("Cancelling...");
        h.cancel();

        W!(ctx, 1.0);
        println!("Confirm note");
        m.on(84, 100); W!(ctx, 0.5); m.off(84);
        println!("Done");
    });
}

// ============================================================================
// TEST 7: SHARED ENERGY - Multiple agents affect shared state
// ============================================================================

fn test7(m: M, r: Arc<AtomicBool>) {
    println!("\n=== Test 7: Shared Energy (ULTRA TERSE) ===\n");

    RUN_WHILE!(100.0, "energy", r, |ctx| {
        let e = V::new(0.3f64);  // Energy level

        // Energy modulator - inside F!, use `c`
        F!(ctx, [e, r], |c| {
            let mut up = true;
            LOOP!(c, {
                if !r.load(Ordering::Relaxed) { break; }
                e.m(|v| {
                    if up { *v = (*v + 0.05).min(1.0); if *v >= 1.0 { up = false; } }
                    else  { *v = (*v - 0.03).max(0.1); if *v <= 0.1 { up = true; } }
                });
                W!(c, 0.5);
            });
        });

        // Bass - inside F!, use `c`
        F!(ctx, [m, e, r], |c| {
            let ns = [36, 38, 40, 41];
            let mut i = 0;
            LOOP!(c, {
                if !r.load(Ordering::Relaxed) { break; }
                let v = (40.0 + e.g() * 80.0) as u8;
                m.on(ns[i % 4], v); W!(c, 1.8); m.off(ns[i % 4]); W!(c, 0.2);
                i += 1;
            });
        });

        // Melody - inside F!, use `c`
        F!(ctx, [m, e, r], |c| {
            let sc = [60, 62, 64, 65, 67, 69, 71, 72];
            LOOP!(c, {
                if !r.load(Ordering::Relaxed) { break; }
                let ev = e.g();
                let v = (50.0 + ev * 70.0) as u8;
                let nn = (2.0 + ev * 4.0) as usize;
                for _ in 0..nn {
                    let n = sc[(c.random() * sc.len() as f64) as usize];
                    m.on(n, v); W!(c, 0.2); m.off(n); W!(c, 0.1);
                }
                e.m(|v| *v = (*v + 0.02).min(1.0));
                W!(c, 0.5);
            });
        });

        // Monitor - outer context is `ctx`
        L!(ctx, 20, {
            if !r.load(Ordering::Relaxed) { break; }
            println!("Energy: {:.2}", e.g());
            W!(ctx, 1.0);
        });
    });
}

// ============================================================================
// TEST 8: CHORD MEMORY - Agents harmonize via shared state
// ============================================================================

fn test8(m: M, r: Arc<AtomicBool>) {
    println!("\n=== Test 8: Chord Memory (ULTRA TERSE) ===\n");

    RUN_WHILE!(80.0, "chord", r, |ctx| {
        let notes = N::new();

        // Root player
        F!(ctx, [m, notes, r], |c| {
            let roots = [48, 50, 52, 53, 55];
            let mut i = 0;
            LOOP!(c, {
                if !r.load(Ordering::Relaxed) { break; }
                notes.clear();
                let root = roots[i % 5];
                m.on(root, 90);
                notes.add(root);
                notes.add(root + 12);
                println!("Root: {} -> {:?}", root, notes.all());
                W!(c, 3.8); m.off(root); W!(c, 0.2);
                i += 1;
            });
        });

        // Harmony
        F!(ctx, [m, notes, r], |c| {
            W!(c, 0.5);
            LOOP!(c, {
                if !r.load(Ordering::Relaxed) { break; }
                if let Some(&root) = notes.all().iter().filter(|&&n| n < 60).next() {
                    let t = root + 16;
                    let f = root + 19;
                    m.on(t, 70); m.on(f, 70);
                    notes.add(t); notes.add(f);
                    W!(c, 2.5);
                    m.off(t); m.off(f);
                    notes.rm(t); notes.rm(f);
                    W!(c, 1.5);
                } else { W!(c, 0.5); }
            });
        });

        // Embellisher
        F!(ctx, [m, notes, r], |c| {
            let all: Vec<u8> = (60..84).collect();
            W!(c, 1.0);
            LOOP!(c, {
                if !r.load(Ordering::Relaxed) { break; }
                let chord = notes.all();
                let pass: Vec<u8> = all.iter()
                    .filter(|n| !chord.iter().any(|ch| *ch % 12 == **n % 12))
                    .copied().collect();
                if !pass.is_empty() {
                    let n = pass[(c.random() * pass.len() as f64) as usize];
                    m.on(n, 50); W!(c, 0.15); m.off(n);
                }
                W!(c, 0.35);
            });
        });

        W!(ctx, 20.0);
    });
}

// ============================================================================
// TEST 9: MESSAGE QUEUE - Inter-agent communication
// ============================================================================

#[derive(Clone, Debug)]
enum Msg { Note(u8, u8), Accent, Rest, Scale(Vec<u8>) }

fn test9(m: M, r: Arc<AtomicBool>) {
    println!("\n=== Test 9: Message Queue (ULTRA TERSE) ===\n");

    RUN_WHILE!(110.0, "queue", r, |ctx| {
        let q = Q::<Msg>::new();

        // Producer
        F!(ctx, [q, r], |c| {
            let scales = [
                vec![60, 62, 64, 65, 67, 69, 71],
                vec![60, 62, 63, 65, 67, 68, 70],
                vec![60, 62, 64, 66, 67, 69, 71],
            ];
            let mut si = 0;
            let mut beat = 0;
            LOOP!(c, {
                if !r.load(Ordering::Relaxed) { break; }
                if beat % 8 == 0 && beat > 0 {
                    si = (si + 1) % 3;
                    q.push(Msg::Scale(scales[si].clone()));
                    println!("Producer: scale change");
                }
                if beat % 4 == 0 { q.push(Msg::Accent); }
                if c.random() < 0.2 { q.push(Msg::Rest); }
                else {
                    let n = scales[si][(c.random() * scales[si].len() as f64) as usize];
                    q.push(Msg::Note(n, 80));
                }
                W!(c, 0.5);
                beat += 1;
            });
        });

        // Consumer
        F!(ctx, [m, q, r], |c| {
            let mut acc = false;
            let mut rst = false;
            #[allow(unused_variables)]
            let mut scale = vec![60, 62, 64, 65, 67, 69, 71];
            LOOP!(c, {
                if !r.load(Ordering::Relaxed) { break; }
                while let Some(msg) = q.pop() {
                    match msg {
                        Msg::Accent => { acc = true; println!("Consumer: ACCENT"); }
                        Msg::Rest => { rst = true; }
                        Msg::Scale(s) => { scale = s; println!("Consumer: new scale"); }
                        Msg::Note(n, v) => {
                            if rst { rst = false; continue; }
                            let vel = if acc { acc = false; (v + 40).min(127) } else { v };
                            m.on(n, vel); W!(c, 0.4); m.off(n);
                        }
                    }
                }
                W!(c, 0.1);
            });
        });

        W!(ctx, 16.0);
    });
}

// ============================================================================
// TEST 10: CALL AND RESPONSE
// ============================================================================

fn test10(m: M, r: Arc<AtomicBool>) {
    println!("\n=== Test 10: Call/Response (ULTRA TERSE) ===\n");

    RUN_WHILE!(90.0, "callresp", r, |ctx| {
        let phrase = V::new(Vec::<(u8, f64)>::new());
        let ready = V::new(false);

        // Caller
        F!(ctx, [m, phrase, ready, r], |c| {
            let sc = [60, 62, 64, 65, 67, 69, 71, 72];
            LOOP!(c, {
                if !r.load(Ordering::Relaxed) { break; }
                let len = 3 + (c.random() * 3.0) as usize;
                let mut ph = Vec::new();
                println!("Caller: playing...");
                for _ in 0..len {
                    let n = sc[(c.random() * sc.len() as f64) as usize];
                    let d = 0.25 + c.random() * 0.5;
                    ph.push((n, d));
                    m.on(n, 100); W!(c, d * 0.9); m.off(n); W!(c, d * 0.1);
                }
                phrase.m(|p| *p = ph);
                ready.s(true);
                W!(c, 4.0);
            });
        });

        // Responder
        F!(ctx, [m, phrase, ready, r], |c| {
            LOOP!(c, {
                if !r.load(Ordering::Relaxed) { break; }
                while !ready.g() { W!(c, 0.1); }
                ready.s(false);
                let ph = phrase.g();
                if ph.is_empty() { continue; }
                println!("Responder: responding...");
                for (n, d) in ph {
                    let t = n + 12;
                    let vd = d * (0.8 + c.random() * 0.4);
                    m.on(t, 80); W!(c, vd * 0.9); m.off(t); W!(c, vd * 0.1);
                }
                W!(c, 1.0);
            });
        });

        W!(ctx, 24.0);
    });
}

// ============================================================================
// TEST 11: COLLECTIVE VOTE
// ============================================================================

fn test11(m: M, r: Arc<AtomicBool>) {
    println!("\n=== Test 11: Collective Vote (ULTRA TERSE) ===\n");

    RUN_WHILE!(80.0, "vote", r, |ctx| {
        let votes = V::new(Vec::<u8>::new());
        let voting = V::new(true);
        let scale = [60, 62, 64, 65, 67, 69, 71, 72];

        // 3 voters - clone before loop to avoid borrow issues
        for vid in 0..3 {
            let votes = votes.clone();
            let voting = voting.clone();
            let r = r.clone();
            let scale = scale;
            F!(ctx, |c| {
                LOOP!(c, {
                    if !r.load(Ordering::Relaxed) { break; }
                    if voting.g() {
                        let pref = match vid {
                            0 => scale[(c.random() * 3.0) as usize],
                            1 => scale[3 + (c.random() * 2.0) as usize],
                            _ => scale[5 + (c.random() * 3.0) as usize],
                        };
                        votes.m(|v| v.push(pref));
                    }
                    W!(c, 0.5);
                });
            });
        }

        // Conductor
        L!(ctx, 16, {
            if !r.load(Ordering::Relaxed) { break; }
            voting.s(true);
            votes.m(|v| v.clear());
            W!(ctx, 0.5);
            voting.s(false);

            let all = votes.g();
            if all.is_empty() { continue; }

            let mut counts: HashMap<u8, usize> = HashMap::new();
            for v in &all { *counts.entry(*v).or_insert(0) += 1; }
            let winner = *counts.iter().max_by_key(|&(_, cnt)| cnt).unwrap().0;

            println!("Round: {:?} -> {}", all, winner);
            m.on(winner, 100); W!(ctx, 1.3); m.off(winner); W!(ctx, 0.2);
        });
    });
}

// ============================================================================
// TEST 12: CONDUCTOR
// ============================================================================

fn test12(m: M, r: Arc<AtomicBool>) {
    println!("\n=== Test 12: Conductor (ULTRA TERSE) ===\n");

    RUN_WHILE!(100.0, "cond", r, |ctx| {
        let p = P::new();
        p.s("dyn", 0.5); p.s("den", 0.5); p.s("reg", 0.5);
        for i in 1..=3 { p.s(&format!("p{}", i), 1.0); }

        // Conductor - capture ctx clone for tempo changes
        let cc = ctx.clone();
        F!(ctx, [p, r, cc], |c| {
            let mut phase = 0;
            LOOP!(c, {
                if !r.load(Ordering::Relaxed) { break; }
                match phase % 4 {
                    0 => { println!("Cond: soft"); p.s("dyn", 0.3); p.s("den", 0.3); p.s("p3", 0.0); }
                    1 => { println!("Cond: build"); p.s("dyn", 0.6); p.s("den", 0.5); p.s("p3", 1.0); }
                    2 => { println!("Cond: CLIMAX"); p.s("dyn", 1.0); p.s("den", 0.9); p.s("reg", 0.8); }
                    _ => { println!("Cond: fade"); p.s("dyn", 0.2); p.s("den", 0.2); p.s("reg", 0.3); p.s("p1", 0.0); p.s("p2", 0.0); }
                }
                cc.set_bpm(80.0 + (phase % 4) as f64 * 20.0);
                W!(c, 8.0);
                phase += 1;
                p.s("p1", 1.0); p.s("p2", 1.0); p.s("reg", 0.5);
            });
        });

        // 3 performers - clone before loop
        for pid in 0..3 {
            let m = m.clone();
            let p = p.clone();
            let r = r.clone();
            let base = match pid { 0 => 36, 1 => 60, _ => 72 };
            let pk = format!("p{}", pid + 1);

            F!(ctx, |c| {
                LOOP!(c, {
                    if !r.load(Ordering::Relaxed) { break; }
                    if p.g(&pk) < 0.5 { W!(c, 0.5); continue; }
                    let dyn_ = p.g("dyn");
                    let den = p.g("den");
                    let reg = p.g("reg");
                    if c.random() > den { W!(c, 0.5); continue; }
                    let n = base + (reg * 12.0) as u8 + (c.random() * 7.0) as u8;
                    let v = (40.0 + dyn_ * 80.0) as u8;
                    m.on(n, v); W!(c, 0.4); m.off(n); W!(c, 0.1);
                });
            });
        }

        W!(ctx, 35.0);
    });
}

// ============================================================================
// MAIN
// ============================================================================

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    let mut list = false;
    let mut dev: Option<usize> = None;
    let mut test: Option<usize> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--list" | "-l" => list = true,
            "--device" | "-d" => { i += 1; dev = args.get(i).and_then(|s| s.parse().ok()); }
            "--test" | "-t" => { i += 1; test = args.get(i).and_then(|s| s.parse().ok()); }
            _ => {}
        }
        i += 1;
    }

    if list { list_ports(); return Ok(()); }

    let dev = dev.ok_or("Missing --device")?;
    let test = test.ok_or("Missing --test")?;
    let m = connect(dev)?;
    let r = Arc::new(AtomicBool::new(true));

    match test {
        1 => test1(m, r),
        2 => test2(m, r),
        3 => test3(m, r),
        4 => test4(m, r),
        5 => test5(m, r),
        6 => test6(m, r),
        7 => test7(m, r),
        8 => test8(m, r),
        9 => test9(m, r),
        10 => test10(m, r),
        11 => test11(m, r),
        12 => test12(m, r),
        _ => println!("Test 1-12 only"),
    }

    Ok(())
}
