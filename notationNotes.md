# Notation and Ergonomics: Rust vs TypeScript Timing Library

This document analyzes the ergonomic differences between the TypeScript and Rust implementations of the timing library, and proposes various approaches to make the Rust API more "notational" and terse.

---

## 1. Core Ergonomic Differences

### 1.1 Wait Syntax

**TypeScript:**
```typescript
await ctx.waitSec(0.5);
await ctx.wait(4);  // beats
```

**Rust (current):**
```rust
let _ = ctx.wait_sec(0.5).await;
let _ = ctx.wait(4.0).await;
// or with error handling:
ctx.wait_sec(0.5).await?;
```

The Rust version requires:
- Explicit `.await` call
- Either `let _ =` to ignore the `Result`, or `?` for propagation
- The function must return `Result<(), WaitError>` if using `?`

### 1.2 Cancellation Propagation

**TypeScript:**
```typescript
// Automatic exit on cancel - exception bubbles up
await ctx.waitSec(1.0);  // If cancelled, throws Error("aborted")
console.log("unreachable if cancelled");
```

**Rust (current):**
```rust
// Result is ignored, execution continues even if cancelled!
let _ = ctx.wait_sec(1.0).await;
println!("This WILL print even if cancelled");

// Must explicitly handle:
if ctx.wait_sec(1.0).await.is_err() {
    return;
}
// Or with `?`:
ctx.wait_sec(1.0).await?;
```

### 1.3 Branch/Closure Syntax

**TypeScript:**
```typescript
ctx.branch(async (c) => {
    await c.wait(4);
    console.log("done");
}, "myBranch");
```

**Rust (current):**
```rust
ctx.branch(
    |c| async move {
        let _ = c.wait(4.0).await;
        println!("done");
    },
    BranchOptions::default(),
);
```

Rust requires:
- `|c| async move { }` instead of `async (c) => {}`
- Explicit `async move` to capture ownership
- `BranchOptions::default()` (no named parameter)

### 1.4 Shared State in Closures

**TypeScript:**
```typescript
const midi = getMidi();
ctx.branch(async (c) => {
    midi.noteOn(60);  // Just works - closure captures by reference
    await c.wait(1);
    midi.noteOff(60);
});
```

**Rust (current):**
```rust
let midi = Rc::new(RefCell::new(get_midi()));
let midi_clone = midi.clone();  // Must clone before moving
ctx.branch(
    move |c| {
        let midi = midi_clone.clone();  // Clone again for async block
        async move {
            midi.borrow_mut().note_on(60);
            let _ = c.wait(1.0).await;
            midi.borrow_mut().note_off(60);
        }
    },
    BranchOptions::default(),
);
```

The Rust version requires:
- Wrapping in `Rc<RefCell<>>`
- Pre-cloning before the closure
- Cloning again inside the closure (for `async move`)
- `.borrow_mut()` on every access

### 1.5 Parallel Waiting

**TypeScript:**
```typescript
const [a, b] = await Promise.all([
    ctx.branchWait(async (c) => { await c.wait(1); }),
    ctx.branchWait(async (c) => { await c.wait(2); }),
]);
```

**Rust (current):**
```rust
let a = ctx.branch_wait(|c| async move { let _ = c.wait(1.0).await; }, BranchOptions::default());
let b = ctx.branch_wait(|c| async move { let _ = c.wait(2.0).await; }, BranchOptions::default());
let _ = a.await;
let _ = b.await;
```

---

## 2. The "Knobs" - Design Trade-offs

Here are the key dimensions that can be adjusted when designing the user experience:

### Knob A: Cancellation Semantics
| Option | Description | Pros | Cons |
|--------|-------------|------|------|
| A1: Ignore by default | `wait()` returns `Result`, user uses `let _ =` | Simple, flexible | Easy to forget, continues after cancel |
| A2: `?` propagation | User must add `?` to every wait | Idiomatic Rust, clear | Verbose, closure needs Result return |
| A3: Macro-wrapped | Macro transforms to `?` | Cleaner syntax | Hidden control flow, macro complexity |
| A4: Panic on cancel | Wait panics, caught at branch boundary | Automatic like TS | Anti-pattern, unsafe if not caught |

### Knob B: Closure Syntax
| Option | Description | Pros | Cons |
|--------|-------------|------|------|
| B1: Raw closures | `\|c\| async move { }` | Standard Rust | Verbose |
| B2: Branch macro | `branch!(c => { })` | Cleaner | Another macro to learn |
| B3: Named functions | `#[branch] async fn my_branch(c: Ctx) { }` | Very clean | Can't inline, proc-macro needed |

### Knob C: Wait Syntax
| Option | Description | Pros | Cons |
|--------|-------------|------|------|
| C1: Method calls | `ctx.wait(1.0).await?` | Standard | Verbose |
| C2: Wait macro | `w!(ctx, 1.0)` or `wait!(c, 1.0)` | Terse | Another thing to import |
| C3: Context macro | `c.w(1.0)` (method) | Terse, method-like | Non-standard, might be confusing |

### Knob D: Resource Sharing
| Option | Description | Pros | Cons |
|--------|-------------|------|------|
| D1: Manual Rc/RefCell | User manages cloning | Full control | Very verbose |
| D2: Shared<T> wrapper | Auto-clone on capture | Less verbose | Hidden behavior |
| D3: Clone macro | `clone!(midi, ctx => { })` | Explicit but easier | Macro learning curve |

### Knob E: Branch Options
| Option | Description | Pros | Cons |
|--------|-------------|------|------|
| E1: Struct parameter | `BranchOptions::default()` | Type-safe | Verbose for common case |
| E2: Builder pattern | `.branch(f).tempo_cloned()` | Fluent | More code in library |
| E3: Macro with kwargs | `branch!(c, tempo=cloned => {})` | Concise | Complex macro |
| E4: Separate methods | `branch_cloned()`, `branch_shared()` | Simple | API explosion |

---

## 3. Proposed Notation Options

Here are several complete design options, each making different trade-offs:

### Option 1: Minimal - Just a Wait Macro

**Philosophy:** Change as little as possible, just address the most painful part.

**Required user conventions:**
- Must use `w!` macro for waits
- Must use `?` at end of `w!` calls if you want cancellation to propagate
- Must still manually clone Rc values

**Macros:**
```rust
/// Short wait macro that includes .await?
macro_rules! w {
    ($ctx:expr, $dur:expr) => {
        $ctx.wait($dur).await?
    };
}

/// Short wait_sec macro
macro_rules! ws {
    ($ctx:expr, $dur:expr) => {
        $ctx.wait_sec($dur).await?
    };
}
```

**Usage:**
```rust
// Before
let _ = ctx.wait(4.0).await;
let _ = ctx.wait_sec(0.5).await;

// After
w!(ctx, 4.0);
ws!(ctx, 0.5);
```

**Closure still looks like:**
```rust
ctx.branch(
    |c| async move {
        w!(c, 1.0);
        w!(c, 2.0);
        Ok(())  // Required for ? to work
    },
    BranchOptions::default(),
);
```

**Pros:** Minimal change, easy to understand
**Cons:** Still verbose closures, requires `Ok(())` return

---

### Option 2: Moderate - Wait + Clone Macros

**Philosophy:** Address both waiting and shared state pain points.

**Required user conventions:**
- Use `w!` for waits
- Use `clone!` for capturing shared resources
- Closures return `Result<(), WaitError>`

**Macros:**
```rust
/// Clone multiple values for use in a closure
macro_rules! clone {
    ($($name:ident),* => $body:expr) => {{
        $(let $name = $name.clone();)*
        $body
    }};
}

/// Wait macro
macro_rules! w {
    ($ctx:expr, $dur:expr) => { $ctx.wait($dur).await? };
}

macro_rules! ws {
    ($ctx:expr, $dur:expr) => { $ctx.wait_sec($dur).await? };
}
```

**Usage:**
```rust
let midi = Rc::new(RefCell::new(get_midi()));

ctx.branch(
    clone!(midi => |c| {
        clone!(midi => async move {
            midi.borrow_mut().note_on(60);
            w!(c, 1.0);
            midi.borrow_mut().note_off(60);
            Ok(())
        })
    }),
    BranchOptions::default(),
);
```

**Pros:** Addresses two major pain points
**Cons:** Double clone! is awkward, still need Ok(())

---

### Option 3: Branch Macro with Auto-Clone

**Philosophy:** One macro handles the whole branch pattern.

**Required user conventions:**
- Use `branch!` for branches
- Use `w!` for waits
- List captured variables in the macro

**Macros:**
```rust
/// Complete branch macro with auto-cloning
macro_rules! branch {
    ($ctx:expr, [$($cap:ident),*], |$c:ident| $body:expr) => {{
        $(let $cap = $cap.clone();)*
        $ctx.branch(
            move |$c| {
                $(let $cap = $cap.clone();)*
                async move {
                    let __result: Result<(), WaitError> = (|| async { $body; Ok(()) })().await;
                    __result
                }
            },
            BranchOptions::default(),
        )
    }};
}

/// Branch wait variant
macro_rules! branch_wait {
    ($ctx:expr, [$($cap:ident),*], |$c:ident| $body:expr) => {{
        $(let $cap = $cap.clone();)*
        $ctx.branch_wait(
            move |$c| {
                $(let $cap = $cap.clone();)*
                async move { $body; }
            },
            BranchOptions::default(),
        )
    }};
}
```

**Usage:**
```rust
let midi = Rc::new(RefCell::new(get_midi()));

branch!(ctx, [midi], |c| {
    midi.borrow_mut().note_on(60);
    w!(c, 1.0);
    midi.borrow_mut().note_off(60);
});

// With no captures:
branch!(ctx, [], |c| {
    w!(c, 1.0);
    println!("done");
});
```

**Pros:** Much cleaner syntax, handles cloning automatically
**Cons:** Macro is more complex, less IDE support

---

### Option 4: Method Extensions + Terse Macros

**Philosophy:** Add short method names to Ctx, use macros sparingly.

**Required user conventions:**
- Use short method names (`.w()`, `.ws()`)
- Use `b!` and `bw!` for branches
- Waits are fire-and-forget (no result handling) by convention

**Implementation (library changes):**
```rust
impl Ctx {
    /// Short wait (beats) - ignores cancellation result
    pub async fn w(&self, beats: f64) {
        let _ = self.wait(beats).await;
    }

    /// Short wait_sec - ignores cancellation result
    pub async fn ws(&self, sec: f64) {
        let _ = self.wait_sec(sec).await;
    }

    /// Checked wait - propagates cancellation
    pub async fn wc(&self, beats: f64) -> Result<(), WaitError> {
        self.wait(beats).await
    }
}

// Macros just for branch syntax
macro_rules! b {
    ($ctx:expr, |$c:ident| $body:expr) => {
        $ctx.branch(|$c| async move { $body }, BranchOptions::default())
    };
}

macro_rules! bw {
    ($ctx:expr, |$c:ident| $body:expr) => {
        $ctx.branch_wait(|$c| async move { $body }, BranchOptions::default())
    };
}
```

**Usage:**
```rust
// Waits (fire and forget by convention)
c.w(1.0).await;
c.ws(0.5).await;

// Branches
b!(ctx, |c| {
    c.w(1.0).await;
    c.w(2.0).await;
});

// With captures - manual but cleaner
let midi = midi.clone();
b!(ctx, |c| {
    let midi = midi.clone();
    midi.borrow_mut().note_on(60);
    c.w(1.0).await;
    midi.borrow_mut().note_off(60);
});
```

**Pros:** More discoverable (methods), simple macros
**Cons:** Still need `.await`, still manual cloning

---

### Option 5: Maximum Terseness - DSL Macro

**Philosophy:** Create a domain-specific language that looks like pseudo-code.

**Required user conventions:**
- Must wrap timing code in `timing!` block
- Special syntax for waits and branches
- Variables are auto-captured

**Macros (proc-macro required):**
```rust
// This would require a procedural macro
timing! {
    ctx {
        ~1.0;          // wait 1 beat
        ~s:0.5;        // wait 0.5 seconds

        fork {         // branch
            ~2.0;
            print("forked");
        }

        join {         // branch_wait
            ~1.0;
            ~1.0;
        }

        // Auto-captures 'midi' and clones it
        fork [midi] {
            midi.note_on(60);
            ~1.0;
            midi.note_off(60);
        }
    }
}
```

**Expands to:**
```rust
{
    let _ = ctx.wait(1.0).await;
    let _ = ctx.wait_sec(0.5).await;

    ctx.branch(|__c| async move {
        let _ = __c.wait(2.0).await;
        print("forked");
    }, BranchOptions::default());

    ctx.branch_wait(|__c| async move {
        let _ = __c.wait(1.0).await;
        let _ = __c.wait(1.0).await;
    }, BranchOptions::default()).await;

    {
        let midi = midi.clone();
        ctx.branch(|__c| {
            let midi = midi.clone();
            async move {
                midi.borrow_mut().note_on(60);
                let _ = __c.wait(1.0).await;
                midi.borrow_mut().note_off(60);
            }
        }, BranchOptions::default());
    }
}
```

**Pros:** Extremely terse, pseudo-code like
**Cons:** Requires proc-macro, non-standard syntax, harder to debug

---

### Option 6: Pragmatic Middle Ground (Recommended)

**Philosophy:** Balance terseness with maintainability. Use simple declarative macros, accept some verbosity in closures.

**Required user conventions:**
1. Use `w!` for all waits (includes `.await?`)
2. Closure blocks return `Ok(())` at the end
3. Use `clone!` when you need to capture shared resources
4. Add `?` suffix to handle cancellation

**Complete macro set:**
```rust
/// Wait for beats - propagates cancellation with ?
macro_rules! w {
    ($ctx:expr, $dur:expr) => {
        $ctx.wait($dur).await?
    };
}

/// Wait for seconds - propagates cancellation with ?
macro_rules! ws {
    ($ctx:expr, $dur:expr) => {
        $ctx.wait_sec($dur).await?
    };
}

/// Clone helper for captures
macro_rules! clone {
    ($($name:ident),+ => $body:expr) => {{
        $(let $name = $name.clone();)+
        $body
    }};
}

/// Convenient branch with opts
macro_rules! branch {
    // No options
    ($ctx:expr, |$c:ident| { $($body:tt)* }) => {
        $ctx.branch(
            |$c| async move { $($body)* Ok::<(), WaitError>(()) },
            BranchOptions::default()
        )
    };
    // With tempo=cloned
    ($ctx:expr, tempo=cloned, |$c:ident| { $($body:tt)* }) => {
        $ctx.branch(
            |$c| async move { $($body)* Ok::<(), WaitError>(()) },
            BranchOptions { tempo: TempoMode::Cloned, ..Default::default() }
        )
    };
    // With rng=shared
    ($ctx:expr, rng=shared, |$c:ident| { $($body:tt)* }) => {
        $ctx.branch(
            |$c| async move { $($body)* Ok::<(), WaitError>(()) },
            BranchOptions { rng: RngMode::Shared, ..Default::default() }
        )
    };
}

/// Branch wait variant
macro_rules! branch_wait {
    ($ctx:expr, |$c:ident| { $($body:tt)* }) => {
        $ctx.branch_wait(
            |$c| async move { $($body)* },
            BranchOptions::default()
        )
    };
}
```

**Complete example - MIDI polyrhythm (compare to midi_demo.rs):**
```rust
// BEFORE (current Rust)
fn run_polyrhythm(midi: Rc<RefCell<MidiSender>>, running: Arc<AtomicBool>) {
    let config = EngineConfig { bpm: 90.0, .. };
    let mut engine = Engine::new(SchedulerMode::Realtime, config);
    let midi2 = midi.clone();

    engine.spawn(move |ctx: Ctx| {
        let midi_3 = midi.clone();
        let midi_4 = midi2.clone();
        let running = running.clone();
        async move {
            let _voice3 = ctx.branch(
                |c| {
                    let midi = midi_3.clone();
                    let running = running.clone();
                    async move {
                        let note = 60;
                        let beats_per_note = 4.0 / 3.0;
                        for _ in 0..24 {
                            if !running.load(Ordering::Relaxed) { break; }
                            midi.borrow_mut().note_on(note, 100);
                            let _ = c.wait(0.1).await;
                            midi.borrow_mut().note_off(note);
                            let _ = c.wait(beats_per_note - 0.1).await;
                        }
                    }
                },
                BranchOptions::default(),
            );
            // ... voice 4 similar
            let _ = ctx.wait(32.0).await;
        }
    });
}

// AFTER (with proposed macros)
fn run_polyrhythm(midi: Rc<RefCell<MidiSender>>, running: Arc<AtomicBool>) {
    let config = EngineConfig { bpm: 90.0, .. };
    let mut engine = Engine::new(SchedulerMode::Realtime, config);

    engine.spawn(clone!(midi, running => |ctx| async move {
        branch!(ctx, |c| {
            clone!(midi, running => {
                let note = 60;
                let beats_per_note = 4.0 / 3.0;
                for _ in 0..24 {
                    if !running.load(Ordering::Relaxed) { break; }
                    midi.borrow_mut().note_on(note, 100);
                    w!(c, 0.1);
                    midi.borrow_mut().note_off(note);
                    w!(c, beats_per_note - 0.1);
                }
            })
        });
        // ... voice 4 similar
        w!(ctx, 32.0);
    }));
}
```

---

## 4. Recommendations by Use Case

### For Exploratory/Composition Use (Maximum Terseness)
Use **Option 5** or **Option 6** with the full macro set. The goal is to get ideas down quickly without fighting syntax.

### For Production/Library Code (Clarity + Safety)
Use **Option 1** or **Option 4**. Keep macros minimal, prefer explicit code that's easier to debug and understand.

### For Teaching/Learning
Use **Option 3** or **Option 6**. The branch macro makes the structure clearer while hiding some complexity.

---

## 5. Cancellation Patterns to Document

Regardless of which macro approach is chosen, users need guidance on cancellation:

### Pattern 1: Fire-and-Forget (Ignore Cancellation)
```rust
// For branches that can be interrupted at any point
let _ = ctx.wait(1.0).await;  // or w_ignore!(c, 1.0)
```

### Pattern 2: Propagating (Exit on Cancel)
```rust
// For branches that should stop immediately on cancel
ctx.wait(1.0).await?;  // or w!(c, 1.0) with Result return
```

### Pattern 3: Check-and-Handle (Custom Behavior)
```rust
if ctx.wait(1.0).await.is_err() {
    cleanup();
    return;
}
```

### Pattern 4: Cancel Handler (Guaranteed Cleanup)
```rust
let handle = ctx.branch(|c| async move {
    play_note(60);
    c.wait(1.0).await;
}, BranchOptions::default());

handle.handle_cancel(|| {
    stop_note(60);  // Always runs on cancel
});
```

---

## 6. Summary: Convention Rules for Users

If using **Option 6 (Recommended)**:

1. **Always use `w!` or `ws!` for waits** - these include `.await?` and propagate cancellation
2. **Use `clone!` for captured resources** - wrap in `clone!(var1, var2 => ...)` before closures
3. **Branch blocks auto-return `Ok(())`** - the macro handles this
4. **Use `handle_cancel` for cleanup** - don't rely on drop or finally semantics
5. **Use `Rc<RefCell<T>>` for shared mutable state** - this is unavoidable in Rust async
6. **Name your branches** - helps debugging (`branch!(ctx, "melody", |c| {...})`)

---

## 7. Future Considerations

### Proc-Macro for #[cancellable]
```rust
#[cancellable]
async fn my_melody(c: Ctx) -> Result<(), WaitError> {
    c.wait(1.0).await;  // Becomes .await?
    c.wait(2.0).await;
    Ok(())  // Auto-added
}
```

### Trait-Based Resource Sharing
```rust
trait Shared: Clone {
    fn share(&self) -> Self { self.clone() }
}

// Auto-impl for Rc<RefCell<T>>
```

### Builder Pattern for Engine
```rust
Engine::builder()
    .bpm(120.0)
    .seed("my_seed")
    .on_cancel(|ctx| { cleanup(); })
    .build()
    .spawn(my_async_fn);
```

---

## 8. Implementation Reality Check

During implementation of `midi_demo_macro.rs`, we discovered an important constraint:

**`ctx.branch()` requires `Future<Output = ()>`**, not `Result<(), WaitError>`.

This means:
- Branch closures must return `()`, not `Result`
- You cannot use `?` to propagate errors out of a branch
- Cancellation must be handled explicitly with `is_err()` checks or ignored

**Practical consequence:** We use `w_!` (fire-and-forget) macros in branches:
```rust
ctx.branch(|c| async move {
    // Use w_! which ignores cancellation
    w_!(c, 1.0);
    w_!(c, 2.0);
    // No return value needed
}, BranchOptions::default());
```

For branches that need to stop on cancellation, check explicitly:
```rust
ctx.branch(|c| async move {
    if c.wait(1.0).await.is_err() {
        return;  // Exit early on cancel
    }
    // Continue...
}, BranchOptions::default());
```

This constraint is actually reasonable - branches are "fire and forget" tasks. If you need error propagation, use `branch_wait` which does return a `Result` that the parent can await.

---

## 9. Appendix: Full Macro Implementation

Here is a complete, ready-to-use macro module:

```rust
//! timing_macros.rs
//!
//! Ergonomic macros for the timing library.

use crate::context::{WaitError, BranchOptions, TempoMode, RngMode};

/// Wait for beats, propagates cancellation.
/// Usage: w!(ctx, 4.0);
#[macro_export]
macro_rules! w {
    ($ctx:expr, $beats:expr) => {
        $ctx.wait($beats).await?
    };
}

/// Wait for seconds, propagates cancellation.
/// Usage: ws!(ctx, 0.5);
#[macro_export]
macro_rules! ws {
    ($ctx:expr, $sec:expr) => {
        $ctx.wait_sec($sec).await?
    };
}

/// Wait for beats, ignores cancellation result.
/// Usage: w_ignore!(ctx, 4.0);
#[macro_export]
macro_rules! w_ignore {
    ($ctx:expr, $beats:expr) => {
        let _ = $ctx.wait($beats).await
    };
}

/// Clone variables for use in closures.
/// Usage: clone!(x, y => async move { ... })
#[macro_export]
macro_rules! clone {
    ($($name:ident),+ => $body:expr) => {{
        $(let $name = $name.clone();)+
        $body
    }};
}

/// Concise branch syntax.
/// Usage: branch!(ctx, |c| { w!(c, 1.0); });
#[macro_export]
macro_rules! branch {
    ($ctx:expr, |$c:ident| { $($body:tt)* }) => {
        $ctx.branch(
            |$c| async move {
                $($body)*
                Ok::<(), $crate::context::WaitError>(())
            },
            $crate::context::BranchOptions::default()
        )
    };
}

/// Concise branch_wait syntax.
/// Usage: branch_wait!(ctx, |c| { w!(c, 1.0); }).await;
#[macro_export]
macro_rules! branch_wait {
    ($ctx:expr, |$c:ident| { $($body:tt)* }) => {
        $ctx.branch_wait(
            |$c| async move { $($body)* },
            $crate::context::BranchOptions::default()
        )
    };
}
```

---

## 10. Ultra-Terse Implementation (midi_demo_proc.rs)

The `midi_demo_proc.rs` file demonstrates the most aggressive terseness achievable with declarative macros. Here's the final syntax:

### Core Macros
```rust
W!(c, 1.0);          // Wait 1 beat
S!(c, 0.5);          // Wait 0.5 seconds
X!(c);               // Break if cancelled
L!(c, 10, {...});    // Loop 10 times with cancel check
LOOP!(c, {...});     // Infinite loop with cancel check
```

### Fork Pattern (Key Insight)
Due to Rust macro hygiene, the inner context must be explicitly named:
```rust
F!(ctx, [m, r], |c| {    // c is the inner context
    W!(c, 1.0);          // Use c inside the block
    m.on(60, 100);       // Captures are auto-cloned
});
```

### Ultra-Short Shared State Types
```rust
V<T>     // Shared value: V::new(0.5), v.g(), v.s(x), v.m(|x| ...)
Q<T>     // Message queue: Q::new(), q.push(x), q.pop()
N        // Note set: N::new(), n.add(60), n.rm(60), n.has(60)
P        // Params map: P::new(), p.s("key", 1.0), p.g("key")
M        // MIDI wrapper: m.on(60, 100), m.off(60)
```

### Convention Summary
1. Outer context (from RUN!) is `ctx`
2. Inside F! blocks, use `|c|` to name the inner context as `c`
3. All shared resources use single-letter types (V, Q, N, P, M)
4. All wait macros are 1-2 characters (W!, S!, X!)

### Example: Polyrhythm
```rust
RUN_WHILE!(90.0, "poly", r, |ctx| {
    F!(ctx, [m, r], |c| {
        L!(c, 24, {
            X!(c); if !r.load(Ordering::Relaxed) { break; }
            m.on(60, 100); W!(c, 0.1); m.off(60); W!(c, 1.23);
        });
    });
    W!(ctx, 32.0);
});
```

---

## 11. Conclusion

The fundamental tension is between:
- **Rust's ownership/borrowing model** (safety, explicitness)
- **Musical notation needs** (terseness, flow, getting out of the way)

The recommended approach (**Option 6**) threads this needle by:
1. Using simple declarative macros (not proc-macros)
2. Keeping the core API unchanged
3. Providing opt-in syntactic sugar
4. Documenting clear conventions for users to follow
5. Not hiding too much complexity (Rc/RefCell remains visible)

The key insight is that some verbosity is acceptable if it's *consistent and predictable*. Users can learn `clone!(a, b => ...)` + `w!(c, 1.0)` as patterns without fully understanding the underlying async/ownership mechanics.
