# Automatic Cancellation Propagation in Rust

## The Problem

In TypeScript, when a context is cancelled, the `wait()` promise rejects, and that rejection **throws an exception** when awaited. Uncaught exceptions propagate up and automatically exit the async function:

```typescript
// TypeScript behavior:
await ctx.wait(20.0);  // If cancelled → throws Error("aborted")
console.log("never reached");  // Exception already exited the function
```

In the current Rust implementation, `wait()` returns `Result<(), WaitError>`. When cancelled it returns `Err(...)`, but:
- Using `let _ =` ignores the error
- Using `.await` alone also ignores it
- Execution continues to the next line

```rust
// Current Rust behavior:
let _ = ctx.wait(20.0).await;  // Returns Err, but we ignore it
println!("This WILL print");   // Still executes
```

The Rust version lacks that automatic "exception propagates up and exits" behavior.

---

## Options for Automatic Cancellation Handling

### Option 1: `?` operator with Result-returning closures

The idiomatic Rust approach. Change `branch()`/`branch_wait()` to expect futures that return `Result`:

```rust
ctx.branch(|c| async move {
    c.wait(1.0).await?;  // Returns early on Err
    c.wait(2.0).await?;
    Ok(())
}, opts);
```

**Pros:** Idiomatic, compiler-enforced, clear semantics
**Cons:** Every closure needs `-> Result<(), WaitError>` + `Ok(())`, every await needs `?`

---

### Option 2: Helper macro to reduce boilerplate

Keep Option 1 but add a macro:

```rust
macro_rules! w {
    ($e:expr) => { $e.await? };
}

ctx.branch(|c| async move {
    w!(c.wait(1.0));
    w!(c.wait(2.0));
    Ok(())
}, opts);
```

Or a more aggressive macro that wraps the whole block:

```rust
ctx.branch(cancellable!(|c| {
    c.wait(1.0);  // macro transforms to .await?
    c.wait(2.0);
}), opts);
```

**Pros:** Cleaner user code
**Cons:** Macro complexity, harder to debug, proc-macro for the aggressive version

---

### Option 3: Change waits to return `()`, use `is_cancelled()` pattern

Make `wait()` return `()` instead of `Result`, check cancellation explicitly:

```rust
c.wait(1.0).await;
if c.is_cancelled() { return; }
```

With a helper macro:

```rust
macro_rules! wait {
    ($ctx:expr, $beats:expr) => {{
        $ctx.wait($beats).await;
        if $ctx.is_cancelled() { return; }
    }};
}

wait!(c, 1.0);
wait!(c, 2.0);
```

**Pros:** No Result types needed, simpler mental model
**Cons:** Still need macro/manual checks, check happens *after* the wait returns

---

### Option 4: Panic on cancel + catch_unwind at branch boundary

Make cancelled waits panic, catch at the branch level:

```rust
// Inside TimeWaitFuture::poll
if self.state.is_cancelled() {
    panic!("cancelled");  // Or use resume_unwind
}

// In branch(), wrap with catch_unwind
```

**Pros:** Truly automatic, no user code changes
**Cons:** Panics for control flow is anti-Rust, `catch_unwind` has limitations (UnwindSafe bounds), potential for inconsistent state

---

### Option 5: Two-tier API

Keep current API, add a "cancellable" variant:

```rust
// Current - manual control
ctx.branch(|c| async move {
    let _ = c.wait(1.0).await;
}, opts);

// New - auto-exits on cancel
ctx.branch_auto(|c| async move {
    c.wait(1.0).await;  // Different return type, auto-propagates
}, opts);
```

The `branch_auto` version could use a wrapper context where waits return a special type.

**Pros:** Backward compatible, choice per-use-case
**Cons:** Two APIs to maintain, potential confusion

---

### Option 6: Proc macro on the async function

```rust
#[cancellable]
async fn my_melody(c: Ctx) {
    c.wait(1.0).await;
    c.wait(2.0).await;
}
```

The proc macro rewrites to insert `?` and change return type.

**Pros:** Very clean
**Cons:** Can't use on inline closures, heavy machinery

---

## Recommendation

**Option 1 + 2 combined** is the most Rust-idiomatic path:

1. Change `branch`/`branch_wait` signatures to expect `Future<Output = Result<(), WaitError>>`
2. Provide a `w!` macro for brevity
3. Document the pattern clearly

```rust
// With the macro
ctx.branch(|c| async move {
    w!(c.wait(1.0));
    w!(c.wait(2.0));
    Ok(())
}, opts);
```

Or if you want to minimize ceremony, **Option 3** is simpler but less idiomatic:

```rust
wait!(c, 1.0);
wait!(c, 2.0);
```

---

## Decision

*TODO: Choose an approach*
