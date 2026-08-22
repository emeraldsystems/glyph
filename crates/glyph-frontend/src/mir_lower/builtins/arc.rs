use glyph_core::ast::Expr;
use glyph_core::mir::{LocalId, MirInst, Rvalue};
use glyph_core::span::Span;
use glyph_core::types::{Mutability, Type};

use crate::resolver::ResolvedSymbol;

use super::super::context::LowerCtx;
use super::super::expr::lower_value;
use super::super::value::infer_value_type;

/// Static Arc construction is intrinsic only when `Arc` resolved to the
/// compiler-owned declaration in `std/sync`. Matching the call's spelling is
/// insufficient: a user declaration named Arc must retain ordinary semantics.
pub(crate) fn is_canonical_arc_new(ctx: &LowerCtx<'_>, name: &str) -> bool {
    name == "Arc::new"
        && matches!(
            ctx.resolver.resolve_symbol("Arc"),
            Some(ResolvedSymbol::Struct(module, symbol))
                if module == "std/sync" && symbol == "Arc"
        )
}

pub(crate) fn lower_arc_new<'a>(
    ctx: &mut LowerCtx<'a>,
    args: &'a [Expr],
    span: Span,
) -> Option<Rvalue> {
    if args.len() != 1 {
        ctx.error("Arc::new expects exactly one argument", Some(span));
        return Some(Rvalue::ConstInt(0));
    }
    if matches!(&args[0], Expr::Ref { .. }) {
        ctx.error(
            "Arc::new cannot own a borrowed reference; move an owned value into the Arc",
            Some(span),
        );
        return Some(Rvalue::ConstInt(0));
    }
    let Some(value) = lower_value(ctx, &args[0]) else {
        return Some(Rvalue::ConstInt(0));
    };
    let Some(elem_type) = infer_value_type(&value, ctx) else {
        ctx.error("could not infer type for Arc::new argument", Some(span));
        return Some(Rvalue::ConstInt(0));
    };
    if matches!(elem_type, Type::Ref(_, _)) {
        ctx.error(
            "Arc::new cannot own a borrowed reference; move an owned value into the Arc",
            Some(span),
        );
        return Some(Rvalue::ConstInt(0));
    }

    let result = ctx.fresh_local(None);
    ctx.locals[result.0 as usize].ty = Some(Type::arc(elem_type.clone()));
    ctx.push_inst(MirInst::Assign {
        local: result,
        value: Rvalue::ArcNew { value, elem_type },
    });
    Some(Rvalue::Move(result))
}

fn arc_receiver(ctx: &mut LowerCtx<'_>, receiver: &Expr, _span: Span) -> Option<(LocalId, Type)> {
    let Expr::Ident(name, ident_span) = receiver else {
        return None;
    };
    let local = ctx.bindings.get(name.0.as_str()).copied().or_else(|| {
        ctx.error(
            format!("unknown identifier '{}'", name.0),
            Some(*ident_span),
        );
        None
    })?;
    if !ctx.check_local_available(local, Some(*ident_span)) {
        return None;
    }
    let ty = ctx.local_ty(local)?.clone();
    let inner = ty.arc_inner_type()?.clone();
    Some((local, inner))
}

fn require_no_args(ctx: &mut LowerCtx<'_>, method: &str, args: &[Expr], span: Span) -> bool {
    if args.is_empty() {
        true
    } else {
        ctx.error(
            format!("Arc.{method}() does not take arguments"),
            Some(span),
        );
        false
    }
}

/// Returns `None` when the receiver is not a canonical Arc and normal method
/// resolution should continue. `Some(None)` means the Arc call was recognized
/// but invalid and already diagnosed.
pub(crate) fn lower_arc_method<'a>(
    ctx: &mut LowerCtx<'a>,
    receiver: &'a Expr,
    method: &str,
    args: &'a [Expr],
    span: Span,
) -> Option<Option<Rvalue>> {
    let (base, elem_type) = arc_receiver(ctx, receiver, span)?;
    match method {
        "clone" => {
            if !require_no_args(ctx, method, args, span) {
                return Some(None);
            }
            let result = ctx.fresh_local(None);
            ctx.locals[result.0 as usize].ty = Some(Type::arc(elem_type.clone()));
            ctx.push_inst(MirInst::Assign {
                local: result,
                value: Rvalue::ArcClone { base, elem_type },
            });
            Some(Some(Rvalue::Move(result)))
        }
        "borrow" => {
            if !require_no_args(ctx, method, args, span) {
                return Some(None);
            }
            let result = ctx.fresh_local(None);
            ctx.locals[result.0 as usize].ty = Some(Type::Ref(
                Box::new(elem_type.clone()),
                Mutability::Immutable,
            ));
            ctx.push_inst(MirInst::Assign {
                local: result,
                value: Rvalue::ArcBorrow { base, elem_type },
            });
            ctx.register_arc_borrow(result, base, span);
            Some(Some(Rvalue::Move(result)))
        }
        "borrow_mut" | "get_mut" => {
            ctx.error(
                "Arc<T> provides immutable access only; use Mutex<T> for shared mutation",
                Some(span),
            );
            Some(None)
        }
        _ => None,
    }
}
