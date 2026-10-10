# Cases and Dependencies

A **case** is Glyph's unit of compilation — similar to a crate in Rust or a
package in Go. Every directory that contains a `glyph.toml` is a case.

A case can be a **binary** (produces an executable), a **library** (provides
code for other cases to import), or both.

## Project Layout

A typical binary case looks like this:

```
my_app/
  glyph.toml
  src/
    main.glyph
    utils.glyph
```

A library case replaces the binary entry point with a `lib.glyph`:

```
my_lib/
  glyph.toml
  src/
    lib.glyph
    helpers.glyph
```

## glyph.toml

### Binary case

```toml
[package]
name = "my_app"
version = "0.1.0"

[[bin]]
name = "my_app"
path = "src/main.glyph"
```

### Library case

```toml
[package]
name = "my_lib"
version = "0.1.0"

[lib]
path = "src/lib.glyph"
```

The `[lib]` section marks the case as a library. The `path` field points to the
library's entry module (defaults to `src/lib.glyph`).

A case can have both `[lib]` and `[[bin]]` sections if it provides a library and
also ships an executable.

## Dependencies

Cases can depend on other cases using the `[dependencies]` table. Currently only
local path-based dependencies are supported.

```toml
[package]
name = "my_app"
version = "0.1.0"

[dependencies]
my_lib = { path = "../my_lib" }

[[bin]]
name = "my_app"
path = "src/main.glyph"
```

The dependency must point to a directory containing a `glyph.toml` with a
`[lib]` target.

## Importing from a Dependency

Once declared in `[dependencies]`, you import from a library case the same way
you import from local modules — just use the case name as the path prefix:

```glyph
// Import symbols defined in my_lib's lib.glyph
from my_lib import greet

// Import from a submodule within my_lib
from my_lib/helpers import format_name

fn main() -> i32 {
  let name = format_name("world")
  greet(name)
  ret 0
}
```

Under the hood, Glyph loads the dependency's source files and merges them into
the module graph with the case name as a prefix. The existing import resolution
handles the rest — no special syntax is needed.

Functions in separate modules or cases can share a name. Each module resolves
its own functions and imports independently, so a helper in a transitive
dependency does not conflict with a local helper.

## Building

Use `glyph build` or `glyph run` from the case root (the directory containing
`glyph.toml`):

```bash
glyph build          # compile the binary
glyph run            # compile and run
```

When you build a binary case that has dependencies, Glyph automatically resolves
and compiles all dependency cases first.

For library-only cases (no `[[bin]]`), `glyph build` type-checks the code
without producing a binary.

## Transitive Dependencies

Dependencies are resolved recursively. If `my_app` depends on `my_lib`, and
`my_lib` depends on `my_util`, then building `my_app` will pull in both. Glyph
resolves the graph in topological order and detects cycles.

## Current Limitations

- **Path-based dependencies only** — there is no package registry yet.
- **No visibility system** — all items in a dependency are importable.
- **Type name collisions** — structs and enums in separate cases must have
  distinct names.
- **Flat namespace** — all dependency names must be unique across the entire
  dependency graph.
