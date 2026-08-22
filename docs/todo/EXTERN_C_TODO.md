# Extern `"C"` — Detailed Implementation TODO

Legend: `[ ]` not started, `[~]` in progress, `[x]` done

This TODO is based on the repo’s FFI boundary requirements (see `NETWORKING_API.md`) and is intended to be the implementation checklist for `extern "C"` support.

## Already In Place (Prereqs)
- [x] Fixed-width primitives exist (`i32/i64/u32/u64/f32/f64/bool`) in `crates/glyph-core/src/lib.rs`
- [x] Raw pointer type exists (`Type::RawPtr`) in `crates/glyph-core/src/lib.rs`
- [x] Parser supports pointer-like generic type forms (`RawPtr<T>`) in `crates/glyph-frontend/src/parser.rs`
- [x] Backend can emit/call internal functions via `Rvalue::Call` (`crates/glyph-backend/src/codegen.rs`)

## Phase 0: Design Lock (Syntax + Semantics)
- [ ] Confirm design decisions below match the intended `extern "C"` surface area (syntax, ABI rules, linkage rules, safety model)
- [ ] Decide surface syntax (pick one and document):
  - [ ] Option A: `extern "C" fn foo(x: i32) -> i32;` (single declaration)
  - [ ] Option B: `extern "C" { fn foo(x: i32) -> i32; }` (block)
  - [ ] Option C: `extern fn foo(...)` (default ABI) + optional `extern "C"`
- [ ] Specify ABI string support: allowed set (`"C"` only in v0?) and parsing rules for invalid strings
- [ ] Decide whether extern declarations are always `pub` exports, or just declarations (importable like any other symbol)
- [ ] Decide name mangling rules for extern:
  - [ ] Is the LLVM symbol name exactly the declared identifier?
  - [ ] Are module qualifiers *never* applied to extern names?
  - [ ] Are aliases allowed for extern declarations (e.g., `extern "C" fn puts as c_puts(...)` or `#[link_name]`)?
- [ ] Define the supported type subset at the FFI boundary (initially):
  - [ ] Allowed params/returns: primitives, `RawPtr<T>`, `&T`? `&mut T`? arrays? `str`?
  - [ ] Explicitly ban/diagnose unsupported: `Shared<T>`, `Own<T>`, slices, non-`repr(C)` structs
- [ ] Specify `void`/unit behavior:
  - [ ] Syntax for “no return” (e.g., omit `->` vs `-> void`)
  - [ ] MIR/backend representation for void returns
- [ ] Decide varargs support:
  - [ ] If unsupported: grammar rejects `...` with a clear diagnostic
  - [ ] If supported later: reserve syntax and AST shape now to avoid churn
- [ ] Define diagnostics requirements (errors should point at the declaration and/or call site):
  - [ ] Unknown extern symbol (link-time vs compile-time expectations)
  - [ ] Unsupported ABI
  - [ ] Unsupported FFI type
  - [ ] Duplicate declarations / conflicting signatures

## Phase 1: Lexer / Tokens
- [x] Add `extern` keyword token (e.g., `TokenKind::Extern`) in `crates/glyph-core/src/lib.rs`
- [x] Ensure the lexer recognizes `extern` as a keyword in `crates/glyph-frontend/src/lexer.rs`
- [x] Confirm string literal tokenization supports `"C"` as a token usable by the parser (already exists, but add coverage)
- [x] Add lexer unit tests:
  - [x] `extern` tokenizes as keyword
  - [x] `extern "C"` includes `Str` token with correct span

## Phase 2: AST Additions (Extern Declarations)
- [x] Extend AST to represent extern functions (in `crates/glyph-core/src/lib.rs`):
  - [x] Add a new `Item` variant (e.g., `Item::ExternFunction(ExternFunctionDecl)`)
  - [x] Add `ExternFunctionDecl` struct containing:
    - [x] `abi: Option<String>` or an `Abi` enum
    - [x] `name: Ident`
    - [x] `params: Vec<Param>`
    - [x] `ret_type: Option<Ident>`
    - [x] `span: Span`
    - [x] (Optional) `link_name: Option<String>` if aliasing is in-scope
- [x] Decide whether extern items live in `Module.items` or a dedicated `Module.externs` list
- [x] Add serde derives / snapshot compatibility (if used elsewhere) for new AST nodes

## Phase 3: Parser Support
- [x] Parse extern function declarations in `crates/glyph-frontend/src/parser.rs`:
  - [x] Recognize `extern` at the item level (imports still must come first)
  - [x] Parse optional ABI string: `extern "C"`
  - [x] Parse `fn` signature (name, params, optional return type)
  - [x] Require a trailing `;` (no body) and produce a good diagnostic if `{` appears
  - [x] Ensure spans cover the entire declaration
- [x] Recovery behavior:
  - [x] On malformed extern decl, synchronize to next item boundary safely
  - [x] Avoid cascading errors that swallow following items
- [x] Add parser tests:
  - [x] Parses a simple extern decl
  - [x] Rejects extern decl with a body
  - [x] Rejects unsupported ABI strings
  - [x] Extern decl + normal function in same module
  - [x] Extern decl works in subdirectory modules (file layout shouldn’t matter)

## Phase 4: Symbol Collection + Multi-Module Imports
- [x] Update module symbol collection to include extern functions in `crates/glyph-frontend/src/module_resolver.rs`:
  - [x] `ModuleSymbols.functions` includes both normal and extern function names
  - [x] Decide whether extern functions are “exported” like normal functions or treated specially (treated same as normal functions)
- [x] Update import scope validation to treat extern functions like functions:
  - [x] Selective import checks validate extern presence in source module
  - [x] Collision detection includes extern-imported symbols
- [x] Add/extend tests:
  - [x] Importing an extern function via selective import works
  - [x] Collision detection triggers if two modules export same extern alias into scope

## Phase 5: Resolver / Type Checking for Extern
- [x] Extend `ResolverContext` to track extern function signatures (in `crates/glyph-frontend/src/resolver.rs`):
  - [x] Data structure for extern signatures (name → params/ret ABI)
  - [x] Decide whether extern signatures live alongside normal `fn_sigs` collection
- [x] Add validation for extern declarations:
  - [x] Disallow generics/templates (if they exist later) in extern signatures (implicitly by allowed type set)
  - [x] Validate parameter/return types against the “FFI type subset” decision
  - [x] Detect duplicate extern decls with conflicting signatures
- [x] Extend symbol resolution (`resolve_symbol`) to also resolve extern functions
- [x] Add resolver tests:
  - [x] Extern function appears as `ResolvedSymbol::Function(...)`
  - [x] Diagnostics for invalid extern parameter types

Extern ownership rule: parameters may use ABI-safe scalars, `str`,
`RawPtr<T>`, or references. Glyph-owned values that require drop glue are
rejected by value because a foreign callee cannot participate in Glyph's drop
contract. References are call-scoped and must not be retained. Supported owned
return types transfer a fresh owner into Glyph.

## Phase 6: MIR Representation
- [x] Decide how MIR represents extern functions:
  - [x] Option A: Add `MirExternFunction { name, abi, params, ret }` list on `MirModule`
  - [ ] Option B: Extend `MirFunction` with `is_extern: bool` and allow no body/blocks
- [x] Update MIR lowering (`crates/glyph-frontend/src/mir_lower.rs`):
  - [x] Collect extern signatures into the call-resolution table
  - [x] Allow `lower_call` to type-check argument count against extern signatures
  - [x] Ensure `Rvalue::Call { name, args }` uses the correct symbol name (no module mangling unless explicitly designed)
- [x] Add MIR tests:
  - [x] Calling extern function lowers without “unknown function”
  - [x] Calling extern with wrong arity produces a diagnostic

## Phase 7: LLVM Backend / Codegen
- [~] Teach codegen to call functions that are *declared but not defined*:
  - [x] Ensure `Rvalue::Call` can find a callee in either:
    - [x] defined MIR functions, or
    - [x] extern declarations table
  - [x] Build the LLVM function type from extern signature (not from MIR body)
  - [x] Declare LLVM function with external linkage when first referenced
- [ ] Apply calling convention:
  - [x] Map ABI `"C"` to the correct LLVM calling convention (default C) / bail on unsupported ABIs in codegen
  - [x] Add a diagnostic for unsupported ABIs (parser rejects non-"C"; backend bails)
- [~] Type lowering coverage for extern signatures:
  - [x] Primitives map correctly
  - [x] `RawPtr<T>` maps to LLVM pointer type for `T`
  - [ ] Decide how `Type::Named("SomeStruct")` works across FFI (likely requires `repr(C)` first) — still open
- [ ] Add backend tests (ideally integration-style):
  - [x] LLVM IR contains an external `declare` for extern functions used by a module
  - [x] Calls use the declared signature (argument/return types)

## Phase 8: Linking + JIT Symbol Resolution
- [x] Decide linking surface:
  - [x] CLI flags (`glyph build --link-lib <name> --link-search <path>`) — implemented in CLI + CodegenOptions
  - [ ] Source annotations (e.g., `@link("c")` or `@link_name("puts")`) — deferred to future
  - [ ] Project manifest (future) — deferred
- [ ] Implement object/exe linking story for extern dependencies:
  - [ ] On Unix: link libc/libSystem as needed (or rely on default toolchain) — TODO: requires invoking system linker (ld/clang)
  - [ ] On Windows: support `ws2_32` for networking (later) — TODO
- [x] Implement JIT resolution for extern symbols:
  - [x] Register process symbols with LLVM using `LLVMAddGlobalMapping` in `jit_execute_i32_with_symbols`
  - [x] Provide symbol map API (HashMap<String, u64>) for test/dev use
- [x] Add JIT tests:
  - [x] Extern call resolves to a provided host function (`jit_resolves_extern_symbol_from_host`)
  - [x] Missing symbol test (`jit_extern_symbol_codegen_without_execution` — verifies IR generation without risky execution)

## Phase 9: `repr(C)` (If Needed for Struct FFI) <---- NOT NEEDED IN PHASE V0
- [ ] Decide if `repr(C)` is required in v0 for any cross-FFI struct usage
- [ ] Add syntax to declare layout (e.g., `@repr(C) struct Foo { ... }`)
- [ ] Enforce layout rules + diagnostics:
  - [ ] No non-FFI field types
  - [ ] Stable field order, alignment, padding rules documented
- [ ] Extend codegen for `repr(C)` struct layout
- [ ] Add tests for layout + field offsets

## Phase 10: End-to-End Examples (Stdlib Networking Enablement)
- [ ] Create minimal libc bindings module (Unix):
  - [ ] `extern "C" fn socket(...) -> i32;` etc.
  - [ ] `extern "C" fn connect(...) -> i32;` etc.
  - [ ] `extern "C" fn close(...) -> i32;`
  - [ ] `extern "C" fn getaddrinfo(...) -> i32;` + `freeaddrinfo`
- [ ] Add a tiny “smoke” stdlib example that compiles (even if it can’t run yet)
- [ ] Add Windows plan items (deferred until Windows target exists)

## Phase 11: Documentation
- [ ] Add user-facing docs for extern declarations:
  - [ ] Syntax
  - [ ] Safety notes / “unsafe” model if introduced
  - [ ] Supported types table
  - [ ] Linking instructions (CLI flags / annotations)
- [ ] Update `FUNCTION_CALLS_DESIGN.md` “Known Limitations” to remove “No extern functions”
- [ ] Add a short recipe for std::sys bindings in `NETWORKING_API.md`
