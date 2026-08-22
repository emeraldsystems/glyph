use std::collections::HashSet;

use glyph_core::ast::{Expr, Ident};
use glyph_core::mir::{LocalId, MirInst, MirValue, Rvalue};
use glyph_core::span::Span;
use glyph_core::types::{Mutability, Type};

use crate::resolver::SelfKind;

use super::builtins::{
    is_canonical_arc_new, is_canonical_channel, is_canonical_mutex_new, is_canonical_scope,
    is_canonical_spawn, lower_arc_method, lower_arc_new, lower_atomic_constructor,
    lower_atomic_method, lower_channel, lower_file_close, lower_file_open,
    lower_file_read_to_string, lower_file_write_string, lower_map_add, lower_map_del,
    lower_map_get, lower_map_has, lower_map_keys, lower_map_static_new,
    lower_map_static_with_capacity, lower_map_update, lower_map_vals, lower_mutex_method,
    lower_mutex_new, lower_own_from_raw, lower_own_into_raw, lower_own_new, lower_print_builtin,
    lower_shared_clone, lower_shared_new, lower_spsc_method, lower_string_as_str,
    lower_string_clone, lower_string_concat, lower_string_ends_with, lower_string_from,
    lower_string_len, lower_string_slice, lower_string_split, lower_string_starts_with,
    lower_string_trim, lower_term_stdout, lower_thread_method, lower_thread_scope,
    lower_thread_spawn, lower_vec_get, lower_vec_len, lower_vec_pop, lower_vec_push,
    lower_vec_static_new, lower_vec_static_with_capacity,
};
use super::context::{LocalState, LowerCtx};
use super::expr::{lower_array_len, lower_ref_expr, lower_value, lower_value_with_expected};
use super::value::infer_value_type;

pub(crate) fn call_types_compatible(actual: &Type, expected: &Type) -> bool {
    if actual == expected {
        return true;
    }
    if matches!(actual, Type::Param(_)) || matches!(expected, Type::Param(_)) {
        return true;
    }
    let is_unit = |ty: &Type| {
        matches!(ty, Type::Void) || matches!(ty, Type::Tuple(elements) if elements.is_empty())
    };
    if is_unit(actual) && is_unit(expected) {
        return true;
    }
    if matches!(
        (actual, expected),
        (Type::Str, Type::String) | (Type::String, Type::Str)
    ) {
        return true;
    }
    match (actual, expected) {
        (
            Type::Function {
                params: actual_params,
                ret: actual_ret,
            },
            Type::Function {
                params: expected_params,
                ret: expected_ret,
            },
        ) => {
            actual_params.len() == expected_params.len()
                && actual_params
                    .iter()
                    .zip(expected_params)
                    .all(|(actual, expected)| call_types_compatible(actual, expected))
                && call_types_compatible(actual_ret, expected_ret)
        }
        (
            Type::BorrowedFunction {
                kind: actual_kind,
                params: actual_params,
                ret: actual_ret,
            },
            Type::BorrowedFunction {
                kind: expected_kind,
                params: expected_params,
                ret: expected_ret,
            },
        ) => {
            (*actual_kind == *expected_kind
                || (*actual_kind == glyph_core::types::BorrowedCallableKind::Fn
                    && *expected_kind == glyph_core::types::BorrowedCallableKind::FnMut))
                && actual_params.len() == expected_params.len()
                && actual_params
                    .iter()
                    .zip(expected_params)
                    .all(|(actual, expected)| call_types_compatible(actual, expected))
                && call_types_compatible(actual_ret, expected_ret)
        }
        (
            Type::Function {
                params: actual_params,
                ret: actual_ret,
            },
            Type::BorrowedFunction {
                params: expected_params,
                ret: expected_ret,
                ..
            },
        ) => {
            actual_params.len() == expected_params.len()
                && actual_params
                    .iter()
                    .zip(expected_params)
                    .all(|(actual, expected)| call_types_compatible(actual, expected))
                && call_types_compatible(actual_ret, expected_ret)
        }
        (Type::Ref(inner, _), expected) => inner.as_ref() == expected,
        (actual, Type::Ref(inner, _)) => actual == inner.as_ref(),
        _ => false,
    }
}

fn validate_call_argument(
    ctx: &mut LowerCtx<'_>,
    value: &MirValue,
    expected: &Type,
    index: usize,
    callee_name: &str,
    span: Span,
) -> bool {
    let Some(actual) = infer_value_type(value, ctx) else {
        ctx.error(
            format!(
                "cannot infer type of argument {} in call to '{}'",
                index + 1,
                callee_name
            ),
            Some(span),
        );
        return false;
    };
    if call_types_compatible(&actual, expected) {
        return true;
    }
    ctx.error(
        format!(
            "argument {} to '{}' has type '{}', expected '{}'",
            index + 1,
            callee_name,
            LowerCtx::type_label(&actual),
            LowerCtx::type_label(expected)
        ),
        Some(span),
    );
    false
}

fn validate_unique_fnmut_argument(
    ctx: &mut LowerCtx<'_>,
    seen: &mut HashSet<LocalId>,
    value: &MirValue,
    expected: &Type,
    span: Span,
) -> bool {
    if !matches!(
        expected,
        Type::BorrowedFunction {
            kind: glyph_core::types::BorrowedCallableKind::FnMut,
            ..
        }
    ) {
        return true;
    }

    let MirValue::Local(local) = value else {
        return true;
    };
    if seen.insert(*local) {
        return true;
    }

    ctx.error(
        "an FnMut callable cannot be aliased across multiple arguments in one call",
        Some(span),
    );
    false
}

fn temporary_call_loan(arg: &Expr, value: &MirValue) -> Option<LocalId> {
    if !matches!(arg, Expr::Ref { .. }) {
        return None;
    }
    match value {
        MirValue::Local(local) => Some(*local),
        _ => None,
    }
}

fn lower_indirect_call<'a>(
    ctx: &mut LowerCtx<'a>,
    callee: glyph_core::mir::LocalId,
    callee_name: &str,
    signature: Type,
    args: &'a [Expr],
    span: Span,
    expected_ret: Option<&Type>,
) -> Option<Rvalue> {
    if !ctx.check_local_available(callee, Some(span)) {
        return None;
    }
    let Some((params, ret)) = signature.function_signature() else {
        ctx.error(
            format!(
                "value '{}' of type '{}' is not callable",
                callee_name,
                LowerCtx::type_label(&signature)
            ),
            Some(span),
        );
        return None;
    };
    if params.len() != args.len() {
        ctx.error(
            format!(
                "callable '{}' expects {} arguments but got {}",
                callee_name,
                params.len(),
                args.len()
            ),
            Some(span),
        );
        return None;
    }
    if let Some(expected) = expected_ret {
        if !call_types_compatible(ret, expected) {
            ctx.error(
                format!(
                    "callable '{}' returns '{}', but '{}' is required here",
                    callee_name,
                    LowerCtx::type_label(ret),
                    LowerCtx::type_label(expected)
                ),
                Some(span),
            );
            return None;
        }
    }

    let mut lowered_args = Vec::with_capacity(args.len());
    let mut temporary_argument_loans = Vec::new();
    let mut fnmut_arguments = HashSet::new();
    for (index, (arg, expected)) in args.iter().zip(params).enumerate() {
        let value = lower_value_with_expected(ctx, arg, Some(expected))?;
        if !validate_call_argument(ctx, &value, expected, index, callee_name, span) {
            return None;
        }
        if !validate_unique_fnmut_argument(ctx, &mut fnmut_arguments, &value, expected, span) {
            return None;
        }
        if !consume_call_local(ctx, &value, span, Some(expected)) {
            return None;
        }
        if let Some(local) = temporary_call_loan(arg, &value) {
            temporary_argument_loans.push(local);
        }
        lowered_args.push(value);
    }

    let tmp = ctx.fresh_local(None);
    ctx.locals[tmp.0 as usize].ty = Some(ret.clone());
    let call = match &signature {
        Type::Function { .. } => Rvalue::CallIndirect {
            callee,
            signature,
            args: lowered_args,
        },
        Type::BorrowedFunction {
            kind: glyph_core::types::BorrowedCallableKind::Fn,
            ..
        } => Rvalue::CallIndirectShared {
            callee,
            signature,
            args: lowered_args,
        },
        Type::BorrowedFunction {
            kind: glyph_core::types::BorrowedCallableKind::FnMut,
            ..
        } => Rvalue::CallIndirectMut {
            callee,
            signature,
            args: lowered_args,
        },
        _ => unreachable!("function_signature accepted only callable variants"),
    };
    ctx.push_inst(MirInst::Assign {
        local: tmp,
        value: call,
    });
    for loan in temporary_argument_loans {
        ctx.release_temporary_call_loan(loan);
    }
    Some(Rvalue::Move(tmp))
}

fn consume_call_local(
    ctx: &mut LowerCtx<'_>,
    value: &MirValue,
    span: Span,
    expected: Option<&Type>,
) -> bool {
    let MirValue::Local(local) = value else {
        return true;
    };

    if matches!(
        ctx.local_states.get(local.0 as usize),
        Some(LocalState::Moved)
    ) {
        return true;
    }

    if matches!(expected, Some(Type::Ref(_, _))) {
        return ctx.check_local_available(*local, Some(span));
    }

    if !ctx.consume_local(*local, Some(span)) {
        return false;
    }

    // consume_local tracks String/Own but not Named structs or
    // App types (Vec/Map). At call sites these are passed by value
    // (shallow copy), so the callee will drop its copy. Mark as Moved
    // to prevent the caller from also dropping and causing a double-free.
    if matches!(
        ctx.local_states.get(local.0 as usize),
        Some(LocalState::Initialized)
    ) {
        if let Some(ty) = ctx.local_ty(*local) {
            if matches!(ty, Type::Named(_) | Type::App { .. }) {
                if let Some(state) = ctx.local_states.get_mut(local.0 as usize) {
                    *state = LocalState::Moved;
                }
            }
        }
    }

    true
}

pub(crate) fn lower_call<'a>(
    ctx: &mut LowerCtx<'a>,
    callee: &'a Expr,
    args: &'a [Expr],
    span: Span,
    _require_value: bool,
    expected_ret: Option<&Type>,
) -> Option<Rvalue> {
    if let Some(builtin) = lower_method_builtin(ctx, callee, args, span) {
        return builtin;
    }

    if let Some(builtin) = lower_static_builtin_with_expected(ctx, callee, args, span, expected_ret)
    {
        return Some(builtin);
    }

    let Expr::Ident(name, _) = callee else {
        ctx.error(
            "call target must be a function or callable local",
            Some(span),
        );
        return None;
    };

    if let Some(local) = ctx.bindings.get(name.0.as_str()).copied() {
        let Some(local_ty) = ctx.local_ty(local).cloned() else {
            ctx.error(
                format!("cannot call '{}' because its type is unknown", name.0),
                Some(span),
            );
            return None;
        };
        return lower_indirect_call(ctx, local, &name.0, local_ty, args, span, expected_ret);
    }

    let Some(sig) = ctx.fn_sigs.get(&name.0) else {
        ctx.error(format!("unknown function '{}'", name.0), Some(span));
        return None;
    };

    if sig.params.len() != args.len() {
        ctx.error(
            format!(
                "function '{}' expects {} arguments but got {}",
                name.0,
                sig.params.len(),
                args.len()
            ),
            Some(span),
        );
        return None;
    }

    // For externs, ensure all parameter types are known
    if sig.params.iter().any(|p| p.is_none()) {
        ctx.error(
            format!("function '{}' has unknown parameter types", name.0),
            Some(span),
        );
        return None;
    }

    if let Some(expected) = expected_ret {
        let actual = sig.ret.as_ref().unwrap_or(&Type::Void);
        if (matches!(actual, Type::Function { .. }) || matches!(expected, Type::Function { .. }))
            && !call_types_compatible(actual, expected)
        {
            ctx.error(
                format!(
                    "function '{}' returns '{}', but '{}' is required here",
                    name.0,
                    LowerCtx::type_label(actual),
                    LowerCtx::type_label(expected)
                ),
                Some(span),
            );
            return None;
        }
    }

    let mut lowered_args = Vec::new();
    let mut temporary_argument_loans = Vec::new();
    let mut fnmut_arguments = HashSet::new();
    for (idx, arg) in args.iter().enumerate() {
        let expected_arg = sig.params.get(idx).and_then(|ty| ty.as_ref());
        let arg_val = lower_value_with_expected(ctx, arg, expected_arg)?;
        if let Some(expected_arg) = expected_arg {
            if !validate_unique_fnmut_argument(
                ctx,
                &mut fnmut_arguments,
                &arg_val,
                expected_arg,
                span,
            ) {
                return None;
            }
        }
        if !consume_call_local(ctx, &arg_val, span, expected_arg) {
            return None;
        }
        if let Some(local) = temporary_call_loan(arg, &arg_val) {
            temporary_argument_loans.push(local);
        }
        lowered_args.push(arg_val);
    }

    let tmp = ctx.fresh_local(None);
    let mut ret_ty = sig.ret.clone();

    if let Some(enum_ctor) = &sig.enum_ctor {
        if let Some(Type::App { base, .. }) = expected_ret {
            if base == &enum_ctor.enum_name {
                ret_ty = Some(expected_ret.unwrap().clone());
            }
        }

        if enum_ctor.has_generics && expected_ret.is_none() && !lowered_args.is_empty() {
            let mut arg_tys = Vec::new();
            for arg in &lowered_args {
                if let Some(arg_ty) = infer_value_type(arg, ctx) {
                    arg_tys.push(arg_ty);
                }
            }
            if !arg_tys.is_empty() {
                ret_ty = Some(Type::App {
                    base: enum_ctor.enum_name.clone(),
                    args: arg_tys,
                });
            }
        }

        if let Some(ret) = ret_ty.as_ref() {
            ctx.locals[tmp.0 as usize].ty = Some(ret.clone());
        } else {
            ctx.locals[tmp.0 as usize].ty = Some(Type::Void);
        }

        let payload = lowered_args.get(0).cloned();
        ctx.push_inst(MirInst::Assign {
            local: tmp,
            value: Rvalue::EnumConstruct {
                enum_name: enum_ctor.enum_name.clone(),
                variant_index: enum_ctor.variant_index as u32,
                payload,
            },
        });
    } else {
        if let Some(ret) = ret_ty.as_ref() {
            ctx.locals[tmp.0 as usize].ty = Some(ret.clone());
        } else {
            ctx.locals[tmp.0 as usize].ty = Some(Type::Void);
        }

        let call_target = sig.target_name.clone();

        ctx.push_inst(MirInst::Assign {
            local: tmp,
            value: Rvalue::Call {
                name: call_target.clone(),
                args: lowered_args,
            },
        });
        for loan in temporary_argument_loans {
            ctx.release_temporary_call_loan(loan);
        }
        if matches!(ctx.local_ty(tmp), Some(Type::Function { .. })) {
            if let Some(provenance) = ctx.callable_return_provenance.get(&call_target).cloned() {
                ctx.callable_provenance.insert(tmp, provenance);
            }
        }
    }

    Some(Rvalue::Move(tmp))
}

/// Infer the type of an expression for method call resolution
pub(crate) fn infer_expr_type(ctx: &LowerCtx, expr: &Expr) -> Option<glyph_core::types::Type> {
    match expr {
        // Local variable: look up in bindings
        Expr::Ident(name, _) => {
            let local = ctx.bindings.get(name.0.as_str()).copied()?;
            ctx.locals.get(local.0 as usize).and_then(|l| l.ty.clone())
        }

        Expr::Lit(glyph_core::ast::Literal::Int(_), _) => Some(Type::I32),
        Expr::Lit(glyph_core::ast::Literal::Bool(_), _) => Some(Type::Bool),
        Expr::Lit(glyph_core::ast::Literal::Char(_), _) => Some(Type::Char),
        Expr::Lit(glyph_core::ast::Literal::Str(_), _) => Some(Type::Str),
        Expr::InterpString { .. } => Some(Type::Str),
        Expr::Unary {
            op: glyph_core::ast::UnaryOp::Not,
            ..
        } => Some(Type::Bool),

        // Struct literal: obvious
        Expr::StructLit { name, .. } => Some(glyph_core::types::Type::Named(name.0.clone())),

        // Field access: get base type, then look up field
        Expr::FieldAccess { base, field, .. } => {
            let base_ty = infer_expr_type(ctx, base)?;
            match base_ty {
                glyph_core::types::Type::Named(struct_name) => {
                    let (field_ty, _) = ctx.resolver.get_field(&struct_name, &field.0)?;
                    Some(field_ty)
                }
                glyph_core::types::Type::Ref(ref inner_ty, _) => {
                    // Dereference and look up field
                    if let glyph_core::types::Type::Named(struct_name) = &**inner_ty {
                        let (field_ty, _) = ctx.resolver.get_field(struct_name, &field.0)?;
                        Some(field_ty)
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }

        // Reference: return reference type
        Expr::Ref {
            expr, mutability, ..
        } => {
            let inner_ty = infer_expr_type(ctx, expr)?;
            Some(glyph_core::types::Type::Ref(
                Box::new(inner_ty),
                *mutability,
            ))
        }

        _ => None,
    }
}

fn lower_borrowed_method_receiver<'a>(
    ctx: &mut LowerCtx<'a>,
    receiver: &'a Expr,
    receiver_ty: &Type,
    mutability: Mutability,
    span: Span,
) -> Option<(MirValue, bool)> {
    if matches!(receiver_ty, Type::Ref(_, _)) {
        return lower_value(ctx, receiver).map(|value| (value, false));
    }

    let rv = lower_ref_expr(ctx, receiver, mutability, span)?;
    let tmp = ctx.fresh_local(None);
    ctx.locals[tmp.0 as usize].ty = Some(Type::Ref(Box::new(receiver_ty.clone()), mutability));
    ctx.push_inst(MirInst::Assign {
        local: tmp,
        value: rv,
    });
    Some((MirValue::Local(tmp), true))
}

pub(crate) fn lower_method_call<'a>(
    ctx: &mut LowerCtx<'a>,
    receiver: &'a Expr,
    method: &Ident,
    args: &'a [Expr],
    span: Span,
) -> Option<Rvalue> {
    if let Some(result) = lower_thread_method(ctx, receiver, &method.0, args, span) {
        return result;
    }
    if let Some(result) = lower_spsc_method(ctx, receiver, &method.0, args, span) {
        return result;
    }
    if let Some(result) = lower_arc_method(ctx, receiver, &method.0, args, span) {
        return result;
    }
    if let Some(result) = lower_mutex_method(ctx, receiver, &method.0, args, span) {
        return result;
    }
    if matches!(
        method.0.as_str(),
        "load" | "store" | "swap" | "compare_exchange" | "fetch_add" | "fetch_sub" | "is_lock_free"
    ) {
        if let Some(rv) = lower_atomic_method(ctx, receiver, &method.0, args, span) {
            return Some(rv);
        }
    }

    // Handle built-in method-like operations that are not tied to interfaces.
    match method.0.as_str() {
        "len" => {
            if !args.is_empty() {
                ctx.error(".len() does not take arguments", Some(span));
                return None;
            }
            if let Some(rv) = lower_vec_len(ctx, receiver, span) {
                return Some(rv);
            }
            if let Some(rv) = lower_string_len(ctx, receiver, args, span) {
                return Some(rv);
            }
            return lower_array_len(ctx, receiver, span);
        }
        "as_str" => {
            if let Some(rv) = lower_string_as_str(ctx, receiver, args, span) {
                return Some(rv);
            }
        }
        "push" => {
            if let Some(rv) = lower_vec_push(ctx, receiver, args, span) {
                return Some(rv);
            }
        }
        "pop" => {
            if let Some(rv) = lower_vec_pop(ctx, receiver, args, span) {
                return Some(rv);
            }
        }
        "add" => return lower_map_add(ctx, receiver, args, span),
        "update" => return lower_map_update(ctx, receiver, args, span),
        "del" => return lower_map_del(ctx, receiver, args, span),
        "get" => {
            if let Some(rv) = lower_vec_get(ctx, receiver, args, span) {
                return Some(rv);
            }
            return lower_map_get(ctx, receiver, args, span);
        }
        "has" => return lower_map_has(ctx, receiver, args, span),
        "keys" => return lower_map_keys(ctx, receiver, args, span),
        "vals" => return lower_map_vals(ctx, receiver, args, span),
        "read_to_string" => return lower_file_read_to_string(ctx, receiver, args, span),
        "write_string" => return lower_file_write_string(ctx, receiver, args, span),
        "write" => {
            // Only dispatch to file_write if the receiver is a File; otherwise
            // fall through to struct method resolution (e.g. WavWriter.write()).
            let recv_ty = infer_expr_type(ctx, receiver);
            let is_file = matches!(&recv_ty, Some(Type::Named(n)) if n == "File")
                || matches!(&recv_ty, Some(Type::Ref(inner, _)) if matches!(inner.as_ref(), Type::Named(n) if n == "File"));
            if is_file {
                return lower_file_write_string(ctx, receiver, args, span);
            }
        }
        "close" => {
            // Only dispatch to file_close if receiver is a File type; otherwise fall through
            // to struct method resolution (e.g., TcpStream.close(), UdpSocket.close())
            let recv_ty = infer_expr_type(ctx, receiver);
            let is_file = matches!(&recv_ty, Some(Type::Named(n)) if n == "File")
                || matches!(&recv_ty, Some(Type::Ref(inner, _)) if matches!(inner.as_ref(), Type::Named(n) if n == "File"));
            if is_file {
                return lower_file_close(ctx, receiver, args, span);
            }
        }
        "concat" => return lower_string_concat(ctx, receiver, args, span),
        "slice" => return lower_string_slice(ctx, receiver, args, span),
        "trim" => return lower_string_trim(ctx, receiver, args, span),
        "split" => return lower_string_split(ctx, receiver, args, span),
        "starts_with" => return lower_string_starts_with(ctx, receiver, args, span),
        "ends_with" => return lower_string_ends_with(ctx, receiver, args, span),
        "into_raw" => {
            if !args.is_empty() {
                ctx.error("into_raw() does not take arguments", Some(span));
                return None;
            }
            return lower_own_into_raw(ctx, receiver, span);
        }
        "clone" => {
            if !args.is_empty() {
                ctx.error(".clone() does not take arguments", Some(span));
                return None;
            }
            // Check if receiver is Shared<T>
            let base_ty = infer_expr_type(ctx, receiver)?;
            if matches!(base_ty, Type::Shared(_)) {
                return lower_shared_clone(ctx, receiver, span);
            }
            let is_string_like = matches!(base_ty, Type::Str | Type::String)
                || matches!(base_ty, Type::Ref(inner, _) if matches!(inner.as_ref(), Type::Str | Type::String));
            if is_string_like {
                return lower_string_clone(ctx, receiver, args, span);
            }
            // If not Shared, fall through to regular method dispatch
        }
        _ => {}
    }

    // 1. Infer receiver type
    let receiver_ty = infer_expr_type(ctx, receiver);
    let receiver_ty = match receiver_ty {
        Some(ty) => ty,
        None => {
            ctx.error(
                "cannot infer type of method receiver; consider adding type annotation",
                Some(span),
            );
            return None;
        }
    };

    // 2. Extract struct name from type (handle both by-value and references)
    let struct_name = match &receiver_ty {
        glyph_core::types::Type::Named(name) => name.clone(),
        glyph_core::types::Type::Ref(inner_ty, _) => {
            // Dereference to get the struct name
            if let glyph_core::types::Type::Named(name) = &**inner_ty {
                name.clone()
            } else {
                ctx.error("methods can only be called on struct types", Some(span));
                return None;
            }
        }
        _ => {
            ctx.error("methods can only be called on struct types", Some(span));
            return None;
        }
    };

    // 3. Look up method (inherent first, then interfaces)
    let mut resolved = ctx
        .resolver
        .get_inherent_method(&struct_name, &method.0)
        .map(|info| (info.function_name.clone(), info.self_kind.clone()));

    if resolved.is_none() {
        if let Some(interfaces) = ctx.resolver.interface_impls.get(&struct_name) {
            let mut found: Option<(String, crate::resolver::MethodInfo)> = None;

            for (iface_name, methods) in interfaces {
                if let Some(info) = methods.get(&method.0) {
                    if let Some((existing_iface, _)) = &found {
                        ctx.error(
                            format!(
                                "method '{}' is provided by multiple interfaces on '{}': '{}' and '{}'",
                                method.0, struct_name, existing_iface, iface_name
                            ),
                            Some(span),
                        );
                        return None;
                    }

                    found = Some((iface_name.clone(), info.clone()));
                }
            }

            if let Some((_, info)) = found {
                resolved = Some((info.function_name.clone(), info.self_kind.clone()));
            }
        }
    }

    let Some((mangled_name, self_kind)) = resolved else {
        ctx.error(
            format!("no method '{}' found on type '{}'", method.0, struct_name),
            Some(span),
        );
        return None;
    };

    // 4. Lower receiver value. Borrowed receivers must not flow through
    // lower_value first, because identifiers there use move semantics.
    let (receiver_arg, temporary_receiver_loan) = match self_kind {
        SelfKind::ByValue => (lower_value(ctx, receiver)?, false),
        SelfKind::Ref => lower_borrowed_method_receiver(
            ctx,
            receiver,
            &receiver_ty,
            Mutability::Immutable,
            span,
        )?,
        SelfKind::MutRef => {
            lower_borrowed_method_receiver(ctx, receiver, &receiver_ty, Mutability::Mutable, span)?
        }
    };

    // 5. Lower remaining arguments
    let sig = ctx.fn_sigs.get(&mangled_name).cloned();
    let receiver_expected = sig
        .as_ref()
        .and_then(|sig| sig.params.first())
        .and_then(|ty| ty.as_ref())
        .cloned();
    if !consume_call_local(ctx, &receiver_arg, span, receiver_expected.as_ref()) {
        return None;
    }
    let receiver_loan_local = if temporary_receiver_loan {
        match &receiver_arg {
            MirValue::Local(local) => Some(*local),
            _ => None,
        }
    } else {
        None
    };
    let mut all_args = vec![receiver_arg];
    let mut temporary_argument_loans = Vec::new();
    for (idx, arg) in args.iter().enumerate() {
        let expected_arg = sig
            .as_ref()
            .and_then(|sig| sig.params.get(idx + 1))
            .and_then(|ty| ty.as_ref());
        let arg_val = lower_value_with_expected(ctx, arg, expected_arg)?;
        if !consume_call_local(ctx, &arg_val, span, expected_arg) {
            return None;
        }
        if let Some(local) = temporary_call_loan(arg, &arg_val) {
            temporary_argument_loans.push(local);
        }
        all_args.push(arg_val);
    }

    // 7. Create call with mangled name (reusing call infrastructure)
    let tmp = ctx.fresh_local(None);
    if let Some(sig) = &sig {
        if let Some(ret) = &sig.ret {
            ctx.locals[tmp.0 as usize].ty = Some(ret.clone());
        } else {
            ctx.locals[tmp.0 as usize].ty = Some(Type::Void);
        }
    }
    ctx.push_inst(MirInst::Assign {
        local: tmp,
        value: Rvalue::Call {
            name: mangled_name,
            args: all_args,
        },
    });
    if let Some(receiver) = receiver_loan_local {
        ctx.release_temporary_call_loan(receiver);
    }
    for loan in temporary_argument_loans {
        ctx.release_temporary_call_loan(loan);
    }

    Some(Rvalue::Move(tmp))
}

fn lower_method_builtin<'a>(
    ctx: &mut LowerCtx<'a>,
    callee: &'a Expr,
    args: &'a [Expr],
    span: Span,
) -> Option<Option<Rvalue>> {
    if let Expr::FieldAccess { base, field, .. } = callee {
        if let Some(result) = lower_thread_method(ctx, base, &field.0, args, span) {
            return Some(result);
        }
        if let Some(result) = lower_spsc_method(ctx, base, &field.0, args, span) {
            return Some(result);
        }
        if let Some(result) = lower_arc_method(ctx, base, &field.0, args, span) {
            return Some(result);
        }
        if let Some(result) = lower_mutex_method(ctx, base, &field.0, args, span) {
            return Some(result);
        }
        if matches!(
            field.0.as_str(),
            "load"
                | "store"
                | "swap"
                | "compare_exchange"
                | "fetch_add"
                | "fetch_sub"
                | "is_lock_free"
        ) {
            return Some(lower_atomic_method(ctx, base, &field.0, args, span));
        }
        match field.0.as_str() {
            "len" => {
                if !args.is_empty() {
                    ctx.error(".len() does not take arguments", Some(span));
                    return Some(None);
                }
                return Some(lower_array_len(ctx, base, span));
            }
            "into_raw" => {
                if !args.is_empty() {
                    ctx.error("into_raw() does not take arguments", Some(span));
                    return Some(None);
                }
                return Some(lower_own_into_raw(ctx, base, span));
            }
            "add" => return Some(lower_map_add(ctx, base, args, span)),
            "update" => return Some(lower_map_update(ctx, base, args, span)),
            "del" => return Some(lower_map_del(ctx, base, args, span)),
            "get" => return Some(lower_map_get(ctx, base, args, span)),
            "has" => return Some(lower_map_has(ctx, base, args, span)),
            "keys" => return Some(lower_map_keys(ctx, base, args, span)),
            "vals" => return Some(lower_map_vals(ctx, base, args, span)),
            _ => {}
        }
    }
    None
}

fn lower_static_builtin_with_expected<'a>(
    ctx: &mut LowerCtx<'a>,
    callee: &'a Expr,
    args: &'a [Expr],
    span: Span,
    expected_ret: Option<&Type>,
) -> Option<Rvalue> {
    let Expr::Ident(name, _) = callee else {
        return None;
    };

    match name.0.as_str() {
        name if is_canonical_spawn(ctx, name) => lower_thread_spawn(ctx, args, span),
        name if is_canonical_scope(ctx, name) => lower_thread_scope(ctx, args, span, expected_ret),
        name if is_canonical_channel(ctx, name) => lower_channel(ctx, args, span, expected_ret),
        name if is_canonical_arc_new(ctx, name) => lower_arc_new(ctx, args, span),
        name if is_canonical_mutex_new(ctx, name) => lower_mutex_new(ctx, args, span),
        "Own::new" => lower_own_new(ctx, args, span),
        "Own::from_raw" => lower_own_from_raw(ctx, args, span),
        "Shared::new" => lower_shared_new(ctx, args, span),
        name if name.starts_with("Atomic") && name.ends_with("::new") => {
            lower_atomic_constructor(ctx, name, args, span)
        }
        "Terminal::stdout" => lower_term_stdout(ctx, args, span),
        "Vec::new" => lower_vec_static_new(ctx, args, span, expected_ret),
        "Vec::with_capacity" => lower_vec_static_with_capacity(ctx, args, span, expected_ret),
        "Map::new" => lower_map_static_new(ctx, args, span, expected_ret),
        "Map::with_capacity" => lower_map_static_with_capacity(ctx, args, span, expected_ret),
        "File::open" => lower_file_open(ctx, args, span, false),
        "File::create" => lower_file_open(ctx, args, span, true),
        "String::from_str" => lower_string_from(ctx, args, span),
        "print" | "std::print" => lower_print_builtin(ctx, args, span, false, false),
        "println" | "std::println" => lower_print_builtin(ctx, args, span, true, false),
        "eprint" | "std::eprint" => lower_print_builtin(ctx, args, span, false, true),
        "eprintln" | "std::eprintln" => lower_print_builtin(ctx, args, span, true, true),
        _ => None,
    }
}
