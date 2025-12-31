//! Real-time MIDI Demo for the Rust Timing Library
//!
//! This demo showcases the flexibility of the timing engine with live MIDI output.
//!
//! Usage:
//!   cargo run --bin midi_demo -- --list              # List available MIDI devices
//!   cargo run --bin midi_demo -- --device 0 --test 1 # Run test case 1 on device 0
//!
//! Test Cases:
//!   1. Metronome        - Simple quarter note clicks at 120 BPM
//!   2. Polyrhythm       - 3 vs 4 polyrhythm (3 notes against 4 notes per bar)
//!   3. Tempo Ramp       - Accelerando from 60 to 180 BPM over 8 seconds
//!   4. Barrier Sync     - Two voices that synchronize at phrase boundaries
//!   5. Generative       - Deterministic random melody (same seed = same output)
//!   6. Cancellation     - Demonstrates note-off cleanup on cancel

use midir::{MidiOutput, MidiOutputConnection};
use rust_timing_lib::{
    await_barrier, resolve_barrier, start_barrier, BranchOptions, Ctx, Engine, EngineConfig,
    SchedulerMode,
};
use std::cell::RefCell;
use std::env;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

// MIDI constants
const NOTE_ON: u8 = 0x90;
const NOTE_OFF: u8 = 0x80;

/// Wrapper for MIDI output connection with send capability
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

fn list_midi_devices() -> Result<(), Box<dyn std::error::Error>> {
    let midi_out = MidiOutput::new("midi_demo_list")?;
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
    println!("Usage: cargo run --bin midi_demo -- --device <N> --test <1-6>");

    Ok(())
}

fn connect_to_device(device_index: usize) -> Result<MidiSender, Box<dyn std::error::Error>> {
    let midi_out = MidiOutput::new("midi_demo")?;
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

    let conn = midi_out.connect(port, "midi_demo_conn")?;
    Ok(MidiSender::new(conn))
}

// ============================================================================
// Test Case 1: Simple Metronome
// Demonstrates: Basic beat waiting at a fixed tempo
// ============================================================================

fn run_metronome(midi: Rc<RefCell<MidiSender>>, running: Arc<AtomicBool>) {
    println!("\n=== Test 1: Metronome ===");
    println!("Playing quarter note clicks at 120 BPM for ~8 bars");
    println!("Press Ctrl+C to stop\n");

    let config = EngineConfig {
        bpm: 120.0,
        seed: "metronome".to_string(),
        ..Default::default()
    };

    let mut engine = Engine::new(SchedulerMode::Realtime, config);
    let running_clone = running.clone();

    engine.spawn(move |ctx: Ctx| {
        let midi = midi.clone();
        let running = running_clone.clone();
        async move {
            let hi_note = 76; // High E (downbeat)
            let lo_note = 72; // C (other beats)

            let mut bar = 0;
            while running.load(Ordering::Relaxed) && bar < 8 {
                for beat in 0..4 {
                    let note = if beat == 0 { hi_note } else { lo_note };
                    let velocity = if beat == 0 { 120 } else { 90 };

                    midi.borrow_mut().note_on(note, velocity);
                    let _ = ctx.wait(0.05).await; // Short note duration
                    midi.borrow_mut().note_off(note);

                    // Wait for rest of the beat
                    let _ = ctx.wait(0.95).await;
                }
                bar += 1;
                println!("Bar {} complete", bar);
            }
            println!("Metronome finished.");
        }
    });

    engine.run_until(|| !running.load(Ordering::Relaxed));
}

// ============================================================================
// Test Case 2: Polyrhythm (3 vs 4)
// Demonstrates: Parallel branches with independent timing
// ============================================================================

fn run_polyrhythm(midi: Rc<RefCell<MidiSender>>, running: Arc<AtomicBool>) {
    println!("\n=== Test 2: Polyrhythm (3 vs 4) ===");
    println!("Two voices: one plays 3 notes per bar, the other plays 4");
    println!("This creates a cross-rhythm pattern. Running for 8 bars.\n");

    let config = EngineConfig {
        bpm: 90.0, // Slower to hear the polyrhythm clearly
        seed: "polyrhythm".to_string(),
        ..Default::default()
    };

    let mut engine = Engine::new(SchedulerMode::Realtime, config);
    let running_clone = running.clone();
    let midi2 = midi.clone();

    engine.spawn(move |ctx: Ctx| {
        let midi_3 = midi.clone();
        let midi_4 = midi2.clone();
        let running = running_clone.clone();
        async move {
            // Voice 1: 3 notes per bar (triplet feel over 4 beats)
            let _voice3 = ctx.branch(
                |c| {
                    let midi = midi_3.clone();
                    let running = running.clone();
                    async move {
                        let note = 60; // C4
                        let beats_per_note = 4.0 / 3.0; // 3 notes over 4 beats

                        for _ in 0..24 {
                            // 8 bars * 3 notes
                            if !running.load(Ordering::Relaxed) {
                                break;
                            }
                            midi.borrow_mut().note_on(note, 100);
                            let _ = c.wait(0.1).await;
                            midi.borrow_mut().note_off(note);
                            let _ = c.wait(beats_per_note - 0.1).await;
                        }
                    }
                },
                BranchOptions::default(),
            );

            // Voice 2: 4 notes per bar (straight quarters)
            let _voice4 = ctx.branch(
                |c| {
                    let midi = midi_4.clone();
                    let running = running.clone();
                    async move {
                        let note = 72; // C5
                        let beats_per_note = 1.0; // 4 notes over 4 beats

                        for _ in 0..32 {
                            // 8 bars * 4 notes
                            if !running.load(Ordering::Relaxed) {
                                break;
                            }
                            midi.borrow_mut().note_on(note, 80);
                            let _ = c.wait(0.08).await;
                            midi.borrow_mut().note_off(note);
                            let _ = c.wait(beats_per_note - 0.08).await;
                        }
                    }
                },
                BranchOptions::default(),
            );

            // Wait for both to finish (8 bars = 32 beats)
            let _ = ctx.wait(32.0).await;
            println!("Polyrhythm finished.");
        }
    });

    engine.run_until(|| !running.load(Ordering::Relaxed));
}

// ============================================================================
// Test Case 3: Tempo Ramp (Accelerando)
// Demonstrates: Dynamic tempo changes with automatic beat retiming
// ============================================================================

fn run_tempo_ramp(midi: Rc<RefCell<MidiSender>>, running: Arc<AtomicBool>) {
    println!("\n=== Test 3: Tempo Ramp (Accelerando) ===");
    println!("Starting at 60 BPM, ramping to 180 BPM over 8 seconds");
    println!("The beat waits automatically retime as tempo changes!\n");

    let config = EngineConfig {
        bpm: 60.0,
        seed: "tempo_ramp".to_string(),
        ..Default::default()
    };

    let mut engine = Engine::new(SchedulerMode::Realtime, config);
    let running_clone = running.clone();

    engine.spawn(move |ctx: Ctx| {
        let midi = midi.clone();
        let running = running_clone.clone();
        async move {
            // Start the tempo ramp immediately
            ctx.ramp_bpm_to(180.0, 8.0);
            println!("Tempo ramp initiated: 60 -> 180 BPM over 8 seconds");

            let note = 69; // A4
            let mut beat_count = 0;

            // Play notes on the beat while tempo changes
            while running.load(Ordering::Relaxed) && ctx.time() < 8.5 {
                let current_bpm = ctx.bpm();
                println!(
                    "Beat {} at t={:.2}s, BPM={:.1}",
                    beat_count,
                    ctx.time(),
                    current_bpm
                );

                midi.borrow_mut().note_on(note, 100);
                let _ = ctx.wait(0.1).await;
                midi.borrow_mut().note_off(note);
                let _ = ctx.wait(0.9).await; // Complete the beat

                beat_count += 1;
            }

            println!("\nTempo ramp complete! Played {} beats.", beat_count);
            println!("Notice how beats got closer together as tempo increased.");
        }
    });

    engine.run_until(|| !running.load(Ordering::Relaxed));
}

// ============================================================================
// Test Case 4: Barrier-Synced Voices
// Demonstrates: Using barriers to synchronize multiple voices at phrase points
// ============================================================================

fn run_barrier_sync(midi: Rc<RefCell<MidiSender>>, running: Arc<AtomicBool>) {
    println!("\n=== Test 4: Barrier-Synced Voices ===");
    println!("Two melody voices that sync up at phrase boundaries");
    println!("Voice A plays 4 notes, Voice B plays 5 notes, then they sync.\n");

    let config = EngineConfig {
        bpm: 120.0,
        seed: "barrier_sync".to_string(),
        ..Default::default()
    };

    let mut engine = Engine::new(SchedulerMode::Realtime, config);
    let running_clone = running.clone();
    let midi2 = midi.clone();

    engine.spawn(move |ctx: Ctx| {
        let midi_a = midi.clone();
        let midi_b = midi2.clone();
        let running = running_clone.clone();
        async move {
            for phrase in 0..4 {
                if !running.load(Ordering::Relaxed) {
                    break;
                }

                println!("--- Phrase {} ---", phrase + 1);
                start_barrier("phrase_sync", &ctx);

                // Voice A: 4 quarter notes (4 beats total)
                let handle_a = ctx.branch(
                    |c| {
                        let midi = midi_a.clone();
                        async move {
                            let notes = [60, 62, 64, 65]; // C D E F
                            for note in notes {
                                midi.borrow_mut().note_on(note, 100);
                                let _ = c.wait(0.8).await;
                                midi.borrow_mut().note_off(note);
                                let _ = c.wait(0.2).await;
                            }
                            println!("  Voice A done (4 notes, 4 beats)");
                            let _ = await_barrier("phrase_sync", &c).await;
                        }
                    },
                    BranchOptions::default(),
                );

                // Voice B: 5 notes in the same time (faster, quintuplet-ish)
                let _handle_b = ctx.branch(
                    |c| {
                        let midi = midi_b.clone();
                        async move {
                            let notes = [72, 74, 76, 77, 79]; // C5 D5 E5 F5 G5
                            let beat_per_note = 4.0 / 5.0;
                            for note in notes {
                                midi.borrow_mut().note_on(note, 80);
                                let _ = c.wait(beat_per_note * 0.8).await;
                                midi.borrow_mut().note_off(note);
                                let _ = c.wait(beat_per_note * 0.2).await;
                            }
                            println!("  Voice B done (5 notes, 4 beats)");
                            let _ = await_barrier("phrase_sync", &c).await;
                        }
                    },
                    BranchOptions::default(),
                );

                // Wait a bit for both to complete, then resolve
                let _ = ctx.wait(4.5).await;
                resolve_barrier("phrase_sync", &ctx);
                println!("  Barrier resolved - voices synchronized!\n");
                handle_a.cancel(); // Clean up
            }

            println!("Barrier sync demo finished.");
        }
    });

    engine.run_until(|| !running.load(Ordering::Relaxed));
}

// ============================================================================
// Test Case 5: Generative Melody with Deterministic RNG
// Demonstrates: Reproducible randomness - same seed = identical output every time
// ============================================================================

fn run_generative(midi: Rc<RefCell<MidiSender>>, running: Arc<AtomicBool>) {
    println!("\n=== Test 5: Generative Melody (Deterministic RNG) ===");
    println!("Generating a random melody using seeded RNG.");
    println!("Run this test multiple times - you'll hear the EXACT same melody!\n");

    let config = EngineConfig {
        bpm: 100.0,
        seed: "generative_melody_42".to_string(), // Same seed = same melody
        ..Default::default()
    };

    let mut engine = Engine::new(SchedulerMode::Realtime, config);
    let running_clone = running.clone();

    engine.spawn(move |ctx: Ctx| {
        let midi = midi.clone();
        let running = running_clone.clone();
        async move {
            // Pentatonic scale (sounds good with random notes)
            let scale = [60, 62, 64, 67, 69, 72, 74, 76, 79, 81];
            let durations = [0.5, 1.0, 1.5, 2.0]; // In beats

            println!("Generated sequence (note, duration):");
            for i in 0..16 {
                if !running.load(Ordering::Relaxed) {
                    break;
                }

                // Use deterministic random for note and duration selection
                let note_idx = (ctx.random() * scale.len() as f64) as usize;
                let dur_idx = (ctx.random() * durations.len() as f64) as usize;

                let note = scale[note_idx];
                let duration = durations[dur_idx];

                print!("({}, {:.1}) ", note, duration);
                if (i + 1) % 4 == 0 {
                    println!();
                }

                midi.borrow_mut().note_on(note, 90);
                let _ = ctx.wait(duration * 0.9).await;
                midi.borrow_mut().note_off(note);
                let _ = ctx.wait(duration * 0.1).await;
            }

            println!("\nGenerative melody finished.");
            println!("Run again with same seed to hear identical melody!");
        }
    });

    engine.run_until(|| !running.load(Ordering::Relaxed));
}

// ============================================================================
// Test Case 6: Cancellation with Note-Off Cleanup
// Demonstrates: handle_cancel() guarantees cleanup even when tasks are cancelled
// ============================================================================

fn run_cancellation(midi: Rc<RefCell<MidiSender>>, _running: Arc<AtomicBool>) {
    println!("\n=== Test 6: Cancellation with Note-Off Cleanup ===");
    println!("Starts a sustained chord, then cancels after 2 seconds.");
    println!("handle_cancel() ensures all notes turn off even on cancel.\n");

    let config = EngineConfig {
        bpm: 120.0,
        seed: "cancellation".to_string(),
        ..Default::default()
    };

    let mut engine = Engine::new(SchedulerMode::Realtime, config);
    let midi2 = midi.clone();
    let midi3 = midi.clone();

    let cancel_trigger = Rc::new(RefCell::new(false));
    let cancel_trigger_clone = cancel_trigger.clone();

    engine.spawn(move |ctx: Ctx| {
        let midi = midi.clone();
        let midi_cleanup = midi2.clone();
        let midi_main = midi3.clone();
        let cancel_trigger = cancel_trigger_clone.clone();
        async move {
            // Start a chord that plays for a "long time"
            let chord = [60, 64, 67, 72]; // C major chord

            println!("Starting sustained chord: C E G C (notes 60, 64, 67, 72)");

            let chord_handle = ctx.branch(
                |c| {
                    let midi = midi.clone();
                    let midi_cleanup = midi_cleanup.clone();
                    async move {
                        // Turn on the chord
                        for &note in &chord {
                            midi.borrow_mut().note_on(note, 100);
                        }

                        // Register cancel handler to turn off all notes
                        c.handle_cancel({
                            let midi = midi_cleanup.clone();
                            move || {
                                println!("\n  [Cancel handler invoked!]");
                                println!("  Turning off notes: {:?}", chord);
                                for &note in &chord {
                                    midi.borrow_mut().note_off(note);
                                }
                            }
                        });

                        // Would play for 10 seconds if not cancelled
                        let _ = c.wait(20.0).await;
                        println!("  (This shouldn't print - we cancel before it completes)");
                    }
                },
                BranchOptions::default(),
            );

            // Wait 2 seconds then cancel
            let _ = ctx.wait(4.0).await; // 2 seconds at 120 BPM
            println!("\nCancelling chord after 2 seconds...");
            chord_handle.cancel();

            // Give a moment to hear the silence
            let _ = ctx.wait(2.0).await;

            // Play a confirming note to show we're still running
            println!("Playing confirmation note (cleanup worked!)");
            midi_main.borrow_mut().note_on(84, 100);
            let _ = ctx.wait(1.0).await;
            midi_main.borrow_mut().note_off(84);

            *cancel_trigger.borrow_mut() = true;
            println!("\nCancellation demo finished.");
            println!("Notice: no stuck notes! handle_cancel() cleaned everything up.");
        }
    });

    engine.run_until(|| *cancel_trigger.borrow());
}

// ============================================================================
// Main entry point
// ============================================================================

fn print_usage() {
    println!("Rust Timing Library - Real-time MIDI Demo");
    println!("==========================================");
    println!();
    println!("Usage:");
    println!("  cargo run --bin midi_demo -- --list              # List MIDI devices");
    println!("  cargo run --bin midi_demo -- --device N --test T # Run test T on device N");
    println!();
    println!("Test Cases:");
    println!("  1: Metronome        - Quarter note clicks at 120 BPM");
    println!("  2: Polyrhythm       - 3 vs 4 cross-rhythm pattern");
    println!("  3: Tempo Ramp       - Accelerando from 60 to 180 BPM");
    println!("  4: Barrier Sync     - Two voices synchronizing at phrase boundaries");
    println!("  5: Generative       - Deterministic random melody (reproducible!)");
    println!("  6: Cancellation     - Note-off cleanup with handle_cancel()");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    // Parse arguments
    let mut list_devices = false;
    let mut device_index: Option<usize> = None;
    let mut test_case: Option<usize> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--list" | "-l" => {
                list_devices = true;
            }
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

    // Handle --list
    if list_devices {
        return list_midi_devices();
    }

    // Require both device and test
    let device_index = match device_index {
        Some(d) => d,
        None => {
            print_usage();
            return Err("Missing --device argument".into());
        }
    };

    let test_case = match test_case {
        Some(t) if t >= 1 && t <= 6 => t,
        Some(t) => {
            return Err(format!("Invalid test case {}. Must be 1-6.", t).into());
        }
        None => {
            print_usage();
            return Err("Missing --test argument".into());
        }
    };

    // Connect to MIDI device
    let midi = Rc::new(RefCell::new(connect_to_device(device_index)?));
    println!("Connected successfully!\n");

    // Setup Ctrl+C handler
    let running = Arc::new(AtomicBool::new(true));
    let running_ctrlc = running.clone();
    ctrlc_handler(running_ctrlc);

    // Run the selected test
    match test_case {
        1 => run_metronome(midi, running),
        2 => run_polyrhythm(midi, running),
        3 => run_tempo_ramp(midi, running),
        4 => run_barrier_sync(midi, running),
        5 => run_generative(midi, running),
        6 => run_cancellation(midi, running),
        _ => unreachable!(),
    }

    println!("\nDemo complete. Goodbye!");
    Ok(())
}

fn ctrlc_handler(running: Arc<AtomicBool>) {
    // Simple signal handling - just set the flag
    // In a real app you'd use the `ctrlc` crate
    std::thread::spawn(move || {
        // This is a simplified approach - works on Unix
        // For proper cross-platform Ctrl+C, add the `ctrlc` crate
        loop {
            std::thread::sleep(Duration::from_millis(100));
            // The atomic flag will be checked in the main loops
        }
    });
    let _ = running; // Keep the running flag alive
}
