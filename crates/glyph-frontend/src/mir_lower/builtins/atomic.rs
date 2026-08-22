use glyph_core::ast::Expr;
use glyph_core::atomic::{AtomicOrdering, AtomicRmwOp, AtomicScalar};
use glyph_core::mir::{LocalId, MirInst, Rvalue};
use glyph_core::span::Span;
use glyph_core::types::Mutability;
use glyph_core::types::Type;

use super::super::context::LowerCtx;
use super::super::expr::{lower_ref_expr, lower_value_with_expected};
use super::super::value::update_local_type_from_rvalue;

fn scalar_type(scalar: AtomicScalar) -> Type {
    match scalar {
        AtomicScalar::Bool => Type::Bool,
        AtomicScalar::I32 => Type::I32,
        AtomicScalar::U32 => Type::U32,
        AtomicScalar::I64 => Type::I64,
        AtomicScalar::U64 => Type::U64,
        AtomicScalar::Usize => Type::Usize,
    }
}

fn constructor_scalar(name: &str) -> Option<AtomicScalar> {
    match name {
        "AtomicBool::new" => Some(AtomicScalar::Bool),
        "AtomicI32::new" => Some(AtomicScalar::I32),
        "AtomicU32::new" => Some(AtomicScalar::U32),
        "AtomicI64::new" => Some(AtomicScalar::I64),
        "AtomicU64::new" => Some(AtomicScalar::U64),
        "AtomicUsize::new" => Some(AtomicScalar::Usize),
        _ => None,
    }
}

pub(crate) fn lower_atomic_constructor<'a>(
    ctx: &mut LowerCtx<'a>,
    name: &str,
    args: &'a [Expr],
    span: Span,
) -> Option<Rvalue> {
    let scalar = constructor_scalar(name)?;
    if args.len() != 1 {
        ctx.error(
            format!("{} expects exactly one initial value", scalar.type_name()),
            Some(span),
        );
        return Some(Rvalue::ConstInt(0));
    }

    let value_ty = scalar_type(scalar);
    let value = lower_value_with_expected(ctx, &args[0], Some(&value_ty))?;
    let result = ctx.fresh_local(None);
    ctx.locals[result.0 as usize].ty = Some(Type::Atomic(scalar));
    ctx.push_inst(MirInst::Assign {
        local: result,
        value: Rvalue::AtomicNew { value, scalar },
    });
    Some(Rvalue::Move(result))
}

fn atomic_receiver<'a>(
    ctx: &mut LowerCtx<'a>,
    receiver: &'a Expr,
    span: Span,
) -> Option<(LocalId, AtomicScalar)> {
    let local = match receiver {
        Expr::Ident(name, _) => {
            let Some(local) = ctx.bindings.get(name.0.as_str()).copied() else {
                ctx.error(format!("unknown identifier '{}'", name.0), Some(span));
                return None;
            };
            if !ctx.check_local_available(local, Some(span)) {
                return None;
            }
            local
        }
        Expr::FieldAccess { .. } => {
            // Atomic operations need the field's address, not a loaded copy of
            // its value. Reuse the ordinary field-borrow lowering so nested
            // references and aggregate layout stay centralized.
            let mut field_ref = lower_ref_expr(ctx, receiver, Mutability::Immutable, span)?;
            let local = ctx.fresh_local(None);
            update_local_type_from_rvalue(ctx, local, &mut field_ref);
            ctx.push_inst(MirInst::Assign {
                local,
                value: field_ref,
            });
            local
        }
        _ => {
            ctx.error(
                "atomic methods require an atomic local, reference, or aggregate field",
                Some(span),
            );
            return None;
        }
    };

    match ctx.local_ty(local) {
        Some(Type::Atomic(scalar)) => Some((local, *scalar)),
        Some(Type::Ref(inner, _)) => match inner.as_ref() {
            Type::Atomic(scalar) => Some((local, *scalar)),
            _ => None,
        },
        _ => None,
    }
}

fn require_arity(
    ctx: &mut LowerCtx<'_>,
    method: &str,
    actual: usize,
    expected: usize,
    span: Span,
) -> bool {
    if actual == expected {
        true
    } else {
        ctx.error(
            format!(
                "atomic .{}() expects {} argument{} but got {}",
                method,
                expected,
                if expected == 1 { "" } else { "s" },
                actual
            ),
            Some(span),
        );
        false
    }
}

fn assign_result(ctx: &mut LowerCtx<'_>, ty: Type, value: Rvalue) -> Rvalue {
    let result = ctx.fresh_local(None);
    ctx.locals[result.0 as usize].ty = Some(ty);
    ctx.push_inst(MirInst::Assign {
        local: result,
        value,
    });
    Rvalue::Move(result)
}

/// Lower the safe public v1 atomic API. There is deliberately no source-level
/// ordering argument; every operation emitted here is SeqCst.
pub(crate) fn lower_atomic_method<'a>(
    ctx: &mut LowerCtx<'a>,
    receiver: &'a Expr,
    method: &str,
    args: &'a [Expr],
    span: Span,
) -> Option<Rvalue> {
    let (atomic, scalar) = atomic_receiver(ctx, receiver, span)?;
    let value_ty = scalar_type(scalar);
    let ordering = AtomicOrdering::SeqCst;

    let rvalue = match method {
        "load" => {
            if !require_arity(ctx, method, args.len(), 0, span) {
                return None;
            }
            Rvalue::AtomicLoad {
                atomic,
                scalar,
                ordering,
            }
        }
        "store" => {
            if !require_arity(ctx, method, args.len(), 1, span) {
                return None;
            }
            let value = lower_value_with_expected(ctx, &args[0], Some(&value_ty))?;
            return Some(assign_result(
                ctx,
                Type::Void,
                Rvalue::AtomicStore {
                    atomic,
                    value,
                    scalar,
                    ordering,
                },
            ));
        }
        "swap" => {
            if !require_arity(ctx, method, args.len(), 1, span) {
                return None;
            }
            let value = lower_value_with_expected(ctx, &args[0], Some(&value_ty))?;
            Rvalue::AtomicRmw {
                atomic,
                value,
                scalar,
                op: AtomicRmwOp::Swap,
                ordering,
            }
        }
        "fetch_add" | "fetch_sub" => {
            if !scalar.is_integer() {
                ctx.error(
                    format!("{} does not support .{}()", scalar.type_name(), method),
                    Some(span),
                );
                return None;
            }
            if !require_arity(ctx, method, args.len(), 1, span) {
                return None;
            }
            let value = lower_value_with_expected(ctx, &args[0], Some(&value_ty))?;
            Rvalue::AtomicRmw {
                atomic,
                value,
                scalar,
                op: if method == "fetch_add" {
                    AtomicRmwOp::Add
                } else {
                    AtomicRmwOp::Sub
                },
                ordering,
            }
        }
        "compare_exchange" => {
            if !require_arity(ctx, method, args.len(), 2, span) {
                return None;
            }
            let expected = lower_value_with_expected(ctx, &args[0], Some(&value_ty))?;
            let desired = lower_value_with_expected(ctx, &args[1], Some(&value_ty))?;
            Rvalue::AtomicCompareExchange {
                atomic,
                expected,
                desired,
                scalar,
                success: ordering,
                failure: ordering,
            }
        }
        "is_lock_free" => {
            if !require_arity(ctx, method, args.len(), 0, span) {
                return None;
            }
            return Some(assign_result(
                ctx,
                Type::Bool,
                Rvalue::AtomicIsLockFree { scalar },
            ));
        }
        _ => return None,
    };

    Some(assign_result(ctx, value_ty, rvalue))
}
