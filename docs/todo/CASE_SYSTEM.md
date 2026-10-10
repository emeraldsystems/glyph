# v0.1 Case System: `[lib]` Targets and Local `[dependencies]`

## Context

Glyph currently has no package/dependency system. A project can contain multiple `.glyph` files that import from each other, but there's no way to depend on code from another project. The `[dependencies]` field in `glyph.toml` is parsed but completely unused (`#[allow(dead_code)]`). This plan adds the minimum viable "case" system: library targets and local path-based dependencies.

## Approach: Namespace Merging

For v0.1, we merge dependency modules into the same module map with case-name prefixes, then let the existing compilation pipeline handle everything unchanged. No changes to `glyph-frontend` or `glyph-backend` are needed (except one collision-detection guard).

When case `my_app` depends on `my_lib`:
1. Load `my_lib`'s `.glyph` files, prefix module IDs: `utils` -> `my_lib/utils`
2. Rewrite `my_lib`'s internal imports to match: `from utils import X` -> `from my_lib/utils import X` (std imports untouched)
3. Merge into the main module map
4. Existing `compile_modules()` handles the rest

## glyph.toml Format

**Library case** (`my_lib/glyph.toml`):
```toml
[package]
name = "my_lib"
version = "0.1.0"

[lib]
path = "src/lib.glyph"   # optional, defaults to src/lib.glyph
```

**Consumer case** (`my_app/glyph.toml`):
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

**Import syntax in consumer**:
```glyph
from my_lib import greet              // symbol defined in lib.glyph
from my_lib/utils import helper       // symbol from submodule
```

## Implementation Steps

### Step 1: Update Manifest structs (`crates/glyph-cli/src/glyph.rs`)

Add `LibTarget` and `DependencySpec` types. Change `Manifest`:

```rust
#[derive(Debug, Deserialize)]
struct LibTarget {
    #[serde(default = "default_lib_path")]
    path: String,
}
fn default_lib_path() -> String { "src/lib.glyph".to_string() }

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DependencySpec {
    Path { path: String },
}

struct Manifest {
    package: PackageSection,
    #[serde(default)]
    lib: Option<LibTarget>,          // NEW
    #[serde(default)]
    bin: Vec<BinTarget>,
    #[serde(default)]
    dependencies: HashMap<String, DependencySpec>,  // CHANGED from toml::value::Table
}
```

### Step 2: Update `validate_manifest()` (`glyph.rs`)

- Allow manifests with `[lib]` but no `[[bin]]`:
  `if manifest.bin.is_empty() && manifest.lib.is_none()` -> error
- Validate `[lib].path`: relative, `.glyph` extension, exists on disk
- Validate each dependency: name is a valid identifier (`[a-zA-Z_][a-zA-Z0-9_]*`), name is not `"std"`, path exists, path contains `glyph.toml`, dependency's manifest has `[lib]`

### Step 3: Create `crates/glyph-cli/src/dependency.rs` (~200 lines)

New module with three core functions:

**`resolve_dependencies(root, manifest, visited) -> Vec<ResolvedDependency>`**
- For each entry in `manifest.dependencies`: resolve path, load its `glyph.toml`, verify `[lib]`, check for cycles via `visited` set (canonicalized paths)
- Recursively resolve transitive dependencies (depth-first, dependency-first order)
- Return flat list in topological order

**`load_dependency_modules(dep) -> (HashMap<String, Module>, HashMap<String, SourceInfo>)`**
- Call `discover_and_parse_modules(dep.lib_root)`
- For each module: prefix ID with `<dep_name>/`, call `rewrite_imports()`
- Dual-insert the lib entry module: both as `"<dep_name>/lib"` AND as `"<dep_name>"` (so `from my_lib import X` works)

**`rewrite_imports(module, dep_name)`**
- For each import in `module.imports`: if first segment is NOT `"std"` and NOT a known dependency name, prepend `dep_name` to `path.segments`

### Step 4: Wire into `build_bin()` (`glyph.rs`)

- Thread `&Manifest` into `build_bin()` (add parameter)
- After `discover_and_parse_modules()`, call `resolve_dependencies()` then `load_dependency_modules()` for each
- Merge dependency modules + sources into the main maps (error on collision)
- Rest of pipeline unchanged

### Step 5: Add name collision detection (`crates/glyph-frontend/src/lib.rs:62-125`)

In `compile_multi_module_graph()`, after lowering each module, check for duplicate function names and struct/enum names in the merged MIR. Emit a clear error:
```
function 'helper' defined in both 'utils' and 'my_lib/utils'.
Cross-case function name collisions are not yet supported.
```

This is a documented v0.1 limitation. Name mangling deferred to v0.2.

GLYPH-91 follow-up: function names are now distinguished by their defining
module when they collide, including functions reached through transitive
imports. Struct and enum name collisions remain a limitation.

### Step 6: Handle lib-only cases (`glyph.rs`)

When `glyph build` runs in a case with `[lib]` but no `[[bin]]`:
- Run frontend compilation (catches errors)
- Skip codegen/linking
- Print: `Checked library case '<name>' successfully`

### Step 7: Update `glyph init` template (`glyph.rs:154-167`)

No change to default template (still generates `[[bin]]`). Future: `glyph init --lib`.

## Files Modified

| File | Change |
|------|--------|
| `crates/glyph-cli/src/glyph.rs` | Manifest structs, validate_manifest(), build_bin() signature, build_cmd(), lib-only build path |
| `crates/glyph-cli/src/dependency.rs` | **NEW** - resolve_dependencies(), load_dependency_modules(), rewrite_imports() |
| `crates/glyph-frontend/src/lib.rs` | Add function/struct name collision detection in compile_multi_module_graph() |

## Key Existing Code (reused, not modified)

- `module_loader.rs:discover_and_parse_modules()` -- reused for loading dependency source files
- `module_loader.rs:module_id_from_path()` -- reused for computing prefixed module IDs
- `module_resolver.rs:resolve_multi_module()` -- handles the merged module map unchanged (import path segments join to module IDs at line 161)
- `linker.rs` -- already supports `object_files: Vec<PathBuf>`, no changes needed
- `ast.rs:Import/ImportPath` (lines 292-315) -- `path.segments` is the Vec we prepend to during rewriting

## Known v0.1 Limitations

1. **Function/struct name collisions** between cases produce an error (no name mangling yet)
2. **No visibility system** -- all symbols in a dependency are importable
3. **Flat dependency namespace** -- all dependency names must be globally unique across the graph
4. **No registry** -- path-based dependencies only

## Verification

1. **Unit tests** in `dependency.rs`: import rewriting logic, manifest parsing with [lib]/[dependencies]
2. **Integration test**: create a temp workspace with a lib case and a bin case that depends on it, run `glyph build`, verify the binary runs correctly
3. **Error case tests**: circular deps, missing [lib], invalid dep name, name collisions
4. **Manual test**: create `examples/case_demo/` with a library case and consumer
5. **Run existing tests**: `cargo test --workspace` to ensure nothing breaks
