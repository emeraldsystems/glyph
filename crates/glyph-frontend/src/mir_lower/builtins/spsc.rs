use glyph_core::ast::{BinaryOp, Expr};
use glyph_core::mir::{LocalId, MirInst, MirValue, Rvalue};
use glyph_core::span::Span;
use glyph_core::types::{Mutability, Type};

use crate::resolver::ResolvedSymbol;

use super::super::context::{LocalState, LowerCtx};
use super::super::expr::lower_value_with_expected;
use super::super::types::tuple_struct_name;
use super::super::value::infer_value_type;

const SEND_RESULT: &str = "TrySendResult";
const RECV_RESULT: &str = "TryRecvResult";

pub(crate) fn is_canonical_channel(ctx: &LowerCtx<'_>, name: &str) -> bool {
    matches!(
        ctx.resolver.resolve_symbol(name),
        Some(ResolvedSymbol::Function(module, symbol))
            if module == "std/sync/spsc" && symbol == "channel"
    )
}

fn contains_borrow(ty: &Type) -> bool {
    match ty {
        Type::Ref(_, _) | Type::BorrowedFunction { .. } => true,
        Type::Array(inner, _) | Type::Own(inner) | Type::RawPtr(inner) | Type::Shared(inner) => {
            contains_borrow(inner)
        }
        Type::App { args, .. } | Type::Tuple(args) | Type::Function { params: args, .. } => {
            args.iter().any(contains_borrow)
        }
        _ => false,
    }
}

fn validate_elem(ctx: &mut LowerCtx<'_>, elem: &Type, span: Span) -> bool {
    if contains_borrow(elem) {
        ctx.error(
            "SPSC channels cannot carry borrowed references or borrowed callables; move owned data into the channel",
            Some(span),
        );
        return false;
    }
    if matches!(elem, Type::Void) || matches!(elem, Type::Tuple(elements) if elements.is_empty()) {
        ctx.error(
            "SPSC channels require an addressable non-unit item type",
            Some(span),
        );
        return false;
    }
    true
}

fn channel_elem_from_expected(expected: Option<&Type>) -> Option<Type> {
    match expected? {
        ty if ty.is_spsc_sender() => ty.spsc_sender_inner_type().cloned(),
        Type::Tuple(endpoints) if endpoints.len() == 2 => {
            let sender = endpoints[0].spsc_sender_inner_type()?;
            let receiver = endpoints[1].spsc_receiver_inner_type()?;
            (sender == receiver).then(|| sender.clone())
        }
        _ => None,
    }
}

fn capacity_value<'a>(
    ctx: &mut LowerCtx<'a>,
    expression: &'a Expr,
    span: Span,
) -> Option<MirValue> {
    let value = lower_value_with_expected(ctx, expression, Some(&Type::Usize))?;
    let actual = infer_value_type(&value, ctx)?;
    if !actual.is_int() {
        ctx.error("SPSC channel capacity must be an integer", Some(span));
        return None;
    }
    Some(value)
}

fn receiver_output_local(
    ctx: &mut LowerCtx<'_>,
    expression: &Expr,
    elem_type: &Type,
    span: Span,
) -> Option<LocalId> {
    let Expr::Ref {
        expr,
        mutability: Mutability::Mutable,
        ..
    } = expression
    else {
        ctx.error(
            "the second channel argument must be `&mut receiver`",
            Some(span),
        );
        return None;
    };
    let Expr::Ident(name, receiver_span) = expr.as_ref() else {
        ctx.error("channel receiver output must be a direct local", Some(span));
        return None;
    };
    let Some(local) = ctx.bindings.get(name.0.as_str()).copied() else {
        ctx.error(
            format!("unknown receiver output `{}`", name.0),
            Some(*receiver_span),
        );
        return None;
    };
    let expected = Type::spsc_receiver(elem_type.clone());
    if ctx.local_ty(local) != Some(&expected) {
        ctx.error(
            format!(
                "channel receiver output has type '{}', expected '{}'",
                ctx.local_ty(local)
                    .map(LowerCtx::type_label)
                    .unwrap_or_else(|| "unknown".into()),
                LowerCtx::type_label(&expected)
            ),
            Some(*receiver_span),
        );
        return None;
    }
    if !ctx.locals[local.0 as usize].mutable {
        ctx.error(
            "channel receiver output must be declared `mut`",
            Some(*receiver_span),
        );
        return None;
    }
    if !matches!(
        ctx.local_states[local.0 as usize],
        LocalState::Uninitialized
    ) {
        ctx.error(
            "channel receiver output must be uninitialized to avoid overwriting an endpoint",
            Some(*receiver_span),
        );
        return None;
    }
    Some(local)
}

pub(crate) fn lower_channel<'a>(
    ctx: &mut LowerCtx<'a>,
    args: &'a [Expr],
    span: Span,
    expected: Option<&Type>,
) -> Option<Rvalue> {
    if !matches!(args.len(), 1 | 2) {
        ctx.error(
            "channel expects capacity, optionally followed by `&mut Receiver<T>`",
            Some(span),
        );
        return Some(Rvalue::ConstInt(0));
    }
    let Some(elem_type) = channel_elem_from_expected(expected) else {
        ctx.error(
            "channel item type is ambiguous; annotate `(Sender<T>, Receiver<T>)` or `Sender<T>`",
            Some(span),
        );
        return Some(Rvalue::ConstInt(0));
    };
    if !validate_elem(ctx, &elem_type, span) {
        return Some(Rvalue::ConstInt(0));
    }
    let capacity = capacity_value(ctx, &args[0], span)?;

    if args.len() == 2 {
        let receiver = receiver_output_local(ctx, &args[1], &elem_type, span)?;
        let sender = ctx.fresh_local(None);
        ctx.locals[sender.0 as usize].ty = Some(Type::spsc_sender(elem_type.clone()));
        ctx.push_inst(MirInst::Assign {
            local: sender,
            value: Rvalue::SpscChannelNew {
                capacity,
                out_receiver: receiver,
                elem_type,
            },
        });
        ctx.local_states[receiver.0 as usize] = LocalState::Initialized;
        return Some(Rvalue::Move(sender));
    }

    let receiver = ctx.fresh_local(None);
    ctx.locals[receiver.0 as usize].ty = Some(Type::spsc_receiver(elem_type.clone()));
    let sender = ctx.fresh_local(None);
    ctx.locals[sender.0 as usize].ty = Some(Type::spsc_sender(elem_type));
    ctx.push_inst(MirInst::Assign {
        local: sender,
        value: Rvalue::SpscChannelNew {
            capacity,
            out_receiver: receiver,
            elem_type: ctx
                .local_ty(sender)
                .and_then(Type::spsc_sender_inner_type)
                .expect("constructed sender")
                .clone(),
        },
    });
    ctx.local_states[receiver.0 as usize] = LocalState::Initialized;
    let tuple_type = Type::Tuple(vec![
        ctx.local_ty(sender).expect("typed sender").clone(),
        ctx.local_ty(receiver).expect("typed receiver").clone(),
    ]);
    let tuple = ctx.fresh_local(None);
    ctx.locals[tuple.0 as usize].ty = Some(tuple_type.clone());
    ctx.push_inst(MirInst::Assign {
        local: tuple,
        value: Rvalue::StructLit {
            struct_name: match &tuple_type {
                Type::Tuple(elements) => tuple_struct_name(elements),
                _ => unreachable!(),
            },
            field_values: vec![
                ("0".into(), MirValue::Local(sender)),
                ("1".into(), MirValue::Local(receiver)),
            ],
        },
    });
    Some(Rvalue::Move(tuple))
}

fn endpoint_receiver(ctx: &mut LowerCtx<'_>, receiver: &Expr) -> Option<(LocalId, Type, bool)> {
    let Expr::Ident(name, span) = receiver else {
        return None;
    };
    let local = ctx.bindings.get(name.0.as_str()).copied()?;
    if !ctx.check_local_available(local, Some(*span)) {
        return None;
    }
    let ty = ctx.local_ty(local)?.clone();
    if let Some(elem) = ty.spsc_sender_inner_type() {
        Some((local, elem.clone(), true))
    } else {
        ty.spsc_receiver_inner_type()
            .cloned()
            .map(|elem| (local, elem, false))
    }
}

fn localize_value(ctx: &mut LowerCtx<'_>, value: MirValue, elem_type: &Type) -> LocalId {
    if let MirValue::Local(local) = value {
        return local;
    }
    let local = ctx.fresh_local(None);
    ctx.locals[local.0 as usize].ty = Some(elem_type.clone());
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

fn branch_on_status(
    ctx: &mut LowerCtx<'_>,
    status: LocalId,
    value: LocalId,
    result_type: Type,
    enum_name: &str,
    value_variant: u32,
    empty_variant: u32,
    disconnected_variant: u32,
) -> Rvalue {
    let is_value = ctx.fresh_local(None);
    ctx.locals[is_value.0 as usize].ty = Some(Type::Bool);
    ctx.push_inst(MirInst::Assign {
        local: is_value,
        value: Rvalue::Binary {
            op: BinaryOp::Eq,
            lhs: MirValue::Local(status),
            rhs: MirValue::Int(0),
        },
    });
    let value_block = ctx.new_block();
    let nonvalue_block = ctx.new_block();
    let empty_block = ctx.new_block();
    let disconnected_block = ctx.new_block();
    let join_block = ctx.new_block();
    let result = ctx.fresh_local(None);
    ctx.locals[result.0 as usize].ty = Some(result_type);
    ctx.push_inst(MirInst::If {
        cond: MirValue::Local(is_value),
        then_bb: value_block,
        else_bb: nonvalue_block,
    });

    ctx.switch_to(value_block);
    ctx.local_states[value.0 as usize] = LocalState::Initialized;
    ctx.push_inst(MirInst::Assign {
        local: result,
        value: Rvalue::EnumConstruct {
            enum_name: enum_name.into(),
            variant_index: value_variant,
            payload: Some(MirValue::Local(value)),
        },
    });
    ctx.push_inst(MirInst::Goto(join_block));

    ctx.local_states[result.0 as usize] = LocalState::Uninitialized;
    ctx.switch_to(nonvalue_block);
    let is_empty = ctx.fresh_local(None);
    ctx.locals[is_empty.0 as usize].ty = Some(Type::Bool);
    ctx.push_inst(MirInst::Assign {
        local: is_empty,
        value: Rvalue::Binary {
            op: BinaryOp::Eq,
            lhs: MirValue::Local(status),
            rhs: MirValue::Int(1),
        },
    });
    ctx.push_inst(MirInst::If {
        cond: MirValue::Local(is_empty),
        then_bb: empty_block,
        else_bb: disconnected_block,
    });

    ctx.switch_to(empty_block);
    ctx.push_inst(MirInst::Assign {
        local: result,
        value: Rvalue::EnumConstruct {
            enum_name: enum_name.into(),
            variant_index: empty_variant,
            payload: None,
        },
    });
    ctx.push_inst(MirInst::Goto(join_block));

    ctx.local_states[result.0 as usize] = LocalState::Uninitialized;
    ctx.switch_to(disconnected_block);
    ctx.push_inst(MirInst::Assign {
        local: result,
        value: Rvalue::EnumConstruct {
            enum_name: enum_name.into(),
            variant_index: disconnected_variant,
            payload: None,
        },
    });
    ctx.push_inst(MirInst::Goto(join_block));
    ctx.local_states[result.0 as usize] = LocalState::Initialized;
    ctx.local_states[value.0 as usize] = LocalState::Moved;
    ctx.switch_to(join_block);
    Rvalue::Move(result)
}

fn lower_try_send<'a>(
    ctx: &mut LowerCtx<'a>,
    sender: LocalId,
    elem_type: Type,
    args: &'a [Expr],
    span: Span,
) -> Option<Rvalue> {
    if args.len() != 1 {
        ctx.error("Sender.try_send expects exactly one value", Some(span));
        return None;
    }
    let value = lower_value_with_expected(ctx, &args[0], Some(&elem_type))?;
    let actual = infer_value_type(&value, ctx)?;
    if actual != elem_type {
        ctx.error(
            format!(
                "Sender.try_send value has type '{}', expected '{}'",
                LowerCtx::type_label(&actual),
                LowerCtx::type_label(&elem_type)
            ),
            Some(span),
        );
        return None;
    }
    let value = localize_value(ctx, value, &elem_type);
    let unsent = ctx.fresh_local(None);
    ctx.locals[unsent.0 as usize].ty = Some(elem_type.clone());
    let status = ctx.fresh_local(None);
    ctx.locals[status.0 as usize].ty = Some(Type::I32);
    ctx.push_inst(MirInst::Assign {
        local: status,
        value: Rvalue::SpscTrySend {
            sender,
            value,
            out_unsent: unsent,
            elem_type: elem_type.clone(),
        },
    });

    // Status 0 is Sent (no payload); statuses 1 and 2 preserve ownership.
    let is_sent = ctx.fresh_local(None);
    ctx.locals[is_sent.0 as usize].ty = Some(Type::Bool);
    ctx.push_inst(MirInst::Assign {
        local: is_sent,
        value: Rvalue::Binary {
            op: BinaryOp::Eq,
            lhs: MirValue::Local(status),
            rhs: MirValue::Int(0),
        },
    });
    let sent_block = ctx.new_block();
    let failure_block = ctx.new_block();
    let full_block = ctx.new_block();
    let disconnected_block = ctx.new_block();
    let join_block = ctx.new_block();
    let result = ctx.fresh_local(None);
    ctx.locals[result.0 as usize].ty = Some(Type::App {
        base: SEND_RESULT.into(),
        args: vec![elem_type],
    });
    ctx.push_inst(MirInst::If {
        cond: MirValue::Local(is_sent),
        then_bb: sent_block,
        else_bb: failure_block,
    });
    ctx.switch_to(sent_block);
    ctx.push_inst(MirInst::Assign {
        local: result,
        value: Rvalue::EnumConstruct {
            enum_name: SEND_RESULT.into(),
            variant_index: 0,
            payload: None,
        },
    });
    ctx.push_inst(MirInst::Goto(join_block));

    ctx.local_states[result.0 as usize] = LocalState::Uninitialized;
    ctx.switch_to(failure_block);
    let is_full = ctx.fresh_local(None);
    ctx.locals[is_full.0 as usize].ty = Some(Type::Bool);
    ctx.push_inst(MirInst::Assign {
        local: is_full,
        value: Rvalue::Binary {
            op: BinaryOp::Eq,
            lhs: MirValue::Local(status),
            rhs: MirValue::Int(1),
        },
    });
    ctx.push_inst(MirInst::If {
        cond: MirValue::Local(is_full),
        then_bb: full_block,
        else_bb: disconnected_block,
    });
    for (block, variant) in [(full_block, 1), (disconnected_block, 2)] {
        ctx.switch_to(block);
        ctx.local_states[unsent.0 as usize] = LocalState::Initialized;
        ctx.push_inst(MirInst::Assign {
            local: result,
            value: Rvalue::EnumConstruct {
                enum_name: SEND_RESULT.into(),
                variant_index: variant,
                payload: Some(MirValue::Local(unsent)),
            },
        });
        ctx.push_inst(MirInst::Goto(join_block));
        ctx.local_states[result.0 as usize] = LocalState::Uninitialized;
    }
    ctx.local_states[result.0 as usize] = LocalState::Initialized;
    ctx.local_states[unsent.0 as usize] = LocalState::Moved;
    ctx.switch_to(join_block);
    Some(Rvalue::Move(result))
}

fn lower_try_recv(
    ctx: &mut LowerCtx<'_>,
    receiver: LocalId,
    elem_type: Type,
    args: &[Expr],
    span: Span,
) -> Option<Rvalue> {
    if !args.is_empty() {
        ctx.error("Receiver.try_recv does not take arguments", Some(span));
        return None;
    }
    let value = ctx.fresh_local(None);
    ctx.locals[value.0 as usize].ty = Some(elem_type.clone());
    let status = ctx.fresh_local(None);
    ctx.locals[status.0 as usize].ty = Some(Type::I32);
    ctx.push_inst(MirInst::Assign {
        local: status,
        value: Rvalue::SpscTryRecv {
            receiver,
            out_value: value,
            elem_type: elem_type.clone(),
        },
    });
    let result_type = Type::App {
        base: RECV_RESULT.into(),
        args: vec![elem_type],
    };
    Some(branch_on_status(
        ctx,
        status,
        value,
        result_type,
        RECV_RESULT,
        0,
        1,
        2,
    ))
}

pub(crate) fn lower_spsc_method<'a>(
    ctx: &mut LowerCtx<'a>,
    receiver: &'a Expr,
    method: &str,
    args: &'a [Expr],
    span: Span,
) -> Option<Option<Rvalue>> {
    let (endpoint, elem_type, is_sender) = endpoint_receiver(ctx, receiver)?;
    match (is_sender, method) {
        (true, "try_send") => Some(lower_try_send(ctx, endpoint, elem_type, args, span)),
        (false, "try_recv") => Some(lower_try_recv(ctx, endpoint, elem_type, args, span)),
        (_, "clone") => {
            ctx.error("SPSC endpoints are unique and cannot be cloned", Some(span));
            Some(None)
        }
        (true, "try_recv") => {
            ctx.error("try_recv is only available on Receiver<T>", Some(span));
            Some(None)
        }
        (false, "try_send") => {
            ctx.error("try_send is only available on Sender<T>", Some(span));
            Some(None)
        }
        _ => None,
    }
}
