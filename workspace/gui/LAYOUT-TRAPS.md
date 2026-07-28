# LAYOUT-TRAPS.md — GPUI layout pitfalls in lindsey

Concrete mistakes that have already cost multiple sessions. Each entry gives the
failing pattern, the symptom, and the fix.

---

## Trap 1 — `max_h` without a definite height

### Failing pattern

```rust
div()
    .max_h(px(400.))       // caps the height but does not establish one
    .child(
        list(state)
            .size_full()   // resolves to ZERO — parent has no definite height
    )
```

### Symptom

Data is present (verified by logging or a `dbg!`) but nothing renders. The
parent collapses to zero height because `max_h` only constrains an upper bound;
it does not contribute a definite height for the flex algorithm to measure
against. A `size_full()` or `flex_1()` child inside a `max_h`-only parent
therefore resolves to zero, and the list or div paints nothing.

Known instances in this codebase:

- **Symbol page docs clipping to one line** — the prose section container had
  `max_h` but no `h`, so only one line of text fit before the clip.
- **Implementations section broken on click** — the impls panel's inner list
  resolved to zero height and rendered empty even when `impls` was non-empty.
- **Refs list rendering empty** — same cause: refs arrived, the store was
  populated, but the list container had zero computed height.

### Fix

Compute a definite height before capping it.

```rust
// GOOD — use a computed height that reflects the actual content size
div()
    .h(px((rows as f32) * ROW_HEIGHT_PX))
    .max_h(px(400.))
    .child(list(state).size_full())

// Also fine — let a parent with a definite size drive via flex_1
div()
    .flex()
    .flex_col()
    .h_full()              // definite height from the window/pane constraint
    .child(
        div()
            .flex_1()      // takes remaining space — definite via flex
            .max_h(px(400.))
            .child(list(state).size_full())
    )
```

### Rule

`max_h` is a constraint, not a size. A child that needs a definite height from
its parent (`size_full`, `h_full`, `flex_1`) will get zero if the parent's
height is unconstrained. Either:

1. Give the parent an explicit `h(...)` first, then add `max_h` on top, or
2. ensure the parent participates in a flex chain that already has a definite
   size at some ancestor.

---

## See also

- `GUI-PLAN.md` §1 (motion and layout doctrine)
- `GUI-LOCAL-PLAN.md` §L0 (stack and dependency law)
- GPUI docs on `StyleRefinement`: `max_size` clips, it does not establish.
