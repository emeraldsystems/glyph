use glyph_core::ast::{BinaryOp, Expr};
use glyph_core::mir::{LocalId, MirInst, MirValue, Rvalue};
use glyph_core::span::Span;
use glyph_core::thread::{
    canonical_scoped_thread_handle_result, canonical_scoped_thread_handle_type,
    canonical_thread_error_type, canonical_thread_handle_result, canonical_thread_handle_type,
    canonical_thread_scope_type, join_result_type, spawn_result_type,
    unit_thread_status_result_type,
};
use glyph_core::thread_safety::{CallableSendProvenance, ThreadSafetyType};
use glyph_core::types::{BorrowedCallableKind, Type};

use crate::resolver::ResolvedSymbol;

use super::super::call::infer_expr_type;
use super::super::context::{LocalState, LowerCtx};
use super::super::expr::lower_value_with_expected;
use super::super::flow::lower_inferred_borrowed_closure_rvalue;
use super::super::value::infer_value_type;

pub(crate) fn is_canonical_spawn(ctx: &LowerCtx<'_>, name: &str) -> bool {
    matches!(
        ctx.resolver.resolve_symbol(name),
        Some(ResolvedSymbol::Function(module, symbol))
            if module == "std/thread" && symbol == "spawn"
    )
}

pub(crate) fn is_canonical_scope(ctx: &LowerCtx<'_>, name: &str) -> bool {
    matches!(
        ctx.resolver.resolve_symbol(name),
        Some(ResolvedSymbol::Function(module, symbol))
            if module == "std/thread" && symbol == "scope"
    )
}

fn lower_scoped_callable<'a>(
    ctx: &mut LowerCtx<'a>,
    expr: &'a Expr,
    params: Vec<Type>,
    expected_result: Option<Type>,
    scoped_callback: bool,
) -> Option<(LocalId, Type)> {
    if let Some(result) = expected_result {
        let expected = Type::BorrowedFunction {
            kind: BorrowedCallableKind::FnMut,
            params,
            ret: Box::new(result),
        };
        let value = lower_value_with_expected(ctx, expr, Some(&expected))?;
        let ty = infer_value_type(&value, ctx)?;
        let local = local_value(ctx, value, ty.clone());
        return Some((local, ty));
    }
    let value = match expr {
        Expr::Closure {
            capture,
            params: closure_params,
            body,
            span,
        } => {
            let rvalue = lower_inferred_borrowed_closure_rvalue(
                ctx,
                *capture,
                closure_params,
                body,
                *span,
                BorrowedCallableKind::FnMut,
                params,
                scoped_callback,
            )?;
            let signature = match &rvalue {
                Rvalue::MakeBorrowedClosure { signature, .. } => signature.clone(),
                _ => return None,
            };
            let local = ctx.fresh_local(None);
            ctx.locals[local.0 as usize].ty = Some(signature.clone());
            ctx.push_inst(MirInst::Assign {
                local,
                value: rvalue,
            });
            return Some((local, signature));
        }
        _ => lower_value_with_expected(ctx, expr, None)?,
    };
    let Some(ty) = infer_value_type(&value, ctx) else {
        ctx.error("scoped callable type is unknown", None);
        return None;
    };
    let local = local_value(ctx, value, ty.clone());
    Some((local, ty))
}

fn local_value(ctx: &mut LowerCtx<'_>, value: MirValue, ty: Type) -> LocalId {
    if let MirValue::Local(local) = value {
        return local;
    }
    let local = ctx.fresh_local(None);
    ctx.locals[local.0 as usize].ty = Some(ty);
    let value = match value {
        MirValue::Int(value) => Rvalue::ConstInt(value),
        MirValue::Float(value) => Rvalue::ConstFloat(value),
        MirValue::Bool(value) => Rvalue::ConstBool(value),
        MirValue::Unit => Rvalue::ConstInt(0),
        MirValue::Local(_) => unreachable!(),
    };
    ctx.push_inst(MirInst::Assign { local, value });
    local
}

fn emit_status_result(
    ctx: &mut LowerCtx<'_>,
    status: LocalId,
    success_payload: Option<MirValue>,
    result_type: Type,
) -> Rvalue {
    let is_ok = ctx.fresh_local(None);
    ctx.locals[is_ok.0 as usize].ty = Some(Type::Bool);
    ctx.push_inst(MirInst::Assign {
        local: is_ok,
        value: Rvalue::Binary {
            op: BinaryOp::Eq,
            lhs: MirValue::Local(status),
            rhs: MirValue::Int(0),
        },
    });
    let ok_block = ctx.new_block();
    let error_block = ctx.new_block();
    let join_block = ctx.new_block();
    let result = ctx.fresh_local(None);
    ctx.locals[result.0 as usize].ty = Some(result_type);
    ctx.push_inst(MirInst::If {
        cond: MirValue::Local(is_ok),
        then_bb: ok_block,
        else_bb: error_block,
    });

    ctx.switch_to(ok_block);
    ctx.push_inst(MirInst::Assign {
        local: result,
        value: Rvalue::EnumConstruct {
            enum_name: "Result".into(),
            variant_index: 0,
            payload: success_payload,
        },
    });
    ctx.push_inst(MirInst::Goto(join_block));

    ctx.local_states[result.0 as usize] = LocalState::Uninitialized;
    ctx.switch_to(error_block);
    let error = ctx.fresh_local(None);
    ctx.locals[error.0 as usize].ty = Some(canonical_thread_error_type());
    ctx.push_inst(MirInst::Assign {
        local: error,
        value: Rvalue::ThreadErrorFromStatus {
            status: MirValue::Local(status),
        },
    });
    ctx.push_inst(MirInst::Assign {
        local: result,
        value: Rvalue::EnumConstruct {
            enum_name: "Result".into(),
            variant_index: 1,
            payload: Some(MirValue::Local(error)),
        },
    });
    ctx.push_inst(MirInst::Goto(join_block));
    ctx.local_states[result.0 as usize] = LocalState::Initialized;
    ctx.switch_to(join_block);
    Rvalue::Move(result)
}

pub(crate) fn lower_thread_scope<'a>(
    ctx: &mut LowerCtx<'a>,
    args: &'a [Expr],
    span: Span,
    expected_ret: Option<&Type>,
) -> Option<Rvalue> {
    if args.len() != 1 {
        ctx.error(
            "std::thread::scope expects one FnMut(Scope) -> R callback",
            Some(span),
        );
        return Some(Rvalue::ConstInt(0));
    }

    let expected_body_result = match expected_ret {
        Some(Type::App { base, args })
            if base.rsplit("::").next() == Some("Result") && args.len() == 2 =>
        {
            Some(args[0].clone())
        }
        _ => None,
    };
    let (body, body_type) = lower_scoped_callable(
        ctx,
        &args[0],
        vec![canonical_thread_scope_type()],
        expected_body_result,
        true,
    )?;
    let Some((params, body_result)) = body_type.function_signature() else {
        ctx.error(
            "std::thread::scope expects an Fn or FnMut callback",
            Some(span),
        );
        return Some(Rvalue::ConstInt(0));
    };
    if params != [canonical_thread_scope_type()] {
        ctx.error(
            format!(
                "std::thread::scope callback must take exactly one Scope argument, found '{}'",
                LowerCtx::type_label(&body_type)
            ),
            Some(span),
        );
        return Some(Rvalue::ConstInt(0));
    }
    let body_result = body_result.clone();

    let raw_scope = ctx.fresh_local(None);
    ctx.locals[raw_scope.0 as usize].ty = Some(glyph_core::thread::private_thread_scope_type());
    let create_status = ctx.fresh_local(None);
    ctx.locals[create_status.0 as usize].ty = Some(Type::I32);
    ctx.push_inst(MirInst::Assign {
        local: create_status,
        value: Rvalue::ThreadScopeCreate {
            out_scope: raw_scope,
        },
    });

    let is_ok = ctx.fresh_local(None);
    ctx.locals[is_ok.0 as usize].ty = Some(Type::Bool);
    ctx.push_inst(MirInst::Assign {
        local: is_ok,
        value: Rvalue::Binary {
            op: BinaryOp::Eq,
            lhs: MirValue::Local(create_status),
            rhs: MirValue::Int(0),
        },
    });
    let ok_block = ctx.new_block();
    let error_block = ctx.new_block();
    let join_block = ctx.new_block();
    let result_type = join_result_type(body_result.clone());
    let result = ctx.fresh_local(None);
    ctx.locals[result.0 as usize].ty = Some(result_type);
    ctx.push_inst(MirInst::If {
        cond: MirValue::Local(is_ok),
        then_bb: ok_block,
        else_bb: error_block,
    });

    ctx.switch_to(ok_block);
    let scope = ctx.fresh_local(None);
    ctx.locals[scope.0 as usize].ty = Some(canonical_thread_scope_type());
    ctx.locals[scope.0 as usize].skip_drop = true;
    ctx.push_inst(MirInst::Assign {
        local: scope,
        value: Rvalue::ThreadScopeFromRaw { raw: raw_scope },
    });
    let callback_result = ctx.fresh_local(None);
    ctx.locals[callback_result.0 as usize].ty = Some(body_result.clone());
    ctx.push_inst(MirInst::Assign {
        local: callback_result,
        value: match body_type {
            Type::BorrowedFunction {
                kind: BorrowedCallableKind::Fn,
                ..
            } => Rvalue::CallIndirectShared {
                callee: body,
                signature: body_type.clone(),
                args: vec![MirValue::Local(scope)],
            },
            Type::BorrowedFunction {
                kind: BorrowedCallableKind::FnMut,
                ..
            } => Rvalue::CallIndirectMut {
                callee: body,
                signature: body_type.clone(),
                args: vec![MirValue::Local(scope)],
            },
            _ => {
                ctx.error(
                    "std::thread::scope does not accept an owned FnOnce callback",
                    Some(span),
                );
                return Some(Rvalue::ConstInt(0));
            }
        },
    });
    ctx.push_inst(MirInst::DropThreadScope(raw_scope));
    ctx.local_states[raw_scope.0 as usize] = LocalState::Moved;
    ctx.push_inst(MirInst::Assign {
        local: result,
        value: Rvalue::EnumConstruct {
            enum_name: "Result".into(),
            variant_index: 0,
            payload: Some(MirValue::Local(callback_result)),
        },
    });
    ctx.push_inst(MirInst::Goto(join_block));

    ctx.local_states[result.0 as usize] = LocalState::Uninitialized;
    ctx.local_states[raw_scope.0 as usize] = LocalState::Uninitialized;
    ctx.switch_to(error_block);
    ctx.push_inst(MirInst::DropThreadScope(raw_scope));
    ctx.local_states[raw_scope.0 as usize] = LocalState::Moved;
    let error = ctx.fresh_local(None);
    ctx.locals[error.0 as usize].ty = Some(canonical_thread_error_type());
    ctx.push_inst(MirInst::Assign {
        local: error,
        value: Rvalue::ThreadErrorFromStatus {
            status: MirValue::Local(create_status),
        },
    });
    ctx.push_inst(MirInst::Assign {
        local: result,
        value: Rvalue::EnumConstruct {
            enum_name: "Result".into(),
            variant_index: 1,
            payload: Some(MirValue::Local(error)),
        },
    });
    ctx.push_inst(MirInst::Goto(join_block));
    ctx.local_states[result.0 as usize] = LocalState::Initialized;
    ctx.switch_to(join_block);
    Some(Rvalue::Move(result))
}

pub(crate) fn lower_thread_spawn<'a>(
    ctx: &mut LowerCtx<'a>,
    args: &'a [Expr],
    span: Span,
) -> Option<Rvalue> {
    if args.len() != 1 {
        ctx.error(
            "std::thread::spawn expects one FnOnce() -> T task",
            Some(span),
        );
        return Some(Rvalue::ConstInt(0));
    }
    // `spawn` is a compiler intrinsic because Glyph does not yet have generic
    // functions. Lower without the unit placeholder signature, then specialize
    // from the concrete callable result.
    let task_value = lower_value_with_expected(ctx, &args[0], None)?;
    let Some(task_type) = infer_value_type(&task_value, ctx) else {
        ctx.error("std::thread::spawn task type is unknown", Some(span));
        return Some(Rvalue::ConstInt(0));
    };
    let Some((params, result)) = task_type.function_signature() else {
        ctx.error(
            format!(
                "std::thread::spawn expects FnOnce() -> T, found '{}'",
                LowerCtx::type_label(&task_type)
            ),
            Some(span),
        );
        return Some(Rvalue::ConstInt(0));
    };
    if !params.is_empty() {
        ctx.error(
            format!(
                "std::thread::spawn task must take no arguments, found '{}'",
                LowerCtx::type_label(&task_type)
            ),
            Some(span),
        );
        return Some(Rvalue::ConstInt(0));
    }
    let result_type = if matches!(result, Type::Void)
        || matches!(result, Type::Tuple(elements) if elements.is_empty())
    {
        Type::Void
    } else {
        result.clone()
    };
    let task = local_value(ctx, task_value, task_type.clone());
    let provenance = ctx
        .callable_provenance
        .get(&task)
        .cloned()
        .unwrap_or_else(|| CallableSendProvenance::Unknown {
            reason: "callable reached spawn without a compiler provenance certificate".into(),
        });
    let checked_task = ThreadSafetyType::callable(task_type, provenance);
    let checked_result = ctx.thread_safety_type(&result_type);
    if let Err(error) =
        ctx.thread_safety_registry
            .validate_spawn_then(&checked_task, &checked_result, || ())
    {
        ctx.error(error.to_string(), Some(span));
        return Some(Rvalue::ConstInt(0));
    }
    if !matches!(
        ctx.local_states.get(task.0 as usize),
        Some(LocalState::Moved)
    ) && !ctx.consume_local(task, Some(span))
    {
        return Some(Rvalue::ConstInt(0));
    }
    let raw_handle = ctx.fresh_thread_handle_local();
    ctx.local_states[raw_handle.0 as usize] = LocalState::Initialized;
    let status = ctx.fresh_local(None);
    ctx.locals[status.0 as usize].ty = Some(Type::I32);
    ctx.push_inst(MirInst::Assign {
        local: status,
        value: if matches!(result_type, Type::Void) {
            Rvalue::ThreadSpawnUnit {
                task,
                out_handle: raw_handle,
            }
        } else {
            Rvalue::ThreadSpawnResult {
                task,
                out_handle: raw_handle,
                result_type: result_type.clone(),
            }
        },
    });
    let public_handle_type = canonical_thread_handle_type(result_type.clone());
    let public_handle = ctx.fresh_local(None);
    ctx.locals[public_handle.0 as usize].ty = Some(public_handle_type);
    ctx.push_inst(MirInst::Assign {
        local: public_handle,
        value: Rvalue::ThreadHandleFromRaw { raw: raw_handle },
    });
    Some(emit_status_result(
        ctx,
        status,
        Some(MirValue::Local(public_handle)),
        spawn_result_type(result_type),
    ))
}

fn lower_scoped_spawn<'a>(
    ctx: &mut LowerCtx<'a>,
    receiver: &'a Expr,
    args: &'a [Expr],
    span: Span,
) -> Option<Rvalue> {
    if args.len() != 1 {
        ctx.error("Scope::spawn expects one Fn/FnMut() -> T task", Some(span));
        return Some(Rvalue::ConstInt(0));
    }
    let Expr::Ident(scope_name, _) = receiver else {
        ctx.error(
            "Scope::spawn requires a direct lexical Scope binding",
            Some(span),
        );
        return Some(Rvalue::ConstInt(0));
    };
    let Some(scope) = ctx.bindings.get(&scope_name.0).copied() else {
        return None;
    };
    if !ctx.check_local_available(scope, Some(span)) {
        return Some(Rvalue::ConstInt(0));
    }

    let (task, task_type) = lower_scoped_callable(ctx, &args[0], Vec::new(), None, false)?;
    let Some((params, result_type)) = task_type.function_signature() else {
        ctx.error("Scope::spawn expects an Fn or FnMut task", Some(span));
        return Some(Rvalue::ConstInt(0));
    };
    if !params.is_empty() {
        ctx.error(
            format!(
                "Scope::spawn task must take no arguments, found '{}'",
                LowerCtx::type_label(&task_type)
            ),
            Some(span),
        );
        return Some(Rvalue::ConstInt(0));
    }
    let result_type = if matches!(result_type, Type::Void)
        || matches!(result_type, Type::Tuple(elements) if elements.is_empty())
    {
        Type::Void
    } else {
        result_type.clone()
    };
    let checked_result = ctx.thread_safety_type(&result_type);
    if let Err(error) = ctx
        .thread_safety_registry
        .check_send("scoped task result", &checked_result)
    {
        ctx.error(error.to_string(), Some(span));
        return Some(Rvalue::ConstInt(0));
    }
    if !ctx.validate_scoped_task_captures(task, span) {
        return Some(Rvalue::ConstInt(0));
    }
    if !ctx.reserve_scoped_task_loans(scope, task, span) {
        return Some(Rvalue::ConstInt(0));
    }

    let raw_handle = ctx.fresh_scoped_thread_handle_local(result_type.clone());
    ctx.local_states[raw_handle.0 as usize] = LocalState::Initialized;
    let status = ctx.fresh_local(None);
    ctx.locals[status.0 as usize].ty = Some(Type::I32);
    ctx.push_inst(MirInst::Assign {
        local: status,
        value: if matches!(result_type, Type::Void) {
            Rvalue::ScopedThreadSpawnUnit {
                scope,
                task,
                out_handle: raw_handle,
            }
        } else {
            Rvalue::ScopedThreadSpawnResult {
                scope,
                task,
                out_handle: raw_handle,
                result_type: result_type.clone(),
            }
        },
    });
    let public_handle = ctx.fresh_local(None);
    ctx.locals[public_handle.0 as usize].ty =
        Some(canonical_scoped_thread_handle_type(result_type.clone()));
    ctx.mark_scoped_thread_handle(public_handle);
    ctx.push_inst(MirInst::Assign {
        local: public_handle,
        value: Rvalue::ScopedThreadHandleFromRaw {
            raw: raw_handle,
            result_type: result_type.clone(),
        },
    });
    ctx.local_states[raw_handle.0 as usize] = LocalState::Moved;
    Some(emit_status_result(
        ctx,
        status,
        Some(MirValue::Local(public_handle)),
        spawn_result_type_for_scoped(result_type),
    ))
}

fn spawn_result_type_for_scoped(result: Type) -> Type {
    Type::App {
        base: "Result".into(),
        args: vec![
            canonical_scoped_thread_handle_type(result),
            canonical_thread_error_type(),
        ],
    }
}

fn lower_scoped_join<'a>(
    ctx: &mut LowerCtx<'a>,
    receiver: &'a Expr,
    receiver_type: Type,
    result_type: Type,
    args: &'a [Expr],
    span: Span,
) -> Option<Rvalue> {
    if !args.is_empty() {
        ctx.error("ScopedJoinHandle::join takes no arguments", Some(span));
        return Some(Rvalue::ConstInt(0));
    }
    let value = lower_value_with_expected(ctx, receiver, Some(&receiver_type))?;
    let handle = local_value(ctx, value, receiver_type);
    if !matches!(
        ctx.local_states.get(handle.0 as usize),
        Some(LocalState::Moved)
    ) && !ctx.consume_local(handle, Some(span))
    {
        return Some(Rvalue::ConstInt(0));
    }
    let raw_handle = ctx.fresh_scoped_thread_handle_local(result_type.clone());
    ctx.push_inst(MirInst::Assign {
        local: raw_handle,
        value: Rvalue::ScopedThreadHandleIntoRaw {
            handle,
            result_type: result_type.clone(),
        },
    });
    let status = ctx.fresh_local(None);
    ctx.locals[status.0 as usize].ty = Some(Type::I32);
    if matches!(result_type, Type::Void) {
        ctx.push_inst(MirInst::Assign {
            local: status,
            value: Rvalue::ScopedThreadJoinUnit { handle: raw_handle },
        });
        ctx.push_inst(MirInst::DropScopedThreadHandle(raw_handle));
        ctx.local_states[raw_handle.0 as usize] = LocalState::Moved;
        return Some(emit_status_result(
            ctx,
            status,
            Some(MirValue::Unit),
            unit_thread_status_result_type(),
        ));
    }
    let result = ctx.fresh_local(None);
    ctx.locals[result.0 as usize].ty = Some(result_type.clone());
    ctx.local_states[result.0 as usize] = LocalState::Uninitialized;
    ctx.push_inst(MirInst::Assign {
        local: status,
        value: Rvalue::ScopedThreadJoinResult {
            handle: raw_handle,
            out_result: result,
            result_type: result_type.clone(),
        },
    });
    ctx.push_inst(MirInst::DropScopedThreadHandle(raw_handle));
    ctx.local_states[raw_handle.0 as usize] = LocalState::Moved;
    Some(emit_status_result(
        ctx,
        status,
        Some(MirValue::Local(result)),
        join_result_type(result_type),
    ))
}

pub(crate) fn lower_thread_method<'a>(
    ctx: &mut LowerCtx<'a>,
    receiver: &'a Expr,
    method: &str,
    args: &'a [Expr],
    span: Span,
) -> Option<Option<Rvalue>> {
    let receiver_type = infer_expr_type(ctx, receiver)?;
    if receiver_type == canonical_thread_scope_type() && method == "spawn" {
        return Some(lower_scoped_spawn(ctx, receiver, args, span));
    }
    if let Some(result_type) = canonical_scoped_thread_handle_result(&receiver_type).cloned() {
        if method == "join" {
            return Some(lower_scoped_join(
                ctx,
                receiver,
                receiver_type,
                result_type,
                args,
                span,
            ));
        }
        if method == "detach" {
            ctx.error(
                "ScopedJoinHandle cannot be detached; scoped children must join before scope exit",
                Some(span),
            );
            return Some(Some(Rvalue::ConstInt(0)));
        }
    }
    if !matches!(method, "join" | "detach") || !args.is_empty() {
        return None;
    }
    let Some(result_type) = canonical_thread_handle_result(&receiver_type).cloned() else {
        return None;
    };
    let value = lower_value_with_expected(ctx, receiver, Some(&receiver_type))?;
    let handle = local_value(ctx, value, receiver_type);
    if !matches!(
        ctx.local_states.get(handle.0 as usize),
        Some(LocalState::Moved)
    ) && !ctx.consume_local(handle, Some(span))
    {
        return Some(Some(Rvalue::ConstInt(0)));
    }
    let raw_handle = ctx.fresh_thread_handle_local();
    ctx.push_inst(MirInst::Assign {
        local: raw_handle,
        value: Rvalue::ThreadHandleIntoRaw { handle },
    });
    let status = ctx.fresh_local(None);
    ctx.locals[status.0 as usize].ty = Some(Type::I32);
    if method == "detach" {
        ctx.push_inst(MirInst::Assign {
            local: status,
            value: Rvalue::ThreadDetachUnit { handle: raw_handle },
        });
        ctx.push_inst(MirInst::DropThreadHandle(raw_handle));
        ctx.local_states[raw_handle.0 as usize] = LocalState::Moved;
        return Some(Some(emit_status_result(
            ctx,
            status,
            Some(MirValue::Unit),
            unit_thread_status_result_type(),
        )));
    }

    if matches!(result_type, Type::Void)
        || matches!(&result_type, Type::Tuple(elements) if elements.is_empty())
    {
        ctx.push_inst(MirInst::Assign {
            local: status,
            value: Rvalue::ThreadJoinUnit { handle: raw_handle },
        });
        ctx.push_inst(MirInst::DropThreadHandle(raw_handle));
        ctx.local_states[raw_handle.0 as usize] = LocalState::Moved;
        return Some(Some(emit_status_result(
            ctx,
            status,
            Some(MirValue::Unit),
            unit_thread_status_result_type(),
        )));
    }

    let result = ctx.fresh_local(None);
    ctx.locals[result.0 as usize].ty = Some(result_type.clone());
    ctx.local_states[result.0 as usize] = LocalState::Uninitialized;
    ctx.push_inst(MirInst::Assign {
        local: status,
        value: Rvalue::ThreadJoinResult {
            handle: raw_handle,
            out_result: result,
            result_type: result_type.clone(),
        },
    });
    ctx.push_inst(MirInst::DropThreadHandle(raw_handle));
    ctx.local_states[raw_handle.0 as usize] = LocalState::Moved;
    Some(Some(emit_status_result(
        ctx,
        status,
        Some(MirValue::Local(result)),
        join_result_type(result_type),
    )))
}
