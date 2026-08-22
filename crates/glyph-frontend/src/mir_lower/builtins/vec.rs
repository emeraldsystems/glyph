use glyph_core::ast::Expr;
use glyph_core::mir::{MirInst, MirValue, Rvalue};
use glyph_core::span::Span;
use glyph_core::types::{Mutability, Type};

use super::super::context::LowerCtx;
use super::super::expr::lower_value;
use super::super::types::vec_elem_type_from_type;

pub(crate) fn lower_vec_len<'a>(
    ctx: &mut LowerCtx<'a>,
    base: &'a Expr,
    span: Span,
) -> Option<Rvalue> {
    let base_local = match base {
        Expr::Ident(ident, _) => ctx.bindings.get(ident.0.as_str()).copied(),
        _ => None,
    }?;

    if !ctx.check_local_available(base_local, Some(span)) {
        return None;
    }

    let Some(base_ty) = ctx
        .locals
        .get(base_local.0 as usize)
        .and_then(|local| local.ty.as_ref())
    else {
        return None;
    };

    if vec_elem_type_from_type(base_ty).is_none() {
        return None;
    }

    let tmp = ctx.fresh_local(None);
    ctx.locals[tmp.0 as usize].ty = Some(Type::Usize);
    ctx.push_inst(MirInst::Assign {
        local: tmp,
        value: Rvalue::VecLen { vec: base_local },
    });
    Some(Rvalue::Move(tmp))
}

pub(crate) fn lower_vec_get<'a>(
    ctx: &mut LowerCtx<'a>,
    receiver: &'a Expr,
    args: &'a [Expr],
    span: Span,
) -> Option<Rvalue> {
    if args.len() != 1 {
        ctx.error("get expects (index)", Some(span));
        return None;
    }

    let receiver_val = lower_value(ctx, receiver)?;
    let receiver_local = match receiver_val {
        MirValue::Local(id) => id,
        _ => {
            ctx.error("get receiver must be a local", Some(span));
            return None;
        }
    };

    let Some(elem_type) = ctx
        .locals
        .get(receiver_local.0 as usize)
        .and_then(|l| l.ty.as_ref())
        .and_then(vec_elem_type_from_type)
    else {
        return None;
    };

    let index = lower_value(ctx, &args[0])?;

    let len_local = ctx.fresh_local(None);
    ctx.locals[len_local.0 as usize].ty = Some(Type::Usize);
    ctx.push_inst(MirInst::Assign {
        local: len_local,
        value: Rvalue::VecLen {
            vec: receiver_local,
        },
    });

    let nonneg_local = ctx.fresh_local(None);
    ctx.locals[nonneg_local.0 as usize].ty = Some(Type::Bool);
    ctx.push_inst(MirInst::Assign {
        local: nonneg_local,
        value: Rvalue::Binary {
            op: glyph_core::ast::BinaryOp::Ge,
            lhs: index.clone(),
            rhs: MirValue::Int(0),
        },
    });

    let upper_local = ctx.fresh_local(None);
    ctx.locals[upper_local.0 as usize].ty = Some(Type::Bool);
    ctx.push_inst(MirInst::Assign {
        local: upper_local,
        value: Rvalue::Binary {
            op: glyph_core::ast::BinaryOp::Lt,
            lhs: index.clone(),
            rhs: MirValue::Local(len_local),
        },
    });

    let elem_ref_type = Type::Ref(Box::new(elem_type.clone()), Mutability::Immutable);
    let result_local = ctx.fresh_local(None);
    ctx.locals[result_local.0 as usize].ty = Some(Type::App {
        base: "Option".into(),
        args: vec![elem_ref_type.clone()],
    });

    let header = ctx.current;
    let check_upper = ctx.new_block();
    let some_bb = ctx.new_block();
    let none_bb = ctx.new_block();
    let done_bb = ctx.new_block();

    ctx.switch_to(header);
    ctx.push_inst(MirInst::If {
        cond: MirValue::Local(nonneg_local),
        then_bb: check_upper,
        else_bb: none_bb,
    });

    ctx.switch_to(check_upper);
    ctx.push_inst(MirInst::If {
        cond: MirValue::Local(upper_local),
        then_bb: some_bb,
        else_bb: none_bb,
    });

    ctx.switch_to(some_bb);
    let elem_local = ctx.fresh_local(None);
    ctx.locals[elem_local.0 as usize].ty = Some(elem_ref_type);
    ctx.push_inst(MirInst::Assign {
        local: elem_local,
        value: Rvalue::VecIndexRef {
            vec: receiver_local,
            elem_type,
            index,
            bounds_check: false,
            mutability: Mutability::Immutable,
        },
    });
    ctx.push_inst(MirInst::Assign {
        local: result_local,
        value: Rvalue::EnumConstruct {
            enum_name: "Option".into(),
            variant_index: 1,
            payload: Some(MirValue::Local(elem_local)),
        },
    });
    ctx.push_inst(MirInst::Goto(done_bb));

    ctx.switch_to(none_bb);
    ctx.push_inst(MirInst::Assign {
        local: result_local,
        value: Rvalue::EnumConstruct {
            enum_name: "Option".into(),
            variant_index: 0,
            payload: None,
        },
    });
    ctx.push_inst(MirInst::Goto(done_bb));

    ctx.switch_to(done_bb);

    Some(Rvalue::Move(result_local))
}

pub(crate) fn lower_vec_push<'a>(
    ctx: &mut LowerCtx<'a>,
    receiver: &'a Expr,
    args: &'a [Expr],
    span: Span,
) -> Option<Rvalue> {
    if args.len() != 1 {
        ctx.error("Vec::push expects exactly one argument", Some(span));
        return None;
    }

    let receiver_val = lower_value(ctx, receiver)?;
    let receiver_local = match receiver_val {
        MirValue::Local(id) => id,
        _ => {
            ctx.error("Vec::push receiver must be a local", Some(span));
            return None;
        }
    };

    let elem_type = ctx
        .locals
        .get(receiver_local.0 as usize)
        .and_then(|l| l.ty.as_ref())
        .and_then(vec_elem_type_from_type)
        .unwrap_or_else(|| {
            ctx.error("could not infer Vec element type", Some(span));
            Type::I32
        });

    let value = lower_value(ctx, &args[0])?;

    let tmp = ctx.fresh_local(None);
    ctx.locals[tmp.0 as usize].ty = Some(Type::App {
        base: "Vec".into(),
        args: vec![elem_type.clone()],
    });
    ctx.push_inst(MirInst::Assign {
        local: tmp,
        value: Rvalue::VecPush {
            vec: receiver_local,
            elem_type,
            value,
        },
    });
    Some(Rvalue::Move(tmp))
}

pub(crate) fn lower_vec_pop<'a>(
    ctx: &mut LowerCtx<'a>,
    receiver: &'a Expr,
    args: &'a [Expr],
    span: Span,
) -> Option<Rvalue> {
    if !args.is_empty() {
        ctx.error("Vec::pop does not take arguments", Some(span));
        return None;
    }

    let receiver_val = lower_value(ctx, receiver)?;
    let receiver_local = match receiver_val {
        MirValue::Local(id) => id,
        _ => {
            ctx.error("Vec::pop receiver must be a local", Some(span));
            return None;
        }
    };

    let elem_type = ctx
        .locals
        .get(receiver_local.0 as usize)
        .and_then(|l| l.ty.as_ref())
        .and_then(vec_elem_type_from_type)
        .unwrap_or_else(|| {
            ctx.error("could not infer Vec element type", Some(span));
            Type::I32
        });

    let tmp = ctx.fresh_local(None);
    ctx.locals[tmp.0 as usize].ty = Some(Type::App {
        base: "Option".into(),
        args: vec![elem_type.clone()],
    });
    ctx.push_inst(MirInst::Assign {
        local: tmp,
        value: Rvalue::VecPop {
            vec: receiver_local,
            elem_type,
        },
    });
    Some(Rvalue::Move(tmp))
}

pub(crate) fn lower_vec_static_new<'a>(
    ctx: &mut LowerCtx<'a>,
    args: &'a [Expr],
    span: Span,
    expected_ret: Option<&Type>,
) -> Option<Rvalue> {
    if !args.is_empty() {
        ctx.error("Vec::new does not take arguments", Some(span));
        return None;
    }

    let elem_type = match expected_ret {
        Some(Type::App { base, args })
            if (base == "Vec" || base.ends_with("::Vec")) && args.len() == 1 =>
        {
            args[0].clone()
        }
        _ => Type::I32,
    };
    let tmp = ctx.fresh_local(None);
    ctx.locals[tmp.0 as usize].ty = Some(Type::App {
        base: "Vec".into(),
        args: vec![elem_type.clone()],
    });
    ctx.push_inst(MirInst::Assign {
        local: tmp,
        value: Rvalue::VecNew { elem_type },
    });
    Some(Rvalue::Move(tmp))
}

pub(crate) fn lower_vec_static_with_capacity<'a>(
    ctx: &mut LowerCtx<'a>,
    args: &'a [Expr],
    span: Span,
    expected_ret: Option<&Type>,
) -> Option<Rvalue> {
    if args.len() != 1 {
        ctx.error("Vec::with_capacity expects one argument", Some(span));
        return None;
    }

    let capacity = lower_value(ctx, &args[0])?;
    let elem_type = match expected_ret {
        Some(Type::App { base, args })
            if (base == "Vec" || base.ends_with("::Vec")) && args.len() == 1 =>
        {
            args[0].clone()
        }
        _ => Type::I32,
    };
    let tmp = ctx.fresh_local(None);
    ctx.locals[tmp.0 as usize].ty = Some(Type::App {
        base: "Vec".into(),
        args: vec![elem_type.clone()],
    });
    ctx.push_inst(MirInst::Assign {
        local: tmp,
        value: Rvalue::VecWithCapacity {
            elem_type,
            capacity,
        },
    });
    Some(Rvalue::Move(tmp))
}
