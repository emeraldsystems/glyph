# Glyph Implementation Progress

**Last Updated:** February 17, 2026 (modulo, hex escapes, void calls, case system, codebase modularization complete)

## 🎉 Major Milestone Achieved!

**We now have a working compiler that generates LLVM IR and can execute code via JIT!**

## ✅ Completed Features

### Phase 1: Foundation (COMPLETED)
- ✅ Return type syntax (`fn main() -> i32`)
- ✅ Function parameters in AST and MIR
- ✅ Type system with primitive types (i32, i64, u32, u64, f32, f64, bool)
- ✅ Type annotations in MIR
- ✅ **Implicit returns** - last expression in block is automatically returned!

### Phase 2: LLVM Backend (COMPLETED)
- ✅ LLVM codegen infrastructure with llvm-sys
- ✅ Primitive type lowering to LLVM
- ✅ Function declaration generation
- ✅ Function body code generation
  - Return statements
  - Arithmetic operations (+, -, *, /)
  - Comparison operations (==, !=, <, <=, >, >=)
  - Control flow (if/else with basic blocks)
  - Local variables with proper alloca/load/store
  - Function parameters
- ✅ JIT execution for testing via LLVM MCJIT
- ✅ End-to-end compilation pipeline working

## 🚀 What Currently Works

You can now compile and execute Glyph programs like:

```glyph
fn main() -> i32 {
  ret 42
}
```

```glyph
// With explicit return
fn add(a: i32, b: i32) -> i32 {
  ret a + b
}

// Or with implicit return (Rust-style!)
fn add(a: i32, b: i32) -> i32 {
  a + b
}
```

```glyph
fn conditional(x: i32) -> i32 {
  if x > 0 {
    ret x
  } else {
    ret 0
  }
}
```

### CLI Commands

```bash
# Check syntax and type errors
glyph-cli check myfile.glyph

# Build and emit LLVM IR
glyph-cli build myfile.glyph

# (Compile and run - stub only for now)
glyph-cli run myfile.glyph
```

## 📊 Test Coverage

- **264 passing tests across workspace** (frontend/core/backend/CLI)
- **44 MIR snapshot tests** (arrays, refs, loops, owns, shared, interface calls, modulo, void calls)
- **37 codegen test fixtures** including arrays/structs/owns/shared/interfaces/strings/hex-escapes/modulo/void-calls
- **Parser + resolver unit tests** plus CLI codegen fixtures
- **Property-based tests** for lexer span correctness

All tests use TDD with snapshot testing via `insta`.

## 🎯 Struct Support (COMPLETE ✅)

**Approach:** Stack-allocated value structs with copy semantics (like C structs)
**No heap allocation in Phase 1; references are rolling out via the pointer roadmap (Phases 4–6 complete)**

### Phase 1: AST Extension ✅ COMPLETE
- [x] Add struct AST nodes (StructDef, FieldDef)
- [x] Extend Item enum with Struct variant
- [x] Extend Expr enum with StructLit and FieldAccess
- [x] Unit tests for AST construction (5 tests passing)

### Phase 2: Parser ✅ COMPLETE
- [x] Parse struct declarations (`struct Point { x: i32, y: i32 }`)
- [x] Parse struct literals (`Point { x: 10, y: 20 }`)
- [x] Parse field access (`p.x`)
- [x] Parser test fixtures (3 files, all parsing successfully)

### Phase 3: Type Resolution ✅ COMPLETE
- [x] Create resolver.rs with StructType registry
- [x] First pass: collect all struct definitions
- [x] Resolve field types (with validation)
- [x] Type resolution tests (6 unit tests passing)
- [x] Error detection: duplicate structs, duplicate fields, undefined types

### Phase 4: MIR Extension ✅ COMPLETE
- [x] Add StructLit and FieldAccess to Rvalue
- [x] Add struct_types to MirModule
- [x] Lower struct expressions to MIR
- [x] MIR snapshot tests (3 fixtures with snapshots)
- [x] Unit tests for struct lowering (2 tests)

### Phase 5: LLVM Codegen ✅ COMPLETE
- [x] Register LLVM struct types (two-pass: create + set body)
- [x] Codegen struct literals (alloca + GEP + store + load)
- [x] Codegen field access (GEP + load)
- [x] Backend integration tests (3 tests verifying LLVM IR)

### Phase 6: Integration ✅ COMPLETE
- [x] Wire up resolver in frontend pipeline
- [x] End-to-end tests (including field sum test)
- [x] Documentation updates
- [x] All 20+ struct tests passing

### Working Example ✅

```glyph
struct Point {
  x: i32
  y: i32
}

fn main() -> i32 {
  let p = Point { x: 10, y: 20 }
  ret p.x + p.y  // returns 30
}
```

**This now compiles and generates correct LLVM IR!**

## 🎉 Function Calls (COMPLETE ✅)

**Function call implementation is now fully working!**

### Features Implemented:
- ✅ Direct function calls (`add(1, 2)`)
- ✅ Recursive functions (factorial, fibonacci)
- ✅ Forward references (any function can call any other)
- ✅ Struct parameters and return values
- ✅ Proper LLVM `call` instruction generation
- ✅ Full MIR lowering with validation

### Test Coverage:
- 3 codegen integration tests
- 2 MIR snapshot tests
- Verified recursion with factorial(5)
- Verified struct return by value

### Example Working Code:
```glyph
fn factorial(n: i32) -> i32 {
  if n <= 1 {
    ret 1
  } else {
    ret n * factorial(n - 1)
  }
}

fn main() -> i32 {
  ret factorial(5)  // returns 120
}
```

## Interfaces & Methods ✅ COMPLETE (Phases 0–7)

**All phases complete:** Parsing, validation, MIR lowering, LLVM codegen verification, and documentation.

**Features:**
- ✅ Interface definitions with method contracts
- ✅ Two implementation styles: inline (`impl I { }` inside struct) and separate (`impl I for S { }`)
- ✅ Method call resolution: built-in → inherent → interface
- ✅ Auto-borrowing for `&self` and `&mut self` parameters
- ✅ Symbol mangling for disambiguation (`Struct::method` vs `Struct::Interface::method`)
- ✅ Compile-time method dispatch (no vtables, no runtime overhead)

**Testing:**
- ✅ 5 codegen tests verifying LLVM IR for interface method calls
- ✅ MIR tests for method resolution and auto-borrowing
- ✅ Parser and resolver tests for syntax and validation
- ✅ 106 total tests passing across workspace

**Documentation:**
- ✅ INTERFACES_DESIGN.md with comprehensive examples
- ✅ Syntax reference, implementation details, and examples

**Example:**
```glyph
interface Drawable {
  fn draw(self: &Point) -> i32;
}

struct Point {
  x: i32
  y: i32

  impl Drawable {
    fn draw(self: &Point) -> i32 {
      ret self.x + self.y
    }
  }
}

fn main() -> i32 {
  let p = Point { x: 10, y: 20 }
  ret p.draw()  // Returns 30
}
```

## 🔧 Reference Support (Phases 4–6 ✅)

Phase 4–6 of the pointer plan are implemented end-to-end:
- ✅ **Phase 4** (Resolver): `resolve_type_name` now understands strings like `"&Point"`/`"&mut Point"`, trims whitespace, validates nesting, and surfaces diagnostics for malformed references.
- ✅ **Phase 5** (MIR): added `Rvalue::Ref`, lowered `Expr::Ref`, assignments, and auto-deref field access; MIR locals keep inferred `Type::Ref` metadata.
- ✅ **Phase 6** (Codegen): references lower to raw LLVM pointers; params/locals always have stack slots so references have stable addresses; field access auto-derefs appropriately.
- 🧪 Added two MIR fixtures (`ref_field_access.glyph`, `ref_param_call.glyph`) with updated insta snapshots.
- 🧪 `cargo test -p glyph-frontend` now covers 19 MIR fixtures (up from 17) and all unit tests pass.

## 🔄 Loop Support (COMPLETE ✅)

Both while and for loops are now fully implemented:

### Features Implemented:
- ✅ **While loops** with condition expressions
- ✅ **For loops** with range syntax (`for i in 0..10`)
- ✅ **Break statement** - exits innermost loop
- ✅ **Continue statement** - skips to next iteration
- ✅ **Nested loops** with proper context tracking
- ✅ **Error diagnostics** for break/continue outside loops

### Implementation Details:
- **AST**: Added `Expr::While`, `Expr::For`, `Stmt::Break`, `Stmt::Continue`
- **Lexer**: Added `In` and `DotDot` tokens for for loop syntax
- **Parser**: `parse_while()` and `parse_for()` follow `parse_if()` pattern
- **MIR Lowering**:
  - While loops: Loop block structure with header, body, exit blocks and back edges
  - For loops: Desugar to while loops (initialize var, check condition, increment)
- **Loop Context**: Stack-based tracking for break/continue targets in nested loops
- **No LLVM changes**: Reused existing `Goto` and `If` instructions

### Test Coverage:
- 10 test fixtures covering while/for loops, break, continue, nested loops, error cases
- 29 MIR snapshot tests passing (up from 19)
- Verified desugaring: for loops correctly lower to while loops in MIR

### Example Code:
```glyph
fn count_to_ten() -> i32 {
    let i = 0
    while i < 10 {
        i = i + 1
    }
    ret i
}

fn sum_range(n: i32) -> i32 {
    let sum = 0
    for i in 0..n {
        sum = sum + i
    }
    ret sum  // returns 0+1+2+...+(n-1)
}

fn search() -> i32 {
    let i = 0
    while true {
        if i == 5 {
            break  // exits loop
        }
        i = i + 1
    }
    ret i
}
```

## ➕ Modulo Operator ✅ COMPLETE

- `%` operator for integer remainder (LLVM `srem`)
- Precedence 6 (same as `*` and `/`)
- MIR snapshot and codegen fixture tests passing

## 🔤 Hex Escape Sequences ✅ COMPLETE

- `\xHH` syntax in regular and interpolated strings
- Two hex digits encode arbitrary byte values (e.g. `"\x1B[31m"` for ANSI escapes)
- Codegen fixture test passing

## 📭 Void Function Calls ✅ COMPLETE

- Functions with no `-> Type` annotation return void
- Void calls can appear mid-block (not only as last expression)
- MIR lowering and codegen support with test fixtures

## 📦 Case System ✅ COMPLETE

- Project manifest via `glyph.toml` with `[lib]`, `[[bin]]`, and `[dependencies]`
- Local path-based dependencies between cases
- Transitive dependency resolution with cycle detection
- Library and binary targets

## 🏗️ Codebase Modularization ✅ COMPLETE

- Parser split into `parser/` submodule (mod.rs, expr.rs, items.rs, stmts.rs, types.rs, imports.rs)
- Resolver split into `resolver/` submodule (mod.rs, const_eval.rs, imports.rs, methods.rs, validation.rs)
- MIR lowering split into `mir_lower/` submodule (mod.rs, expr.rs, flow.rs, call.rs, context.rs, etc.)
- Backend codegen split into `codegen/` submodule (rvalue.rs, functions.rs, types.rs, string.rs, vec.rs, etc.)

## 🚀 What's Next

Priorities:
1. **For-in over collections** — extend `for x in collection` beyond range syntax
2. **Borrowed closures** — owned `FnOnce` is implemented; `Fn`/`FnMut` remains deferred (see [CLOSURES_CONCURRENCY.md](CLOSURES_CONCURRENCY.md))
3. **Trait / interface improvements** — default methods, generic constraints
4. **Borrow checker** — lifetime analysis and move checking

## 🏗️ Architecture

```
Source Code (.glyph)
    ↓
┌──────────────────────────────────┐
│  glyph-frontend                  │
│  ├─ Lexer                        │  ✅ Complete
│  ├─ Parser (parser/)             │  ✅ Modularized
│  ├─ Resolver (resolver/)         │  ✅ Modularized
│  ├─ (Type Checker - future)      │  🔜 Not needed yet
│  └─ MIR Lowering (mir_lower/)    │  ✅ Modularized
└──────────────────────────────────┘
    ↓ MIR (SSA-like IR)
┌──────────────────────────────────┐
│  glyph-backend                   │
│  └─ LLVM Codegen (codegen/)      │  ✅ Modularized
└──────────────────────────────────┘
    ↓ LLVM IR
┌──────────────────────────────────┐
│  LLVM                            │
│  ├─ Optimization                 │  ✅ Via LLVM
│  ├─ JIT Execution                │  ✅ MCJIT
│  └─ Native Code Gen              │  ✅ via linker.rs
└──────────────────────────────────┘
```

## 📁 Repository Structure

```
glyph/
├── crates/
│   ├── glyph-core/src/       # AST (ast.rs), MIR (mir.rs), types, spans, diagnostics
│   ├── glyph-frontend/src/
│   │   ├── lexer.rs           # Tokenizer
│   │   ├── parser/            # Modularized parser
│   │   ├── resolver/          # Modularized resolver
│   │   ├── mir_lower/         # Modularized MIR lowering
│   │   └── stdlib.rs          # Standard library definitions
│   ├── glyph-backend/src/
│   │   ├── codegen.rs         # Codegen entry point
│   │   ├── codegen/           # Modularized codegen
│   │   └── linker.rs          # Native linking
│   ├── glyph-cli/             # CLI commands (glyph, glyph-cli)
│   ├── glyphlsp/              # LSP server
│   └── glyphfmt/              # Formatter
├── runtime/                   # Minimal C runtime for I/O
├── tests/fixtures/
│   ├── parse/                 # Parser test cases
│   ├── mir/                   # MIR test cases (44 fixtures)
│   └── codegen/               # Codegen test cases (37 fixtures)
├── examples/                  # Example glyph projects
└── docs/                      # mdBook docs and design docs
```

## 🛠️ Development Setup

### Prerequisites

- Rust 2024 edition
- LLVM 20.x (via homebrew on macOS: `brew install llvm`)

### Environment

```bash
export LLVM_SYS_201_PREFIX=/opt/homebrew/Cellar/llvm/20.1.8
```

### Build & Test

```bash
# Build everything
cargo build --release

# Run all tests (requires LLVM)
cargo test --workspace

# Run specific package tests
cargo test --package glyph-frontend
cargo test --package glyph-backend --features codegen

# Update snapshots
INSTA_UPDATE=always cargo test
```

## 📝 Recent Changes

### January 8, 2026 - Late Evening

**Implemented Struct Parsing (Phases 1-2):**

1. **AST Extension (Phase 1)**
   - Added `StructDef` and `FieldDef` types
   - Extended `Item` enum with `Struct` variant
   - Added `Expr::StructLit` and `Expr::FieldAccess`
   - Created 5 unit tests for AST construction

2. **Parser Implementation (Phase 2)**
   - Implemented `parse_struct()` for struct definitions
   - Implemented `parse_struct_lit()` for struct literals
   - Extended `parse_call_or_primary()` for field access (`.` operator)
   - Updated main parse loop to handle `struct` keyword
   - Created 3 parser test fixtures (all passing)

3. **Verified Correctness**
   - All 28 tests passing (12 frontend + 5 AST + 11 MIR)
   - Can parse: `struct Point { x: i32, y: i32 }`
   - Can parse: `Point { x: 10, y: 20 }`
   - Can parse: `p.x` and `p.y`
   - No breaking changes to existing functionality

4. **Committed to Git**
   - Initial commit with full working compiler
   - 64 files, 6015 lines of code
   - Complete LLVM backend + struct parsing foundation

### January 8, 2026 - Evening Update

**Implemented Implicit Returns:**

1. **Modified MIR Lowering**
   - `lower_block` now returns `Option<MirValue>` 
   - Last expression in a block is automatically returned
   - Matches Rust/Swift semantics: `fn add(a: i, b: i) { a + b }`

2. **Updated Tests**
   - Added `mir_implicit_return.glyph` test fixture
   - Snapshot validates correct MIR generation
   - End-to-end codegen test confirms LLVM IR is identical for explicit vs implicit returns

3. **Verified Correctness**
   - Both `ret a + b` and `a + b` generate identical LLVM IR
   - All 28 tests passing (12 MIR, 4 backend, 12 frontend)

### January 8, 2026 - Morning

**Major Implementation Session:**

1. **Extended AST for Return Types**
   - Added `ret_type: Option<Ident>` to Function
   - Updated parser to handle `-> Type` syntax
   - Created test fixture: `fn_with_return_type.glyph`

2. **Enhanced MIR with Types**
   - Added `Type` enum in glyph-core (I32, I64, Bool, etc.)
   - Added `ty: Option<Type>` to Local
   - Added `ret_type: Option<Type>` to MirFunction
   - Added `params: Vec<LocalId>` to track function parameters
   - Parameters now properly mapped to locals in MIR

3. **Implemented Complete LLVM Backend**
   - Created `codegen.rs` with full LLVM IR generation
   - Type mapping: Glyph types → LLVM types
   - Function codegen with parameters and return types
   - Basic block generation for control flow
   - Instruction lowering:
     - Binary operations (arithmetic and comparison)
     - Return statements
     - Conditional branches
     - Local variable management (alloca/load/store)
   - JIT execution via LLVM MCJIT

4. **Updated CLI**
   - Added `codegen` feature flag
   - Integrated LlvmBackend instead of NullBackend
   - Now emits real LLVM IR

5. **Added Comprehensive Tests**
   - Backend codegen tests with snapshot validation
   - JIT execution test (returns 42 from compiled code!)
   - End-to-end compilation tests

6. **Fixed Issues**
   - Added Hash + Eq derives to LocalId and BlockId
   - Fixed parameter handling in codegen (direct values, not allocas)
   - Fixed JIT ownership issues with module cloning
   - Resolved LLVM version mismatch (using 20.x with llvm-sys 201)

## 🔄 Design Decisions

### Why Skip Type Checking for Now?

We're following a pragmatic "working executable first" approach:
- Basic type information is tracked in MIR (from explicit annotations)
- This is sufficient for simple programs with explicit types
- Full type inference and checking can be added later
- Gets us to a working demo faster

### Why MCJIT Instead of ORC JIT?

- MCJIT is simpler and well-supported in llvm-sys
- Sufficient for our testing needs
- ORC can be added later for production use

### Current Limitations

1. **No borrow checking** - no lifetime analysis yet
2. **Limited type inference** - locals infer types, but generics sometimes need annotation
3. **No closures** - first-class functions not yet supported

## 📈 Estimated Timeline

- ✅ **Phase 1 & 2: Basic Compilation** - COMPLETE (3 days)
- ✅ **Phase 3: Struct Parsing** - COMPLETE
- ✅ **Phase 4: Struct Type Checking** - COMPLETE
- ✅ **Phase 5: Struct Codegen** - COMPLETE

**Struct support delivered end-to-end (AST → parser → resolver → MIR → LLVM).**

## 🎯 Success Metrics

- [x] Can compile simple functions to LLVM IR
- [x] Can execute compiled code via JIT
- [x] Implicit returns work (Rust-style last expression)
- [x] All tests pass with snapshot validation
- [x] Can compile struct definitions
- [x] Can compile struct literals
- [x] Can compile field access
- [x] End-to-end struct example works

## 🤝 Contributing

This is a learning/demonstration project following TDD principles:
1. Write a test fixture in `tests/fixtures/`
2. Run tests with `INSTA_UPDATE=always cargo test`
3. Review snapshots to verify correctness
4. Implement feature to make tests pass

## 📚 References

- [LLMS.md](LLMS.md) - Language specification
- [PLAN.md](PLAN.md) - Detailed implementation plan
- [llvm-sys docs](https://docs.rs/llvm-sys/) - LLVM bindings reference
