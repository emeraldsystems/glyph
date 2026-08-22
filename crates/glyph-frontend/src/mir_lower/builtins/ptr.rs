use glyph_core::ast::Expr;
use glyph_core::mir::{MirInst, MirValue, Rvalue};
use glyph_core::span::Span;
use glyph_core::types::Type;

use super::super::context::LowerCtx;
use super::super::expr::lower_value;
use super::super::value::infer_value_type;

pub(crate) fn lower_own_new<'a>(
    ctx: &mut LowerCtx<'a>,
    args: &'a [Expr],
    span: Span,
) -> Option<Rvalue> {
    if args.len() != 1 {
        ctx.error("Own::new expects exactly one argument", Some(span));
        return None;
    }
    let value = lower_value(ctx, &args[0])?;
    let elem_ty = infer_value_type(&value, ctx).or_else(|| {
        ctx.error("could not infer type for Own::new argument", Some(span));
        None
    })?;

    let tmp = ctx.fresh_local(None);
    ctx.locals[tmp.0 as usize].ty = Some(Type::Own(Box::new(elem_ty.clone())));
    ctx.push_inst(MirInst::Assign {
        local: tmp,
        value: Rvalue::OwnNew {
            value,
            elem_type: elem_ty,
        },
    });
    Some(Rvalue::Move(tmp))
}

pub(crate) fn lower_own_from_raw<'a>(
    ctx: &mut LowerCtx<'a>,
    args: &'a [Expr],
    span: Span,
) -> Option<Rvalue> {
    if args.len() != 1 {
        ctx.error("Own::from_raw expects exactly one argument", Some(span));
        return None;
    }
    let ptr = lower_value(ctx, &args[0])?;
    let elem_ty = match infer_value_type(&ptr, ctx) {
        Some(Type::RawPtr(inner)) => inner.as_ref().clone(),
        Some(other) => {
            ctx.error(
                format!("Own::from_raw requires RawPtr but got {:?}", other),
                Some(span),
            );
            return None;
        }
        None => {
            ctx.error("could not infer pointer type for Own::from_raw", Some(span));
            return None;
        }
    };

    let tmp = ctx.fresh_local(None);
    ctx.locals[tmp.0 as usize].ty = Some(Type::Own(Box::new(elem_ty.clone())));
    ctx.push_inst(MirInst::Assign {
        local: tmp,
        value: Rvalue::OwnFromRaw {
            ptr,
            elem_type: elem_ty,
        },
    });
    Some(Rvalue::Move(tmp))
}

pub(crate) fn lower_shared_new<'a>(
    ctx: &mut LowerCtx<'a>,
    args: &'a [Expr],
    span: Span,
) -> Option<Rvalue> {
    if args.len() != 1 {
        ctx.error("Shared::new expects exactly one argument", Some(span));
        return None;
    }
    let value = lower_value(ctx, &args[0])?;
    let elem_ty = infer_value_type(&value, ctx).or_else(|| {
        ctx.error("could not infer type for Shared::new argument", Some(span));
        None
    })?;

    let tmp = ctx.fresh_local(None);
    ctx.locals[tmp.0 as usize].ty = Some(Type::Shared(Box::new(elem_ty.clone())));
    ctx.push_inst(MirInst::Assign {
        local: tmp,
        value: Rvalue::SharedNew {
            value,
            elem_type: elem_ty,
        },
    });
    Some(Rvalue::Move(tmp))
}

pub(crate) fn lower_shared_clone<'a>(
    ctx: &mut LowerCtx<'a>,
    base: &'a Expr,
    span: Span,
) -> Option<Rvalue> {
    let base_local = match base {
        Expr::Ident(name, ident_span) => {
            let Some(local) = ctx.bindings.get(name.0.as_str()).copied() else {
                ctx.error(
                    format!("unknown identifier '{}'", name.0),
                    Some(*ident_span),
                );
                return None;
            };
            if !ctx.check_local_available(local, Some(*ident_span)) {
                return None;
            }
            local
        }
        _ => {
            ctx.error(".clone() requires a local variable", Some(span));
            return None;
        }
    };
    let elem_ty = ctx
        .locals
        .get(base_local.0 as usize)
        .and_then(|l| l.ty.as_ref())
        .and_then(|ty| ty.shared_inner_type())
        .cloned()
        .or_else(|| {
            ctx.error("could not infer inner type for .clone()", Some(span));
            None
        })?;

    let tmp = ctx.fresh_local(None);
    ctx.locals[tmp.0 as usize].ty = Some(Type::Shared(Box::new(elem_ty.clone())));
    ctx.push_inst(MirInst::Assign {
        local: tmp,
        value: Rvalue::SharedClone {
            base: base_local,
            elem_type: elem_ty,
        },
    });
    Some(Rvalue::Move(tmp))
}

pub(crate) fn lower_own_into_raw<'a>(
    ctx: &mut LowerCtx<'a>,
    base: &'a Expr,
    span: Span,
) -> Option<Rvalue> {
    let base_val = lower_value(ctx, base)?;
    let base_local = match base_val {
        MirValue::Local(id) => id,
        _ => {
            ctx.error("into_raw() target must be a local variable", Some(span));
            return None;
        }
    };

    let elem_ty = match ctx.local_ty(base_local) {
        Some(Type::Own(inner)) => inner.as_ref().clone(),
        Some(other) => {
            ctx.error(
                format!(
                    "into_raw() is only supported on Own values, got {:?}",
                    other
                ),
                Some(span),
            );
            return None;
        }
        None => {
            ctx.error("into_raw() target has unknown type", Some(span));
            return None;
        }
    };

    let tmp = ctx.fresh_local(None);
    ctx.locals[tmp.0 as usize].ty = Some(Type::RawPtr(Box::new(elem_ty.clone())));
    ctx.push_inst(MirInst::Assign {
        local: tmp,
        value: Rvalue::OwnIntoRaw {
            base: base_local,
            elem_type: elem_ty,
        },
    });
    Some(Rvalue::Move(tmp))
}
