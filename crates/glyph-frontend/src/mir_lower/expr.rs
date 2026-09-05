use glyph_core::ast::{BinaryOp, Expr, Ident, UnaryOp};
use glyph_core::mir::{LocalId, MirInst, MirValue, Rvalue};
use glyph_core::span::Span;
use glyph_core::types::{EnumType, Mutability, Type};

use crate::resolver::{ConstValue, ResolvedSymbol};

use super::call::{call_types_compatible, lower_call, lower_method_call};
use super::context::{LocalState, LowerCtx};
use super::flow::{
    lower_block_with_expected, lower_closure_rvalue, lower_for, lower_for_in, lower_if_value,
    lower_while,
};
use super::types::{tuple_struct_name, vec_elem_type_from_type};
use super::value::{
    bool_rvalue, coerce_to_bool, infer_numeric_result_type, infer_value_type, local_struct_name,
    rvalue_from_value, rvalue_to_value, update_local_type_from_rvalue,
};

fn consume_value_local(ctx: &mut LowerCtx<'_>, value: &MirValue, span: Span) {
    let MirValue::Local(local) = value else {
        return;
    };
    if matches!(
        ctx.local_states.get(local.0 as usize),
        Some(LocalState::Moved)
    ) {
        return;
    }
    let _ = ctx.consume_local(*local, Some(span));
}

fn type_is_copy(ty: &Type) -> bool {
    matches!(
        ty,
        Type::I8
            | Type::I16
            | Type::I32
            | Type::I64
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::U64
            | Type::Usize
            | Type::F32
            | Type::F64
            | Type::Bool
            | Type::Char
            | Type::Str
            | Type::Ref(_, _)
            | Type::RawPtr(_)
            | Type::Shared(_)
    )
}

fn validate_callable_expectation(
    ctx: &mut LowerCtx<'_>,
    actual: &Type,
    expected: Option<&Type>,
    subject: &str,
    span: Span,
) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    if actual.function_signature().is_none() && expected.function_signature().is_none() {
        return true;
    }
    if call_types_compatible(actual, expected) {
        return true;
    }
    ctx.error(
        format!(
            "{} has type '{}', expected '{}'",
            subject,
            LowerCtx::type_label(actual),
            LowerCtx::type_label(expected)
        ),
        Some(span),
    );
    false
}

fn lower_function_item(
    ctx: &mut LowerCtx<'_>,
    name: &str,
    span: Span,
    expected: Option<&Type>,
) -> Option<Rvalue> {
    let sig = ctx.fn_sigs.get(name).cloned()?;
    let Some(mut signature) = sig.callable_type() else {
        ctx.error(
            format!("'{}' cannot be used as a callable value", name),
            Some(span),
        );
        return None;
    };
    if !validate_callable_expectation(
        ctx,
        &signature,
        expected,
        &format!("function '{}'", name),
        span,
    ) {
        return None;
    }
    if matches!(expected, Some(Type::BorrowedFunction { .. })) {
        signature = expected.expect("matched Some above").clone();
    }
    Some(Rvalue::FunctionRef {
        name: sig.target_name,
        signature,
    })
}

fn is_move_context(expected: Option<&Type>, field_type: &Type) -> bool {
    match expected {
        Some(Type::Str) if matches!(field_type, Type::String | Type::Str) => false,
        _ => true,
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

fn lower_field_access_ref<'a>(
    ctx: &mut LowerCtx<'a>,
    expr: &'a Expr,
    span: Span,
) -> Option<LocalId> {
    match expr {
        Expr::Ident(ident, ident_span) => {
            let local = ctx.bindings.get(ident.0.as_str()).copied().or_else(|| {
                ctx.error(
                    format!("unknown identifier '{}'", ident.0),
                    Some(*ident_span),
                );
                None
            })?;
            if ctx.check_local_available(local, Some(*ident_span)) {
                Some(local)
            } else {
                None
            }
        }
        Expr::FieldAccess { base, field, span } => {
            let base_local = lower_field_access_ref(ctx, base, *span)?;
            let (_, field_index) = resolve_field_info(ctx, base_local, field, *span)?;
            let mut rv = Rvalue::FieldRef {
                base: base_local,
                field_name: field.0.clone(),
                field_index,
                mutability: Mutability::Immutable,
            };
            let tmp = ctx.fresh_local(None);
            update_local_type_from_rvalue(ctx, tmp, &mut rv);
            ctx.push_inst(MirInst::Assign {
                local: tmp,
                value: rv,
            });
            Some(tmp)
        }
        _ => {
            ctx.error("field access base must be a local", Some(span));
            None
        }
    }
}

pub(crate) fn lower_expr<'a>(ctx: &mut LowerCtx<'a>, expr: &'a Expr) -> Option<Rvalue> {
    lower_expr_with_expected(ctx, expr, None)
}

/// Inclusive value range an integer literal may take for `ty`.
///
/// `u64` and `usize` are deliberately absent: the parser accepts any literal up
/// to `u64::MAX` and stores it in an `i64`, so a large `u64` literal reaches
/// lowering already reinterpreted as negative and cannot be checked from that
/// representation. `char` is absent because char literals are not `Literal::Int`.
fn int_literal_range(ty: &Type) -> Option<(i128, i128)> {
    Some(match ty {
        Type::I8 => (i8::MIN as i128, i8::MAX as i128),
        Type::I16 => (i16::MIN as i128, i16::MAX as i128),
        Type::I32 => (i32::MIN as i128, i32::MAX as i128),
        Type::I64 => (i64::MIN as i128, i64::MAX as i128),
        Type::U8 => (0, u8::MAX as i128),
        Type::U16 => (0, u16::MAX as i128),
        Type::U32 => (0, u32::MAX as i128),
        _ => return None,
    })
}

/// The compile-time value of an integer literal, seeing through a leading `-`
/// so `-32768` is judged as one value rather than as `32768`.
fn const_int_literal(expr: &Expr) -> Option<i128> {
    match expr {
        Expr::Lit(glyph_core::ast::Literal::Int(i), _) => Some(*i as i128),
        Expr::Unary {
            op: UnaryOp::Neg,
            expr,
            ..
        } => const_int_literal(expr).map(|v| -v),
        _ => None,
    }
}

/// Reject `let x: i16 = 40000` where the literal cannot be represented, instead
/// of letting codegen truncate it into a different value without a word.
fn check_int_literal_range(ctx: &mut LowerCtx<'_>, expr: &Expr, expected: Option<&Type>) {
    let Some(ty) = expected else { return };
    let Some((min, max)) = int_literal_range(ty) else {
        return;
    };
    let Some(value) = const_int_literal(expr) else {
        return;
    };
    if value < min || value > max {
        ctx.error(
            format!(
                "integer literal {} is out of range for '{}' (valid range {}..={})",
                value,
                LowerCtx::type_label(ty),
                min,
                max
            ),
            super::value::expr_span(expr),
        );
    }
}

pub(crate) fn lower_expr_with_expected<'a>(
    ctx: &mut LowerCtx<'a>,
    expr: &'a Expr,
    expected: Option<&Type>,
) -> Option<Rvalue> {
    check_int_literal_range(ctx, expr, expected);
    match expr {
        Expr::Lit(glyph_core::ast::Literal::Int(i), _) => {
            // If we have an expected integer type, create a typed local so the
            // literal carries the correct width through MIR to codegen.
            if let Some(ty) = expected {
                if ty.is_int() && *ty != Type::I32 {
                    let tmp = ctx.fresh_local(None);
                    ctx.locals[tmp.0 as usize].ty = Some(ty.clone());
                    ctx.push_inst(MirInst::Assign {
                        local: tmp,
                        value: Rvalue::ConstInt(*i),
                    });
                    return Some(Rvalue::Move(tmp));
                }
            }
            Some(Rvalue::ConstInt(*i))
        }
        Expr::Lit(glyph_core::ast::Literal::Float(fl), _) => {
            // Bare float literals default to f64; an expected f32 type creates a
            // typed local so the literal carries its width through MIR to codegen.
            if let Some(ty) = expected {
                if ty.is_float() && *ty != Type::F64 {
                    let tmp = ctx.fresh_local(None);
                    ctx.locals[tmp.0 as usize].ty = Some(ty.clone());
                    ctx.push_inst(MirInst::Assign {
                        local: tmp,
                        value: Rvalue::ConstFloat(*fl),
                    });
                    return Some(Rvalue::Move(tmp));
                }
            }
            Some(Rvalue::ConstFloat(*fl))
        }
        Expr::Lit(glyph_core::ast::Literal::Bool(b), _) => Some(Rvalue::ConstBool(*b)),
        Expr::Lit(glyph_core::ast::Literal::Char(c), _) => {
            let tmp = ctx.fresh_local(None);
            ctx.locals[tmp.0 as usize].ty = Some(Type::Char);
            ctx.push_inst(MirInst::Assign {
                local: tmp,
                value: Rvalue::ConstInt(*c as i64),
            });
            Some(Rvalue::Move(tmp))
        }
        Expr::Lit(glyph_core::ast::Literal::Str(s), _) => Some(Rvalue::StringLit {
            content: s.clone(),
            global_name: ctx.fresh_string_global(),
        }),
        Expr::InterpString { segments, span } => lower_interp_string(ctx, segments, *span),
        Expr::Match {
            scrutinee,
            arms,
            span,
        } => lower_match(ctx, scrutinee, arms, true, *span, expected),
        Expr::Ident(ident, span) => {
            if let Some(local) = ctx.bindings.get(ident.0.as_str()).copied() {
                if let Some(actual) = ctx.local_ty(local).cloned() {
                    if !validate_callable_expectation(
                        ctx,
                        &actual,
                        expected,
                        &format!("value '{}'", ident.0),
                        *span,
                    ) {
                        return None;
                    }
                }
                let implicit = ctx.implicit_deref_type(local).cloned();
                if implicit.is_some() && !matches!(expected, Some(Type::Ref(_, _))) {
                    Some(Rvalue::Deref {
                        base: local,
                        ty: implicit.expect("checked above"),
                    })
                } else if ctx.consume_local(local, Some(*span)) {
                    Some(Rvalue::Move(local))
                } else {
                    None
                }
            } else if let Some(value) = lookup_const_value(ctx, ident.0.as_str(), *span) {
                lower_const_rvalue(ctx, value)
            } else if is_unit_enum_ctor(ctx, ident.0.as_str()) {
                // Bare unit-variant paths (`Val::Nil`, `None`) construct the
                // variant; delegate to call lowering with zero arguments.
                lower_call(ctx, expr, &[], *span, false, expected)
            } else if ctx.fn_sigs.contains_key(ident.0.as_str()) {
                lower_function_item(ctx, ident.0.as_str(), *span, expected)
            } else {
                None
            }
        }
        Expr::Unary { op, expr, span } => match op {
            UnaryOp::Not => lower_unary_not(ctx, expr, *span),
            UnaryOp::Neg => lower_unary_neg(ctx, expr, *span),
        },
        Expr::Binary { op, lhs, rhs, .. } => match *op {
            glyph_core::ast::BinaryOp::And | glyph_core::ast::BinaryOp::Or => {
                lower_logical(ctx, op, lhs, rhs)
            }
            _ => lower_binary(ctx, op, lhs, rhs),
        },
        Expr::Call { callee, args, span } => lower_call(ctx, callee, args, *span, false, expected),
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            let value = lower_if_value(ctx, cond, then_block, else_block.as_ref(), expected);
            value.and_then(rvalue_from_value)
        }
        Expr::Block(block) => {
            lower_block_with_expected(ctx, block, expected, true, true).and_then(rvalue_from_value)
        }
        Expr::While { cond, body, .. } => {
            lower_while(ctx, cond, body);
            None
        }
        Expr::For {
            var,
            start,
            end,
            body,
            span,
        } => {
            lower_for(ctx, var, start, end, body, *span);
            None
        }
        Expr::ForIn {
            var,
            iter,
            body,
            span,
        } => {
            lower_for_in(ctx, var, iter, body, *span);
            None
        }
        Expr::StructLit { name, fields, span } => lower_struct_lit(ctx, name, fields, *span),
        Expr::FieldAccess { base, field, span } => {
            lower_field_access(ctx, base, field, *span, expected)
        }
        Expr::Ref {
            expr,
            mutability,
            span,
        } => lower_ref_expr(ctx, expr, *mutability, *span),
        Expr::ArrayLit { elements, span } => lower_array_lit(ctx, elements, *span),
        Expr::Index { base, index, span } => lower_array_index(ctx, base, index, *span),
        Expr::MethodCall {
            receiver,
            method,
            args,
            span,
        } => lower_method_call(ctx, receiver, method, args, *span),
        Expr::Tuple { elements, span } => lower_tuple_expr(ctx, elements, *span),
        Expr::Try { expr, span } => lower_try(ctx, expr, *span),
        Expr::Cast { expr, target, span } => lower_cast(ctx, expr, target, *span),
        Expr::Closure {
            capture,
            params,
            body,
            span,
        } => lower_closure_rvalue(ctx, *capture, params, body, *span, expected),
    }
}

fn lower_cast<'a>(
    ctx: &mut LowerCtx<'a>,
    expr: &'a Expr,
    target: &glyph_core::ast::TypeExpr,
    span: Span,
) -> Option<Rvalue> {
    use super::types::{resolve_type_name, type_expr_to_string};

    let Some(to_ty) = resolve_type_name(&type_expr_to_string(target), ctx.resolver) else {
        ctx.error("unknown cast target type", Some(span));
        return None;
    };

    if !(to_ty.is_int() || to_ty.is_float()) {
        ctx.error(
            format!(
                "cast target must be a numeric type (integer or float), got '{}'",
                type_expr_to_string(target)
            ),
            Some(span),
        );
        return None;
    }

    let value = lower_value(ctx, expr)?;
    let from_ty = infer_value_type(&value, ctx).unwrap_or(Type::I32);

    let castable_source =
        from_ty.is_int() || from_ty.is_float() || matches!(from_ty, Type::Char | Type::Bool);
    if !castable_source {
        ctx.error(
            "only numeric values (integers, floats, char, bool) can be cast with 'as'",
            Some(span),
        );
        return None;
    }

    let tmp = ctx.fresh_local(None);
    ctx.locals[tmp.0 as usize].ty = Some(to_ty.clone());
    ctx.push_inst(MirInst::Assign {
        local: tmp,
        value: Rvalue::Cast {
            value,
            from: from_ty,
            to: to_ty,
        },
    });
    Some(Rvalue::Move(tmp))
}

/// True when `name` resolves to a zero-parameter enum variant constructor,
/// so a bare identifier like `Val::Nil` or `None` builds that variant.
fn is_unit_enum_ctor(ctx: &LowerCtx<'_>, name: &str) -> bool {
    ctx.fn_sigs.get(name).map_or(false, |sig| {
        sig.enum_ctor.is_some() && sig.params.is_empty()
    })
}

fn lookup_const_value<'a>(ctx: &mut LowerCtx<'a>, name: &str, span: Span) -> Option<ConstValue> {
    if let Some(binding) = ctx.resolver.consts.get(name) {
        return Some(binding.value.clone());
    }

    if !matches!(
        ctx.resolver.resolve_symbol(name),
        Some(ResolvedSymbol::Const(_, _))
    ) {
        return None;
    }

    crate::resolver::resolve_const_value(name, span, ctx.module, ctx.resolver, &mut ctx.diagnostics)
}

fn lower_const_rvalue<'a>(ctx: &mut LowerCtx<'a>, value: ConstValue) -> Option<Rvalue> {
    let val = lower_const_value(ctx, value)?;
    rvalue_from_value(val)
}

fn lower_const_value<'a>(ctx: &mut LowerCtx<'a>, value: ConstValue) -> Option<MirValue> {
    match value {
        ConstValue::Int(i) => Some(MirValue::Int(i)),
        ConstValue::Bool(b) => Some(MirValue::Bool(b)),
        ConstValue::Char(c) => {
            let tmp = ctx.fresh_local(None);
            ctx.locals[tmp.0 as usize].ty = Some(Type::Char);
            ctx.push_inst(MirInst::Assign {
                local: tmp,
                value: Rvalue::ConstInt(c as i64),
            });
            Some(MirValue::Local(tmp))
        }
        ConstValue::Str(s) => {
            let tmp = ctx.fresh_local(None);
            ctx.locals[tmp.0 as usize].ty = Some(Type::Str);
            let global_name = ctx.fresh_string_global();
            ctx.push_inst(MirInst::Assign {
                local: tmp,
                value: Rvalue::StringLit {
                    content: s,
                    global_name,
                },
            });
            Some(MirValue::Local(tmp))
        }
    }
}

fn lower_unary_not<'a>(ctx: &mut LowerCtx<'a>, expr: &'a Expr, _span: Span) -> Option<Rvalue> {
    let value = lower_value(ctx, expr)?;
    let coerced = coerce_to_bool(value);
    let tmp = ctx.fresh_local(None);
    ctx.locals[tmp.0 as usize].ty = Some(Type::Bool);
    ctx.push_inst(MirInst::Assign {
        local: tmp,
        value: Rvalue::Binary {
            op: BinaryOp::Eq,
            lhs: coerced,
            rhs: MirValue::Bool(false),
        },
    });
    Some(Rvalue::Move(tmp))
}

fn lower_unary_neg<'a>(ctx: &mut LowerCtx<'a>, expr: &'a Expr, _span: Span) -> Option<Rvalue> {
    let value = lower_value(ctx, expr)?;
    let tmp = ctx.fresh_local(None);
    let ty = infer_value_type(&value, ctx).unwrap_or(Type::I32);
    let zero = if ty.is_float() {
        MirValue::Float(0.0)
    } else {
        MirValue::Int(0)
    };
    ctx.locals[tmp.0 as usize].ty = Some(ty);
    ctx.push_inst(MirInst::Assign {
        local: tmp,
        value: Rvalue::Binary {
            op: BinaryOp::Sub,
            lhs: zero,
            rhs: value,
        },
    });
    Some(Rvalue::Move(tmp))
}

fn lower_binary<'a>(
    ctx: &mut LowerCtx<'a>,
    op: &glyph_core::ast::BinaryOp,
    lhs: &'a Expr,
    rhs: &'a Expr,
) -> Option<Rvalue> {
    // For contextual integer typing: if one side is a literal and the other
    // is a typed local, propagate the type to the literal side.
    let lhs_val = lower_value(ctx, lhs)?;
    let lhs_ty = infer_value_type(&lhs_val, ctx);
    let rhs_expected = if let Some(ty) = &lhs_ty {
        if ty.is_int() || ty.is_float() {
            Some(ty.clone())
        } else {
            None
        }
    } else {
        None
    };
    let rhs_val = lower_value_with_expected(ctx, rhs, rhs_expected.as_ref())?;
    // If rhs was typed but lhs was an untyped int literal, re-lower lhs
    // This handles `1 + pos` where pos is usize. Since lhs is already lowered
    // we can't re-lower it, but the coerce_int_binop in codegen handles this.
    let tmp = ctx.fresh_local(None);

    if matches!(op, glyph_core::ast::BinaryOp::Add) {
        let lhs_ty = infer_value_type(&lhs_val, ctx);
        let rhs_ty = infer_value_type(&rhs_val, ctx);
        let is_string_like = |ty: Option<&Type>| {
            matches!(ty, Some(Type::String | Type::Str))
                || matches!(ty, Some(Type::Ref(inner, _)) if matches!(inner.as_ref(), Type::String | Type::Str))
        };

        if is_string_like(lhs_ty.as_ref()) && is_string_like(rhs_ty.as_ref()) {
            let base_local = match &lhs_val {
                MirValue::Local(id) => *id,
                _ => return None,
            };
            ctx.locals[tmp.0 as usize].ty = Some(Type::String);
            ctx.push_inst(MirInst::Assign {
                local: tmp,
                value: Rvalue::StringConcat {
                    base: base_local,
                    value: rhs_val,
                },
            });
            return Some(Rvalue::Move(tmp));
        }
    }

    // Comparisons always produce bool.
    //
    // If we leave the type unset, codegen will default the local slot to i32,
    // but LLVM ICmp returns i1; that mismatch later breaks `if`/`while` lowering
    // (branch conditions become i32).
    match op {
        glyph_core::ast::BinaryOp::Eq
        | glyph_core::ast::BinaryOp::Ne
        | glyph_core::ast::BinaryOp::Lt
        | glyph_core::ast::BinaryOp::Le
        | glyph_core::ast::BinaryOp::Gt
        | glyph_core::ast::BinaryOp::Ge => {
            ctx.locals[tmp.0 as usize].ty = Some(Type::Bool);
        }
        glyph_core::ast::BinaryOp::Add
        | glyph_core::ast::BinaryOp::Sub
        | glyph_core::ast::BinaryOp::Mul
        | glyph_core::ast::BinaryOp::Div
        | glyph_core::ast::BinaryOp::Mod => {
            if let Some(numeric) = infer_numeric_result_type(&lhs_val, &rhs_val, ctx) {
                ctx.locals[tmp.0 as usize].ty = Some(numeric);
            }
        }
        _ => {}
    }
    ctx.push_inst(MirInst::Assign {
        local: tmp,
        value: Rvalue::Binary {
            op: op.clone(),
            lhs: lhs_val,
            rhs: rhs_val,
        },
    });
    Some(Rvalue::Move(tmp))
}

fn lower_interp_string(
    ctx: &mut LowerCtx<'_>,
    segments: &[glyph_core::ast::InterpSegment],
    span: Span,
) -> Option<Rvalue> {
    let mut buf = String::new();
    for seg in segments {
        match seg {
            glyph_core::ast::InterpSegment::Literal(s) => buf.push_str(s),
            glyph_core::ast::InterpSegment::Expr(_) => {
                ctx.error(
                    "interpolation holes are not yet lowered; only literal segments are supported",
                    Some(span),
                );
                return None;
            }
        }
    }

    Some(Rvalue::StringLit {
        content: buf,
        global_name: ctx.fresh_string_global(),
    })
}

pub(crate) fn lower_logical<'a>(
    ctx: &mut LowerCtx<'a>,
    op: &glyph_core::ast::BinaryOp,
    lhs: &'a Expr,
    rhs: &'a Expr,
) -> Option<Rvalue> {
    let lhs_val = lower_value(ctx, lhs).unwrap_or(MirValue::Bool(false));
    let lhs_bool = coerce_to_bool(lhs_val.clone());
    let result = ctx.fresh_local(None);
    ctx.locals[result.0 as usize].ty = Some(Type::Bool);
    let rhs_block = ctx.new_block();
    let join_block = ctx.new_block();

    match op {
        glyph_core::ast::BinaryOp::And => {
            ctx.push_inst(MirInst::Assign {
                local: result,
                value: bool_rvalue(lhs_bool.clone()),
            });
            ctx.push_inst(MirInst::If {
                cond: lhs_bool,
                then_bb: rhs_block,
                else_bb: join_block,
            });
            ctx.switch_to(rhs_block);
            let rhs_val = lower_value(ctx, rhs).unwrap_or(MirValue::Bool(false));
            ctx.push_inst(MirInst::Assign {
                local: result,
                value: bool_rvalue(rhs_val),
            });
        }
        glyph_core::ast::BinaryOp::Or => {
            ctx.push_inst(MirInst::Assign {
                local: result,
                value: bool_rvalue(lhs_bool.clone()),
            });
            ctx.push_inst(MirInst::If {
                cond: lhs_bool,
                then_bb: join_block,
                else_bb: rhs_block,
            });
            ctx.switch_to(rhs_block);
            let rhs_val = lower_value(ctx, rhs).unwrap_or(MirValue::Bool(false));
            ctx.push_inst(MirInst::Assign {
                local: result,
                value: bool_rvalue(rhs_val),
            });
        }
        _ => return lower_binary(ctx, op, lhs, rhs),
    }

    ctx.push_inst(MirInst::Goto(join_block));
    ctx.switch_to(join_block);
    Some(Rvalue::Move(result))
}

pub(crate) fn lower_match<'a>(
    ctx: &mut LowerCtx<'a>,
    scrutinee: &'a Expr,
    arms: &'a [glyph_core::ast::MatchArm],
    require_value: bool,
    span: Span,
    expected: Option<&Type>,
) -> Option<Rvalue> {
    // Evaluate scrutinee into a local
    let scrut_val = lower_value(ctx, scrutinee)?;
    let scrut_local = match scrut_val {
        MirValue::Local(id) => id,
        MirValue::Int(_) | MirValue::Float(_) | MirValue::Bool(_) | MirValue::Unit => {
            let tmp = ctx.fresh_local(None);
            let rv = rvalue_from_value(scrut_val)?;
            ctx.locals[tmp.0 as usize].ty = None;
            ctx.push_inst(MirInst::Assign {
                local: tmp,
                value: rv,
            });
            tmp
        }
    };
    // `match` consumes its scrutinee by value in this lowering model.
    if let Some(state) = ctx.local_states.get_mut(scrut_local.0 as usize) {
        *state = LocalState::Moved;
    }

    let (enum_name, enum_args) = match ctx.locals[scrut_local.0 as usize].ty.clone() {
        Some(Type::Enum(name)) => (name, None),
        Some(Type::App { base, args }) => (base, Some(args)),
        _ => {
            ctx.error("match scrutinee must be an enum value", Some(span));
            return None;
        }
    };

    let enum_def = match ctx.resolver.get_enum(&enum_name) {
        Some(e) => e.clone(),
        None => {
            ctx.error(format!("unknown enum type '{}'", enum_name), Some(span));
            return None;
        }
    };

    fn substitute_match_params(ty: &Type, args: Option<&[Type]>) -> Type {
        let Some(args) = args else {
            return ty.clone();
        };
        match ty {
            Type::Param(p) if p == "T" => args.get(0).cloned().unwrap_or_else(|| ty.clone()),
            Type::Param(p) if p == "E" => args.get(1).cloned().unwrap_or_else(|| ty.clone()),
            Type::Ref(inner, m) => {
                Type::Ref(Box::new(substitute_match_params(inner, Some(args))), *m)
            }
            Type::Array(inner, size) => {
                Type::Array(Box::new(substitute_match_params(inner, Some(args))), *size)
            }
            Type::Own(inner) => Type::Own(Box::new(substitute_match_params(inner, Some(args)))),
            Type::RawPtr(inner) => {
                Type::RawPtr(Box::new(substitute_match_params(inner, Some(args))))
            }
            Type::Shared(inner) => {
                Type::Shared(Box::new(substitute_match_params(inner, Some(args))))
            }
            Type::App { base, args: targs } => Type::App {
                base: base.clone(),
                args: targs
                    .iter()
                    .map(|a| substitute_match_params(a, Some(args)))
                    .collect(),
            },
            Type::Tuple(elems) => Type::Tuple(
                elems
                    .iter()
                    .map(|e| substitute_match_params(e, Some(args)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    // Exhaustiveness check
    let mut seen_variants = std::collections::HashSet::new();
    let mut has_wildcard = false;
    for arm in arms {
        match &arm.pattern {
            glyph_core::ast::MatchPattern::Wildcard => has_wildcard = true,
            glyph_core::ast::MatchPattern::Variant { name, .. } => {
                seen_variants.insert(name.0.clone());
            }
        }
    }
    if !has_wildcard && seen_variants.len() < enum_def.variants.len() {
        ctx.error(
            "non-exhaustive match: cover all variants or add `_` arm",
            Some(span),
        );
    }

    // Tag extraction
    let tag_local = ctx.fresh_local(None);
    ctx.locals[tag_local.0 as usize].ty = Some(Type::I32);
    ctx.push_inst(MirInst::Assign {
        local: tag_local,
        value: Rvalue::EnumTag { base: scrut_local },
    });

    // Result local if needed
    let result_local = if require_value {
        let l = ctx.fresh_local(None);
        if let Some(ty) = expected {
            ctx.locals[l.0 as usize].ty = Some(ty.clone());
        }
        Some(l)
    } else {
        None
    };

    let join_block = ctx.new_block();
    let mut arm_blocks = Vec::new();
    for _ in arms {
        arm_blocks.push(ctx.new_block());
    }

    // Build chain of comparisons.
    //
    // NOTE: we must emit the tag compare + branch into the *current* block of
    // the chain. Previously we were appending `Assign`s via `ctx.push_inst`
    // (which always targets `ctx.current`) while writing the `If` terminator
    // into a different block (`current`). That could leave blocks without a
    // terminator and also append instructions after a terminator.
    let mut current = ctx.current;
    for (idx, arm) in arms.iter().enumerate() {
        // Ensure subsequent `push_inst`s land in the right block.
        ctx.switch_to(current);

        let arm_block = arm_blocks[idx];
        match &arm.pattern {
            glyph_core::ast::MatchPattern::Wildcard => {
                ctx.push_inst(MirInst::Goto(arm_block));
                break;
            }
            glyph_core::ast::MatchPattern::Variant { name, .. } => {
                let variant_index = enum_def
                    .variants
                    .iter()
                    .position(|v| v.name == name.0)
                    .unwrap_or_else(|| {
                        ctx.error(
                            format!("unknown variant '{}' for enum '{}'", name.0, enum_name),
                            Some(arm.span),
                        );
                        0
                    });
                let cond_local = ctx.fresh_local(None);
                ctx.locals[cond_local.0 as usize].ty = Some(Type::Bool);
                ctx.push_inst(MirInst::Assign {
                    local: cond_local,
                    value: Rvalue::Binary {
                        op: BinaryOp::Eq,
                        lhs: MirValue::Local(tag_local),
                        rhs: MirValue::Int(variant_index as i64),
                    },
                });

                let else_block = if idx == arms.len() - 1 {
                    let sink = ctx.new_block();
                    ctx.blocks[sink.0 as usize]
                        .insts
                        .push(MirInst::Goto(join_block));
                    sink
                } else {
                    ctx.new_block()
                };

                ctx.push_inst(MirInst::If {
                    cond: MirValue::Local(cond_local),
                    then_bb: arm_block,
                    else_bb: else_block,
                });

                current = else_block;
            }
        }
    }

    // Lower arms
    //
    // IMPORTANT: lowering arms can move locals. We must not let moves in one arm
    // affect the move-state seen by subsequent arms, otherwise we can get false
    // "use of moved value" diagnostics when a value is moved in only some arms.
    //
    // We snapshot the local state for the locals that exist before the arms,
    // restore that snapshot at the start of each arm, and then conservatively
    // merge the resulting states (Moved wins, then Uninitialized, then Initialized)
    // for the join point.
    let base_len = ctx.local_states.len();
    let base_states = ctx.local_states.clone();
    let mut merged_states = base_states.clone();
    let mut any_reaches_join = false;

    for (idx, arm) in arms.iter().enumerate() {
        let arm_block = arm_blocks[idx];

        // Reset pre-existing locals to their pre-arm state.
        for i in 0..base_len {
            ctx.local_states[i] = base_states[i];
        }

        ctx.switch_to(arm_block);
        ctx.enter_scope();

        if let glyph_core::ast::MatchPattern::Variant { name, binding } = &arm.pattern {
            if let Some(bind_ident) = binding {
                if let Some(variant) = enum_def.variants.iter().find(|v| v.name == name.0) {
                    if let Some(payload_ty) = variant.payload.clone() {
                        let payload_ty = substitute_match_params(&payload_ty, enum_args.as_deref());
                        let binding_local = ctx.fresh_local(Some(&bind_ident.0));
                        ctx.locals[binding_local.0 as usize].ty = Some(payload_ty.clone());
                        ctx.bind_name(&bind_ident.0, binding_local);
                        ctx.register_source_binding(&bind_ident.0, arm.span, binding_local);
                        ctx.push_inst(MirInst::Assign {
                            local: binding_local,
                            value: Rvalue::EnumPayload {
                                base: scrut_local,
                                variant_index: enum_def
                                    .variants
                                    .iter()
                                    .position(|v| v.name == name.0)
                                    .unwrap_or(0)
                                    as u32,
                                payload_type: payload_ty,
                            },
                        });
                    }
                }
            }
        }

        let arm_expected_ty: Option<Type> = expected.cloned().or_else(|| {
            result_local
                .and_then(|l| ctx.locals.get(l.0 as usize))
                .and_then(|l| l.ty.clone())
        });
        let arm_val = lower_value_with_expected(ctx, &arm.expr, arm_expected_ty.as_ref());
        if require_value {
            if let Some(val) = arm_val {
                let res_local = result_local.expect("match result local missing");
                if ctx.locals[res_local.0 as usize].ty.is_none() {
                    ctx.locals[res_local.0 as usize].ty = match &val {
                        MirValue::Local(id) => ctx.locals[id.0 as usize].ty.clone(),
                        MirValue::Int(_) => Some(Type::I32),
                        MirValue::Float(_) => Some(Type::F64),
                        MirValue::Bool(_) => Some(Type::Bool),
                        MirValue::Unit => None,
                    };
                }

                if let Some(rv) = rvalue_from_value(val) {
                    ctx.push_inst(MirInst::Assign {
                        local: res_local,
                        value: rv,
                    });
                }
            }
        }

        let arm_reaches_join = !ctx.terminated();
        if arm_reaches_join {
            ctx.push_inst(MirInst::Goto(join_block));
        }
        ctx.exit_scope();

        if arm_reaches_join {
            any_reaches_join = true;
            // Merge move-state back into the join state.
            for i in 0..base_len {
                merged_states[i] = match (merged_states[i], ctx.local_states[i]) {
                    (LocalState::Moved, _) | (_, LocalState::Moved) => LocalState::Moved,
                    (LocalState::Uninitialized, _) | (_, LocalState::Uninitialized) => {
                        LocalState::Uninitialized
                    }
                    _ => LocalState::Initialized,
                };
            }
        }
    }

    // Apply merged local state for the join point.
    if any_reaches_join {
        for i in 0..base_len {
            ctx.local_states[i] = merged_states[i];
        }
    } else {
        for i in 0..base_len {
            ctx.local_states[i] = base_states[i];
        }
    }

    ctx.switch_to(join_block);

    result_local.map(Rvalue::Move)
}

fn resolve_try_variant(
    ctx: &mut LowerCtx<'_>,
    enum_name: &str,
    enum_def: &EnumType,
    variant_name: &str,
    enum_args: Option<&[Type]>,
    span: Span,
) -> Option<(u32, Option<Type>)> {
    let (index, variant) = enum_def
        .variants
        .iter()
        .enumerate()
        .find(|(_, variant)| variant.name == variant_name)
        .or_else(|| {
            ctx.error(
                format!(
                    "enum '{}' is missing '{}' variant required by `?`",
                    enum_name, variant_name
                ),
                Some(span),
            );
            None
        })?;

    let generic_payload = match (enum_name, variant_name, enum_args) {
        ("Result", "Ok", Some(args)) => args.first().cloned(),
        ("Result", "Err", Some(args)) => args.get(1).cloned(),
        ("Option", "Some", Some(args)) => args.first().cloned(),
        _ => None,
    };

    Some((
        index as u32,
        generic_payload.or_else(|| variant.payload.clone()),
    ))
}

fn resolve_try_return_enum(
    ctx: &mut LowerCtx<'_>,
    fn_ret: &Option<Type>,
    expected_enum: &str,
    span: Span,
) -> Option<String> {
    match fn_ret {
        Some(Type::App { base, .. }) if base == expected_enum => Some(base.clone()),
        Some(Type::Enum(name)) if name == expected_enum => Some(name.clone()),
        Some(_) => {
            ctx.error(
                format!(
                    "the `?` operator on '{}' values requires the enclosing function to return '{}'",
                    expected_enum, expected_enum
                ),
                Some(span),
            );
            None
        }
        None => None,
    }
}

// Lower the `?` (try) operator.
//
// `expr?` desugars to:
//   match expr { Ok(v) => v, Err(e) => ret Err(e) }
//
// The enclosing function must return Result<_, E> or Option<_> for the early
// return to be valid.
fn lower_try<'a>(ctx: &mut LowerCtx<'a>, inner_expr: &'a Expr, span: Span) -> Option<Rvalue> {
    // 1. Lower the inner expression
    let scrut_val = lower_value(ctx, inner_expr)?;
    let scrut_local = match scrut_val {
        MirValue::Local(id) => id,
        _ => {
            let tmp = ctx.fresh_local(None);
            let rv = rvalue_from_value(scrut_val)?;
            ctx.push_inst(MirInst::Assign {
                local: tmp,
                value: rv,
            });
            tmp
        }
    };
    // `?` is lowered as a by-value match and consumes the scrutinee.
    if let Some(state) = ctx.local_states.get_mut(scrut_local.0 as usize) {
        *state = LocalState::Moved;
    }

    // 2. Check that the scrutinee is a Result or Option type
    let scrut_ty = ctx.locals[scrut_local.0 as usize].ty.clone();
    let (enum_name, enum_args) = match &scrut_ty {
        Some(Type::Enum(name)) => (name.clone(), None),
        Some(Type::App { base, args }) => (base.clone(), Some(args.clone())),
        _ => {
            ctx.error(
                "the `?` operator can only be used on Result or Option values",
                Some(span),
            );
            return None;
        }
    };

    // Determine if this is Result<T, E> or Option<T>
    let is_result = enum_name == "Result";
    let is_option = enum_name == "Option";
    if !is_result && !is_option {
        ctx.error(
            format!(
                "the `?` operator can only be used on Result or Option values, got '{}'",
                enum_name
            ),
            Some(span),
        );
        return None;
    }

    // 3. Check that the enclosing function returns a compatible type
    let fn_ret = ctx.fn_ret_type.clone();
    if fn_ret.is_none() {
        ctx.error(
            "the `?` operator requires the enclosing function to return Result or Option",
            Some(span),
        );
        return None;
    }

    // 4. Get the enum definition
    let enum_def = match ctx.resolver.get_enum(&enum_name) {
        Some(e) => e.clone(),
        None => {
            ctx.error(
                format!("unknown enum type '{}' in `?` operator", enum_name),
                Some(span),
            );
            return None;
        }
    };

    let (ok_variant_name, err_variant_name) = if is_result {
        ("Ok", "Err")
    } else {
        ("Some", "None")
    };
    let ret_enum_name = resolve_try_return_enum(
        ctx,
        &fn_ret,
        if is_result { "Result" } else { "Option" },
        span,
    )?;

    // 5. Resolve variant indices by name and substitute generic payload types.
    let (ok_variant_index, ok_payload_ty) = resolve_try_variant(
        ctx,
        &enum_name,
        &enum_def,
        ok_variant_name,
        enum_args.as_deref(),
        span,
    )?;
    let (err_variant_index, err_payload_ty) = resolve_try_variant(
        ctx,
        &enum_name,
        &enum_def,
        err_variant_name,
        enum_args.as_deref(),
        span,
    )?;

    // 6. Extract tag
    let tag_local = ctx.fresh_local(None);
    ctx.locals[tag_local.0 as usize].ty = Some(Type::I32);
    ctx.push_inst(MirInst::Assign {
        local: tag_local,
        value: Rvalue::EnumTag { base: scrut_local },
    });

    // 7. Create blocks: ok_block (extract Ok payload), err_block (early return), join_block
    let ok_block = ctx.new_block();
    let err_block = ctx.new_block();
    let join_block = ctx.new_block();

    // 8. Compare tag to Ok variant
    let cond_local = ctx.fresh_local(None);
    ctx.locals[cond_local.0 as usize].ty = Some(Type::Bool);
    ctx.push_inst(MirInst::Assign {
        local: cond_local,
        value: Rvalue::Binary {
            op: glyph_core::ast::BinaryOp::Eq,
            lhs: MirValue::Local(tag_local),
            rhs: MirValue::Int(ok_variant_index as i64),
        },
    });
    ctx.push_inst(MirInst::If {
        cond: MirValue::Local(cond_local),
        then_bb: ok_block,
        else_bb: err_block,
    });

    // 9. Ok block: extract payload
    let result_local = ctx.fresh_local(None);
    if let Some(ty) = &ok_payload_ty {
        ctx.locals[result_local.0 as usize].ty = Some(ty.clone());
    }
    let branch_base_len = ctx.local_states.len();
    let branch_base_states = ctx.local_states.clone();

    ctx.switch_to(ok_block);
    if let Some(payload_ty) = &ok_payload_ty {
        ctx.push_inst(MirInst::Assign {
            local: result_local,
            value: Rvalue::EnumPayload {
                base: scrut_local,
                variant_index: ok_variant_index,
                payload_type: payload_ty.clone(),
            },
        });
    }
    ctx.push_inst(MirInst::Goto(join_block));
    let join_len = ctx.local_states.len();
    let join_states = ctx.local_states.clone();

    // 10. Err block: early return with Err(e) or None
    for (idx, state) in branch_base_states
        .iter()
        .take(branch_base_len)
        .copied()
        .enumerate()
    {
        ctx.local_states[idx] = state;
    }
    ctx.switch_to(err_block);
    if is_result {
        // Extract error payload and wrap in Err for the return type
        if let Some(err_ty) = &err_payload_ty {
            let err_payload = ctx.fresh_local(None);
            ctx.locals[err_payload.0 as usize].ty = Some(err_ty.clone());
            ctx.push_inst(MirInst::Assign {
                local: err_payload,
                value: Rvalue::EnumPayload {
                    base: scrut_local,
                    variant_index: err_variant_index,
                    payload_type: err_ty.clone(),
                },
            });

            // Construct the Err variant for the function's return type
            let ret_err = ctx.fresh_local(None);
            ctx.locals[ret_err.0 as usize].ty = fn_ret.clone();
            ctx.push_inst(MirInst::Assign {
                local: ret_err,
                value: Rvalue::EnumConstruct {
                    enum_name: ret_enum_name.clone(),
                    variant_index: err_variant_index,
                    payload: Some(MirValue::Local(err_payload)),
                },
            });
            if let Some(state) = ctx.local_states.get_mut(ret_err.0 as usize) {
                *state = LocalState::Moved;
            }
            ctx.drop_all_active_locals();
            ctx.push_inst(MirInst::Return(Some(MirValue::Local(ret_err))));
        } else {
            ctx.drop_all_active_locals();
            ctx.push_inst(MirInst::Return(None));
        }
    } else {
        // Option: return None
        let ret_none = ctx.fresh_local(None);
        ctx.locals[ret_none.0 as usize].ty = fn_ret.clone();
        ctx.push_inst(MirInst::Assign {
            local: ret_none,
            value: Rvalue::EnumConstruct {
                enum_name: ret_enum_name.clone(),
                variant_index: err_variant_index,
                payload: None,
            },
        });
        if let Some(state) = ctx.local_states.get_mut(ret_none.0 as usize) {
            *state = LocalState::Moved;
        }
        ctx.drop_all_active_locals();
        ctx.push_inst(MirInst::Return(Some(MirValue::Local(ret_none))));
    }

    // 11. Continue from join block with the unwrapped Ok value
    for (idx, state) in join_states.iter().take(join_len).copied().enumerate() {
        ctx.local_states[idx] = state;
    }
    ctx.switch_to(join_block);
    Some(Rvalue::Move(result_local))
}

pub(crate) fn lower_struct_lit<'a>(
    ctx: &mut LowerCtx<'a>,
    name: &'a Ident,
    fields: &'a [(Ident, Expr)],
    span: Span,
) -> Option<Rvalue> {
    let struct_name = name.0.clone();
    let Some(struct_type) = ctx.resolver.get_struct(&struct_name) else {
        ctx.error(format!("unknown struct '{}'", struct_name), Some(span));
        return None;
    };

    let mut ok = true;

    let mut lowered_fields = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for (field_name, expr) in fields {
        if !seen.insert(field_name.0.as_str()) {
            ctx.error(
                format!("duplicate field '{}' in struct literal", field_name.0),
                Some(span),
            );
            ok = false;
            continue;
        }

        if !struct_type.fields.iter().any(|(n, _)| n == &field_name.0) {
            ctx.error(
                format!(
                    "struct '{}' has no field named '{}'",
                    struct_name, field_name.0
                ),
                Some(span),
            );
            ok = false;
            continue;
        }

        // The field's declared type is not threaded into lowering, but it is
        // what an integer literal has to fit, so range-check against it here.
        let field_ty = struct_type
            .fields
            .iter()
            .find(|(n, _)| n == &field_name.0)
            .map(|(_, ty)| ty.clone());
        check_int_literal_range(ctx, expr, field_ty.as_ref());

        match lower_value(ctx, expr) {
            Some(value) => {
                consume_value_local(ctx, &value, span);
                lowered_fields.push((field_name.0.clone(), value));
            }
            None => ok = false,
        }
    }

    for (required, _) in &struct_type.fields {
        if !lowered_fields.iter().any(|(n, _)| n == required) {
            ctx.error(
                format!(
                    "missing field '{}' in struct literal '{}'",
                    required, struct_name
                ),
                Some(span),
            );
            ok = false;
        }
    }

    if !ok {
        return None;
    }

    let tmp = ctx.fresh_local(None);
    ctx.locals[tmp.0 as usize].ty = Some(Type::Named(struct_name.clone()));

    ctx.push_inst(MirInst::Assign {
        local: tmp,
        value: Rvalue::StructLit {
            struct_name,
            field_values: lowered_fields,
        },
    });

    Some(Rvalue::Move(tmp))
}

pub(crate) fn lower_tuple_expr<'a>(
    ctx: &mut LowerCtx<'a>,
    elements: &'a [Expr],
    span: Span,
) -> Option<Rvalue> {
    // Empty tuple is unit type
    if elements.is_empty() {
        return Some(Rvalue::ConstInt(0));
    }

    let mut elem_types = Vec::new();
    let mut elem_values = Vec::new();

    // Lower each element and infer its type
    for elem_expr in elements {
        let val = lower_value(ctx, elem_expr)?;
        consume_value_local(ctx, &val, span);

        // Infer type from the element expression
        let ty = match &val {
            MirValue::Int(_) => Type::I32,
            MirValue::Float(_) => Type::F64,
            MirValue::Bool(_) => Type::Bool,
            MirValue::Local(local_id) => ctx
                .locals
                .get(local_id.0 as usize)
                .and_then(|local| local.ty.clone())
                .ok_or_else(|| {
                    ctx.error("cannot infer type of tuple element", Some(span));
                })
                .ok()?,
            MirValue::Unit => Type::Void,
        };

        elem_types.push(ty);
        elem_values.push(val);
    }

    // Generate tuple struct name using monomorphize-style type_key
    let struct_name = tuple_struct_name(&elem_types);

    // Create field values with numbered fields (0, 1, 2, ...)
    let field_values: Vec<(String, MirValue)> = elem_values
        .into_iter()
        .enumerate()
        .map(|(idx, val)| (idx.to_string(), val))
        .collect();

    let tmp = ctx.fresh_local(None);
    ctx.locals[tmp.0 as usize].ty = Some(Type::Tuple(elem_types));

    ctx.push_inst(MirInst::Assign {
        local: tmp,
        value: Rvalue::StructLit {
            struct_name,
            field_values,
        },
    });

    Some(Rvalue::Move(tmp))
}

pub(crate) fn lower_array_lit<'a>(
    ctx: &mut LowerCtx<'a>,
    elements: &'a [Expr],
    span: Span,
) -> Option<Rvalue> {
    // Check for empty array literal
    if elements.is_empty() {
        ctx.error("empty array literals are not supported", Some(span));
        return None;
    }

    // Lower all elements
    let mut mir_elements = Vec::new();
    for elem in elements {
        let mir_val = lower_value(ctx, elem)?;
        consume_value_local(ctx, &mir_val, span);
        mir_elements.push(mir_val);
    }

    // Infer element type from first element
    let elem_type = infer_value_type(&mir_elements[0], ctx)?;

    // Validate all elements have same type
    for (i, val) in mir_elements.iter().enumerate().skip(1) {
        let val_ty = infer_value_type(val, ctx)?;
        if val_ty != elem_type {
            ctx.error(
                format!(
                    "array element {} has type {:?}, expected {:?}",
                    i, val_ty, elem_type
                ),
                Some(span),
            );
            return None;
        }
    }

    let array_size = mir_elements.len();
    let array_type = Type::Array(Box::new(elem_type.clone()), array_size);

    let tmp = ctx.fresh_local(None);
    ctx.locals[tmp.0 as usize].ty = Some(array_type);

    ctx.push_inst(MirInst::Assign {
        local: tmp,
        value: Rvalue::ArrayLit {
            elem_type,
            elements: mir_elements,
        },
    });

    Some(Rvalue::Move(tmp))
}

pub(crate) fn lower_array_index<'a>(
    ctx: &mut LowerCtx<'a>,
    base: &'a Expr,
    index: &'a Expr,
    span: Span,
) -> Option<Rvalue> {
    // Lower base to local
    let base_val = lower_value(ctx, base)?;
    let base_local = match base_val {
        MirValue::Local(local_id) => local_id,
        _ => {
            // If base is not a local, create one
            let tmp = ctx.fresh_local(None);
            ctx.push_inst(MirInst::Assign {
                local: tmp,
                value: match base_val {
                    MirValue::Int(i) => Rvalue::ConstInt(i),
                    MirValue::Float(f) => Rvalue::ConstFloat(f),
                    MirValue::Bool(b) => Rvalue::ConstBool(b),
                    MirValue::Local(_) => unreachable!(),
                    MirValue::Unit => unreachable!("unit value cannot be indexed"),
                },
            });
            tmp
        }
    };

    // Get array type from base local
    let base_type = ctx
        .locals
        .get(base_local.0 as usize)
        .and_then(|local| local.ty.as_ref())?;

    let (elem_type, is_vec) = match base_type {
        Type::Array(elem_ty, _) => (elem_ty.as_ref().clone(), false),
        ty if vec_elem_type_from_type(ty).is_some() => (vec_elem_type_from_type(ty).unwrap(), true),
        _ => {
            ctx.error(
                format!("cannot index into non-array type {:?}", base_type),
                Some(span),
            );
            return None;
        }
    };

    // Lower index expression
    let index_val = lower_value(ctx, index)?;

    // Create temp with element type
    let tmp = ctx.fresh_local(None);
    ctx.locals[tmp.0 as usize].ty = Some(elem_type.clone());

    let value = if is_vec {
        Rvalue::VecIndex {
            vec: base_local,
            elem_type,
            index: index_val,
            bounds_check: true,
        }
    } else {
        Rvalue::ArrayIndex {
            base: base_local,
            index: index_val,
            bounds_check: true, // Always check bounds
        }
    };

    ctx.push_inst(MirInst::Assign { local: tmp, value });

    Some(Rvalue::Move(tmp))
}

pub(crate) fn lower_array_len<'a>(
    ctx: &mut LowerCtx<'a>,
    base: &'a Expr,
    span: Span,
) -> Option<Rvalue> {
    let base_local = match base {
        Expr::Ident(ident, _) => ctx.bindings.get(ident.0.as_str()).copied(),
        _ => None,
    };

    let base_local = match base_local {
        Some(local) => local,
        None => {
            ctx.error(".len() target must be a local array", Some(span));
            return None;
        }
    };

    let Some(base_ty) = ctx
        .locals
        .get(base_local.0 as usize)
        .and_then(|local| local.ty.as_ref())
    else {
        ctx.error("array length target has unknown type", Some(span));
        return None;
    };

    if !matches!(base_ty, Type::Array(_, _)) {
        ctx.error(".len() is only supported on arrays", Some(span));
        return None;
    }

    let tmp = ctx.fresh_local(None);
    ctx.locals[tmp.0 as usize].ty = Some(Type::I32);
    ctx.push_inst(MirInst::Assign {
        local: tmp,
        value: Rvalue::ArrayLen { base: base_local },
    });
    Some(Rvalue::Move(tmp))
}

pub(crate) fn lower_field_access<'a>(
    ctx: &mut LowerCtx<'a>,
    base: &'a Expr,
    field: &'a Ident,
    span: Span,
    expected: Option<&Type>,
) -> Option<Rvalue> {
    let base_local = match base {
        Expr::FieldAccess { .. } => lower_field_access_ref(ctx, base, span)?,
        _ => {
            let base_val = lower_value(ctx, base)?;
            match base_val {
                MirValue::Local(id) => id,
                _ => {
                    ctx.error("field access base must be a local", Some(span));
                    return None;
                }
            }
        }
    };

    let (field_type, field_index) = resolve_field_info(ctx, base_local, field, span)?;

    if matches!(expected, Some(Type::String)) && matches!(field_type, Type::Str) {
        ctx.error(
            format!(
                "cannot move borrowed `str` field '{}' into `String`; use `String::from_str(<struct>.{})`",
                field.0, field.0
            ),
            Some(span),
        );
        return None;
    }

    if is_move_context(expected, &field_type) && !type_is_copy(&field_type) {
        ctx.error(
            format!(
                "cannot move field '{}' out of a struct; borrow it (`let x: str = <struct>.{}`), clone it (`<struct>.{}.clone()`), or move the whole struct",
                field.0, field.0, field.0
            ),
            Some(span),
        );
        return None;
    }

    let result_type = if matches!(expected, Some(Type::Str)) && matches!(field_type, Type::String) {
        Type::Str
    } else {
        field_type
    };

    let tmp = ctx.fresh_local(None);
    ctx.locals[tmp.0 as usize].ty = Some(result_type);

    ctx.push_inst(MirInst::Assign {
        local: tmp,
        value: Rvalue::FieldAccess {
            base: base_local,
            field_name: field.0.clone(),
            field_index: field_index as u32,
        },
    });

    Some(Rvalue::Move(tmp))
}

pub(crate) fn lower_ref_expr<'a>(
    ctx: &mut LowerCtx<'a>,
    expr: &'a Expr,
    mutability: Mutability,
    span: Span,
) -> Option<Rvalue> {
    match expr {
        Expr::Ident(ident, _) => {
            let Some(local) = ctx.bindings.get(ident.0.as_str()).copied() else {
                ctx.error(format!("unknown identifier '{}'", ident.0), Some(span));
                return None;
            };
            if mutability == Mutability::Mutable {
                let can_mutably_borrow = ctx.locals.get(local.0 as usize).is_some_and(|binding| {
                    binding.mutable || matches!(binding.ty, Some(Type::Ref(_, Mutability::Mutable)))
                });
                if !can_mutably_borrow {
                    ctx.error(
                        format!("cannot mutably borrow immutable variable `{}`", ident.0),
                        Some(span),
                    );
                    return None;
                }
            }
            let kind = if mutability == Mutability::Mutable {
                glyph_core::mir::BorrowKind::Mutable
            } else {
                glyph_core::mir::BorrowKind::Shared
            };
            if !ctx.validate_new_lexical_borrow(local, kind, span) {
                return None;
            }
            if !ctx.check_local_available(local, Some(span)) {
                return None;
            }
            Some(Rvalue::Ref {
                base: local,
                mutability,
            })
        }
        Expr::FieldAccess { base, field, .. } => {
            let base_local = match base.as_ref() {
                Expr::Ident(ident, _) => {
                    let Some(local) = ctx.bindings.get(ident.0.as_str()).copied() else {
                        ctx.error(format!("unknown identifier '{}'", ident.0), Some(span));
                        return None;
                    };
                    if !ctx.check_local_available(local, Some(span)) {
                        return None;
                    }
                    local
                }
                _ => {
                    ctx.error("field reference base must be a local", Some(span));
                    return None;
                }
            };

            let base_type = ctx
                .locals
                .get(base_local.0 as usize)
                .and_then(|l| l.ty.as_ref());
            let mut inner = match base_type {
                Some(ty) => ty,
                None => {
                    ctx.error("field access base has unknown type", Some(span));
                    return None;
                }
            };
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

            let (_field_type, field_index, _struct_name) = match inner {
                Type::Tuple(elem_types) => {
                    if let Ok(idx) = field.0.parse::<usize>() {
                        if idx < elem_types.len() {
                            (elem_types[idx].clone(), idx, tuple_struct_name(elem_types))
                        } else {
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
                    } else {
                        ctx.error(
                            format!(
                                "tuple field access must use numeric index, got '{}'",
                                field.0
                            ),
                            Some(span),
                        );
                        return None;
                    }
                }
                _ => {
                    let Some(struct_name) = local_struct_name(ctx, base_local) else {
                        ctx.error("field access base is not a struct", Some(span));
                        return None;
                    };

                    let Some((field_type, field_index)) =
                        ctx.resolver.get_field(&struct_name, &field.0)
                    else {
                        ctx.error(
                            format!("struct '{}' has no field named '{}'", struct_name, field.0),
                            Some(span),
                        );
                        return None;
                    };

                    (field_type, field_index, struct_name)
                }
            };

            Some(Rvalue::FieldRef {
                base: base_local,
                field_name: field.0.clone(),
                field_index: field_index as u32,
                mutability,
            })
        }
        _ => {
            ctx.error("references can only be taken to locals", Some(span));
            None
        }
    }
}

pub(crate) fn lower_value<'a>(ctx: &mut LowerCtx<'a>, expr: &'a Expr) -> Option<MirValue> {
    lower_value_with_expected(ctx, expr, None)
}

pub(crate) fn lower_value_with_expected<'a>(
    ctx: &mut LowerCtx<'a>,
    expr: &'a Expr,
    expected: Option<&Type>,
) -> Option<MirValue> {
    check_int_literal_range(ctx, expr, expected);
    match expr {
        Expr::Lit(glyph_core::ast::Literal::Int(i), _) => {
            // If we have an expected integer type, create a typed local so the
            // literal carries the correct width through MIR to codegen.
            if let Some(ty) = expected {
                if ty.is_int() && *ty != Type::I32 {
                    let tmp = ctx.fresh_local(None);
                    ctx.locals[tmp.0 as usize].ty = Some(ty.clone());
                    ctx.push_inst(MirInst::Assign {
                        local: tmp,
                        value: Rvalue::ConstInt(*i),
                    });
                    return Some(MirValue::Local(tmp));
                }
            }
            Some(MirValue::Int(*i))
        }
        Expr::Lit(glyph_core::ast::Literal::Float(fl), _) => {
            // Bare float literals default to f64; an expected f32 type creates a
            // typed local so the literal carries its width through MIR to codegen.
            if let Some(ty) = expected {
                if ty.is_float() && *ty != Type::F64 {
                    let tmp = ctx.fresh_local(None);
                    ctx.locals[tmp.0 as usize].ty = Some(ty.clone());
                    ctx.push_inst(MirInst::Assign {
                        local: tmp,
                        value: Rvalue::ConstFloat(*fl),
                    });
                    return Some(MirValue::Local(tmp));
                }
            }
            Some(MirValue::Float(*fl))
        }
        Expr::Lit(glyph_core::ast::Literal::Bool(b), _) => Some(MirValue::Bool(*b)),
        Expr::Lit(glyph_core::ast::Literal::Char(c), _) => {
            let tmp = ctx.fresh_local(None);
            ctx.locals[tmp.0 as usize].ty = Some(Type::Char);
            ctx.push_inst(MirInst::Assign {
                local: tmp,
                value: Rvalue::ConstInt(*c as i64),
            });
            Some(MirValue::Local(tmp))
        }
        Expr::Lit(glyph_core::ast::Literal::Str(s), _) => {
            let tmp = ctx.fresh_local(None);
            ctx.locals[tmp.0 as usize].ty = Some(Type::Str);
            let rv = Rvalue::StringLit {
                content: s.clone(),
                global_name: ctx.fresh_string_global(),
            };
            ctx.push_inst(MirInst::Assign {
                local: tmp,
                value: rv,
            });
            Some(MirValue::Local(tmp))
        }
        Expr::InterpString { segments, span } => {
            let rv = lower_interp_string(ctx, segments, *span)?;
            let tmp = ctx.fresh_local(None);
            ctx.locals[tmp.0 as usize].ty = Some(Type::Str);
            ctx.push_inst(MirInst::Assign {
                local: tmp,
                value: rv,
            });
            Some(MirValue::Local(tmp))
        }
        Expr::Ident(ident, span) => {
            if let Some(local) = ctx.bindings.get(ident.0.as_str()).copied() {
                if let Some(actual) = ctx.local_ty(local).cloned() {
                    if !validate_callable_expectation(
                        ctx,
                        &actual,
                        expected,
                        &format!("value '{}'", ident.0),
                        *span,
                    ) {
                        return None;
                    }
                }
                let implicit = ctx.implicit_deref_type(local).cloned();
                if let Some(inner) = implicit.filter(|_| !matches!(expected, Some(Type::Ref(_, _))))
                {
                    let tmp = ctx.fresh_local(None);
                    ctx.locals[tmp.0 as usize].ty = Some(inner.clone());
                    ctx.locals[tmp.0 as usize].skip_drop = !type_is_copy(&inner);
                    ctx.push_inst(MirInst::Assign {
                        local: tmp,
                        value: Rvalue::Deref {
                            base: local,
                            ty: inner,
                        },
                    });
                    Some(MirValue::Local(tmp))
                } else if ctx.consume_local(local, Some(*span)) {
                    Some(MirValue::Local(local))
                } else {
                    None
                }
            } else if let Some(value) = lookup_const_value(ctx, ident.0.as_str(), *span) {
                lower_const_value(ctx, value)
            } else if is_unit_enum_ctor(ctx, ident.0.as_str()) {
                // Bare unit-variant paths (`Val::Nil`, `None`) construct the
                // variant; delegate to call lowering with zero arguments.
                lower_call(ctx, expr, &[], *span, false, expected).and_then(rvalue_to_value)
            } else if ctx.fn_sigs.contains_key(ident.0.as_str()) {
                let function_ref = lower_function_item(ctx, ident.0.as_str(), *span, expected)?;
                let signature = match &function_ref {
                    Rvalue::FunctionRef { signature, .. } => signature.clone(),
                    _ => unreachable!(),
                };
                let tmp = ctx.fresh_local(None);
                ctx.locals[tmp.0 as usize].ty = Some(signature);
                ctx.push_inst(MirInst::Assign {
                    local: tmp,
                    value: function_ref,
                });
                Some(MirValue::Local(tmp))
            } else {
                None
            }
        }
        Expr::Unary { op, expr, span } => match op {
            UnaryOp::Not => lower_unary_not(ctx, expr, *span).and_then(rvalue_to_value),
            UnaryOp::Neg => lower_unary_neg(ctx, expr, *span).and_then(rvalue_to_value),
        },
        Expr::Cast { expr, target, span } => {
            lower_cast(ctx, expr, target, *span).and_then(rvalue_to_value)
        }
        Expr::Binary { op, lhs, rhs, .. } => match *op {
            glyph_core::ast::BinaryOp::And | glyph_core::ast::BinaryOp::Or => {
                lower_logical(ctx, op, lhs, rhs).and_then(rvalue_to_value)
            }
            _ => lower_binary(ctx, op, lhs, rhs).and_then(rvalue_to_value),
        },
        Expr::Call { callee, args, span } => {
            lower_call(ctx, callee, args, *span, true, expected).and_then(rvalue_to_value)
        }
        Expr::MethodCall {
            receiver,
            method,
            args,
            span,
        } => lower_method_call(ctx, receiver, method, args, *span).and_then(rvalue_to_value),
        Expr::Match {
            scrutinee,
            arms,
            span,
        } => lower_match(ctx, scrutinee, arms, true, *span, expected).and_then(rvalue_to_value),
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => lower_if_value(ctx, cond, then_block, else_block.as_ref(), expected),
        Expr::Block(block) => lower_block_with_expected(ctx, block, expected, true, true),
        Expr::StructLit { name, fields, span } => {
            lower_struct_lit(ctx, name, fields, *span).and_then(rvalue_to_value)
        }
        Expr::FieldAccess { base, field, span } => {
            lower_field_access(ctx, base, field, *span, expected).and_then(rvalue_to_value)
        }
        Expr::Ref {
            expr,
            mutability,
            span,
        } => {
            let mut rv = lower_ref_expr(ctx, expr, *mutability, *span)?;
            let tmp = ctx.fresh_local(None);
            update_local_type_from_rvalue(ctx, tmp, &mut rv);
            ctx.push_inst(MirInst::Assign {
                local: tmp,
                value: rv,
            });
            Some(MirValue::Local(tmp))
        }
        Expr::ArrayLit { elements, span } => {
            lower_array_lit(ctx, elements, *span).and_then(rvalue_to_value)
        }
        Expr::Index { base, index, span } => {
            lower_array_index(ctx, base, index, *span).and_then(rvalue_to_value)
        }
        Expr::Tuple { elements, span } => {
            lower_tuple_expr(ctx, elements, *span).and_then(rvalue_to_value)
        }
        Expr::Try { expr, span } => lower_try(ctx, expr, *span).and_then(rvalue_to_value),
        Expr::Closure {
            capture,
            params,
            body,
            span,
        } => {
            let rv = lower_closure_rvalue(ctx, *capture, params, body, *span, expected)?;
            let signature = match &rv {
                Rvalue::MakeClosure { signature, .. }
                | Rvalue::MakeBorrowedClosure { signature, .. } => signature.clone(),
                _ => unreachable!("closure lowering always constructs a closure"),
            };
            let tmp = ctx.fresh_local(None);
            ctx.locals[tmp.0 as usize].ty = Some(signature);
            ctx.push_inst(MirInst::Assign {
                local: tmp,
                value: rv,
            });
            Some(MirValue::Local(tmp))
        }
        _ => None,
    }
}
