use glyph_core::ast::{BinaryOp, Expr};
use glyph_core::mir::{LocalId, MirInst, MirValue, Rvalue};
use glyph_core::span::Span;
use glyph_core::thread::{
    canonical_thread_error_type, canonical_thread_handle_result, canonical_thread_handle_type,
    join_result_type, spawn_result_type, unit_thread_status_result_type,
};
use glyph_core::thread_safety::{CallableSendProvenance, ThreadSafetyType};
use glyph_core::types::Type;

use crate::resolver::ResolvedSymbol;

use super::super::call::infer_expr_type;
use super::super::context::{LocalState, LowerCtx};
use super::super::expr::lower_value_with_expected;
use super::super::value::infer_value_type;

pub(crate) fn is_canonical_spawn(ctx: &LowerCtx<'_>, name: &str) -> bool {
    matches!(
        ctx.resolver.resolve_symbol(name),
        Some(ResolvedSymbol::Function(module, symbol))
            if module == "std/thread" && symbol == "spawn"
    )
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

pub(crate) fn lower_thread_method<'a>(
    ctx: &mut LowerCtx<'a>,
    receiver: &'a Expr,
    method: &str,
    args: &'a [Expr],
    span: Span,
) -> Option<Option<Rvalue>> {
    if !matches!(method, "join" | "detach") || !args.is_empty() {
        return None;
    }
    let receiver_type = infer_expr_type(ctx, receiver)?;
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
