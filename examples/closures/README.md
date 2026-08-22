# Owned Closure Example

This example exercises Glyph's first closure release: owned, move-only
`FnOnce` values. It covers zero/one/many parameters, expression and block
bodies, contextual parameter inference, named-function coercion, captured
state, returning a closure, storing it in a local, and passing a callback.

Run it from the repository root:

```bash
glyph run examples/closures/src/main.glyph
```

The program exits successfully when all callback results add up to 42.

The callback capability is intentionally one-shot. A `FnOnce` value is
consumed by its call; repeated iterator callbacks need the later borrowed
`Fn`/`FnMut` work. Closure creation may allocate, so construct callbacks on a
control/worker thread rather than in an audio callback.
