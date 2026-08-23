use glyph_core::ast::{Block, CaptureMode, Expr, Function, Ident, Param, Stmt};
use glyph_core::diag::Severity;
use glyph_core::mir::{
    BlockId, BorrowCaptureSource, BorrowKind, CaptureTransfer, LocalId, MirBorrowCapture,
    MirCapture, MirFunction, MirInst, MirValue, Rvalue,
};
use glyph_core::span::Span;
use glyph_core::types::{BorrowedCallableKind, Mutability, Type};

use crate::resolver::ResolverContext;
use crate::{CaptureAccess, CaptureOwnership, analyze_function_closure_ownership};

use super::context::{LocalState, LowerCtx};
use super::expr::{lower_expr, lower_expr_with_expected, lower_value, lower_value_with_expected};
use super::types::{resolve_type_name, type_expr_to_string, vec_elem_type_from_type};
use super::value::{
    coerce_to_bool, expr_span, infer_value_type, local_struct_name, rvalue_from_value,
    update_local_type_from_rvalue,
};

fn is_void_type(ty: &Type) -> bool {
    matches!(ty, Type::Void) || matches!(ty, Type::Tuple(elem_types) if elem_types.is_empty())
}

fn type_contains_borrow(
    ty: &Type,
    resolver: &ResolverContext,
    visiting: &mut std::collections::HashSet<String>,
) -> bool {
    match ty {
        Type::Ref(_, _) | Type::BorrowedFunction { .. } => true,
        Type::Array(inner, _) | Type::Own(inner) | Type::RawPtr(inner) | Type::Shared(inner) => {
            type_contains_borrow(inner, resolver, visiting)
        }
        Type::App { args, .. } | Type::Tuple(args) => args
            .iter()
            .any(|arg| type_contains_borrow(arg, resolver, visiting)),
        Type::Function { params, ret } => {
            params
                .iter()
                .any(|param| type_contains_borrow(param, resolver, visiting))
                || type_contains_borrow(ret, resolver, visiting)
        }
        Type::Named(name) => {
            if !visiting.insert(name.clone()) {
                return false;
            }
            let borrowed = resolver.struct_types.get(name).is_some_and(|layout| {
                layout
                    .fields
                    .iter()
                    .any(|(_, field)| type_contains_borrow(field, resolver, visiting))
            });
            visiting.remove(name);
            borrowed
        }
        Type::Enum(name) => {
            if !visiting.insert(name.clone()) {
                return false;
            }
            let borrowed = resolver.enum_types.get(name).is_some_and(|layout| {
                layout.variants.iter().any(|variant| {
                    variant
                        .payload
                        .as_ref()
                        .is_some_and(|payload| type_contains_borrow(payload, resolver, visiting))
                })
            });
            visiting.remove(name);
            borrowed
        }
        _ => false,
    }
}

/// Lowering failures must never be silent: if an expression failed to lower
/// and nothing was reported along the way, emit a diagnostic instead of
/// letting the statement quietly become a Nop (the pattern that let float
/// and char literals miscompile silently for a long time).
fn ensure_lowering_reported(
    ctx: &mut LowerCtx<'_>,
    diags_before: usize,
    what: &str,
    span: Option<Span>,
) {
    if ctx.diagnostics.len() == diags_before {
        ctx.error(
            format!(
                "{} could not be lowered to MIR; this expression form is not supported here",
                what
            ),
            span,
        );
    }
}

fn is_void_value(ctx: &LowerCtx<'_>, value: &MirValue) -> bool {
    match value {
        MirValue::Unit => true,
        MirValue::Local(local) => ctx
            .locals
            .get(local.0 as usize)
            .and_then(|l| l.ty.as_ref())
            .map(is_void_type)
            .unwrap_or(false),
        _ => false,
    }
}

fn imports_sys_argv(resolver: &ResolverContext) -> bool {
    resolver
        .import_scope
        .as_ref()
        .and_then(|scope| scope.direct_symbols.get("argv"))
        .map(|(module, name)| module == "std/sys" && name == "argv")
        .unwrap_or(false)
}

pub(crate) fn lower_function(
    func: &Function,
    module: &glyph_core::ast::Module,
    resolver: &ResolverContext,
    fn_sigs: &std::collections::HashMap<String, super::signatures::FnSig>,
) -> (
    MirFunction,
    Vec<MirFunction>,
    Vec<glyph_core::diag::Diagnostic>,
) {
    // Resolve return type
    let ret_type = func
        .ret_type
        .as_ref()
        .and_then(|t| crate::resolver::resolve_type_expr_to_type(t, resolver));

    let mut ctx = LowerCtx::new(resolver, module, fn_sigs, func.name.0.clone());
    ctx.fn_ret_type = ret_type.clone();
    if let Some(ret) = ret_type.as_ref()
        && type_contains_borrow(ret, resolver, &mut std::collections::HashSet::new())
    {
        ctx.error(
            if matches!(ret, Type::BorrowedFunction { .. }) {
                "a borrowed callable cannot escape through a function return; use owned FnOnce"
            } else {
                "borrowed references cannot escape through return; return owned data instead"
            },
            func.ret_type.as_ref().map(|ret| ret.span()),
        );
    }
    if ret_type
        .as_ref()
        .is_some_and(LowerCtx::type_contains_mutex_guard)
    {
        ctx.error(
            "MutexGuard cannot escape through a function return type",
            func.ret_type.as_ref().map(|ret| ret.span()),
        );
    }
    let closure_analysis = analyze_function_closure_ownership(func, resolver);
    let closure_analysis_failed = closure_analysis
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error);
    ctx.closure_infos = closure_analysis.closures;
    ctx.diagnostics.extend(closure_analysis.diagnostics);

    // Create locals for parameters and bind them
    let mut param_locals = Vec::new();
    for param in &func.params {
        let local = ctx.fresh_local(Some(&param.name.0));
        // Try to infer parameter type from annotation
        if let Some(ty) = param
            .ty
            .as_ref()
            .and_then(|t| crate::resolver::resolve_type_expr_to_type(t, resolver))
        {
            if ty == glyph_core::thread::canonical_thread_scope_type() {
                ctx.register_scoped_callback_scope(local);
            }
            ctx.locals[local.0 as usize].ty = Some(ty);
        }
        if let Some(state) = ctx.local_states.get_mut(local.0 as usize) {
            *state = LocalState::Initialized;
        }
        ctx.bind_name(&param.name.0, local);
        ctx.register_source_binding(&param.name.0, param.span, local);
        param_locals.push(local);
    }

    // Ownership/cycle/escape errors make closure conversion unsound. Keep a
    // skeletal function for diagnostic tooling, but do not emit MakeClosure
    // or any lifted body from a function whose analysis failed.
    if closure_analysis_failed {
        ctx.push_inst(MirInst::Return(None));
        let lowered = MirFunction {
            name: func.name.0.clone(),
            ret_type,
            params: param_locals,
            locals: ctx.locals,
            blocks: ctx.blocks,
        };
        return (lowered, Vec::new(), ctx.diagnostics);
    }

    if imports_sys_argv(resolver) && !ctx.bindings.contains_key("argv") {
        let argv_local = ctx.fresh_local(Some("argv"));
        ctx.locals[argv_local.0 as usize].ty = Some(Type::App {
            base: "Vec".into(),
            args: vec![Type::String],
        });
        if let Some(state) = ctx.local_states.get_mut(argv_local.0 as usize) {
            *state = LocalState::Initialized;
        }
        ctx.locals[argv_local.0 as usize].skip_drop = true;
        ctx.bind_name("argv", argv_local);
        ctx.push_inst(MirInst::Assign {
            local: argv_local,
            value: Rvalue::Call {
                name: "argv".into(),
                args: Vec::new(),
            },
        });
    }

    let mut implicit_return =
        lower_block_with_expected(&mut ctx, &func.body, ret_type.as_ref(), false, true);
    let returns_void = ret_type.as_ref().map_or(true, |ty| is_void_type(ty));
    if returns_void {
        // A void function discards whatever its last expression produced
        // (e.g. a Vec snapshot from a trailing `.push()`); the value's local
        // stays live and is dropped with the rest of the scope. Returning it
        // emitted `ret <aggregate>` in a void LLVM function.
        implicit_return = None;
    }
    if !ctx.terminated() {
        // If block returned a value, use it; otherwise return void
        ctx.drop_all_active_locals();
        ctx.push_inst(MirInst::Return(implicit_return));
    }

    let lowered = MirFunction {
        name: func.name.0.clone(),
        ret_type,
        params: param_locals,
        locals: ctx.locals,
        blocks: ctx.blocks,
    };

    let closure_order: std::collections::HashMap<_, _> = ctx
        .closure_infos
        .iter()
        .map(|info| {
            (
                format!(
                    "{}::__glyph_closure_{}",
                    ctx.closure_name_root, info.closure_id
                ),
                info.closure_id,
            )
        })
        .collect();
    ctx.lifted_functions.sort_by_key(|function| {
        closure_order
            .get(&function.name)
            .copied()
            .unwrap_or(u32::MAX)
    });

    (lowered, ctx.lifted_functions, ctx.diagnostics)
}

fn type_has_concrete_layout(ty: &Type) -> bool {
    match ty {
        Type::Param(_) => false,
        Type::App { args, .. }
        | Type::Tuple(args)
        | Type::Function { params: args, .. }
        | Type::BorrowedFunction { params: args, .. } => {
            args.iter().all(type_has_concrete_layout)
                && match ty {
                    Type::Function { ret, .. } | Type::BorrowedFunction { ret, .. } => {
                        type_has_concrete_layout(ret)
                    }
                    _ => true,
                }
        }
        Type::Ref(inner, _)
        | Type::Array(inner, _)
        | Type::Own(inner)
        | Type::RawPtr(inner)
        | Type::Shared(inner) => type_has_concrete_layout(inner),
        _ => true,
    }
}

fn mir_value_type(ctx: &LowerCtx<'_>, value: Option<&MirValue>) -> Type {
    match value {
        None | Some(MirValue::Unit) => Type::Void,
        Some(MirValue::Int(_)) => Type::I32,
        Some(MirValue::Float(_)) => Type::F64,
        Some(MirValue::Bool(_)) => Type::Bool,
        Some(MirValue::Local(local)) => ctx.local_ty(*local).cloned().unwrap_or(Type::Void),
    }
}

fn closure_return_types(ctx: &LowerCtx<'_>) -> Vec<Type> {
    ctx.blocks
        .iter()
        .flat_map(|block| block.insts.iter())
        .filter_map(|inst| match inst {
            MirInst::Return(value) => Some(mir_value_type(ctx, value.as_ref())),
            _ => None,
        })
        .collect()
}

/// Convert one source closure into an owned or lexically borrowed callable
/// plus a collision-safe lifted MIR function. Capture locals are looked up by
/// declaration identity, not just spelling, so shadowed bindings remain
/// distinct.
pub(crate) fn lower_closure_rvalue<'a>(
    ctx: &mut LowerCtx<'a>,
    capture_mode: CaptureMode,
    params: &'a [Param],
    body: &'a Expr,
    span: Span,
    expected: Option<&Type>,
) -> Option<Rvalue> {
    lower_closure_rvalue_impl(ctx, capture_mode, params, body, span, expected, None)
}

/// Lower a compiler-intrinsic borrowed callback whose parameter types are
/// fixed by the intrinsic while its result type remains inferred from the
/// closure body.
pub(crate) fn lower_inferred_borrowed_closure_rvalue<'a>(
    ctx: &mut LowerCtx<'a>,
    capture_mode: CaptureMode,
    params: &'a [Param],
    body: &'a Expr,
    span: Span,
    kind: BorrowedCallableKind,
    expected_params: Vec<Type>,
    scoped_callback: bool,
) -> Option<Rvalue> {
    lower_closure_rvalue_impl(
        ctx,
        capture_mode,
        params,
        body,
        span,
        None,
        Some((kind, expected_params, scoped_callback)),
    )
}

fn lower_closure_rvalue_impl<'a>(
    ctx: &mut LowerCtx<'a>,
    capture_mode: CaptureMode,
    params: &'a [Param],
    body: &'a Expr,
    span: Span,
    expected: Option<&Type>,
    forced_borrowed: Option<(BorrowedCallableKind, Vec<Type>, bool)>,
) -> Option<Rvalue> {
    let Some(info) = ctx
        .closure_infos
        .iter()
        .find(|info| info.span == span)
        .cloned()
    else {
        ctx.error(
            "closure ownership analysis did not produce metadata for this expression",
            Some(span),
        );
        return None;
    };

    if info.capture_mode != capture_mode {
        ctx.error(
            "closure capture mode changed between analysis and MIR lowering",
            Some(span),
        );
        return None;
    }

    let borrowed_kind =
        forced_borrowed
            .as_ref()
            .map(|(kind, _, _)| *kind)
            .or_else(|| match expected {
                Some(Type::BorrowedFunction { kind, .. }) => Some(*kind),
                _ => None,
            });
    if borrowed_kind.is_some() && capture_mode == CaptureMode::Move {
        ctx.error(
            "a move closure is an owned FnOnce and cannot be used as a borrowed Fn/FnMut callback",
            Some(span),
        );
        return None;
    }

    let expected_signature = if let Some((_, params, _)) = &forced_borrowed {
        Some((params.clone(), None))
    } else {
        match expected {
            Some(Type::Function { params, ret })
            | Some(Type::BorrowedFunction { params, ret, .. }) => {
                Some((params.clone(), Some(ret.as_ref().clone())))
            }
            Some(other) => {
                ctx.error(
                    format!(
                        "closure has a callable type, but '{}' is required here",
                        LowerCtx::type_label(other)
                    ),
                    Some(span),
                );
                return None;
            }
            None => None,
        }
    };

    if let Some((expected_params, _)) = &expected_signature {
        if expected_params.len() != params.len() {
            ctx.error(
                format!(
                    "closure expects {} parameters from its contextual callable type, but declares {}",
                    expected_params.len(),
                    params.len()
                ),
                Some(span),
            );
            return None;
        }
    }

    let mut callable_params = Vec::with_capacity(params.len());
    for (index, param) in params.iter().enumerate() {
        let annotated = param
            .ty
            .as_ref()
            .and_then(|ty| crate::resolver::resolve_type_expr_to_type(ty, ctx.resolver));
        let contextual = expected_signature
            .as_ref()
            .and_then(|(types, _)| types.get(index))
            .cloned();
        if let (Some(annotated), Some(contextual)) = (&annotated, &contextual) {
            if !super::call::call_types_compatible(annotated, contextual) {
                ctx.error(
                    format!(
                        "closure parameter '{}' has type '{}', but its contextual callable type requires '{}'",
                        param.name.0,
                        LowerCtx::type_label(annotated),
                        LowerCtx::type_label(contextual)
                    ),
                    Some(param.span),
                );
                return None;
            }
        }
        let Some(param_ty) = annotated.or(contextual) else {
            ctx.error(
                format!(
                    "cannot infer type of closure parameter '{}'; add a type annotation or a callable context",
                    param.name.0
                ),
                Some(param.span),
            );
            return None;
        };
        if !type_has_concrete_layout(&param_ty) {
            ctx.error(
                format!(
                    "closure parameter '{}' has unknown runtime layout '{}'",
                    param.name.0,
                    LowerCtx::type_label(&param_ty)
                ),
                Some(param.span),
            );
            return None;
        }
        callable_params.push(param_ty);
    }

    let lifted_name = format!(
        "{}::__glyph_closure_{}",
        ctx.closure_name_root, info.closure_id
    );
    let mut lifted = LowerCtx::new(ctx.resolver, ctx.module, ctx.fn_sigs, lifted_name.clone());
    lifted.closure_name_root = ctx.closure_name_root.clone();
    lifted.closure_infos = ctx.closure_infos.clone();

    let mut mir_captures = Vec::with_capacity(info.captures.len());
    let mut mir_borrow_captures = Vec::with_capacity(info.captures.len());
    let mut lifted_params = Vec::with_capacity(info.captures.len() + params.len());
    for capture in &info.captures {
        let Some(ty) = capture.resolved_type.clone() else {
            ctx.error(
                format!(
                    "cannot lower capture '{}' because its runtime layout is unknown; add a type annotation",
                    capture.name
                ),
                Some(capture.first_use_span),
            );
            return None;
        };
        if !type_has_concrete_layout(&ty) {
            ctx.error(
                format!(
                    "cannot lower capture '{}' with unknown runtime layout '{}'",
                    capture.name,
                    LowerCtx::type_label(&ty)
                ),
                Some(capture.first_use_span),
            );
            return None;
        }
        let Some(source_local) = ctx.source_binding_local(&capture.name, capture.declaration_span)
        else {
            ctx.error(
                format!(
                    "could not map closure capture '{}' to its source declaration",
                    capture.name
                ),
                Some(capture.declaration_span),
            );
            return None;
        };
        if borrowed_kind.is_none()
            && !ctx.check_local_available(source_local, Some(capture.first_use_span))
        {
            return None;
        }

        let lifted_capture_ty = if let Some(kind) = borrowed_kind {
            if capture.access == CaptureAccess::Move {
                ctx.error(
                    format!(
                        "borrowed {} closure cannot move capture `{}`; use an owned FnOnce or borrow the value in the operation",
                        match kind {
                            BorrowedCallableKind::Fn => "Fn",
                            BorrowedCallableKind::FnMut => "FnMut",
                        },
                        capture.name
                    ),
                    Some(capture.first_use_span),
                );
                return None;
            }
            if kind == BorrowedCallableKind::Fn && capture.access == CaptureAccess::Mutate {
                ctx.error(
                    format!(
                        "Fn closure cannot mutate capture `{}`; use FnMut",
                        capture.name
                    ),
                    Some(capture.first_use_span),
                );
                return None;
            }
            let borrow = if capture.access == CaptureAccess::Mutate {
                BorrowKind::Mutable
            } else {
                BorrowKind::Shared
            };
            if !ctx.validate_new_lexical_borrow(source_local, borrow, capture.first_use_span)
                || !ctx.check_local_available(source_local, Some(capture.first_use_span))
            {
                return None;
            }
            if borrow == BorrowKind::Mutable
                && !ctx
                    .locals
                    .get(source_local.0 as usize)
                    .is_some_and(|local| local.mutable)
            {
                ctx.error(
                    format!(
                        "FnMut closure cannot mutably capture immutable variable `{}`; declare it with `let mut`",
                        capture.name
                    ),
                    Some(capture.first_use_span),
                );
                return None;
            }
            mir_borrow_captures.push(MirBorrowCapture {
                name: capture.name.clone(),
                local: source_local,
                ty: ty.clone(),
                borrow,
                source: match ctx.local_ty(source_local) {
                    Some(Type::Ref(inner, mutability)) if inner.as_ref() == &ty => {
                        if borrow == BorrowKind::Mutable && *mutability != Mutability::Mutable {
                            ctx.error(
                                format!(
                                    "cannot mutably reborrow shared capture `{}` for an FnMut closure",
                                    capture.name
                                ),
                                Some(capture.first_use_span),
                            );
                            return None;
                        }
                        BorrowCaptureSource::Reborrow
                    }
                    _ => BorrowCaptureSource::Local,
                },
            });
            Type::Ref(
                Box::new(ty.clone()),
                if borrow == BorrowKind::Mutable {
                    Mutability::Mutable
                } else {
                    Mutability::Immutable
                },
            )
        } else {
            mir_captures.push(MirCapture {
                name: capture.name.clone(),
                local: source_local,
                ty: ty.clone(),
                transfer: match capture.ownership {
                    CaptureOwnership::Copy => CaptureTransfer::Copy,
                    CaptureOwnership::Move => CaptureTransfer::Move,
                },
            });
            ty.clone()
        };

        let local = lifted.fresh_local(Some(&capture.name));
        lifted.locals[local.0 as usize].ty = Some(lifted_capture_ty);
        lifted.locals[local.0 as usize].mutable = capture.access == CaptureAccess::Mutate;
        if borrowed_kind.is_some() {
            lifted.mark_implicit_deref(local);
        }
        lifted.local_states[local.0 as usize] = LocalState::Initialized;
        lifted.bind_name(&capture.name, local);
        lifted.register_source_binding(&capture.name, capture.declaration_span, local);
        lifted_params.push(local);
    }

    for (param, ty) in params.iter().zip(&callable_params) {
        let local = lifted.fresh_local(Some(&param.name.0));
        lifted.locals[local.0 as usize].ty = Some(ty.clone());
        lifted.local_states[local.0 as usize] = LocalState::Initialized;
        lifted.bind_name(&param.name.0, local);
        lifted.register_source_binding(&param.name.0, param.span, local);
        lifted_params.push(local);
        if ty == &glyph_core::thread::canonical_thread_scope_type() {
            lifted.register_scoped_callback_scope(local);
        }
    }

    let expected_ret = expected_signature.as_ref().and_then(|(_, ret)| ret.clone());
    lifted.fn_ret_type = expected_ret.clone();
    let mut implicit_return = match body {
        Expr::Block(block) => {
            lower_block_with_expected(&mut lifted, block, expected_ret.as_ref(), true, true)
        }
        _ => lower_value_with_expected(&mut lifted, body, expected_ret.as_ref()),
    };
    if expected_ret.as_ref().is_some_and(is_void_type)
        && implicit_return
            .as_ref()
            .is_some_and(|value| is_void_value(&lifted, value))
    {
        implicit_return = None;
    }
    if !lifted.terminated() {
        if let Some(MirValue::Local(local)) = implicit_return.as_ref() {
            lifted.local_states[local.0 as usize] = LocalState::Moved;
        }
        lifted.drop_all_active_locals();
        lifted.push_inst(MirInst::Return(implicit_return));
    }

    let return_types = closure_return_types(&lifted);
    let inferred_ret = return_types.first().cloned().unwrap_or(Type::Void);
    for actual in &return_types {
        let required = expected_ret.as_ref().unwrap_or(&inferred_ret);
        if !super::call::call_types_compatible(actual, required) {
            lifted.error(
                format!(
                    "closure return has type '{}', expected '{}'",
                    LowerCtx::type_label(actual),
                    LowerCtx::type_label(required)
                ),
                Some(span),
            );
        }
    }
    let ret = expected_ret.unwrap_or(inferred_ret);
    let signature = match borrowed_kind {
        Some(kind) => Type::BorrowedFunction {
            kind,
            params: callable_params,
            ret: Box::new(ret.clone()),
        },
        None => Type::Function {
            params: callable_params,
            ret: Box::new(ret.clone()),
        },
    };
    let lifted_function = MirFunction {
        name: lifted_name.clone(),
        ret_type: (!is_void_type(&ret)).then_some(ret),
        params: lifted_params,
        locals: lifted.locals,
        blocks: lifted.blocks,
    };

    ctx.diagnostics.extend(lifted.diagnostics);
    ctx.lifted_functions.extend(lifted.lifted_functions);
    ctx.lifted_functions.push(lifted_function);

    Some(if borrowed_kind.is_some() {
        Rvalue::MakeBorrowedClosure {
            function: lifted_name,
            signature,
            captures: mir_borrow_captures,
        }
    } else {
        Rvalue::MakeClosure {
            function: lifted_name,
            signature,
            captures: mir_captures,
        }
    })
}

pub(crate) fn lower_block<'a>(ctx: &mut LowerCtx<'a>, block: &'a Block) -> Option<MirValue> {
    lower_block_with_expected(ctx, block, None, false, false)
}

pub(crate) fn lower_block_with_expected<'a>(
    ctx: &mut LowerCtx<'a>,
    block: &'a Block,
    expected: Option<&Type>,
    control_value_context: bool,
    move_returned_local: bool,
) -> Option<MirValue> {
    ctx.enter_scope();
    let mut last_value = None;

    for (i, stmt) in block.stmts.iter().enumerate() {
        let is_last = i == block.stmts.len() - 1;

        match stmt {
            Stmt::Let {
                name,
                mutable,
                ty,
                value,
                span,
            } => {
                let local = ctx.fresh_local(Some(&name.0));
                ctx.bind_name(&name.0, local);
                ctx.register_source_binding(&name.0, *span, local);

                // Set mutability
                ctx.locals[local.0 as usize].mutable = *mutable;

                if let Some(annot_ty) = ty
                    .as_ref()
                    .and_then(|t| resolve_type_name(&type_expr_to_string(t), ctx.resolver))
                {
                    ctx.locals[local.0 as usize].ty = Some(annot_ty);
                }

                let expected = ctx.locals[local.0 as usize].ty.clone();
                let diags_before = ctx.diagnostics.len();
                if let Some(mut rv) = value
                    .as_ref()
                    .and_then(|e| lower_expr_with_expected(ctx, e, expected.as_ref()))
                {
                    update_local_type_from_rvalue(ctx, local, &mut rv);
                    ctx.push_inst(MirInst::Assign { local, value: rv });
                } else {
                    if let Some(init) = value.as_ref() {
                        ensure_lowering_reported(
                            ctx,
                            diags_before,
                            "let initializer",
                            expr_span(init),
                        );
                    }
                    ctx.push_inst(MirInst::Nop);
                }
            }
            Stmt::Ret(expr, ret_span) => {
                let expected = ctx.fn_ret_type.clone();
                let diags_before = ctx.diagnostics.len();
                let value = expr
                    .as_ref()
                    .and_then(|e| lower_value_with_expected(ctx, e, expected.as_ref()));
                if value.is_none() {
                    if let Some(ret_expr) = expr.as_ref() {
                        ensure_lowering_reported(
                            ctx,
                            diags_before,
                            "return value",
                            expr_span(ret_expr).or(Some(*ret_span)),
                        );
                    }
                }
                ctx.reject_mutex_guard_return(value.as_ref(), *ret_span);
                ctx.reject_lexical_loan_return(value.as_ref(), *ret_span);
                if let Some(MirValue::Local(local)) = value.as_ref() {
                    if let Some(state) = ctx.local_states.get_mut(local.0 as usize) {
                        *state = LocalState::Moved;
                    }
                }
                ctx.drop_all_active_locals();
                ctx.push_inst(MirInst::Return(value));
            }
            Stmt::Break(span) => {
                if let Some(loop_ctx) = ctx.current_loop().cloned() {
                    ctx.drop_scopes_after_depth(loop_ctx.scope_depth);
                    let exit = loop_ctx.exit;
                    ctx.push_inst(MirInst::Goto(exit));
                } else {
                    ctx.error("break statement outside of loop", Some(*span));
                    ctx.push_inst(MirInst::Nop);
                }
            }
            Stmt::Continue(span) => {
                if let Some(loop_ctx) = ctx.current_loop().cloned() {
                    ctx.drop_scopes_after_depth(loop_ctx.scope_depth);
                    ctx.push_inst(MirInst::Goto(loop_ctx.continue_target));
                } else {
                    ctx.error("continue statement outside of loop", Some(*span));
                    ctx.push_inst(MirInst::Nop);
                }
            }
            Stmt::Expr(expr, _) => {
                if is_last {
                    // Last expression is the implicit return value
                    match expr {
                        Expr::If {
                            cond,
                            then_block,
                            else_block,
                            ..
                        } => {
                            if control_value_context {
                                last_value = lower_if_value(
                                    ctx,
                                    cond,
                                    then_block,
                                    else_block.as_ref(),
                                    expected,
                                );
                            } else {
                                lower_if(ctx, cond, then_block, else_block.as_ref());
                            }
                        }
                        Expr::While { cond, body, span } => {
                            lower_while(ctx, cond, body);
                            if control_value_context {
                                if !matches!(expected, Some(Type::Void)) {
                                    ctx.error("while expression produces unit", Some(*span));
                                }
                                last_value = Some(MirValue::Unit);
                            }
                        }
                        Expr::For {
                            var,
                            start,
                            end,
                            body,
                            span,
                        } => {
                            lower_for(ctx, var, start, end, body, *span);
                            if control_value_context {
                                if !matches!(expected, Some(Type::Void)) {
                                    ctx.error("for expression produces unit", Some(*span));
                                }
                                last_value = Some(MirValue::Unit);
                            }
                        }
                        Expr::ForIn {
                            var,
                            iter,
                            body,
                            span,
                        } => {
                            lower_for_in(ctx, var, iter, body, *span);
                            if control_value_context {
                                if !matches!(expected, Some(Type::Void)) {
                                    ctx.error("for-in expression produces unit", Some(*span));
                                }
                                last_value = Some(MirValue::Unit);
                            }
                        }
                        _ => {
                            last_value = lower_value_with_expected(ctx, expr, expected);
                        }
                    }
                } else {
                    match expr {
                        Expr::If {
                            cond,
                            then_block,
                            else_block,
                            ..
                        } => lower_if(ctx, cond, then_block, else_block.as_ref()),
                        Expr::While { cond, body, .. } => lower_while(ctx, cond, body),
                        Expr::For {
                            var,
                            start,
                            end,
                            body,
                            span,
                        } => lower_for(ctx, var, start, end, body, *span),
                        Expr::ForIn {
                            var,
                            iter,
                            body,
                            span,
                        } => lower_for_in(ctx, var, iter, body, *span),
                        Expr::Block(block) => {
                            let _ = lower_block_with_expected(ctx, block, None, false, false);
                            ctx.push_inst(MirInst::Nop);
                        }
                        _ => {
                            let diags_before = ctx.diagnostics.len();
                            if lower_expr(ctx, expr).is_none() {
                                ensure_lowering_reported(
                                    ctx,
                                    diags_before,
                                    "expression statement",
                                    expr_span(expr),
                                );
                            }
                            ctx.push_inst(MirInst::Nop);
                        }
                    }
                }
            }
            Stmt::Assign {
                target,
                value,
                span,
            } => {
                let target_diags_before = ctx.diagnostics.len();
                if let Some(assign_target) = lower_assignment_target(ctx, target, *span) {
                    match assign_target {
                        AssignmentTarget::Local(local) => {
                            let expected = ctx.locals[local.0 as usize].ty.clone();
                            let diags_before = ctx.diagnostics.len();
                            if let Some(mut rv) =
                                lower_expr_with_expected(ctx, value, expected.as_ref())
                            {
                                update_local_type_from_rvalue(ctx, local, &mut rv);
                                ctx.push_inst(MirInst::Assign { local, value: rv });
                            } else {
                                ensure_lowering_reported(
                                    ctx,
                                    diags_before,
                                    "assignment value",
                                    expr_span(value).or(Some(*span)),
                                );
                                ctx.push_inst(MirInst::Nop);
                            }
                        }
                        AssignmentTarget::Field {
                            base,
                            field_name,
                            field_index,
                            field_type,
                        } => {
                            let diags_before = ctx.diagnostics.len();
                            if let Some(rv) =
                                lower_expr_with_expected(ctx, value, Some(&field_type))
                            {
                                ctx.push_inst(MirInst::AssignField {
                                    base,
                                    field_name,
                                    field_index,
                                    value: rv,
                                });
                            } else {
                                ensure_lowering_reported(
                                    ctx,
                                    diags_before,
                                    "assignment value",
                                    expr_span(value).or(Some(*span)),
                                );
                                ctx.push_inst(MirInst::Nop);
                            }
                        }
                        AssignmentTarget::Index {
                            base,
                            index,
                            elem_type,
                        } => {
                            let diags_before = ctx.diagnostics.len();
                            if let Some(rv) = lower_expr_with_expected(ctx, value, Some(&elem_type))
                            {
                                ctx.push_inst(MirInst::AssignIndex {
                                    base,
                                    index,
                                    value: rv,
                                });
                            } else {
                                ensure_lowering_reported(
                                    ctx,
                                    diags_before,
                                    "assignment value",
                                    expr_span(value).or(Some(*span)),
                                );
                                ctx.push_inst(MirInst::Nop);
                            }
                        }
                    }
                } else {
                    ensure_lowering_reported(
                        ctx,
                        target_diags_before,
                        "assignment target",
                        Some(*span),
                    );
                    ctx.push_inst(MirInst::Nop);
                }
            }
        }

        if ctx.terminated() {
            break;
        }
    }

    if move_returned_local {
        if let Some(MirValue::Local(local)) = last_value {
            ctx.reject_arc_loan_scope_escape(local);
            ctx.reject_mutex_guard_return(Some(&MirValue::Local(local)), block.span);
            ctx.reject_lexical_loan_return(Some(&MirValue::Local(local)), block.span);
            if let Some(state) = ctx.local_states.get_mut(local.0 as usize) {
                *state = LocalState::Moved;
            }
        }
    }

    let result = last_value;
    ctx.exit_scope();
    result
}

fn merge_local_states(merged: &mut [LocalState], current: &[LocalState]) {
    for (merged_state, current_state) in merged.iter_mut().zip(current.iter()) {
        *merged_state = match (*merged_state, *current_state) {
            (LocalState::Moved, _) | (_, LocalState::Moved) => LocalState::Moved,
            (LocalState::Uninitialized, _) | (_, LocalState::Uninitialized) => {
                LocalState::Uninitialized
            }
            _ => LocalState::Initialized,
        };
    }
}

fn restore_local_states(ctx: &mut LowerCtx<'_>, states: &[LocalState], len: usize) {
    for i in 0..len {
        ctx.local_states[i] = states[i];
    }
}

pub(crate) fn lower_if<'a>(
    ctx: &mut LowerCtx<'a>,
    cond: &'a Expr,
    then_blk: &'a Block,
    else_blk: Option<&'a Block>,
) {
    let header = ctx.current;
    let then_id = ctx.new_block();
    let else_id = else_blk.map(|_| ctx.new_block());
    let join_id = ctx.new_block();

    // Lower short-circuiting conditions directly into control flow.
    // Using `lower_value()` here is incorrect because `&&`/`||` lowerings emit
    // their own `If`/`Goto` terminators.
    ctx.switch_to(header);
    lower_cond_branch(ctx, cond, then_id, else_id.unwrap_or(join_id));

    let base_len = ctx.local_states.len();
    let base_states = ctx.local_states.clone();
    let mut merged_states = base_states.clone();
    let mut any_reaches_join = false;

    ctx.switch_to(then_id);
    let _ = lower_block(ctx, then_blk);
    let then_terminated = ctx.terminated();
    if !then_terminated {
        ctx.push_inst(MirInst::Goto(join_id));
    }
    if !then_terminated {
        any_reaches_join = true;
        merge_local_states(
            &mut merged_states[..base_len],
            &ctx.local_states[..base_len],
        );
    }

    if let Some(else_block) = else_blk {
        let else_id = else_id.unwrap();
        restore_local_states(ctx, &base_states, base_len);
        ctx.switch_to(else_id);
        let _ = lower_block(ctx, else_block);
        let else_terminated = ctx.terminated();
        if !else_terminated {
            ctx.push_inst(MirInst::Goto(join_id));
        }
        if !else_terminated {
            any_reaches_join = true;
            merge_local_states(
                &mut merged_states[..base_len],
                &ctx.local_states[..base_len],
            );
        }
    } else {
        any_reaches_join = true;
    }

    if any_reaches_join {
        restore_local_states(ctx, &merged_states, base_len);
    } else {
        restore_local_states(ctx, &base_states, base_len);
    }

    ctx.switch_to(join_id);
}

pub(crate) fn lower_if_value<'a>(
    ctx: &mut LowerCtx<'a>,
    cond: &'a Expr,
    then_blk: &'a Block,
    else_blk: Option<&'a Block>,
    expected: Option<&Type>,
) -> Option<MirValue> {
    let Some(else_block) = else_blk else {
        lower_if(ctx, cond, then_blk, None);
        return Some(MirValue::Unit);
    };

    let header = ctx.current;
    let then_id = ctx.new_block();
    let else_id = ctx.new_block();
    let join_id = ctx.new_block();

    ctx.switch_to(header);
    lower_cond_branch(ctx, cond, then_id, else_id);

    let result_local = ctx.fresh_local(None);
    if let Some(ty) = expected {
        ctx.locals[result_local.0 as usize].ty = Some(ty.clone());
    }

    let base_len = ctx.local_states.len();
    let base_states = ctx.local_states.clone();
    let mut merged_states = base_states.clone();
    let mut any_reaches_join = false;

    let assign_result = |ctx: &mut LowerCtx<'a>, value: Option<MirValue>, span: Span| {
        let Some(value) = value else {
            if !matches!(expected, Some(Type::Void)) {
                ctx.error("if expression branch missing value", Some(span));
            }
            return;
        };

        let inferred = infer_value_type(&value, ctx);
        if ctx.locals[result_local.0 as usize].ty.is_none() {
            ctx.locals[result_local.0 as usize].ty = inferred.clone();
        }
        if matches!(value, MirValue::Unit) && !matches!(expected, Some(Type::Void)) {
            ctx.error("if expression branch produced unit", Some(span));
            return;
        }
        if let Some(rv) = rvalue_from_value(value) {
            ctx.push_inst(MirInst::Assign {
                local: result_local,
                value: rv,
            });
        }
    };

    ctx.switch_to(then_id);
    let then_val = lower_block_with_expected(ctx, then_blk, expected, true, true);
    let then_terminated = ctx.terminated();
    if !then_terminated {
        assign_result(ctx, then_val, then_blk.span);
        ctx.push_inst(MirInst::Goto(join_id));
        any_reaches_join = true;
        merge_local_states(
            &mut merged_states[..base_len],
            &ctx.local_states[..base_len],
        );
    }

    restore_local_states(ctx, &base_states, base_len);
    ctx.switch_to(else_id);
    let else_val = lower_block_with_expected(ctx, else_block, expected, true, true);
    let else_terminated = ctx.terminated();
    if !else_terminated {
        assign_result(ctx, else_val, else_block.span);
        ctx.push_inst(MirInst::Goto(join_id));
        any_reaches_join = true;
        merge_local_states(
            &mut merged_states[..base_len],
            &ctx.local_states[..base_len],
        );
    }

    if any_reaches_join {
        restore_local_states(ctx, &merged_states, base_len);
    } else {
        restore_local_states(ctx, &base_states, base_len);
    }

    ctx.switch_to(join_id);
    if matches!(expected, Some(Type::Void)) {
        Some(MirValue::Unit)
    } else {
        Some(MirValue::Local(result_local))
    }
}

pub(crate) fn lower_cond_branch<'a>(
    ctx: &mut LowerCtx<'a>,
    cond: &'a Expr,
    then_bb: BlockId,
    else_bb: BlockId,
) {
    match cond {
        Expr::Binary {
            op: glyph_core::ast::BinaryOp::And,
            lhs,
            rhs,
            ..
        } => {
            // (lhs && rhs): if lhs then eval rhs else false
            let mid = ctx.new_block();
            lower_cond_branch(ctx, lhs, mid, else_bb);
            ctx.switch_to(mid);
            lower_cond_branch(ctx, rhs, then_bb, else_bb);
        }
        Expr::Binary {
            op: glyph_core::ast::BinaryOp::Or,
            lhs,
            rhs,
            ..
        } => {
            // (lhs || rhs): if lhs then true else eval rhs
            let mid = ctx.new_block();
            lower_cond_branch(ctx, lhs, then_bb, mid);
            ctx.switch_to(mid);
            lower_cond_branch(ctx, rhs, then_bb, else_bb);
        }
        _ => {
            let mut cond_val = lower_value(ctx, cond).unwrap_or(MirValue::Bool(false));
            if let Some(cond_ty) = infer_value_type(&cond_val, ctx) {
                if is_void_type(&cond_ty) {
                    ctx.error("condition expression has void type", expr_span(cond));
                    cond_val = MirValue::Bool(false);
                }
            }
            let cond_bool = coerce_to_bool(cond_val);
            ctx.push_inst(MirInst::If {
                cond: cond_bool,
                then_bb,
                else_bb,
            });
        }
    }
}

pub(crate) fn lower_while<'a>(ctx: &mut LowerCtx<'a>, cond: &'a Expr, body_block: &'a Block) {
    let header_block = ctx.new_block();
    let body_bb = ctx.new_block();
    let exit_bb = ctx.new_block();

    // Jump from current block to header
    if !ctx.terminated() {
        ctx.push_inst(MirInst::Goto(header_block));
    }

    // Header: evaluate condition and branch
    ctx.switch_to(header_block);
    lower_cond_branch(ctx, cond, body_bb, exit_bb);

    let base_len = ctx.local_states.len();
    let base_states = ctx.local_states.clone();
    let mut merged_states = base_states.clone();

    // Body: lower statements, add back edge
    ctx.switch_to(body_bb);
    ctx.enter_loop(header_block, exit_bb);
    let _ = lower_block(ctx, body_block);
    ctx.exit_loop();

    let body_terminated = ctx.terminated();
    if !body_terminated {
        ctx.push_inst(MirInst::Goto(header_block)); // Back edge
    }

    let body_returns = ctx
        .blocks
        .get(ctx.current.0 as usize)
        .and_then(|block| block.insts.last())
        .map(|inst| matches!(inst, MirInst::Return(_)))
        .unwrap_or(false);

    if !body_returns {
        merge_local_states(
            &mut merged_states[..base_len],
            &ctx.local_states[..base_len],
        );
    }
    restore_local_states(ctx, &merged_states, base_len);

    // Exit: continue after loop
    ctx.switch_to(exit_bb);
}

pub(crate) fn lower_for<'a>(
    ctx: &mut LowerCtx<'a>,
    var: &'a Ident,
    start: &'a Expr,
    end: &'a Expr,
    body_block: &'a Block,
    span: Span,
) {
    // Initialize loop variable: let var = start
    let var_local = ctx.fresh_local(Some(&var.0));
    ctx.bind_name(&var.0, var_local);
    ctx.register_source_binding(&var.0, span, var_local);

    let diags_before = ctx.diagnostics.len();
    if let Some(mut rv) = lower_expr(ctx, start) {
        update_local_type_from_rvalue(ctx, var_local, &mut rv);
        ctx.push_inst(MirInst::Assign {
            local: var_local,
            value: rv,
        });
    } else {
        ensure_lowering_reported(ctx, diags_before, "for-loop start bound", expr_span(start));
        ctx.push_inst(MirInst::Nop);
    }

    // Create loop blocks:
    // - header: range condition
    // - body_bb: user body
    // - continue_bb: increment + back edge (continue target)
    // - exit_bb: loop exit
    let header_block = ctx.new_block();
    let body_bb = ctx.new_block();
    let continue_bb = ctx.new_block();
    let exit_bb = ctx.new_block();

    // Jump to header
    if !ctx.terminated() {
        ctx.push_inst(MirInst::Goto(header_block));
    }

    // Header: check condition (var < end)
    ctx.switch_to(header_block);
    let var_val = MirValue::Local(var_local);
    let var_type = ctx.locals[var_local.0 as usize].ty.clone();
    let end_val =
        lower_value_with_expected(ctx, end, var_type.as_ref()).unwrap_or(MirValue::Int(0));
    let cond_temp = ctx.fresh_local(None);
    ctx.locals[cond_temp.0 as usize].ty = Some(Type::Bool);
    ctx.push_inst(MirInst::Assign {
        local: cond_temp,
        value: Rvalue::Binary {
            op: glyph_core::ast::BinaryOp::Lt,
            lhs: var_val.clone(),
            rhs: end_val,
        },
    });
    ctx.push_inst(MirInst::If {
        cond: MirValue::Local(cond_temp),
        then_bb: body_bb,
        else_bb: exit_bb,
    });

    let base_len = ctx.local_states.len();
    let base_states = ctx.local_states.clone();
    let mut merged_states = base_states.clone();

    // Body: execute loop body
    ctx.switch_to(body_bb);
    ctx.enter_loop(continue_bb, exit_bb);
    let _ = lower_block(ctx, body_block);
    ctx.exit_loop();

    // Fallthrough path runs increment.
    // `continue` statements also jump directly to continue_bb.
    let body_terminated = ctx.terminated();
    let body_returns = ctx
        .blocks
        .get(ctx.current.0 as usize)
        .and_then(|block| block.insts.last())
        .map(|inst| matches!(inst, MirInst::Return(_)))
        .unwrap_or(false);
    if !body_terminated {
        ctx.push_inst(MirInst::Goto(continue_bb));
    }

    ctx.switch_to(continue_bb);
    let inc_temp = ctx.fresh_local(None);
    let var_ty = ctx.locals[var_local.0 as usize].ty.clone();
    ctx.locals[inc_temp.0 as usize].ty = var_ty.clone();
    // Create a typed increment value matching the loop variable type
    let inc_val = if let Some(ty) = &var_ty {
        if ty.is_int() && *ty != Type::I32 {
            let one_tmp = ctx.fresh_local(None);
            ctx.locals[one_tmp.0 as usize].ty = Some(ty.clone());
            ctx.push_inst(MirInst::Assign {
                local: one_tmp,
                value: Rvalue::ConstInt(1),
            });
            MirValue::Local(one_tmp)
        } else {
            MirValue::Int(1)
        }
    } else {
        MirValue::Int(1)
    };
    ctx.push_inst(MirInst::Assign {
        local: inc_temp,
        value: Rvalue::Binary {
            op: glyph_core::ast::BinaryOp::Add,
            lhs: var_val.clone(),
            rhs: inc_val,
        },
    });
    ctx.push_inst(MirInst::Assign {
        local: var_local,
        value: Rvalue::Move(inc_temp),
    });
    ctx.push_inst(MirInst::Goto(header_block));

    if !body_returns {
        merge_local_states(
            &mut merged_states[..base_len],
            &ctx.local_states[..base_len],
        );
    }
    restore_local_states(ctx, &merged_states, base_len);

    // Exit: continue after loop
    ctx.switch_to(exit_bb);
}

// Lower `for x in collection { body }` by desugaring to an index-based while loop:
//   let mut _i: usize = 0
//   while _i < collection.len() { let x = collection[_i]; body; _i = _i + 1; }
pub(crate) fn lower_for_in<'a>(
    ctx: &mut LowerCtx<'a>,
    var: &'a Ident,
    iter_expr: &'a Expr,
    body_block: &'a Block,
    span: Span,
) {
    use super::types::vec_elem_type_from_type;

    // 1. Lower the collection expression into a local
    let iter_val = lower_value(ctx, iter_expr);
    let iter_local = match iter_val {
        Some(MirValue::Local(id)) => id,
        Some(val) => {
            let tmp = ctx.fresh_local(None);
            if let Some(rv) = rvalue_from_value(val) {
                ctx.push_inst(MirInst::Assign {
                    local: tmp,
                    value: rv,
                });
            }
            tmp
        }
        None => {
            ctx.error(
                "could not lower collection expression in for-in loop",
                expr_span(iter_expr),
            );
            return;
        }
    };

    // 2. Determine element type from the collection type
    let iter_type = ctx.locals[iter_local.0 as usize].ty.clone();
    let (elem_type, is_vec, _is_array) = match &iter_type {
        Some(ty) if vec_elem_type_from_type(ty).is_some() => {
            (vec_elem_type_from_type(ty).unwrap(), true, false)
        }
        Some(Type::Array(elem_ty, _)) => (elem_ty.as_ref().clone(), false, true),
        _ => {
            ctx.error(
                "for-in loop requires a Vec or array collection",
                expr_span(iter_expr),
            );
            return;
        }
    };

    // 3. Create index variable: let mut _i: usize = 0
    let idx_local = ctx.fresh_local(None);
    ctx.locals[idx_local.0 as usize].ty = Some(Type::Usize);
    ctx.locals[idx_local.0 as usize].mutable = true;
    // Create a typed zero for usize
    let zero_tmp = ctx.fresh_local(None);
    ctx.locals[zero_tmp.0 as usize].ty = Some(Type::Usize);
    ctx.push_inst(MirInst::Assign {
        local: zero_tmp,
        value: Rvalue::ConstInt(0),
    });
    ctx.push_inst(MirInst::Assign {
        local: idx_local,
        value: Rvalue::Move(zero_tmp),
    });

    // 4. Get collection length
    let len_local = ctx.fresh_local(None);
    ctx.locals[len_local.0 as usize].ty = Some(Type::Usize);
    let len_rvalue = if is_vec {
        Rvalue::VecLen { vec: iter_local }
    } else {
        Rvalue::ArrayLen { base: iter_local }
    };
    ctx.push_inst(MirInst::Assign {
        local: len_local,
        value: len_rvalue,
    });

    // 5. Create loop blocks:
    // - header: bounds check
    // - body_bb: bind element + user body
    // - continue_bb: index increment + back edge (continue target)
    // - exit_bb: loop exit
    let header_block = ctx.new_block();
    let body_bb = ctx.new_block();
    let continue_bb = ctx.new_block();
    let exit_bb = ctx.new_block();

    if !ctx.terminated() {
        ctx.push_inst(MirInst::Goto(header_block));
    }

    // 6. Header: _i < len
    ctx.switch_to(header_block);
    let cond_temp = ctx.fresh_local(None);
    ctx.locals[cond_temp.0 as usize].ty = Some(Type::Bool);
    ctx.push_inst(MirInst::Assign {
        local: cond_temp,
        value: Rvalue::Binary {
            op: glyph_core::ast::BinaryOp::Lt,
            lhs: MirValue::Local(idx_local),
            rhs: MirValue::Local(len_local),
        },
    });
    ctx.push_inst(MirInst::If {
        cond: MirValue::Local(cond_temp),
        then_bb: body_bb,
        else_bb: exit_bb,
    });

    let base_len = ctx.local_states.len();
    let base_states = ctx.local_states.clone();
    let mut merged_states = base_states.clone();

    // 7. Body: let x = collection[_i]; body
    ctx.switch_to(body_bb);
    ctx.enter_loop(continue_bb, exit_bb);

    // Bind loop variable
    let var_local = ctx.fresh_local(Some(&var.0));
    ctx.locals[var_local.0 as usize].ty = Some(elem_type.clone());
    ctx.bind_name(&var.0, var_local);
    ctx.register_source_binding(&var.0, span, var_local);

    let index_rvalue = if is_vec {
        Rvalue::VecIndex {
            vec: iter_local,
            elem_type,
            index: MirValue::Local(idx_local),
            bounds_check: false, // bounds already ensured by _i < len
        }
    } else {
        Rvalue::ArrayIndex {
            base: iter_local,
            index: MirValue::Local(idx_local),
            bounds_check: false,
        }
    };
    ctx.push_inst(MirInst::Assign {
        local: var_local,
        value: index_rvalue,
    });

    // VecIndex marks the destination as Moved (to prevent double-free of
    // shallow copies). But for-in semantically moves each element out of
    // the collection, so the loop variable must be usable in the body.
    if let Some(state) = ctx.local_states.get_mut(var_local.0 as usize) {
        *state = LocalState::Initialized;
    }

    // Lower body
    let _ = lower_block(ctx, body_block);
    ctx.exit_loop();

    // Fallthrough path runs increment.
    // `continue` statements also jump directly to continue_bb.
    let body_terminated = ctx.terminated();
    let body_returns = ctx
        .blocks
        .get(ctx.current.0 as usize)
        .and_then(|block| block.insts.last())
        .map(|inst| matches!(inst, MirInst::Return(_)))
        .unwrap_or(false);

    if !body_terminated {
        ctx.push_inst(MirInst::Goto(continue_bb));
    }

    ctx.switch_to(continue_bb);
    let one_tmp = ctx.fresh_local(None);
    ctx.locals[one_tmp.0 as usize].ty = Some(Type::Usize);
    ctx.push_inst(MirInst::Assign {
        local: one_tmp,
        value: Rvalue::ConstInt(1),
    });
    let inc_temp = ctx.fresh_local(None);
    ctx.locals[inc_temp.0 as usize].ty = Some(Type::Usize);
    ctx.push_inst(MirInst::Assign {
        local: inc_temp,
        value: Rvalue::Binary {
            op: glyph_core::ast::BinaryOp::Add,
            lhs: MirValue::Local(idx_local),
            rhs: MirValue::Local(one_tmp),
        },
    });
    ctx.push_inst(MirInst::Assign {
        local: idx_local,
        value: Rvalue::Move(inc_temp),
    });
    ctx.push_inst(MirInst::Goto(header_block));

    if !body_returns {
        merge_local_states(
            &mut merged_states[..base_len],
            &ctx.local_states[..base_len],
        );
    }
    restore_local_states(ctx, &merged_states, base_len);

    // 8. Exit block
    ctx.switch_to(exit_bb);
}

enum AssignmentTarget {
    Local(LocalId),
    Field {
        base: LocalId,
        field_name: String,
        field_index: u32,
        field_type: Type,
    },
    Index {
        base: LocalId,
        index: MirValue,
        elem_type: Type,
    },
}

fn collect_field_chain<'a>(expr: &'a Expr, fields: &mut Vec<&'a Ident>) -> &'a Expr {
    match expr {
        Expr::FieldAccess { base, field, .. } => {
            fields.push(field);
            collect_field_chain(base, fields)
        }
        _ => expr,
    }
}

fn assignment_root_is_mutable(
    ctx: &mut LowerCtx<'_>,
    local_id: LocalId,
    name: &str,
    span: Span,
) -> bool {
    let Some(local) = ctx.locals.get(local_id.0 as usize) else {
        return true;
    };

    match local.ty.as_ref() {
        Some(Type::Ref(_, Mutability::Immutable)) => {
            ctx.error("cannot assign through immutable reference", Some(span));
            false
        }
        Some(Type::Ref(_, Mutability::Mutable)) => true,
        _ => {
            if !local.mutable {
                ctx.error(
                    format!("cannot assign to immutable variable '{}'", name),
                    Some(span),
                );
                false
            } else {
                true
            }
        }
    }
}

fn resolve_field_info(
    ctx: &mut LowerCtx<'_>,
    base_local: LocalId,
    field: &Ident,
    span: Span,
) -> Option<(Type, u32)> {
    let base_type = match ctx
        .locals
        .get(base_local.0 as usize)
        .and_then(|l| l.ty.as_ref())
    {
        Some(ty) => ty,
        None => {
            ctx.error("field access base has unknown type", Some(span));
            return None;
        }
    };

    let mut inner = base_type;
    loop {
        match inner {
            Type::Ref(inner_ty, _)
            | Type::Own(inner_ty)
            | Type::RawPtr(inner_ty)
            | Type::Shared(inner_ty) => {
                inner = inner_ty.as_ref();
            }
            _ => break,
        }
    }

    if let Type::Tuple(elem_types) = inner {
        let Ok(idx) = field.0.parse::<usize>() else {
            ctx.error(
                format!(
                    "tuple field access must use numeric index, got '{}'",
                    field.0
                ),
                Some(span),
            );
            return None;
        };

        if idx >= elem_types.len() {
            ctx.error(
                format!(
                    "tuple index {} out of bounds (len is {})",
                    idx,
                    elem_types.len()
                ),
                Some(span),
            );
            return None;
        }

        return Some((elem_types[idx].clone(), idx as u32));
    }

    let Some(struct_name) = local_struct_name(ctx, base_local) else {
        ctx.error("field access base is not a struct", Some(span));
        return None;
    };

    let Some((field_type, field_index)) = ctx.resolver.get_field(&struct_name, &field.0) else {
        ctx.error(
            format!("struct '{}' has no field named '{}'", struct_name, field.0),
            Some(span),
        );
        return None;
    };

    Some((field_type, field_index as u32))
}

fn lower_assignment_target<'a>(
    ctx: &mut LowerCtx<'a>,
    target: &'a Expr,
    span: Span,
) -> Option<AssignmentTarget> {
    match target {
        Expr::Ident(ident, ident_span) => {
            let local_id = ctx.bindings.get(ident.0.as_str()).copied().or_else(|| {
                ctx.error(
                    format!("unknown identifier '{}'", ident.0),
                    Some(*ident_span),
                );
                None
            })?;

            if !assignment_root_is_mutable(ctx, local_id, &ident.0, *ident_span) {
                return None;
            }

            Some(AssignmentTarget::Local(local_id))
        }
        Expr::FieldAccess { .. } => {
            let mut fields = Vec::new();
            let root = collect_field_chain(target, &mut fields);
            fields.reverse();

            let Some((last_field, rest)) = fields.split_last() else {
                ctx.error(
                    "assignment target must be an identifier or field access",
                    Some(span),
                );
                return None;
            };

            let Expr::Ident(root_ident, root_span) = root else {
                ctx.error(
                    "assignment target must start with an identifier",
                    expr_span(root).or(Some(span)),
                );
                return None;
            };

            let root_local = ctx
                .bindings
                .get(root_ident.0.as_str())
                .copied()
                .or_else(|| {
                    ctx.error(
                        format!("unknown identifier '{}'", root_ident.0),
                        Some(*root_span),
                    );
                    None
                })?;

            if !assignment_root_is_mutable(ctx, root_local, &root_ident.0, *root_span) {
                return None;
            }

            let mut base_local = root_local;
            for field in rest.iter() {
                let (_, field_index) = resolve_field_info(ctx, base_local, field, span)?;
                let mut rv = Rvalue::FieldRef {
                    base: base_local,
                    field_name: field.0.clone(),
                    field_index,
                    mutability: Mutability::Mutable,
                };
                let tmp = ctx.fresh_local(None);
                update_local_type_from_rvalue(ctx, tmp, &mut rv);
                ctx.push_inst(MirInst::Assign {
                    local: tmp,
                    value: rv,
                });
                base_local = tmp;
            }

            let (field_type, field_index) = resolve_field_info(ctx, base_local, last_field, span)?;

            Some(AssignmentTarget::Field {
                base: base_local,
                field_name: last_field.0.clone(),
                field_index,
                field_type,
            })
        }
        Expr::Index {
            base,
            index,
            span: index_span,
        } => {
            let Expr::Ident(base_ident, base_span) = base.as_ref() else {
                ctx.error(
                    "indexed assignment base must be an identifier (index a local Vec or array; for struct fields, borrow the field into a local first)",
                    expr_span(base).or(Some(*index_span)),
                );
                return None;
            };

            let base_local = ctx
                .bindings
                .get(base_ident.0.as_str())
                .copied()
                .or_else(|| {
                    ctx.error(
                        format!("unknown identifier '{}'", base_ident.0),
                        Some(*base_span),
                    );
                    None
                })?;

            if !assignment_root_is_mutable(ctx, base_local, &base_ident.0, *base_span) {
                return None;
            }

            let base_ty = ctx
                .locals
                .get(base_local.0 as usize)
                .and_then(|l| l.ty.clone());
            let elem_type = match base_ty.as_ref() {
                Some(ty) => {
                    let mut inner = ty;
                    while let Type::Ref(inner_ty, _) = inner {
                        inner = inner_ty.as_ref();
                    }
                    if let Type::Array(elem, _) = inner {
                        Some(elem.as_ref().clone())
                    } else {
                        vec_elem_type_from_type(ty)
                    }
                }
                None => None,
            };
            let Some(elem_type) = elem_type else {
                ctx.error(
                    "indexed assignment target must be a Vec or fixed-size array",
                    Some(*index_span),
                );
                return None;
            };

            let index_val = lower_value_with_expected(ctx, index, Some(&Type::Usize))?;

            Some(AssignmentTarget::Index {
                base: base_local,
                index: index_val,
                elem_type,
            })
        }
        _ => {
            ctx.error(
                "assignment target must be an identifier or field access",
                expr_span(target).or(Some(span)),
            );
            None
        }
    }
}
