# Language Fix: Proper Typed Int Literals + Enum/Match Soundness

> **Status: RESOLVED (GLYPH-9).** All milestones below are complete. The full
> `std/json/parser.glyph` is the only parser shipped (`parser_inline.glyph`
> has been deleted), it compiles and runs cleanly, and nested/Unicode parsing
> is covered by `crates/glyph-cli/tests/std_json_parser.rs`. This document is
> kept for its historical technical record of the integer-literal-typing and
> enum/match soundness work; for the current JSON API and status, see
> `docs/book/src/json.md`.

This document captures the plan for a *proper language-level fix* (not a stdlib workaround) to support the full `std/json/parser.glyph` implementation, especially nested JSON parsing.

## Problem Statement

The repository contains a real recursive-descent JSON parser at `crates/glyph-frontend/src/stdlib/json/parser.glyph`. However, some builds ship a minimal stub parser (`crates/glyph-frontend/src/stdlib/json/parser_inline.glyph`) because the real parser exercises several frontend/backend correctness gaps.

The user-visible symptom is that `examples/json_parser` fails its nested checks because `std/json/parser::parse` always returns `Ok(Null)`.

The underlying technical issues show up as LLVM verifier errors when trying to compile the full parser:

- Unterminated basic blocks (LLVM: "Basic Block ... does not have terminator")
- Integer width/type mismatches at call sites (LLVM: "Call parameter type does not match function signature")
  - Commonly `usize`/`i64` vs default `i32` integer literals
- Return type mismatches for generic enums (LLVM: "Function return type does not match operand type")

We already fixed one major source of invalid IR: MIR lowering for `match` was emitting instructions into the wrong block (unterminated BBs).

The remaining work is to make integer literals and generic-enum returns fully sound across parsing/typing/MIR/codegen.

## Goals

- Make the full `std/json/parser.glyph` compile and run in CI.
- Ensure nested JSON parsing works (arrays/objects) and trailing characters are rejected.
- Avoid stdlib-only hacks (e.g. sprinkling `: usize` everywhere) as the primary solution.
- Add regression tests so we never silently regress to a stubbed parser.

## Non-goals (for this document)

- Implementing additional JSON features (unicode escapes, full escape set, etc.).
- Reworking the entire type system.

## Root Cause Analysis (Hypotheses)

### 1) Integer literals are under-typed

Many literal `0`, `1`, etc. values end up as `i32` in MIR/codegen because:

- Local inference may leave a literal-typed temp with `ty: None`.
- Codegen defaults unknown locals to `i32`.
- But `usize` in this compiler maps to a platform word size (typically `i64` on 64-bit targets).

This becomes fatal in contexts like:

- Calling functions that take `usize` (or structs storing `usize`), e.g. index args, position counters.
- Interacting with extern functions like `glyph_byte_at(const char*, size_t)`.

### 2) Type-directed lowering is incomplete

Even when types are known at the AST/resolver stage, MIR lowering currently does not always propagate those types to temporaries.

### 3) Generic enum layout / returns must be consistent

`ParseResult<T>` is a generic enum. If any monomorphization / enum-construction path is inconsistent, we can end up returning the wrong enum instantiation.

This is especially likely in complex functions like `parse_string_scan` which returns `ParseResult<ParsedString>` and has many early `ret Err(...)` exits.

## Proposed Fixes (Language-Level)

### A) Contextual integer literal typing (primary fix)

Implement *contextual typing* for integer literals during type checking / MIR lowering:

- If an integer literal appears in a context where an expected type is known (assignment target type, parameter type, struct field type, etc.), then the literal should adopt that expected type.

Examples:

- `parse_value(input, 0)`
  - The second parameter is `usize`, so literal `0` becomes `usize`.
- `let i: usize = 0` already works; but we want to avoid having to write it.
- `pos + 1`
  - If `pos` is `usize`, then literal `1` becomes `usize`.

Implementation notes:

- This likely belongs in the resolver/type-checking pass (attach a concrete `Type` to integer literal AST nodes), or in MIR lowering if MIR lowering has access to the resolved type of the expression.

### B) Typed MIR temps for arithmetic and comparisons

Ensure MIR temporaries created by lowering have correct types, not `None`.

- Comparisons already changed to produce `Bool` temps.
- Arithmetic temps should adopt the operand type (or the expected type from context).

This prevents the backend from "guessing" types.

### C) Explicit implicit casts inserted in MIR (when safe)

Even with contextual typing, there will be cases that need coercions. Add a small, explicit coercion mechanism:

- If a value of integer type `i32` is used where `usize` is required (or vice versa), insert a widening/narrowing cast during MIR lowering or in a dedicated MIR pass.
- Prefer widening casts (e.g. `i32 -> usize`) when the source is a literal or proven-safe.

This is the proper alternative to ad-hoc backend coercion at the LLVM level.

### D) Enum/Result return soundness

Make sure `Err(ParseError)` returns the correct `ParseResult<T>` instantiation.

Checklist:

- Enum construction in MIR should encode the concrete enum type (including type args).
- Monomorphization should produce a distinct concrete enum type per `T`.
- Codegen must use the correct concrete LLVM struct type for that monomorphized enum.

If needed, add debug assertions in frontend/backend (in debug builds) to ensure that:

- The enum constructor used matches the destination type.
- Return value type equals function return type.

## Steps / Milestones

### Milestone 1: Make integer literals properly typed

- Add expected-type propagation for integer literals.
- Ensure MIR lowering propagates the resolved types into temps.
- Add/extend tests for:
  - `usize` function args with literal values
  - `usize` arithmetic (`pos + 1`) with literals

### Milestone 2: Make `std/json/parser.glyph` compile (no stub)

- Switch stdlib embedding from `parser_inline.glyph` to `parser.glyph`.
- Fix any remaining lowering/codegen issues exposed by parser compilation.

### Milestone 3: Make nested parsing pass at runtime

- Run `examples/json_parser` with the real parser:
  - nested arrays
  - nested objects
  - trailing character rejection

### Milestone 4: Regression tests

- Add a runtime test (in `crates/glyph-cli` or an integration test harness) that:
  - compiles and runs a small glyph snippet calling `parse` on nested JSON
  - validates shape with `match`
  - returns non-zero on failure
- Ensure CI runs this test.

## Acceptance Criteria

- `glyph run` in `examples/json_parser` passes on CI with the real parser.
- `std/json/parser::parse` does not ship as a stub in the default build.
- No LLVM verifier errors when compiling `crates/glyph-frontend/src/stdlib/json/parser.glyph`.
- Added tests catch regressions in:
  - integer literal typing
  - enum/Result returns
  - match lowering

## Notes

- It is acceptable to temporarily keep `parser_inline.glyph` behind a feature flag, but default builds should use the real parser once milestones 1-3 are done.
