//! Real-time MIDI Demo with Ergonomic Macros
//!
//! This demo showcases the macro-based API for the timing engine.
//! It ports the original midi_demo.rs examples and adds new examples
//! demonstrating flexible state-sharing patterns for musical "agents".
//!
//! Usage:
//!   cargo run --bin midi_demo_macro -- --list              # List MIDI devices
//!   cargo run --bin midi_demo_macro -- --device 0 --test 1 # Run test case 1
//!
//! Test Cases (Ported):
//!   1. Metronome        - Simple quarter note clicks at 120 BPM
//!   2. Polyrhythm       - 3 vs 4 polyrhythm
//!   3. Tempo Ramp       - Accelerando from 60 to 180 BPM
//!   4. Barrier Sync     - Two voices synchronizing at phrase boundaries
//!   5. Generative       - Deterministic random melody
//!   6. Cancellation     - Note-off cleanup on cancel
//!
//! Test Cases (New - State Sharing):
//!   7. Shared Energy    - Multiple voices affect a shared "energy" level
//!   8. Chord Memory     - Agents harmonize based on shared chord state
//!   9. Message Queue    - Inter-agent communication via event queue
//!  10. Call Response    - One agent plays, another responds
//!  11. Collective Vote  - Agents vote on the next note to play
//!  12. Conductor        - A conductor agent controls multiple performers

use midir::{MidiOutput, MidiOutputConnection};
use rust_timing_lib::{
    await_barrier, resolve_barrier, start_barrier, BranchOptions, Ctx, Engine, EngineConfig,
    SchedulerMode,
};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::env;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

// ============================================================================
// MACROS MODULE - Ergonomic helpers for the timing library
// ============================================================================

/// Wait for beats, propagates cancellation via `?`.
/// Usage: w!(ctx, 4.0);
/// Note: Requires the containing async fn to return Result<(), WaitError>.
/// For fire-and-forget waits in branches that return `()`, use `w_!` instead.
#[allow(unused_macros)]
macro_rules! w {
    ($ctx:expr, $beats:expr) => {
        $ctx.wait($beats).await?
    };
}

/// Wait for seconds, propagates cancellation via `?`.
/// Usage: ws!(ctx, 0.5);
/// Note: Requires the containing async fn to return Result<(), WaitError>.
/// For fire-and-forget waits in branches that return `()`, use `ws_!` instead.
#[allow(unused_macros)]
macro_rules! ws {
    ($ctx:expr, $sec:expr) => {
        $ctx.wait_sec($sec).await?
    };
}

/// Wait for beats, ignores cancellation (fire-and-forget).
/// Usage: w_!(ctx, 4.0);
macro_rules! w_ {
    ($ctx:expr, $beats:expr) => {
        let _ = $ctx.wait($beats).await;
    };
}

/// Wait for seconds, ignores cancellation (fire-and-forget).
/// Usage: ws_!(ctx, 0.5);
macro_rules! ws_ {
    ($ctx:expr, $sec:expr) => {
        let _ = $ctx.wait_sec($sec).await;
    };
}

/// Clone variables for use in async closures.
/// Usage: clone!(x, y => async move { ... })
macro_rules! clone {
    ($($name:ident),+ => $body:expr) => {{
        $(let $name = $name.clone();)+
        $body
    }};
}

// ============================================================================
// SHARED STATE HELPERS - Accessible patterns for Rust newcomers
// ============================================================================

/// A shared value that can be read/written by multiple agents.
/// This wraps Rc<RefCell<T>> with a simpler API.
///
/// # Example
/// ```
/// let energy = Shared::new(0.5);
/// energy.set(0.8);
/// let val = energy.get();
/// energy.modify(|e| *e += 0.1);
/// ```
#[derive(Debug)]
pub struct Shared<T>(Rc<RefCell<T>>);

impl<T> Shared<T> {
    /// Create a new shared value.
    pub fn new(val: T) -> Self {
        Shared(Rc::new(RefCell::new(val)))
    }

    /// Modify the value with a closure.
    pub fn modify<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        f(&mut self.0.borrow_mut())
    }

    /// Get a clone of the inner Rc for manual usage if needed.
    pub fn inner(&self) -> Rc<RefCell<T>> {
        self.0.clone()
    }
}

impl<T: Clone> Shared<T> {
    /// Get a copy of the value.
    pub fn get(&self) -> T {
        self.0.borrow().clone()
    }
}

impl<T: Copy> Shared<T> {
    /// Set the value.
    pub fn set(&self, val: T) {
        *self.0.borrow_mut() = val;
    }
}

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Shared(self.0.clone())
    }
}

/// A shared queue for message passing between agents.
/// Messages can be pushed by any agent and popped by any other.
pub struct MessageQueue<T>(Rc<RefCell<VecDeque<T>>>);

impl<T> MessageQueue<T> {
    pub fn new() -> Self {
        MessageQueue(Rc::new(RefCell::new(VecDeque::new())))
    }

    pub fn push(&self, msg: T) {
        self.0.borrow_mut().push_back(msg);
    }

    pub fn pop(&self) -> Option<T> {
        self.0.borrow_mut().pop_front()
    }

    pub fn is_empty(&self) -> bool {
        self.0.borrow().is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.borrow().len()
    }

    /// Drain all messages, returning them as a Vec.
    pub fn drain_all(&self) -> Vec<T> {
        self.0.borrow_mut().drain(..).collect()
    }
}

impl<T> Clone for MessageQueue<T> {
    fn clone(&self) -> Self {
        MessageQueue(self.0.clone())
    }
}

impl<T> Default for MessageQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// A shared set of currently playing notes.
/// Useful for agents that need to harmonize or avoid collisions.
pub struct NoteSet(Rc<RefCell<std::collections::HashSet<u8>>>);

impl NoteSet {
    pub fn new() -> Self {
        NoteSet(Rc::new(RefCell::new(std::collections::HashSet::new())))
    }

    pub fn add(&self, note: u8) {
        self.0.borrow_mut().insert(note);
    }

    pub fn remove(&self, note: u8) {
        self.0.borrow_mut().remove(&note);
    }

    pub fn contains(&self, note: u8) -> bool {
        self.0.borrow().contains(&note)
    }

    pub fn notes(&self) -> Vec<u8> {
        self.0.borrow().iter().copied().collect()
    }

    pub fn len(&self) -> usize {
        self.0.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.borrow().is_empty()
    }

    pub fn clear(&self) {
        self.0.borrow_mut().clear();
    }
}

impl Clone for NoteSet {
    fn clone(&self) -> Self {
        NoteSet(self.0.clone())
    }
}

impl Default for NoteSet {
    fn default() -> Self {
        Self::new()
    }
}

/// A shared parameter map for named parameters.
/// Agents can read/write parameters by name.
pub struct Params(Rc<RefCell<std::collections::HashMap<String, f64>>>);

impl Params {
    pub fn new() -> Self {
        Params(Rc::new(RefCell::new(std::collections::HashMap::new())))
    }

    pub fn set(&self, name: &str, value: f64) {
        self.0.borrow_mut().insert(name.to_string(), value);
    }

    pub fn get(&self, name: &str) -> Option<f64> {
        self.0.borrow().get(name).copied()
    }

    pub fn get_or(&self, name: &str, default: f64) -> f64 {
        self.0.borrow().get(name).copied().unwrap_or(default)
    }

    pub fn modify(&self, name: &str, f: impl FnOnce(f64) -> f64) {
        let mut map = self.0.borrow_mut();
        if let Some(val) = map.get_mut(name) {
            *val = f(*val);
        }
    }
}

impl Clone for Params {
    fn clone(&self) -> Self {
        Params(self.0.clone())
    }
}

impl Default for Params {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// MIDI SETUP
// ============================================================================

const NOTE_ON: u8 = 0x90;
const NOTE_OFF: u8 = 0x80;

/// Wrapper for MIDI output - now using Shared<> pattern
struct MidiSender {
    conn: MidiOutputConnection,
}

impl MidiSender {
    fn new(conn: MidiOutputConnection) -> Self {
        Self { conn }
    }

    fn note_on(&mut self, note: u8, velocity: u8) {
        let _ = self.conn.send(&[NOTE_ON, note, velocity]);
    }

    fn note_off(&mut self, note: u8) {
        let _ = self.conn.send(&[NOTE_OFF, note, 0]);
    }
}

/// Type alias for shared MIDI - the common pattern
type SharedMidi = Shared<MidiSender>;

fn list_midi_devices() -> Result<(), Box<dyn std::error::Error>> {
    let midi_out = MidiOutput::new("midi_demo_macro_list")?;
    let ports = midi_out.ports();

    if ports.is_empty() {
        println!("No MIDI output devices found.");
        return Ok(());
    }

    println!("Available MIDI output devices:");
    println!("------------------------------");
    for (i, port) in ports.iter().enumerate() {
        let name = midi_out.port_name(port).unwrap_or_else(|_| "Unknown".to_string());
        println!("  {}: {}", i, name);
    }
    println!();
    println!("Usage: cargo run --bin midi_demo_macro -- --device <N> --test <1-12>");

    Ok(())
}

fn connect_to_device(device_index: usize) -> Result<SharedMidi, Box<dyn std::error::Error>> {
    let midi_out = MidiOutput::new("midi_demo_macro")?;
    let ports = midi_out.ports();

    if device_index >= ports.len() {
        return Err(format!(
            "Device index {} out of range. Only {} devices available.",
            device_index,
            ports.len()
        )
        .into());
    }

    let port = &ports[device_index];
    let port_name = midi_out.port_name(port)?;
    println!("Connecting to MIDI device: {}", port_name);

    let conn = midi_out.connect(port, "midi_demo_macro_conn")?;
    Ok(Shared::new(MidiSender::new(conn)))
}

// ============================================================================
// TEST 1: METRONOME (Ported with macros)
// ============================================================================

fn run_metronome(midi: SharedMidi, running: Arc<AtomicBool>) {
    println!("\n=== Test 1: Metronome (Macro Version) ===");
    println!("Playing quarter note clicks at 120 BPM for ~8 bars");
    println!("Press Ctrl+C to stop\n");

    let config = EngineConfig {
        bpm: 120.0,
        seed: "metronome".to_string(),
        ..Default::default()
    };

    let mut engine = Engine::new(SchedulerMode::Realtime, config);

    // Note: clone! macro makes capturing cleaner
    engine.spawn(clone!(midi, running => |ctx: Ctx| async move {
        let hi_note = 76;
        let lo_note = 72;

        let mut bar = 0;
        while running.load(Ordering::Relaxed) && bar < 8 {
            for beat in 0..4 {
                let note = if beat == 0 { hi_note } else { lo_note };
                let velocity = if beat == 0 { 120 } else { 90 };

                midi.modify(|m| m.note_on(note, velocity));
                ws_!(ctx, 0.05);
                midi.modify(|m| m.note_off(note));
                ws_!(ctx, 0.20);  // Rest of beat (at 120 BPM, quarter = 0.5s)
            }
            bar += 1;
            println!("Bar {} complete", bar);
        }
        println!("Metronome finished.");
    }));

    engine.run_until(|| !running.load(Ordering::Relaxed));
}

// ============================================================================
// TEST 2: POLYRHYTHM (Ported with macros)
// ============================================================================

fn run_polyrhythm(midi: SharedMidi, running: Arc<AtomicBool>) {
    println!("\n=== Test 2: Polyrhythm 3 vs 4 (Macro Version) ===");
    println!("Two voices: 3 notes vs 4 notes per bar. Running for 8 bars.\n");

    let config = EngineConfig {
        bpm: 90.0,
        seed: "polyrhythm".to_string(),
        ..Default::default()
    };

    let mut engine = Engine::new(SchedulerMode::Realtime, config);

    engine.spawn(clone!(midi, running => |ctx: Ctx| async move {
        // Voice 1: 3 notes per bar (triplet feel)
        let _voice3 = ctx.branch(
            clone!(midi, running => |c| async move {
                let note = 60;
                let beats_per_note = 4.0 / 3.0;

                for _ in 0..24 {
                    if !running.load(Ordering::Relaxed) { break; }
                    midi.modify(|m| m.note_on(note, 100));
                    w_!(c, 0.1);
                    midi.modify(|m| m.note_off(note));
                    w_!(c, beats_per_note - 0.1);
                }
            }),
            BranchOptions::default(),
        );

        // Voice 2: 4 notes per bar (straight quarters)
        let _voice4 = ctx.branch(
            clone!(midi, running => |c| async move {
                let note = 72;

                for _ in 0..32 {
                    if !running.load(Ordering::Relaxed) { break; }
                    midi.modify(|m| m.note_on(note, 80));
                    w_!(c, 0.08);
                    midi.modify(|m| m.note_off(note));
                    w_!(c, 0.92);
                }
            }),
            BranchOptions::default(),
        );

        w_!(ctx, 32.0);
        println!("Polyrhythm finished.");
    }));

    engine.run_until(|| !running.load(Ordering::Relaxed));
}

// ============================================================================
// TEST 3: TEMPO RAMP (Ported with macros)
// ============================================================================

fn run_tempo_ramp(midi: SharedMidi, running: Arc<AtomicBool>) {
    println!("\n=== Test 3: Tempo Ramp (Macro Version) ===");
    println!("Starting at 60 BPM, ramping to 180 BPM over 8 seconds\n");

    let config = EngineConfig {
        bpm: 60.0,
        seed: "tempo_ramp".to_string(),
        ..Default::default()
    };

    let mut engine = Engine::new(SchedulerMode::Realtime, config);

    engine.spawn(clone!(midi, running => |ctx: Ctx| async move {
        ctx.ramp_bpm_to(180.0, 8.0);
        println!("Tempo ramp initiated: 60 -> 180 BPM over 8 seconds");

        let note = 69;
        let mut beat_count = 0;

        while running.load(Ordering::Relaxed) && ctx.time() < 8.5 {
            println!("Beat {} at t={:.2}s, BPM={:.1}", beat_count, ctx.time(), ctx.bpm());

            midi.modify(|m| m.note_on(note, 100));
            w_!(ctx, 0.1);
            midi.modify(|m| m.note_off(note));
            w_!(ctx, 0.9);

            beat_count += 1;
        }

        println!("\nTempo ramp complete! Played {} beats.", beat_count);
    }));

    engine.run_until(|| !running.load(Ordering::Relaxed));
}

// ============================================================================
// TEST 4: BARRIER SYNC (Ported with macros)
// ============================================================================

fn run_barrier_sync(midi: SharedMidi, running: Arc<AtomicBool>) {
    println!("\n=== Test 4: Barrier Sync (Macro Version) ===");
    println!("Two voices that sync up at phrase boundaries\n");

    let config = EngineConfig {
        bpm: 120.0,
        seed: "barrier_sync".to_string(),
        ..Default::default()
    };

    let mut engine = Engine::new(SchedulerMode::Realtime, config);

    engine.spawn(clone!(midi, running => |ctx: Ctx| async move {
        for phrase in 0..4 {
            if !running.load(Ordering::Relaxed) { break; }

            println!("--- Phrase {} ---", phrase + 1);
            start_barrier("phrase_sync", &ctx);

            // Voice A: 4 quarter notes
            let handle_a = ctx.branch(
                clone!(midi => |c| async move {
                    let notes = [60, 62, 64, 65];
                    for note in notes {
                        midi.modify(|m| m.note_on(note, 100));
                        w_!(c, 0.8);
                        midi.modify(|m| m.note_off(note));
                        w_!(c, 0.2);
                    }
                    println!("  Voice A done");
                    let _ = await_barrier("phrase_sync", &c).await;
                    }),
                BranchOptions::default(),
            );

            // Voice B: 5 faster notes
            let _handle_b = ctx.branch(
                clone!(midi => |c| async move {
                    let notes = [72, 74, 76, 77, 79];
                    let beat_per_note = 4.0 / 5.0;
                    for note in notes {
                        midi.modify(|m| m.note_on(note, 80));
                        w_!(c, beat_per_note * 0.8);
                        midi.modify(|m| m.note_off(note));
                        w_!(c, beat_per_note * 0.2);
                    }
                    println!("  Voice B done");
                    let _ = await_barrier("phrase_sync", &c).await;
                    }),
                BranchOptions::default(),
            );

            w_!(ctx, 4.5);
            resolve_barrier("phrase_sync", &ctx);
            println!("  Barrier resolved!\n");
            handle_a.cancel();
        }

        println!("Barrier sync demo finished.");
    }));

    engine.run_until(|| !running.load(Ordering::Relaxed));
}

// ============================================================================
// TEST 5: GENERATIVE MELODY (Ported with macros)
// ============================================================================

fn run_generative(midi: SharedMidi, running: Arc<AtomicBool>) {
    println!("\n=== Test 5: Generative Melody (Macro Version) ===");
    println!("Deterministic random melody - same seed = same output!\n");

    let config = EngineConfig {
        bpm: 100.0,
        seed: "generative_melody_42".to_string(),
        ..Default::default()
    };

    let mut engine = Engine::new(SchedulerMode::Realtime, config);

    engine.spawn(clone!(midi, running => |ctx: Ctx| async move {
        let scale = [60, 62, 64, 67, 69, 72, 74, 76, 79, 81];
        let durations = [0.5, 1.0, 1.5, 2.0];

        println!("Generated sequence (note, duration):");
        for i in 0..16 {
            if !running.load(Ordering::Relaxed) { break; }

            let note_idx = (ctx.random() * scale.len() as f64) as usize;
            let dur_idx = (ctx.random() * durations.len() as f64) as usize;

            let note = scale[note_idx];
            let duration = durations[dur_idx];

            print!("({}, {:.1}) ", note, duration);
            if (i + 1) % 4 == 0 { println!(); }

            midi.modify(|m| m.note_on(note, 90));
            w_!(ctx, duration * 0.9);
            midi.modify(|m| m.note_off(note));
            w_!(ctx, duration * 0.1);
        }

        println!("\nGenerative melody finished.");
    }));

    engine.run_until(|| !running.load(Ordering::Relaxed));
}

// ============================================================================
// TEST 6: CANCELLATION (Ported with macros)
// ============================================================================

fn run_cancellation(midi: SharedMidi, _running: Arc<AtomicBool>) {
    println!("\n=== Test 6: Cancellation (Macro Version) ===");
    println!("Demonstrates handle_cancel() for guaranteed cleanup\n");

    let config = EngineConfig {
        bpm: 120.0,
        seed: "cancellation".to_string(),
        ..Default::default()
    };

    let mut engine = Engine::new(SchedulerMode::Realtime, config);
    let done = Shared::new(false);

    engine.spawn(clone!(midi, done => |ctx: Ctx| async move {
        let chord = [60, 64, 67, 72];

        println!("Starting sustained chord: C E G C");

        let chord_handle = ctx.branch(
            clone!(midi => |c| async move {
                for &note in &chord {
                    midi.modify(|m| m.note_on(note, 100));
                }

                // Register cleanup
                c.handle_cancel(clone!(midi => move || {
                    println!("\n  [Cancel handler invoked!]");
                    for &note in &chord {
                        midi.modify(|m| m.note_off(note));
                    }
                }));

                if c.wait(20.0).await.is_err() {
                    return;
                }
            }),
            BranchOptions::default(),
        );

        w_!(ctx, 4.0);
        println!("\nCancelling chord after 2 seconds...");
        chord_handle.cancel();

        w_!(ctx, 2.0);

        println!("Playing confirmation note");
        midi.modify(|m| m.note_on(84, 100));
        w_!(ctx, 1.0);
        midi.modify(|m| m.note_off(84));

        done.set(true);
        println!("\nCancellation demo finished.");
    }));

    engine.run_until(|| done.get());
}

// ============================================================================
// TEST 7: SHARED ENERGY (New - State Sharing)
// Multiple voices affect a shared "energy" level that influences dynamics
// ============================================================================

fn run_shared_energy(midi: SharedMidi, running: Arc<AtomicBool>) {
    println!("\n=== Test 7: Shared Energy ===");
    println!("Multiple voices affect a shared 'energy' level.");
    println!("Energy affects note velocity. Watch the dynamics change!\n");

    let config = EngineConfig {
        bpm: 100.0,
        seed: "shared_energy".to_string(),
        ..Default::default()
    };

    let mut engine = Engine::new(SchedulerMode::Realtime, config);

    // Shared energy level (0.0 to 1.0)
    let energy = Shared::new(0.3_f64);

    engine.spawn(clone!(midi, running, energy => |ctx: Ctx| async move {
        // Energy modulator - slowly changes the energy level
        let _energy_mod = ctx.branch(
            clone!(energy, running => |c| async move {
                let mut increasing = true;
                while running.load(Ordering::Relaxed) {
                    energy.modify(|e| {
                        if increasing {
                            *e = (*e + 0.05).min(1.0);
                            if *e >= 1.0 { increasing = false; }
                        } else {
                            *e = (*e - 0.03).max(0.1);
                            if *e <= 0.1 { increasing = true; }
                        }
                    });
                    w_!(c, 0.5);
                }
            }),
            BranchOptions::default(),
        );

        // Bass voice - reads energy for velocity
        let _bass = ctx.branch(
            clone!(midi, energy, running => |c| async move {
                let notes = [36, 38, 40, 41];
                let mut idx = 0;
                while running.load(Ordering::Relaxed) {
                    let vel = (40.0 + energy.get() * 80.0) as u8;
                    let note = notes[idx % notes.len()];
                    midi.modify(|m| m.note_on(note, vel));
                    w_!(c, 1.8);
                    midi.modify(|m| m.note_off(note));
                    w_!(c, 0.2);
                    idx += 1;
                }
            }),
            BranchOptions::default(),
        );

        // Melody voice - also reads energy, contributes to it
        let _melody = ctx.branch(
            clone!(midi, energy, running => |c| async move {
                let scale = [60, 62, 64, 65, 67, 69, 71, 72];
                while running.load(Ordering::Relaxed) {
                    let e = energy.get();
                    let vel = (50.0 + e * 70.0) as u8;

                    // Higher energy = more notes
                    let num_notes = (2.0 + e * 4.0) as usize;
                    for _ in 0..num_notes {
                        let note = scale[(c.random() * scale.len() as f64) as usize];
                        midi.modify(|m| m.note_on(note, vel));
                        w_!(c, 0.2);
                        midi.modify(|m| m.note_off(note));
                        w_!(c, 0.1);
                    }

                    // Contribute to energy based on note density
                    energy.modify(|e| *e = (*e + 0.02).min(1.0));

                    w_!(c, 0.5);
                }
            }),
            BranchOptions::default(),
        );

        // Print energy periodically
        for _ in 0..20 {
            if !running.load(Ordering::Relaxed) { break; }
            println!("Energy: {:.2}", energy.get());
            w_!(ctx, 1.0);
        }

        println!("Shared energy demo finished.");
    }));

    engine.run_until(|| !running.load(Ordering::Relaxed));
}

// ============================================================================
// TEST 8: CHORD MEMORY (New - State Sharing)
// Agents harmonize based on what notes are currently playing
// ============================================================================

fn run_chord_memory(midi: SharedMidi, running: Arc<AtomicBool>) {
    println!("\n=== Test 8: Chord Memory ===");
    println!("Agents harmonize based on shared chord state.\n");

    let config = EngineConfig {
        bpm: 80.0,
        seed: "chord_memory".to_string(),
        ..Default::default()
    };

    let mut engine = Engine::new(SchedulerMode::Realtime, config);

    // Shared note set - tracks what's currently playing
    let playing_notes = NoteSet::new();

    engine.spawn(clone!(midi, running, playing_notes => |ctx: Ctx| async move {
        // Root player - plays bass notes, registers them
        let _root = ctx.branch(
            clone!(midi, playing_notes, running => |c| async move {
                let roots = [48, 50, 52, 53, 55];  // C, D, E, F, G
                let mut idx = 0;
                while running.load(Ordering::Relaxed) {
                    let root = roots[idx % roots.len()];

                    // Clear old chord
                    for note in playing_notes.notes() {
                        playing_notes.remove(note);
                    }

                    // Play new root
                    midi.modify(|m| m.note_on(root, 90));
                    playing_notes.add(root);
                    playing_notes.add(root + 12);  // Indicate octave above is part of chord

                    println!("Root: {} (chord notes: {:?})", root, playing_notes.notes());

                    w_!(c, 3.8);
                    midi.modify(|m| m.note_off(root));
                    w_!(c, 0.2);
                    idx += 1;
                }
            }),
            BranchOptions::default(),
        );

        // Harmony player - adds 3rd and 5th based on root
        let _harmony = ctx.branch(
            clone!(midi, playing_notes, running => |c| async move {
                w_!(c, 0.5);  // Slight delay
                while running.load(Ordering::Relaxed) {
                    let notes = playing_notes.notes();
                    if let Some(&root) = notes.iter().filter(|&&n| n < 60).next() {
                        // Add major 3rd and 5th in upper octave
                        let third = root + 16;  // Major 3rd up an octave
                        let fifth = root + 19;  // 5th up an octave

                        midi.modify(|m| m.note_on(third, 70));
                        midi.modify(|m| m.note_on(fifth, 70));
                        playing_notes.add(third);
                        playing_notes.add(fifth);

                        w_!(c, 2.5);

                        midi.modify(|m| m.note_off(third));
                        midi.modify(|m| m.note_off(fifth));
                        playing_notes.remove(third);
                        playing_notes.remove(fifth);

                        w_!(c, 1.5);
                    } else {
                        w_!(c, 0.5);
                    }
                }
            }),
            BranchOptions::default(),
        );

        // Embellisher - adds passing tones avoiding chord tones
        let _embellish = ctx.branch(
            clone!(midi, playing_notes, running => |c| async move {
                let all_notes: Vec<u8> = (60..84).collect();
                w_!(c, 1.0);
                while running.load(Ordering::Relaxed) {
                    let chord = playing_notes.notes();
                    // Find notes NOT in the chord (passing tones)
                    let passing: Vec<u8> = all_notes.iter()
                        .filter(|n| !chord.iter().any(|c| *c % 12 == **n % 12))
                        .copied()
                        .collect();

                    if !passing.is_empty() {
                        let note = passing[(c.random() * passing.len() as f64) as usize];
                        midi.modify(|m| m.note_on(note, 50));
                        w_!(c, 0.15);
                        midi.modify(|m| m.note_off(note));
                    }
                    w_!(c, 0.35);
                }
            }),
            BranchOptions::default(),
        );

        w_!(ctx, 20.0);
        println!("Chord memory demo finished.");
    }));

    engine.run_until(|| !running.load(Ordering::Relaxed));
}

// ============================================================================
// TEST 9: MESSAGE QUEUE (New - State Sharing)
// Inter-agent communication via event queue
// ============================================================================

/// Message types for agent communication
#[derive(Clone, Debug)]
enum AgentMessage {
    NoteEvent { note: u8, velocity: u8 },
    Accent,
    Rest,
    ChangeScale(Vec<u8>),
}

fn run_message_queue(midi: SharedMidi, running: Arc<AtomicBool>) {
    println!("\n=== Test 9: Message Queue ===");
    println!("Agents communicate via shared message queue.\n");

    let config = EngineConfig {
        bpm: 110.0,
        seed: "message_queue".to_string(),
        ..Default::default()
    };

    let mut engine = Engine::new(SchedulerMode::Realtime, config);

    // Message queue for communication
    let messages = MessageQueue::<AgentMessage>::new();

    engine.spawn(clone!(midi, running, messages => |ctx: Ctx| async move {
        // Producer agent - sends note events and commands
        let _producer = ctx.branch(
            clone!(messages, running => |c| async move {
                let scales = vec![
                    vec![60, 62, 64, 65, 67, 69, 71],  // C major
                    vec![60, 62, 63, 65, 67, 68, 70],  // C minor
                    vec![60, 62, 64, 66, 67, 69, 71],  // C lydian
                ];

                let mut scale_idx = 0;
                let mut beat = 0;
                while running.load(Ordering::Relaxed) {
                    // Every 8 beats, change scale
                    if beat % 8 == 0 && beat > 0 {
                        scale_idx = (scale_idx + 1) % scales.len();
                        messages.push(AgentMessage::ChangeScale(scales[scale_idx].clone()));
                        println!("Producer: Changing scale!");
                    }

                    // Send accent on downbeats
                    if beat % 4 == 0 {
                        messages.push(AgentMessage::Accent);
                    }

                    // Random rests
                    if c.random() < 0.2 {
                        messages.push(AgentMessage::Rest);
                    } else {
                        let note = scales[scale_idx][(c.random() * scales[scale_idx].len() as f64) as usize];
                        messages.push(AgentMessage::NoteEvent { note, velocity: 80 });
                    }

                    w_!(c, 0.5);
                    beat += 1;
                }
            }),
            BranchOptions::default(),
        );

        // Consumer agent - reads messages and plays notes
        let _consumer = ctx.branch(
            clone!(midi, messages, running => |c| async move {
                let mut accent_next = false;
                let mut rest_next = false;
                let mut current_scale = vec![60, 62, 64, 65, 67, 69, 71];

                while running.load(Ordering::Relaxed) {
                    // Process all pending messages
                    while let Some(msg) = messages.pop() {
                        match msg {
                            AgentMessage::Accent => {
                                accent_next = true;
                                println!("Consumer: Got ACCENT");
                            }
                            AgentMessage::Rest => {
                                rest_next = true;
                            }
                            AgentMessage::ChangeScale(scale) => {
                                current_scale = scale;
                                println!("Consumer: Scale changed to {:?}", current_scale);
                            }
                            AgentMessage::NoteEvent { note, velocity } => {
                                if rest_next {
                                    rest_next = false;
                                    continue;
                                }

                                let vel = if accent_next {
                                    accent_next = false;
                                    (velocity + 40).min(127)
                                } else {
                                    velocity
                                };

                                midi.modify(|m| m.note_on(note, vel));
                                w_!(c, 0.4);
                                midi.modify(|m| m.note_off(note));
                            }
                        }
                    }
                    w_!(c, 0.1);
                }
            }),
            BranchOptions::default(),
        );

        w_!(ctx, 16.0);
        println!("Message queue demo finished.");
    }));

    engine.run_until(|| !running.load(Ordering::Relaxed));
}

// ============================================================================
// TEST 10: CALL AND RESPONSE (New - State Sharing)
// One agent plays a phrase, another responds
// ============================================================================

fn run_call_response(midi: SharedMidi, running: Arc<AtomicBool>) {
    println!("\n=== Test 10: Call and Response ===");
    println!("One agent plays, another responds with variation.\n");

    let config = EngineConfig {
        bpm: 90.0,
        seed: "call_response".to_string(),
        ..Default::default()
    };

    let mut engine = Engine::new(SchedulerMode::Realtime, config);

    // Shared phrase buffer - stores last played phrase
    let last_phrase = Shared::new(Vec::<(u8, f64)>::new());
    let phrase_ready = Shared::new(false);

    engine.spawn(clone!(midi, running, last_phrase, phrase_ready => |ctx: Ctx| async move {
        // Caller - plays phrases and stores them
        let _caller = ctx.branch(
            clone!(midi, last_phrase, phrase_ready, running => |c| async move {
                let scale = [60, 62, 64, 65, 67, 69, 71, 72];

                while running.load(Ordering::Relaxed) {
                    // Generate a random phrase (3-5 notes)
                    let phrase_len = 3 + (c.random() * 3.0) as usize;
                    let mut phrase = Vec::new();

                    println!("Caller: Playing phrase...");
                    for _ in 0..phrase_len {
                        let note = scale[(c.random() * scale.len() as f64) as usize];
                        let dur = 0.25 + c.random() * 0.5;
                        phrase.push((note, dur));

                        midi.modify(|m| m.note_on(note, 100));
                        w_!(c, dur * 0.9);
                        midi.modify(|m| m.note_off(note));
                        w_!(c, dur * 0.1);
                    }

                    // Store phrase for responder
                    last_phrase.modify(|p| *p = phrase);
                    phrase_ready.set(true);

                    // Wait for response
                    w_!(c, 4.0);
                }
            }),
            BranchOptions::default(),
        );

        // Responder - waits for phrase, then responds with variation
        let _responder = ctx.branch(
            clone!(midi, last_phrase, phrase_ready, running => |c| async move {
                while running.load(Ordering::Relaxed) {
                    // Wait for a phrase
                    while !phrase_ready.get() {
                        w_!(c, 0.1);
                    }
                    phrase_ready.set(false);

                    // Get the phrase
                    let phrase = last_phrase.get();
                    if phrase.is_empty() {
                        continue;
                    }

                    println!("Responder: Responding with variation...");

                    // Play variation - transpose up and vary rhythm
                    for (note, dur) in phrase {
                        let transposed = note + 12;  // Octave up
                        let varied_dur = dur * (0.8 + c.random() * 0.4);

                        midi.modify(|m| m.note_on(transposed, 80));
                        w_!(c, varied_dur * 0.9);
                        midi.modify(|m| m.note_off(transposed));
                        w_!(c, varied_dur * 0.1);
                    }

                    w_!(c, 1.0);
                }
            }),
            BranchOptions::default(),
        );

        w_!(ctx, 24.0);
        println!("Call and response demo finished.");
    }));

    engine.run_until(|| !running.load(Ordering::Relaxed));
}

// ============================================================================
// TEST 11: COLLECTIVE VOTE (New - State Sharing)
// Multiple agents vote on the next note to play
// ============================================================================

fn run_collective_vote(midi: SharedMidi, running: Arc<AtomicBool>) {
    println!("\n=== Test 11: Collective Vote ===");
    println!("Three agents vote on each note. Majority wins!\n");

    let config = EngineConfig {
        bpm: 80.0,
        seed: "collective_vote".to_string(),
        ..Default::default()
    };

    let mut engine = Engine::new(SchedulerMode::Realtime, config);

    // Vote storage
    let votes = Shared::new(Vec::<u8>::new());
    let vote_phase = Shared::new(true);  // true = voting, false = playing

    engine.spawn(clone!(midi, running, votes, vote_phase => |ctx: Ctx| async move {
        let scale = [60, 62, 64, 65, 67, 69, 71, 72];

        // Three voter agents
        for voter_id in 0..3 {
            let votes = votes.clone();
            let vote_phase = vote_phase.clone();
            let running = running.clone();

            ctx.branch(
                move |c| async move {
                    while running.load(Ordering::Relaxed) {
                        if vote_phase.get() {
                            // Cast vote based on agent's preference
                            let preference = match voter_id {
                                0 => scale[(c.random() * 3.0) as usize],         // Prefers lower notes
                                1 => scale[3 + (c.random() * 2.0) as usize],     // Prefers middle
                                _ => scale[5 + (c.random() * 3.0) as usize],     // Prefers higher
                            };
                            votes.modify(|v| v.push(preference));
                        }
                        w_!(c, 0.5);
                    }
                    },
                BranchOptions::default(),
            );
        }

        // Conductor - collects votes and plays result
        for round in 0..16 {
            if !running.load(Ordering::Relaxed) { break; }

            // Voting phase
            vote_phase.set(true);
            votes.modify(|v| v.clear());
            w_!(ctx, 0.5);
            vote_phase.set(false);

            // Count votes
            let all_votes = votes.get();
            if all_votes.is_empty() {
                continue;
            }

            // Find most common vote (simple majority)
            let mut counts: std::collections::HashMap<u8, usize> = std::collections::HashMap::new();
            for v in &all_votes {
                *counts.entry(*v).or_insert(0) += 1;
            }
            let winner = *counts.iter().max_by_key(|&(_, count)| count).unwrap().0;

            println!("Round {}: Votes {:?} -> Winner: {}", round + 1, all_votes, winner);

            // Play the winning note
            midi.modify(|m| m.note_on(winner, 100));
            w_!(ctx, 1.3);
            midi.modify(|m| m.note_off(winner));
            w_!(ctx, 0.2);
        }

        println!("Collective vote demo finished.");
    }));

    engine.run_until(|| !running.load(Ordering::Relaxed));
}

// ============================================================================
// TEST 12: CONDUCTOR (New - State Sharing)
// A conductor agent controls multiple performer agents
// ============================================================================

fn run_conductor(midi: SharedMidi, running: Arc<AtomicBool>) {
    println!("\n=== Test 12: Conductor ===");
    println!("A conductor controls tempo, dynamics, and cues for performers.\n");

    let config = EngineConfig {
        bpm: 100.0,
        seed: "conductor".to_string(),
        ..Default::default()
    };

    let mut engine = Engine::new(SchedulerMode::Realtime, config);

    // Conductor's parameters
    let params = Params::new();
    params.set("dynamics", 0.5);      // 0.0 = pp, 1.0 = ff
    params.set("density", 0.5);       // Note density
    params.set("register", 0.5);      // 0.0 = low, 1.0 = high
    params.set("performer_1", 1.0);   // 1.0 = play, 0.0 = rest
    params.set("performer_2", 1.0);
    params.set("performer_3", 1.0);

    engine.spawn(clone!(midi, running, params => |ctx: Ctx| async move {
        // Conductor - changes parameters over time
        let conductor_ctx = ctx.clone();
        let _conductor = ctx.branch(
            clone!(params, running => |c| async move {
                let mut phase = 0;
                while running.load(Ordering::Relaxed) {
                    match phase % 4 {
                        0 => {
                            println!("Conductor: Soft and sparse");
                            params.set("dynamics", 0.3);
                            params.set("density", 0.3);
                            params.set("performer_3", 0.0);  // Silence performer 3
                        }
                        1 => {
                            println!("Conductor: Building...");
                            params.set("dynamics", 0.6);
                            params.set("density", 0.5);
                            params.set("performer_3", 1.0);
                        }
                        2 => {
                            println!("Conductor: Climax!");
                            params.set("dynamics", 1.0);
                            params.set("density", 0.9);
                            params.set("register", 0.8);
                        }
                        _ => {
                            println!("Conductor: Fade out");
                            params.set("dynamics", 0.2);
                            params.set("density", 0.2);
                            params.set("register", 0.3);
                            params.set("performer_1", 0.0);
                            params.set("performer_2", 0.0);
                        }
                    }

                    // Tempo changes too!
                    let new_bpm = 80.0 + (phase % 4) as f64 * 20.0;
                    conductor_ctx.set_bpm(new_bpm);
                    println!("  (Tempo: {} BPM)", new_bpm);

                    w_!(c, 8.0);
                    phase += 1;

                    // Reset performers
                    params.set("performer_1", 1.0);
                    params.set("performer_2", 1.0);
                    params.set("register", 0.5);
                }
            }),
            BranchOptions::default(),
        );

        // Three performer agents - each responds to conductor's parameters
        for performer_id in 0..3 {
            let midi = midi.clone();
            let params = params.clone();
            let running = running.clone();

            let base_note = match performer_id {
                0 => 36,  // Bass
                1 => 60,  // Mid
                _ => 72,  // High
            };

            let param_name = format!("performer_{}", performer_id + 1);

            ctx.branch(
                move |c| async move {
                    while running.load(Ordering::Relaxed) {
                        // Check if we should play
                        if params.get_or(&param_name, 1.0) < 0.5 {
                            w_!(c, 0.5);
                            continue;
                        }

                        let dynamics = params.get_or("dynamics", 0.5);
                        let density = params.get_or("density", 0.5);
                        let register = params.get_or("register", 0.5);

                        // Probability of playing this beat
                        if c.random() > density {
                            w_!(c, 0.5);
                            continue;
                        }

                        // Calculate note based on register
                        let offset = (register * 12.0) as u8;
                        let note = base_note + offset + (c.random() * 7.0) as u8;
                        let velocity = (40.0 + dynamics * 80.0) as u8;

                        midi.modify(|m| m.note_on(note, velocity));
                        w_!(c, 0.4);
                        midi.modify(|m| m.note_off(note));
                        w_!(c, 0.1);
                    }
                    },
                BranchOptions::default(),
            );
        }

        w_!(ctx, 35.0);
        println!("Conductor demo finished.");
    }));

    engine.run_until(|| !running.load(Ordering::Relaxed));
}

// ============================================================================
// MAIN
// ============================================================================

fn print_usage() {
    println!("Rust Timing Library - MIDI Demo with Macros");
    println!("============================================");
    println!();
    println!("Usage:");
    println!("  cargo run --bin midi_demo_macro -- --list");
    println!("  cargo run --bin midi_demo_macro -- --device N --test T");
    println!();
    println!("Ported Tests (1-6):");
    println!("  1: Metronome        - Quarter notes at 120 BPM");
    println!("  2: Polyrhythm       - 3 vs 4 cross-rhythm");
    println!("  3: Tempo Ramp       - Accelerando 60->180 BPM");
    println!("  4: Barrier Sync     - Synchronized phrase boundaries");
    println!("  5: Generative       - Deterministic random melody");
    println!("  6: Cancellation     - Note-off cleanup");
    println!();
    println!("New State-Sharing Tests (7-12):");
    println!("  7: Shared Energy    - Multiple voices affect shared dynamics");
    println!("  8: Chord Memory     - Agents harmonize via shared chord state");
    println!("  9: Message Queue    - Inter-agent message passing");
    println!(" 10: Call Response    - One plays, another responds");
    println!(" 11: Collective Vote  - Agents vote on next note");
    println!(" 12: Conductor        - Conductor controls performers");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    let mut list_devices = false;
    let mut device_index: Option<usize> = None;
    let mut test_case: Option<usize> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--list" | "-l" => list_devices = true,
            "--device" | "-d" => {
                i += 1;
                if i < args.len() {
                    device_index = args[i].parse().ok();
                }
            }
            "--test" | "-t" => {
                i += 1;
                if i < args.len() {
                    test_case = args[i].parse().ok();
                }
            }
            "--help" | "-h" => {
                print_usage();
                return Ok(());
            }
            _ => {}
        }
        i += 1;
    }

    if list_devices {
        return list_midi_devices();
    }

    let device_index = match device_index {
        Some(d) => d,
        None => {
            print_usage();
            return Err("Missing --device argument".into());
        }
    };

    let test_case = match test_case {
        Some(t) if (1..=12).contains(&t) => t,
        Some(t) => {
            return Err(format!("Invalid test case {}. Must be 1-12.", t).into());
        }
        None => {
            print_usage();
            return Err("Missing --test argument".into());
        }
    };

    let midi = connect_to_device(device_index)?;
    println!("Connected successfully!\n");

    let running = Arc::new(AtomicBool::new(true));
    let running_ctrlc = running.clone();

    // Simple signal handling
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    let _ = running_ctrlc;

    match test_case {
        1 => run_metronome(midi, running),
        2 => run_polyrhythm(midi, running),
        3 => run_tempo_ramp(midi, running),
        4 => run_barrier_sync(midi, running),
        5 => run_generative(midi, running),
        6 => run_cancellation(midi, running),
        7 => run_shared_energy(midi, running),
        8 => run_chord_memory(midi, running),
        9 => run_message_queue(midi, running),
        10 => run_call_response(midi, running),
        11 => run_collective_vote(midi, running),
        12 => run_conductor(midi, running),
        _ => unreachable!(),
    }

    println!("\nDemo complete. Goodbye!");
    Ok(())
}
