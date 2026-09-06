# Documentation Index

Where things live under `docs/`:

- **`docs/book/`** — the mdBook user/language guide. Build with
  `mdbook build docs/book`; read the generated book at
  https://glyph-lang.github.io/glyph/book/. Includes
  [`limitations.md`](book/src/limitations.md) (known language/compiler
  limitations) and [`sequencer.md`](book/src/sequencer.md) (a walkthrough of
  the largest Glyph program written, now GlyphAudio's engine).
- **`docs/release/`** — release engineering docs.
  - [`validation-matrix.md`](release/validation-matrix.md) — the release
    validation matrix (GLYPH-11): build/run coverage across representative
    example programs, with a runnable script (`scripts/release-matrix.sh` /
    `just release-matrix`), current results, and known blockers.
- **`docs/plan/`** — design and implementation plans (generics, closures,
  concurrency, maps, vectors, the whitepaper, etc.), written while the
  corresponding feature was in progress.
- **`docs/todo/`** — open work-tracking notes per subsystem (case system,
  compiler diagnostics, extern C, formatting, JSON parsing, modules,
  stdlib I/O, `Vec`).
- **`docs/design/`** — design notes, APIs, formatting summaries, and other
  architectural write-ups (local working notes; not tracked in git).
- **`docs/language_expansion/`** — implementation-ready roadmap and feature
  specs for the language + stdlib expansion cycle (local working notes; not
  tracked in git).
- **`docs/reports/`** — standalone analysis reports (e.g. LLM
  reasoning-efficiency study).

For the top-level project overview, build/install instructions, and example
index, see the repository [`README.md`](../README.md).
