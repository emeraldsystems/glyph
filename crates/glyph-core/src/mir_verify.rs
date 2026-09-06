//! A structural verifier for [`MirModule`], run once before any LLVM
//! lowering (GLYPH-3).
//!
//! Backend codegen (`crates/glyph-backend/src/codegen/*.rs`) trusts a long
//! list of MIR invariants without checking them in one place: in-range block
//! and local ids, a terminator on every block, no instructions after it,
//! locals whose declared type codegen can actually materialize, and enum
//! variant indices that fit the enum's declared shape. When one of these is
//! violated the backend fails with a `panic!`/`unwrap()` deep inside LLVM
//! lowering (or, worse, builds a corrupt module silently). This module turns
//! those failures into a single, descriptive, pre-codegen pass.
//!
//! # What is checked
//! - Every [`BlockId`] a terminator names is in range for the function.
//! - Every [`LocalId`] named anywhere in a function (params, instructions,
//!   rvalues) is in range for that function's `locals`.
//! - Every block is non-empty and ends with a terminator
//!   ([`MirInst::Return`], [`MirInst::Goto`], or [`MirInst::If`]); no
//!   instruction follows the terminator in the same block.
//! - No declared type (function return type, local type, struct/enum field
//!   type, extern function signature) contains a bare [`Type::Param`] — by
//!   the time MIR reaches this pass, `monomorphize_mir` has already deleted
//!   every generic template from `struct_types`/`enum_types` and rewritten
//!   every reachable `Type::Param` to a concrete type, so a surviving
//!   `Type::Param` means monomorphization missed a spot.
//! - No declared type contains a [`Type::App`] application that codegen does
//!   not special-case. **This is narrower than "reject all `Type::App`":**
//!   empirically (see the doc comment on [`is_recognized_type_app`]) ordinary
//!   generic containers (`Vec<T>`, `Map<K, V>`, `Option<T>`, `Result<T, E>`,
//!   ...) are always lowered to `Type::Named`/`Type::Enum` with a mangled
//!   name before a local's declared type is set; `Type::App` only survives
//!   to codegen for the fixed set of canonical runtime types
//!   (`std::sync::Arc<T>`, `Mutex<T>`, `MutexGuard<T>`, thread/scoped-thread
//!   handles, SPSC `Sender<T>`/`Receiver<T>`) that
//!   `CodegenContext::get_llvm_type` special-cases by hand. Anything else is
//!   exactly the "generic types must be monomorphized before codegen" bail
//!   that backend's `get_llvm_type` would otherwise raise at LLVM-lowering
//!   time; this pass raises the same failure earlier, with a location.
//! - `Rvalue::EnumConstruct`/`Rvalue::EnumPayload` variant indices are within
//!   bounds for the named enum's declared variant list (when the enum is
//!   found in the module's `enum_types`; an unresolvable enum name is not
//!   flagged here to avoid guessing at cross-module wiring this pass cannot
//!   see).
//! - Within a single straight-line block: a local is never read again after
//!   an explicit [`MirInst::Drop`] of it, unless a later [`MirInst::Assign`]
//!   in that same block reinitializes it first.
//!
//! # What is deliberately NOT checked, and why
//! - **Dropping the same local twice in a block.** This looks like an
//!   obvious companion to the use-after-drop check above, and an earlier
//!   version of this pass rejected it — until
//!   `mutex_codegen::duplicate_guard_drop_is_idempotent_and_unlocks_only_once`
//!   (a pre-existing, still-valid backend test) turned out to rely on
//!   exactly that shape: `MutexGuard`'s drop glue
//!   (`codegen/mutex.rs`) deliberately nulls the guard's storage on first
//!   drop, making a second `MirInst::Drop` of the same local a documented,
//!   intentional no-op rather than a bug. "Never drop a local twice" is
//!   therefore not an invariant this MIR actually upholds, so this pass does
//!   not check it.
//! - **`BinaryOp::And`/`BinaryOp::Or` as a raw `Rvalue::Binary`.** Confirmed
//!   by reading both sides: `mir_lower::expr::lower_logical` always lowers
//!   `&&`/`||` to an explicit branch-and-join (never emits
//!   `Rvalue::Binary { op: And | Or, .. }`), and
//!   `codegen::rvalue::codegen_rvalue` explicitly returns
//!   `Err(anyhow!("logical ops should be lowered in MIR"))` for that shape.
//!   So the ticket's assumption holds and this *is* checked (see
//!   [`check_no_logical_binary`]).
//! - **Cross-block dataflow for moves/drops.** The same-block drop/move
//!   check above is deliberately limited to one block: proving a local is
//!   dead across a `Goto`/`If` join requires real dataflow (a live-locals
//!   analysis), which this pass does not attempt. A false negative here is
//!   silence, not a wrong rejection, so it is the safe direction to err in.
//! - **`Rvalue::Move(x)` as a "drop" event.** Unlike an explicit
//!   `MirInst::Drop`, a `Move` does not always mean `x` becomes unreadable —
//!   trivially-`Copy` locals (e.g. `i32`) are read again after being moved
//!   from in this MIR's lowering, and telling those apart from a real
//!   use-after-move needs the same type-directed ownership analysis the
//!   frontend's move-checker already performs during lowering (see
//!   `mir_lower/context.rs`'s `type_requires_owned_local_tracking`). Getting
//!   this wrong in either direction is exactly the false-positive risk this
//!   pass is meant to avoid, so it is left to that existing frontend pass.
//! - **Reachability / dead blocks.** Not an invariant backend relies on;
//!   out of scope for a correctness verifier.
//! - **Struct/enum name existence for `Type::Named`/`Type::Enum`.** Backend
//!   already reports a clear `unknown type ... (not found in enum or struct
//!   types)` error for this; duplicating it here would either have to
//!   reimplement backend's canonical-alias table (`Stdout`, `File`, ...) or
//!   risk a false positive on it drifting out of sync.

use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;

use crate::ast::BinaryOp;
use crate::diag::Diagnostic;
use crate::mir::{
    BlockId, LocalId, MirBlock, MirExternFunction, MirFunction, MirInst, MirModule, MirValue,
    Rvalue,
};
use crate::types::Type;

/// One structural problem found in a [`MirModule`], with enough location
/// information to find it again without re-running the pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyError {
    /// The function the problem was found in, or a `"struct <name>"` /
    /// `"enum <name>"` / `"extern <name>"` pseudo-function for problems
    /// found in module-level type/signature declarations.
    pub function: String,
    pub block: Option<BlockId>,
    pub inst_index: Option<usize>,
    pub message: String,
}

impl VerifyError {
    fn new(function: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            function: function.into(),
            block: None,
            inst_index: None,
            message: message.into(),
        }
    }

    fn at_block(mut self, block: BlockId) -> Self {
        self.block = Some(block);
        self
    }

    fn at_inst(mut self, inst_index: usize) -> Self {
        self.inst_index = Some(inst_index);
        self
    }

    /// Render as an error [`Diagnostic`]. MIR carries no source spans, so the
    /// diagnostic's span is always `None`; callers that have a way to map a
    /// function name back to a span should attach one themselves.
    pub fn to_diagnostic(&self) -> Diagnostic {
        Diagnostic::error(self.to_string(), None)
    }
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "in `{}`", self.function)?;
        if let Some(block) = self.block {
            write!(f, " bb{}", block.0)?;
        }
        if let Some(idx) = self.inst_index {
            write!(f, "[{}]", idx)?;
        }
        write!(f, ": {}", self.message)
    }
}

impl std::error::Error for VerifyError {}

/// Convert verifier errors into error [`Diagnostic`]s, in order.
pub fn errors_to_diagnostics(errors: &[VerifyError]) -> Vec<Diagnostic> {
    errors.iter().map(VerifyError::to_diagnostic).collect()
}

/// Render every error as one multi-line string, suitable for an `anyhow!`
/// bail right before LLVM lowering starts.
pub fn format_errors(errors: &[VerifyError]) -> String {
    errors
        .iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Verify every function, extern declaration, and type definition in
/// `module`. Returns every problem found (never stops at the first one) so a
/// caller can report them all at once; an empty vector means the module is
/// well-formed enough for backend codegen to trust.
///
/// This never clones `module` and does a single linear pass over it (plus a
/// bounded-depth recursive walk per declared `Type`, which is proportional to
/// that type's own size, not the program's).
pub fn verify_module(module: &MirModule) -> Vec<VerifyError> {
    let mut errors = Vec::new();

    for (name, struct_ty) in &module.struct_types {
        for (field_name, field_ty) in &struct_ty.fields {
            if let Err(msg) = check_type(field_ty) {
                errors.push(VerifyError::new(
                    format!("struct {}", name),
                    format!("field `{}` has invalid type: {}", field_name, msg),
                ));
            }
        }
    }

    for (name, enum_ty) in &module.enum_types {
        for variant in &enum_ty.variants {
            if let Some(payload) = &variant.payload {
                if let Err(msg) = check_type(payload) {
                    errors.push(VerifyError::new(
                        format!("enum {}", name),
                        format!(
                            "variant `{}` payload has invalid type: {}",
                            variant.name, msg
                        ),
                    ));
                }
            }
        }
    }

    for ext in &module.extern_functions {
        verify_extern_function(ext, &mut errors);
    }

    for func in &module.functions {
        verify_function(func, module, &mut errors);
    }

    errors
}

fn verify_extern_function(ext: &MirExternFunction, errors: &mut Vec<VerifyError>) {
    let label = format!("extern {}", ext.name);
    for (i, param_ty) in ext.params.iter().enumerate() {
        if let Err(msg) = check_type(param_ty) {
            errors.push(VerifyError::new(
                label.clone(),
                format!("param {} has invalid type: {}", i, msg),
            ));
        }
    }
    if let Some(ret_ty) = &ext.ret_type {
        if let Err(msg) = check_type(ret_ty) {
            errors.push(VerifyError::new(
                label.clone(),
                format!("return type is invalid: {}", msg),
            ));
        }
    }
}

fn verify_function(func: &MirFunction, module: &MirModule, errors: &mut Vec<VerifyError>) {
    let name = &func.name;
    let n_locals = func.locals.len();
    let n_blocks = func.blocks.len();

    for (i, param) in func.params.iter().enumerate() {
        if param.0 as usize >= n_locals {
            errors.push(VerifyError::new(
                name.clone(),
                format!(
                    "param {} references out-of-range local {:?} (function has {} locals)",
                    i, param, n_locals
                ),
            ));
        }
    }

    if let Some(ret_ty) = &func.ret_type {
        if let Err(msg) = check_type(ret_ty) {
            errors.push(VerifyError::new(
                name.clone(),
                format!("return type is invalid: {}", msg),
            ));
        }
    }

    for (i, local) in func.locals.iter().enumerate() {
        if let Some(ty) = &local.ty {
            if let Err(msg) = check_type(ty) {
                errors.push(VerifyError::new(
                    name.clone(),
                    format!("local {} has invalid type: {}", i, msg),
                ));
            }
        }
    }

    if func.blocks.is_empty() {
        errors.push(VerifyError::new(
            name.clone(),
            "function has no basic blocks",
        ));
        return;
    }

    for (bidx, block) in func.blocks.iter().enumerate() {
        let block_id = BlockId(bidx as u32);
        verify_block(func, module, block_id, block, n_locals, n_blocks, errors);
    }
}

fn verify_block(
    func: &MirFunction,
    module: &MirModule,
    block_id: BlockId,
    block: &MirBlock,
    n_locals: usize,
    n_blocks: usize,
    errors: &mut Vec<VerifyError>,
) {
    let name = &func.name;

    if block.insts.is_empty() {
        errors.push(
            VerifyError::new(name.clone(), "block is empty; missing a terminator")
                .at_block(block_id),
        );
        return;
    }

    // Same-block use-after-drop tracking (see module docs for scope; note
    // this deliberately does not flag a *second* Drop of an already-dropped
    // local as its own error class — see the check site below for why).
    let mut dropped: HashSet<LocalId> = HashSet::new();

    let last_index = block.insts.len() - 1;
    for (idx, inst) in block.insts.iter().enumerate() {
        // 1. Local/block id bounds, over every id mentioned anywhere in the
        //    instruction (targets, operands, and drop operands alike).
        let mut ids = Vec::new();
        collect_all_locals_in_inst(inst, &mut ids);
        for id in ids {
            if id.0 as usize >= n_locals {
                errors.push(
                    VerifyError::new(
                        name.clone(),
                        format!(
                            "references out-of-range local {:?} (function has {} locals)",
                            id, n_locals
                        ),
                    )
                    .at_block(block_id)
                    .at_inst(idx),
                );
            }
        }
        for target in terminator_targets(inst) {
            if target.0 as usize >= n_blocks {
                errors.push(
                    VerifyError::new(
                        name.clone(),
                        format!(
                            "references out-of-range block {:?} (function has {} blocks)",
                            target, n_blocks
                        ),
                    )
                    .at_block(block_id)
                    .at_inst(idx),
                );
            }
        }

        // 2. Terminator placement.
        if is_terminator(inst) && idx != last_index {
            errors.push(
                VerifyError::new(
                    name.clone(),
                    "instruction follows a terminator in the same block",
                )
                .at_block(block_id)
                .at_inst(idx),
            );
        }

        // 3. Enum variant index bounds.
        if let Some(rvalue) = rvalue_of(inst) {
            check_enum_indices(rvalue, func, module, name, block_id, idx, errors);
            check_no_logical_binary(rvalue, name, block_id, idx, errors);
        }

        // 4. Same-block use-after-drop. Note there is deliberately no
        // "double drop" check here: `MutexGuard`'s drop glue (see
        // `codegen/mutex.rs`) is intentionally idempotent — it nulls the
        // guard's storage on first drop so a second `MirInst::Drop` of the
        // same local is a documented no-op, not a bug (backend test
        // `mutex_codegen::duplicate_guard_drop_is_idempotent_and_unlocks_only_once`
        // exercises exactly this MIR shape). A blanket "never drop the same
        // local twice in a block" rule is therefore not an invariant this
        // MIR actually upholds.
        let mut reads = Vec::new();
        collect_read_locals_in_inst(inst, &mut reads);
        for id in reads {
            if dropped.contains(&id) {
                errors.push(
                    VerifyError::new(
                        name.clone(),
                        format!("local {:?} used after being dropped in this block", id),
                    )
                    .at_block(block_id)
                    .at_inst(idx),
                );
            }
        }
        if let MirInst::Drop(id) = inst {
            dropped.insert(*id);
        }
        // An `Assign` target, or an rvalue's `out_*`-style destination field
        // (see `rvalue_write_only_targets`), is freshly (re)initialized by
        // this instruction, clearing any prior same-block "dropped" mark.
        // `AssignField`/`AssignIndex`'s `base` is deliberately excluded: it
        // is mutated *through*, not fully reinitialized, by those two.
        match inst {
            MirInst::Assign { local, value } => {
                dropped.remove(local);
                for id in rvalue_write_only_targets(value) {
                    dropped.remove(&id);
                }
            }
            MirInst::AssignField { value, .. } | MirInst::AssignIndex { value, .. } => {
                for id in rvalue_write_only_targets(value) {
                    dropped.remove(&id);
                }
            }
            _ => {}
        }
    }

    let last = &block.insts[last_index];
    if !is_terminator(last) {
        errors.push(
            VerifyError::new(name.clone(), "block does not end with a terminator")
                .at_block(block_id)
                .at_inst(last_index),
        );
    }
}

fn is_terminator(inst: &MirInst) -> bool {
    matches!(
        inst,
        MirInst::Return(_) | MirInst::Goto(_) | MirInst::If { .. }
    )
}

fn terminator_targets(inst: &MirInst) -> Vec<BlockId> {
    match inst {
        MirInst::Goto(bb) => vec![*bb],
        MirInst::If { then_bb, else_bb, .. } => vec![*then_bb, *else_bb],
        _ => Vec::new(),
    }
}

/// The `Rvalue` an instruction carries, if any (all three assignment-shaped
/// instructions carry exactly one).
fn rvalue_of(inst: &MirInst) -> Option<&Rvalue> {
    match inst {
        MirInst::Assign { value, .. }
        | MirInst::AssignField { value, .. }
        | MirInst::AssignIndex { value, .. } => Some(value),
        _ => None,
    }
}

fn check_no_logical_binary(
    rvalue: &Rvalue,
    func_name: &str,
    block_id: BlockId,
    idx: usize,
    errors: &mut Vec<VerifyError>,
) {
    if let Rvalue::Binary { op, .. } = rvalue {
        if matches!(op, BinaryOp::And | BinaryOp::Or) {
            errors.push(
                VerifyError::new(
                    func_name.to_string(),
                    format!(
                        "raw `{:?}` reached MIR as an eager Rvalue::Binary; logical operators must \
                         be lowered to control flow (see lower_logical) before codegen",
                        op
                    ),
                )
                .at_block(block_id)
                .at_inst(idx),
            );
        }
    }
}

fn check_enum_indices(
    rvalue: &Rvalue,
    func: &MirFunction,
    module: &MirModule,
    func_name: &str,
    block_id: BlockId,
    idx: usize,
    errors: &mut Vec<VerifyError>,
) {
    match rvalue {
        Rvalue::EnumConstruct {
            enum_name,
            variant_index,
            ..
        } => {
            if let Some(enum_ty) = module.enum_types.get(enum_name) {
                if *variant_index as usize >= enum_ty.variants.len() {
                    errors.push(
                        VerifyError::new(
                            func_name.to_string(),
                            format!(
                                "EnumConstruct variant index {} out of range for enum `{}` ({} variant(s))",
                                variant_index,
                                enum_name,
                                enum_ty.variants.len()
                            ),
                        )
                        .at_block(block_id)
                        .at_inst(idx),
                    );
                }
            }
        }
        Rvalue::EnumPayload {
            base,
            variant_index,
            ..
        } => {
            if let Some(enum_name) = resolve_enum_name(*base, func) {
                if let Some(enum_ty) = module.enum_types.get(enum_name) {
                    if *variant_index as usize >= enum_ty.variants.len() {
                        errors.push(
                            VerifyError::new(
                                func_name.to_string(),
                                format!(
                                    "EnumPayload variant index {} out of range for enum `{}` ({} variant(s))",
                                    variant_index,
                                    enum_name,
                                    enum_ty.variants.len()
                                ),
                            )
                            .at_block(block_id)
                            .at_inst(idx),
                        );
                    }
                }
            }
        }
        _ => {}
    }
}

fn resolve_enum_name(base: LocalId, func: &MirFunction) -> Option<&str> {
    let ty = func.locals.get(base.0 as usize)?.ty.as_ref()?;
    match ty {
        Type::Enum(name) => Some(name.as_str()),
        Type::Ref(inner, _) => match inner.as_ref() {
            Type::Enum(name) => Some(name.as_str()),
            _ => None,
        },
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Type validity: reject Type::Param everywhere, and Type::App everywhere
// except the fixed set of canonical runtime applications codegen special-
// cases directly (see the module doc comment for how this was established).
// ---------------------------------------------------------------------------

/// `Type::App { base, .. }` applications that `CodegenContext::get_llvm_type`
/// materializes directly instead of bailing on. Mirrors (by calling the same
/// public predicates) the exact set backend accepts: `Type::arc`/`is_arc`,
/// `is_mutex`, `is_mutex_guard`, the canonical (scoped) thread handle
/// representations in `glyph_core::thread`, and the two SPSC endpoints.
/// Every other generic container (`Vec<T>`, `Map<K, V>`, `Option<T>`,
/// `Result<T, E>`, `TrySendResult<T>`, `TryRecvResult<T>`, ...) is confirmed
/// (by lowering small probe programs and inspecting the resulting
/// `Local::ty`) to already be a `Type::Named`/`Type::Enum` with a mangled
/// name (e.g. `Vec<i16>` -> `Type::Named("Vec$i16")`) by the time it reaches
/// a local's declared type, so a `Type::App` with any other `base` here is a
/// leftover generic application that monomorphization/lowering failed to
/// resolve.
fn is_recognized_type_app(ty: &Type) -> bool {
    ty.is_arc()
        || ty.is_mutex()
        || ty.is_mutex_guard()
        || ty.is_spsc_sender()
        || ty.is_spsc_receiver()
        || crate::thread::is_canonical_thread_handle(ty)
        || crate::thread::is_canonical_scoped_thread_handle(ty)
        || crate::thread::private_scoped_thread_handle_result(ty).is_some()
}

/// Recursively check that `ty` contains no bare `Type::Param` and no
/// unrecognized `Type::App` (see [`is_recognized_type_app`]). Depth is bounded
/// by the type's own structure, not by program size.
fn check_type(ty: &Type) -> Result<(), String> {
    match ty {
        // Wording deliberately matches `CodegenContext::get_llvm_type`'s own
        // bail (`crates/glyph-backend/src/codegen/types.rs`) verbatim: this
        // check exists to raise the exact same failure earlier, with a
        // location, and at least two pre-existing backend tests
        // (`mutex_codegen::forged_mutex_and_guard_applications_are_rejected_by_codegen`,
        // `spsc_codegen::spsc_rejects_noncanonical_endpoints_and_unsupported_targets`)
        // assert on this substring.
        Type::Param(p) => Err(format!(
            "generic types must be monomorphized before codegen (param: {})",
            p
        )),
        Type::App { base, args } => {
            if is_recognized_type_app(ty) {
                for arg in args {
                    check_type(arg)?;
                }
                Ok(())
            } else {
                Err(format!(
                    "generic types must be monomorphized before codegen (app: {}<{:?}>)",
                    base, args
                ))
            }
        }
        Type::Ref(inner, _)
        | Type::Array(inner, _)
        | Type::Own(inner)
        | Type::RawPtr(inner)
        | Type::Shared(inner) => check_type(inner),
        Type::Function { params, ret } | Type::BorrowedFunction { params, ret, .. } => {
            for p in params {
                check_type(p)?;
            }
            check_type(ret)
        }
        Type::Tuple(elems) => {
            for e in elems {
                check_type(e)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Exhaustive LocalId extraction. Two flavors: "all" (for bounds checking,
// includes write targets) and "reads" (excludes an Assign's own write
// target, for the same-block drop/use-after-drop check).
// ---------------------------------------------------------------------------

fn push_local(out: &mut Vec<LocalId>, id: LocalId) {
    out.push(id);
}

fn push_value(out: &mut Vec<LocalId>, v: &MirValue) {
    if let MirValue::Local(id) = v {
        out.push(*id);
    }
}

fn collect_all_locals_in_inst(inst: &MirInst, out: &mut Vec<LocalId>) {
    match inst {
        MirInst::Assign { local, value } => {
            push_local(out, *local);
            collect_rvalue_locals(value, out);
        }
        MirInst::AssignField { base, value, .. } => {
            push_local(out, *base);
            collect_rvalue_locals(value, out);
        }
        MirInst::AssignIndex { base, index, value } => {
            push_local(out, *base);
            push_value(out, index);
            collect_rvalue_locals(value, out);
        }
        MirInst::Return(v) => {
            if let Some(v) = v {
                push_value(out, v);
            }
        }
        MirInst::Goto(_) => {}
        MirInst::If { cond, .. } => push_value(out, cond),
        MirInst::Drop(id)
        | MirInst::DropThreadHandle(id)
        | MirInst::DropThreadScope(id)
        | MirInst::DrainThreadScope(id)
        | MirInst::DropScopedThreadHandle(id) => push_local(out, *id),
        MirInst::Nop => {}
    }
}

/// Like [`collect_all_locals_in_inst`], but excludes an `Assign`'s own write
/// target (that is a definition, not a use) so it is safe to feed straight
/// into the same-block use-after-drop check.
fn collect_read_locals_in_inst(inst: &MirInst, out: &mut Vec<LocalId>) {
    match inst {
        MirInst::Assign { value, .. } => push_rvalue_reads(value, out),
        MirInst::AssignField { base, value, .. } => {
            // `base` is read (mutated through); `value`'s write-only fields
            // (see `rvalue_write_only_targets`) are filtered out.
            push_local(out, *base);
            push_rvalue_reads(value, out);
        }
        MirInst::AssignIndex { base, index, value } => {
            push_local(out, *base);
            push_value(out, index);
            push_rvalue_reads(value, out);
        }
        // `Drop`'s own operand is the drop event itself, not a use of the
        // local's value. Dropping an already-dropped local is intentionally
        // not flagged at all (see `verify_block`'s drop-tracking comment),
        // so this must not be treated as a "read" that would trip the
        // use-after-drop check on a second `Drop` of the same local.
        MirInst::Drop(_) => {}
        _ => collect_all_locals_in_inst(inst, out),
    }
}

/// Every local `rv` reads, excluding its documented `out_*`-style
/// destination fields (see [`rvalue_write_only_targets`]) — those are
/// written, not read, so they must not trip the same-block use-after-drop
/// check. Bounds-checking still sees them via [`collect_rvalue_locals`].
fn push_rvalue_reads(rv: &Rvalue, out: &mut Vec<LocalId>) {
    let mut all = Vec::new();
    collect_rvalue_locals(rv, &mut all);
    let write_only = rvalue_write_only_targets(rv);
    out.extend(all.into_iter().filter(|id| !write_only.contains(id)));
}

/// Local ids `rv` only initializes (never reads): the `out_handle`/
/// `out_result`/`out_scope`/`out_receiver`/`out_unsent`/`out_value`-style
/// fields on the thread/SPSC rvalues, which this MIR's own naming convention
/// already marks as destinations. A dropped local written through one of
/// these is being freshly reinitialized, exactly like an `Assign` target, so
/// `verify_block` also uses this to clear a prior same-block "dropped" mark.
fn rvalue_write_only_targets(rv: &Rvalue) -> Vec<LocalId> {
    match rv {
        Rvalue::ThreadSpawnUnit { out_handle, .. }
        | Rvalue::ThreadSpawnResult { out_handle, .. }
        | Rvalue::ScopedThreadSpawnUnit { out_handle, .. }
        | Rvalue::ScopedThreadSpawnResult { out_handle, .. } => vec![*out_handle],
        Rvalue::ThreadJoinResult { out_result, .. }
        | Rvalue::ScopedThreadJoinResult { out_result, .. } => vec![*out_result],
        Rvalue::ThreadScopeCreate { out_scope } => vec![*out_scope],
        Rvalue::SpscChannelNew { out_receiver, .. } => vec![*out_receiver],
        Rvalue::SpscTrySend { out_unsent, .. } => vec![*out_unsent],
        Rvalue::SpscTryRecv { out_value, .. } => vec![*out_value],
        _ => Vec::new(),
    }
}

fn collect_rvalue_locals(rv: &Rvalue, out: &mut Vec<LocalId>) {
    match rv {
        Rvalue::ConstInt(_) | Rvalue::ConstFloat(_) | Rvalue::ConstBool(_) => {}
        Rvalue::Move(id) => push_local(out, *id),
        Rvalue::Deref { base, .. } => push_local(out, *base),
        Rvalue::StringLit { .. } => {}
        Rvalue::Binary { lhs, rhs, .. } => {
            push_value(out, lhs);
            push_value(out, rhs);
        }
        Rvalue::Cast { value, .. } => push_value(out, value),
        Rvalue::Call { args, .. } => {
            for a in args {
                push_value(out, a);
            }
        }
        Rvalue::FunctionRef { .. } => {}
        Rvalue::MakeClosure { captures, .. } => {
            for c in captures {
                push_local(out, c.local);
            }
        }
        Rvalue::MakeBorrowedClosure { captures, .. } => {
            for c in captures {
                push_local(out, c.local);
            }
        }
        Rvalue::CallIndirect { callee, args, .. }
        | Rvalue::CallIndirectShared { callee, args, .. }
        | Rvalue::CallIndirectMut { callee, args, .. } => {
            push_local(out, *callee);
            for a in args {
                push_value(out, a);
            }
        }
        Rvalue::ThreadSpawnUnit { task, out_handle } => {
            push_local(out, *task);
            push_local(out, *out_handle);
        }
        Rvalue::ThreadSpawnResult {
            task, out_handle, ..
        } => {
            push_local(out, *task);
            push_local(out, *out_handle);
        }
        Rvalue::ThreadJoinUnit { handle } => push_local(out, *handle),
        Rvalue::ThreadJoinResult {
            handle, out_result, ..
        } => {
            push_local(out, *handle);
            push_local(out, *out_result);
        }
        Rvalue::ThreadDetachUnit { handle } => push_local(out, *handle),
        Rvalue::ThreadHandleFromRaw { raw } => push_local(out, *raw),
        Rvalue::ThreadHandleIntoRaw { handle } => push_local(out, *handle),
        Rvalue::ThreadErrorFromStatus { status } => push_value(out, status),
        Rvalue::ThreadScopeCreate { out_scope } => push_local(out, *out_scope),
        Rvalue::ThreadScopeExit { scope } => push_local(out, *scope),
        Rvalue::ThreadScopeFromRaw { raw } => push_local(out, *raw),
        Rvalue::ThreadScopeDrain { scope } => push_local(out, *scope),
        Rvalue::ScopedThreadSpawnUnit {
            scope,
            task,
            out_handle,
        } => {
            push_local(out, *scope);
            push_local(out, *task);
            push_local(out, *out_handle);
        }
        Rvalue::ScopedThreadSpawnResult {
            scope,
            task,
            out_handle,
            ..
        } => {
            push_local(out, *scope);
            push_local(out, *task);
            push_local(out, *out_handle);
        }
        Rvalue::ScopedThreadJoinUnit { handle } => push_local(out, *handle),
        Rvalue::ScopedThreadJoinResult {
            handle, out_result, ..
        } => {
            push_local(out, *handle);
            push_local(out, *out_result);
        }
        Rvalue::ScopedThreadHandleFromRaw { raw, .. } => push_local(out, *raw),
        Rvalue::ScopedThreadHandleIntoRaw { handle, .. } => push_local(out, *handle),
        Rvalue::StructLit { field_values, .. } => {
            for (_, v) in field_values {
                push_value(out, v);
            }
        }
        Rvalue::FieldAccess { base, .. } => push_local(out, *base),
        Rvalue::FieldRef { base, .. } => push_local(out, *base),
        Rvalue::Ref { base, .. } => push_local(out, *base),
        Rvalue::ArrayLit { elements, .. } => {
            for e in elements {
                push_value(out, e);
            }
        }
        Rvalue::ArrayIndex { base, index, .. } => {
            push_local(out, *base);
            push_value(out, index);
        }
        Rvalue::ArrayLen { base } => push_local(out, *base),
        Rvalue::VecNew { .. } => {}
        Rvalue::VecWithCapacity { capacity, .. } => push_value(out, capacity),
        Rvalue::VecPush { vec, value, .. } => {
            push_local(out, *vec);
            push_value(out, value);
        }
        Rvalue::VecPop { vec, .. } => push_local(out, *vec),
        Rvalue::VecLen { vec } => push_local(out, *vec),
        Rvalue::VecIndex { vec, index, .. } => {
            push_local(out, *vec);
            push_value(out, index);
        }
        Rvalue::VecIndexRef { vec, index, .. } => {
            push_local(out, *vec);
            push_value(out, index);
        }
        Rvalue::MapNew { .. } => {}
        Rvalue::MapWithCapacity { capacity, .. } => push_value(out, capacity),
        Rvalue::MapAdd { map, key, value, .. } => {
            push_local(out, *map);
            push_value(out, key);
            push_value(out, value);
        }
        Rvalue::MapUpdate { map, key, value, .. } => {
            push_local(out, *map);
            push_value(out, key);
            push_value(out, value);
        }
        Rvalue::MapDel { map, key, .. } => {
            push_local(out, *map);
            push_value(out, key);
        }
        Rvalue::MapGet { map, key, .. } => {
            push_local(out, *map);
            push_value(out, key);
        }
        Rvalue::MapHas { map, key, .. } => {
            push_local(out, *map);
            push_value(out, key);
        }
        Rvalue::MapKeys { map, .. } => push_local(out, *map),
        Rvalue::MapVals { map, .. } => push_local(out, *map),
        Rvalue::FileOpen { path, .. } => push_value(out, path),
        Rvalue::FileReadToString { file } => push_local(out, *file),
        Rvalue::FileWriteString { file, contents } => {
            push_local(out, *file);
            push_value(out, contents);
        }
        Rvalue::FileClose { file } => push_local(out, *file),
        Rvalue::StringLen { base } => push_local(out, *base),
        Rvalue::StringConcat { base, value } => {
            push_local(out, *base);
            push_value(out, value);
        }
        Rvalue::StringSlice { base, start, len } => {
            push_local(out, *base);
            push_value(out, start);
            push_value(out, len);
        }
        Rvalue::StringTrim { base } => push_local(out, *base),
        Rvalue::StringSplit { base, sep } => {
            push_local(out, *base);
            push_value(out, sep);
        }
        Rvalue::StringStartsWith { base, needle } => {
            push_local(out, *base);
            push_value(out, needle);
        }
        Rvalue::StringEndsWith { base, needle } => {
            push_local(out, *base);
            push_value(out, needle);
        }
        Rvalue::StringClone { base } => push_local(out, *base),
        Rvalue::OwnNew { value, .. } => push_value(out, value),
        Rvalue::OwnIntoRaw { base, .. } => push_local(out, *base),
        Rvalue::OwnFromRaw { ptr, .. } => push_value(out, ptr),
        Rvalue::RawPtrNull { .. } => {}
        Rvalue::SharedNew { value, .. } => push_value(out, value),
        Rvalue::SharedClone { base, .. } => push_local(out, *base),
        Rvalue::ArcNew { value, .. } => push_value(out, value),
        Rvalue::ArcClone { base, .. } => push_local(out, *base),
        Rvalue::ArcBorrow { base, .. } => push_local(out, *base),
        Rvalue::MutexNew { value, .. } => push_value(out, value),
        Rvalue::MutexLock { base, .. } => push_local(out, *base),
        Rvalue::MutexTryLock { base, .. } => push_local(out, *base),
        Rvalue::MutexGuardIsAcquired { guard, .. } => push_local(out, *guard),
        Rvalue::MutexGuardBorrow { guard, .. } => push_local(out, *guard),
        Rvalue::SpscChannelNew { capacity, out_receiver, .. } => {
            push_value(out, capacity);
            push_local(out, *out_receiver);
        }
        Rvalue::SpscTrySend {
            sender,
            value,
            out_unsent,
            ..
        } => {
            push_local(out, *sender);
            push_local(out, *value);
            push_local(out, *out_unsent);
        }
        Rvalue::SpscTryRecv {
            receiver, out_value, ..
        } => {
            push_local(out, *receiver);
            push_local(out, *out_value);
        }
        Rvalue::AtomicNew { value, .. } => push_value(out, value),
        Rvalue::AtomicLoad { atomic, .. } => push_local(out, *atomic),
        Rvalue::AtomicStore { atomic, value, .. } => {
            push_local(out, *atomic);
            push_value(out, value);
        }
        Rvalue::AtomicRmw { atomic, value, .. } => {
            push_local(out, *atomic);
            push_value(out, value);
        }
        Rvalue::AtomicCompareExchange {
            atomic,
            expected,
            desired,
            ..
        } => {
            push_local(out, *atomic);
            push_value(out, expected);
            push_value(out, desired);
        }
        Rvalue::AtomicFence { .. } => {}
        Rvalue::AtomicIsLockFree { .. } => {}
        Rvalue::EnumConstruct { payload, .. } => {
            if let Some(p) = payload {
                push_value(out, p);
            }
        }
        Rvalue::EnumTag { base } => push_local(out, *base),
        Rvalue::EnumPayload { base, .. } => push_local(out, *base),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atomic::AtomicScalar;
    use crate::mir::{Local, MirBlock};
    use crate::types::{EnumType, EnumVariant, Mutability, StructType};

    fn simple_function(blocks: Vec<MirBlock>, locals: Vec<Local>) -> MirFunction {
        MirFunction {
            name: "f".into(),
            ret_type: Some(Type::I32),
            params: Vec::new(),
            locals,
            blocks,
        }
    }

    fn int_local() -> Local {
        Local {
            name: None,
            ty: Some(Type::I32),
            mutable: false,
            skip_drop: false,
        }
    }

    fn module_with(func: MirFunction) -> MirModule {
        MirModule {
            struct_types: HashMap::new(),
            enum_types: HashMap::new(),
            functions: vec![func],
            extern_functions: Vec::new(),
        }
    }

    #[test]
    fn well_formed_function_verifies_clean() {
        let func = simple_function(
            vec![MirBlock {
                insts: vec![MirInst::Return(Some(MirValue::Int(0)))],
            }],
            vec![],
        );
        let module = module_with(func);
        assert!(verify_module(&module).is_empty());
    }

    #[test]
    fn rejects_goto_to_out_of_range_block() {
        let func = simple_function(
            vec![MirBlock {
                insts: vec![MirInst::Goto(BlockId(5))],
            }],
            vec![],
        );
        let errors = verify_module(&module_with(func));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("out-of-range block"));
    }

    #[test]
    fn rejects_if_to_out_of_range_block() {
        let func = simple_function(
            vec![MirBlock {
                insts: vec![MirInst::If {
                    cond: MirValue::Bool(true),
                    then_bb: BlockId(1),
                    else_bb: BlockId(2),
                }],
            }],
            vec![],
        );
        let errors = verify_module(&module_with(func));
        assert_eq!(errors.len(), 2, "both then_bb and else_bb are out of range");
        assert!(errors.iter().all(|e| e.message.contains("out-of-range block")));
    }

    #[test]
    fn rejects_out_of_range_local_id() {
        let func = simple_function(
            vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(0),
                        value: Rvalue::Move(LocalId(7)),
                    },
                    MirInst::Return(None),
                ],
            }],
            vec![int_local()],
        );
        let errors = verify_module(&module_with(func));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("out-of-range local"));
    }

    #[test]
    fn rejects_missing_terminator() {
        let func = simple_function(
            vec![MirBlock {
                insts: vec![MirInst::Assign {
                    local: LocalId(0),
                    value: Rvalue::ConstInt(1),
                }],
            }],
            vec![int_local()],
        );
        let errors = verify_module(&module_with(func));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("does not end with a terminator"));
    }

    #[test]
    fn rejects_empty_block() {
        let func = simple_function(vec![MirBlock { insts: vec![] }], vec![]);
        let errors = verify_module(&module_with(func));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("missing a terminator"));
    }

    #[test]
    fn rejects_instruction_after_terminator() {
        let func = simple_function(
            vec![MirBlock {
                insts: vec![
                    MirInst::Return(None),
                    MirInst::Assign {
                        local: LocalId(0),
                        value: Rvalue::ConstInt(1),
                    },
                ],
            }],
            vec![int_local()],
        );
        let errors = verify_module(&module_with(func));
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("follows a terminator")),
            "{:?}",
            errors
        );
    }

    #[test]
    fn rejects_bare_type_param_in_local() {
        let mut local = int_local();
        local.ty = Some(Type::Param("T".into()));
        let func = simple_function(
            vec![MirBlock {
                insts: vec![MirInst::Return(None)],
            }],
            vec![local],
        );
        let errors = verify_module(&module_with(func));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("must be monomorphized before codegen"));
    }

    #[test]
    fn rejects_type_param_nested_inside_ref() {
        let mut local = int_local();
        local.ty = Some(Type::Ref(
            Box::new(Type::Param("T".into())),
            Mutability::Immutable,
        ));
        let func = simple_function(
            vec![MirBlock {
                insts: vec![MirInst::Return(None)],
            }],
            vec![local],
        );
        let errors = verify_module(&module_with(func));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("must be monomorphized before codegen"));
    }

    #[test]
    fn rejects_unrecognized_type_app() {
        // A `Vec`-shaped `Type::App` should never survive to a local's
        // declared type (see the module doc comment); this stands in for any
        // generic application monomorphization failed to rewrite.
        let mut local = int_local();
        local.ty = Some(Type::App {
            base: "Vec".into(),
            args: vec![Type::I32],
        });
        let func = simple_function(
            vec![MirBlock {
                insts: vec![MirInst::Return(None)],
            }],
            vec![local],
        );
        let errors = verify_module(&module_with(func));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("must be monomorphized before codegen"));
    }

    #[test]
    fn accepts_canonical_arc_type_app() {
        let mut local = int_local();
        local.ty = Some(Type::arc(Type::I32));
        let func = simple_function(
            vec![MirBlock {
                insts: vec![MirInst::Return(None)],
            }],
            vec![local],
        );
        assert!(verify_module(&module_with(func)).is_empty());
    }

    #[test]
    fn accepts_canonical_mutex_and_guard_type_app() {
        let mut a = int_local();
        a.ty = Some(Type::mutex(Type::I32));
        let mut b = int_local();
        b.ty = Some(Type::mutex_guard(Type::I32));
        let func = simple_function(
            vec![MirBlock {
                insts: vec![MirInst::Return(None)],
            }],
            vec![a, b],
        );
        assert!(verify_module(&module_with(func)).is_empty());
    }

    #[test]
    fn accepts_canonical_spsc_type_app() {
        let mut sender = int_local();
        sender.ty = Some(Type::spsc_sender(Type::I32));
        let mut receiver = int_local();
        receiver.ty = Some(Type::spsc_receiver(Type::I32));
        let func = simple_function(
            vec![MirBlock {
                insts: vec![MirInst::Return(None)],
            }],
            vec![sender, receiver],
        );
        assert!(verify_module(&module_with(func)).is_empty());
    }

    #[test]
    fn rejects_logical_and_as_raw_binary() {
        let func = simple_function(
            vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(0),
                        value: Rvalue::Binary {
                            op: BinaryOp::And,
                            lhs: MirValue::Bool(true),
                            rhs: MirValue::Bool(false),
                        },
                    },
                    MirInst::Return(None),
                ],
            }],
            vec![Local {
                name: None,
                ty: Some(Type::Bool),
                mutable: false,
                skip_drop: false,
            }],
        );
        let errors = verify_module(&module_with(func));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("logical operators"));
    }

    #[test]
    fn rejects_logical_or_as_raw_binary() {
        let func = simple_function(
            vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(0),
                        value: Rvalue::Binary {
                            op: BinaryOp::Or,
                            lhs: MirValue::Bool(true),
                            rhs: MirValue::Bool(false),
                        },
                    },
                    MirInst::Return(None),
                ],
            }],
            vec![Local {
                name: None,
                ty: Some(Type::Bool),
                mutable: false,
                skip_drop: false,
            }],
        );
        let errors = verify_module(&module_with(func));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("logical operators"));
    }

    #[test]
    fn accepts_eager_comparison_binary() {
        let func = simple_function(
            vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(0),
                        value: Rvalue::Binary {
                            op: BinaryOp::Lt,
                            lhs: MirValue::Int(1),
                            rhs: MirValue::Int(2),
                        },
                    },
                    MirInst::Return(None),
                ],
            }],
            vec![Local {
                name: None,
                ty: Some(Type::Bool),
                mutable: false,
                skip_drop: false,
            }],
        );
        assert!(verify_module(&module_with(func)).is_empty());
    }

    #[test]
    fn rejects_out_of_range_enum_construct_variant() {
        let mut module = module_with(simple_function(
            vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(0),
                        value: Rvalue::EnumConstruct {
                            enum_name: "Color".into(),
                            variant_index: 9,
                            payload: None,
                        },
                    },
                    MirInst::Return(None),
                ],
            }],
            vec![Local {
                name: None,
                ty: Some(Type::Enum("Color".into())),
                mutable: false,
                skip_drop: false,
            }],
        ));
        module.enum_types.insert(
            "Color".into(),
            EnumType {
                name: "Color".into(),
                variants: vec![
                    EnumVariant {
                        name: "Red".into(),
                        payload: None,
                    },
                    EnumVariant {
                        name: "Blue".into(),
                        payload: None,
                    },
                ],
            },
        );
        let errors = verify_module(&module);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("EnumConstruct"));
        assert!(errors[0].message.contains("out of range"));
    }

    #[test]
    fn accepts_in_range_enum_construct_variant() {
        let mut module = module_with(simple_function(
            vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(0),
                        value: Rvalue::EnumConstruct {
                            enum_name: "Color".into(),
                            variant_index: 1,
                            payload: None,
                        },
                    },
                    MirInst::Return(None),
                ],
            }],
            vec![Local {
                name: None,
                ty: Some(Type::Enum("Color".into())),
                mutable: false,
                skip_drop: false,
            }],
        ));
        module.enum_types.insert(
            "Color".into(),
            EnumType {
                name: "Color".into(),
                variants: vec![
                    EnumVariant {
                        name: "Red".into(),
                        payload: None,
                    },
                    EnumVariant {
                        name: "Blue".into(),
                        payload: None,
                    },
                ],
            },
        );
        assert!(verify_module(&module).is_empty());
    }

    #[test]
    fn rejects_out_of_range_enum_payload_variant() {
        let base_local = Local {
            name: None,
            ty: Some(Type::Enum("Color".into())),
            mutable: false,
            skip_drop: false,
        };
        let dest_local = Local {
            name: None,
            ty: Some(Type::I32),
            mutable: false,
            skip_drop: false,
        };
        let mut module = module_with(simple_function(
            vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(1),
                        value: Rvalue::EnumPayload {
                            base: LocalId(0),
                            variant_index: 4,
                            payload_type: Type::I32,
                        },
                    },
                    MirInst::Return(None),
                ],
            }],
            vec![base_local, dest_local],
        ));
        module.enum_types.insert(
            "Color".into(),
            EnumType {
                name: "Color".into(),
                variants: vec![EnumVariant {
                    name: "Red".into(),
                    payload: Some(Type::I32),
                }],
            },
        );
        let errors = verify_module(&module);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("EnumPayload"));
    }

    #[test]
    fn use_after_drop_in_same_block_is_rejected() {
        let func = simple_function(
            vec![MirBlock {
                insts: vec![
                    MirInst::Drop(LocalId(0)),
                    MirInst::Assign {
                        local: LocalId(1),
                        value: Rvalue::Move(LocalId(0)),
                    },
                    MirInst::Return(None),
                ],
            }],
            vec![int_local(), int_local()],
        );
        let errors = verify_module(&module_with(func));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("used after being dropped"));
    }

    #[test]
    fn rewriting_an_out_parameter_after_dropping_it_is_accepted() {
        // Mirrors backend test
        // `spsc_codegen::final_endpoint_drains_droppable_payloads_exactly_once`:
        // `Rvalue::SpscTrySend`'s `out_unsent` is a documented write-only
        // destination (its previous contents are irrelevant; the call always
        // overwrites it), so dropping it and then calling `SpscTrySend`
        // again with it as `out_unsent` is a legitimate reinitialization, not
        // a use-after-drop. Same shape for every other `out_*` field (see
        // `rvalue_write_only_targets`).
        let elem = Type::Own(Box::new(Type::I32));
        let func = simple_function(
            vec![MirBlock {
                insts: vec![
                    MirInst::Drop(LocalId(2)),
                    MirInst::Assign {
                        local: LocalId(3),
                        value: Rvalue::SpscTrySend {
                            sender: LocalId(0),
                            value: LocalId(1),
                            out_unsent: LocalId(2),
                            elem_type: elem.clone(),
                        },
                    },
                    MirInst::Return(None),
                ],
            }],
            vec![
                Local {
                    name: None,
                    ty: Some(Type::spsc_sender(elem.clone())),
                    mutable: false,
                    skip_drop: false,
                },
                Local {
                    name: None,
                    ty: Some(elem.clone()),
                    mutable: false,
                    skip_drop: false,
                },
                Local {
                    name: None,
                    ty: Some(elem),
                    mutable: false,
                    skip_drop: false,
                },
                Local {
                    name: None,
                    ty: Some(Type::I32),
                    mutable: false,
                    skip_drop: false,
                },
            ],
        );
        assert!(verify_module(&module_with(func)).is_empty());
    }

    #[test]
    fn double_drop_of_the_same_local_in_one_block_is_accepted() {
        // Mirrors backend test
        // `mutex_codegen::duplicate_guard_drop_is_idempotent_and_unlocks_only_once`:
        // MutexGuard drop glue is idempotent by design, so a second `Drop` of
        // the same local without an intervening reassignment must not be
        // flagged (see the module doc comment's "deliberately NOT checked"
        // section).
        let func = simple_function(
            vec![MirBlock {
                insts: vec![
                    MirInst::Drop(LocalId(0)),
                    MirInst::Drop(LocalId(0)),
                    MirInst::Return(None),
                ],
            }],
            vec![int_local()],
        );
        assert!(verify_module(&module_with(func)).is_empty());
    }

    #[test]
    fn reassignment_after_drop_clears_the_drop_and_is_accepted() {
        let func = simple_function(
            vec![MirBlock {
                insts: vec![
                    MirInst::Drop(LocalId(0)),
                    MirInst::Assign {
                        local: LocalId(0),
                        value: Rvalue::ConstInt(42),
                    },
                    MirInst::Assign {
                        local: LocalId(1),
                        value: Rvalue::Move(LocalId(0)),
                    },
                    MirInst::Return(None),
                ],
            }],
            vec![int_local(), int_local()],
        );
        assert!(verify_module(&module_with(func)).is_empty());
    }

    #[test]
    fn drop_of_scoped_thread_handle_kinds_is_not_treated_as_a_value_drop() {
        // DrainThreadScope legitimately runs more than once on a live scope
        // (see the module docs); only the generic `Drop` triggers same-block
        // tracking.
        let func = simple_function(
            vec![MirBlock {
                insts: vec![
                    MirInst::DrainThreadScope(LocalId(0)),
                    MirInst::DrainThreadScope(LocalId(0)),
                    MirInst::Return(None),
                ],
            }],
            vec![int_local()],
        );
        assert!(verify_module(&module_with(func)).is_empty());
    }

    #[test]
    fn extern_function_with_param_type_param_is_rejected() {
        let module = MirModule {
            struct_types: HashMap::new(),
            enum_types: HashMap::new(),
            functions: Vec::new(),
            extern_functions: vec![MirExternFunction {
                name: "bad_extern".into(),
                ret_type: Some(Type::Void),
                params: vec![Type::Param("T".into())],
                abi: Some("C".into()),
                link_name: None,
            }],
        };
        let errors = verify_module(&module);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].function.contains("bad_extern"));
    }

    #[test]
    fn struct_field_type_param_is_rejected() {
        let mut module = MirModule::default();
        module.struct_types.insert(
            "Bad".into(),
            StructType {
                name: "Bad".into(),
                fields: vec![("x".into(), Type::Param("T".into()))],
            },
        );
        let errors = verify_module(&module);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("field `x`"));
    }

    #[test]
    fn atomic_scalar_type_is_unaffected_by_type_checks() {
        // Sanity: Type::Atomic must not be misclassified as an App/Param
        // issue by check_type's fallthrough arm.
        let mut local = int_local();
        local.ty = Some(Type::Atomic(AtomicScalar::I32));
        let func = simple_function(
            vec![MirBlock {
                insts: vec![MirInst::Return(None)],
            }],
            vec![local],
        );
        assert!(verify_module(&module_with(func)).is_empty());
    }

    #[test]
    fn params_out_of_range_is_rejected() {
        let func = MirFunction {
            name: "f".into(),
            ret_type: None,
            params: vec![LocalId(3)],
            locals: vec![int_local()],
            blocks: vec![MirBlock {
                insts: vec![MirInst::Return(None)],
            }],
        };
        let errors = verify_module(&module_with(func));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("out-of-range local"));
    }
}
