# Next Session Quick Start Guide

**Last Updated:** February 17, 2026
**Current:** Modulo, hex escapes, void calls, case system, for-in, `?` operator, codebase modularization COMPLETE ✅

> Historical handoff note: this page predates the closure implementation.
> Owned `FnOnce` semantics are now defined authoritatively in
> [CLOSURES_CONCURRENCY.md](CLOSURES_CONCURRENCY.md).

---

## 🎯 IMMEDIATE NEXT STEPS

1) **Consider Next Major Feature:**
   - For-in over collections — extend `for x in collection` beyond range syntax
   - Extend owned `FnOnce` closures with the deferred borrowed `Fn`/`FnMut` model
   - Trait / interface improvements — default methods, generic constraints
   - Borrow checker & lifetimes (very high complexity)

**Previous Status:** Modulo operator, hex escape sequences, void function calls, case system with dependencies, codebase modularization, and `?` operator all complete. 264 tests passing.

---

## Current Status

### ✅ What's Working

**Compiler Features:**
- Complete lexer/parser/resolver + MIR lowering (modularized into submodules)
- LLVM codegen with JIT execution (modularized into submodules)
- Structs, function calls, references, loops, for-in
- Arrays: literals, indexing with bounds checks, `.len()`
- Ownership pointers: Own<T> (exclusive), Shared<T> (refcounted), RawPtr<T> (unsafe)
- Interfaces: parsing/impl validation, method-call resolution, auto-borrowing, LLVM codegen verified
- Modulo operator (`%`), hex escape sequences (`\xHH`), void function calls
- `?` operator for error propagation on `Result` types
- Case system with `glyph.toml`, local path-based dependencies, transitive resolution
- `print_str` from `std/io` (print without newline)

**Test Coverage:**
- Workspace tests green (`cargo test`) — 264 tests passing
- 44 MIR snapshot tests (arrays, refs, loops, owns, shared, interface calls, modulo, void calls)
- 37 codegen test fixtures (arrays/structs/owns/shared/interfaces/strings/hex-escapes/modulo/void-calls)
- Resolver/parser/unit coverage intact

**Example Code That Works:**
```glyph
// Structs with functions
struct Point {
  x: i32
  y: i32
}

fn make_point(x: i32, y: i32) -> Point {
  Point { x: x, y: y }  // implicit return
}

// Recursive functions
fn factorial(n: i32) -> i32 {
  if n <= 1 {
    ret 1
  } else {
    ret n * factorial(n - 1)
  }
}

// While loops with break
fn find_value(target: i32) -> i32 {
  let i = 0
  while true {
    if i == target {
      break
    }
    i = i + 1
  }
  ret i
}

// For loops (NEW!)
fn sum_range(n: i32) -> i32 {
  let sum = 0
  for i in 0..n {
    sum = sum + i
  }
  ret sum  // returns sum of 0..n-1
}

// Reference-counted pointers (NEW!)
fn use_shared() -> i32 {
  let s1 = Shared::new(42)
  let s2 = s1.clone()  // refcount = 2
  let s3 = s2.clone()  // refcount = 3
  ret 0  // drops s3, s2, s1 in order, last one frees
}

fn main() -> i32 {
  let pt = make_point(10, 20)
  let fact = factorial(5)
  let found = find_value(7)
  let range_sum = sum_range(10)
  ret pt.x + pt.y + fact + found + range_sum  // returns 202 (30 + 120 + 7 + 45)
}
```

---

## Struct Support ✅ COMPLETE

All struct implementation phases complete:
- ✅ AST + Parser + Type Resolution + MIR + LLVM Codegen
- ✅ 20+ tests passing

## Function Calls ✅ COMPLETE

All function call implementation complete:
- ✅ MIR lowering with validation (argument count checked)
- ✅ LLVM codegen for direct calls
- ✅ Recursive functions working (factorial tested)
- ✅ Forward references supported (any order)
- ✅ Struct parameters and return values
- ✅ 5 integration tests passing

## Reference Support 🔧 (Phases 4–6 done)

Latest progress on the pointer roadmap:
- ✅ **Phase 4** – resolver understands `&T` / `&mut T` names and reports malformed references.
- ✅ **Phase 5** – MIR gained `Rvalue::Ref`, assignment lowering, and auto-deref logic for field access.
- ✅ **Phase 6** – LLVM codegen now emits raw pointer types for references, keeps locals/params in stack slots, and auto-derefs before field GEPs.
- 🧪 Added MIR fixtures `ref_field_access.glyph` and `ref_param_call.glyph` to exercise reference semantics.
- 📦 `cargo test -p glyph-frontend` fully green (19 MIR snapshots).

## For Loop Support ✅ COMPLETE

For loops with range syntax are now fully implemented:

**Features:**
- ✅ Syntax: `for var in start..end { body }`
- ✅ Added `In` and `DotDot` tokens to lexer
- ✅ Added `For` variant to `Expr` enum with var, start, end, body
- ✅ Parser implementation following `parse_while()` pattern
- ✅ MIR lowering desugars for loops to while loops
- ✅ Proper variable initialization and increment
- ✅ Loop context tracking (break/continue work in for loops)
- ✅ 2 test fixtures with snapshots

**Implementation Details:**
- For loops desugar at MIR lowering time into equivalent while loops
- Loop variable initialized before entering loop
- Condition checked at loop header: `var < end`
- Body executed, then variable incremented by 1
- Back edge to header for next iteration

**Example:**
```glyph
fn sum_to_n(n: i32) -> i32 {
  let sum = 0
  for i in 0..n {
    sum = sum + i
  }
  ret sum
}
```

**Desugars to:**
```glyph
fn sum_to_n(n: i32) -> i32 {
  let sum = 0
  let i = 0
  while i < n {
    sum = sum + i
    i = i + 1
  }
  ret sum
}
```

## Shared<T> Reference Counting ✅ COMPLETE

**Status:** Fully implemented and tested

**Completed Work:**
- [x] Type system: Type::Shared(Box<Type>) variant
- [x] Parser: Shared<T> syntax recognition
- [x] MIR lowering: SharedNew and SharedClone rvalues
- [x] Copy semantics: No move errors (unlike Own<T>)
- [x] LLVM codegen: Memory layout [refcount: usize, data: T]
- [x] Drop glue: Decrement refcount, free when zero
- [x] 4 MIR snapshot tests + 1 codegen test

**Example Working Code:**
```glyph
fn main() -> i32 {
  let s1 = Shared::new(42)
  let s2 = s1.clone()  // refcount = 2
  let s3 = s2.clone()  // refcount = 3
  ret 0
  // s3, s2, s1 drop in order, last one frees memory
}
```

**See**: `SHARED_DESIGN.md` for full documentation

## What to Build Next

1) **For-in over collections** — extend `for x in collection` beyond range syntax
2) **Borrowed closures** — extend implemented owned `FnOnce` values with `Fn`/`FnMut`
3) **Trait / interface improvements** — default methods, generic constraints
4) **Borrow checker & lifetimes** — lifetime analysis and move checking

---

## Environment Setup

```bash
# Set LLVM path
export LLVM_SYS_201_PREFIX=/opt/homebrew/Cellar/llvm/20.1.8

# Build
cargo build --release

# Run all tests
cargo test --workspace

# Test specific package
cargo test --package glyph-frontend

# Update snapshots
INSTA_UPDATE=always cargo test

# Test CLI
./target/release/glyph-cli check file.glyph
./target/release/glyph-cli build file.glyph
```

---

## Key Design Decisions

✅ **Stack allocation for structs** — copied by value like C
✅ **Heap via Own<T> / Shared<T>** — explicit ownership semantics
✅ **Case system** — `glyph.toml` manifest with local path-based dependencies
✅ **Modularized codebase** — parser, resolver, mir_lower, codegen split into focused submodules

---

## Files Organization

```
crates/
├── glyph-core/src/
│   ├── ast.rs                    # AST node types
│   ├── mir.rs                    # MIR definitions
│   ├── types.rs                  # Type system
│   └── ...                       # span, token, diag
├── glyph-frontend/src/
│   ├── lexer.rs                  # Tokenizer
│   ├── parser/                   # Modularized parser (expr, items, stmts, types, imports)
│   ├── resolver/                 # Modularized resolver (const_eval, imports, methods, validation)
│   ├── mir_lower/                # Modularized MIR lowering (expr, flow, call, context, etc.)
│   ├── stdlib.rs                 # Standard library definitions
│   └── lib.rs                    # Frontend pipeline wiring
└── glyph-backend/src/
    ├── codegen.rs                # Codegen entry point
    ├── codegen/                  # Modularized codegen (rvalue, functions, types, string, vec, etc.)
    ├── linker.rs                 # Native linking
    └── lib.rs                    # Backend entry

tests/fixtures/
├── parse/                        # Parser test cases
├── mir/                          # 44 MIR fixture tests
└── codegen/                      # 37 codegen fixture tests
```

---

## Quick Commands

```bash
# Check what's changed
git status
git diff

# Run demo
./demo.sh

# Test struct parsing
./target/release/glyph-cli check tests/fixtures/parse/struct_def.glyph

# Run specific test
cargo test struct_def

# See test output
cargo test -- --nocapture
```

---

Ready to continue! Pick from "What to Build Next" above.
