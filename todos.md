# Rust Timing Library TODOs

## Pending Features

### `finally` API for BranchHandle

The TypeScript implementation has a `.finally()` method on branch handles that guarantees cleanup code runs when a branch completes (either normally or via cancellation). This is used for patterns like:

```typescript
const note1 = root.branch(async (c) => {
    await c.waitSec(0.30);
    log(c, "note1_off_in_branch");
}, "note1");
note1.finally(() => log(root, "note1_off_finally"));
```

**Implementation needed:**
- Add `finally()` method to `BranchHandle` in `context.rs`
- Store cleanup callbacks and invoke them on completion or cancellation
- Handle the `FnOnce` vs `FnMut` issue (see `rustImplementationPlan.md` section 11)

**Test needed:**
- Port `noteoff_finally_guaranteed_on_cancel` test from TypeScript
- Verify finally callbacks run on normal completion
- Verify finally callbacks run on cancellation
- Verify finally callbacks run in deterministic order

## Notes

- The TypeScript test `noteoff_finally_guaranteed_on_cancel` cannot be ported until the `finally` API is implemented
