use glyph_core::ast::Expr;
use glyph_core::mir::{LocalId, MirValue, Rvalue};
use glyph_core::span::Span;
use glyph_core::types::Type;

use super::context::LowerCtx;
use super::types::struct_name_from_type;

pub(crate) fn rvalue_from_value(val: MirValue) -> Option<Rvalue> {
    match val {
        MirValue::Int(i) => Some(Rvalue::ConstInt(i)),
        MirValue::Float(f) => Some(Rvalue::ConstFloat(f)),
        MirValue::Bool(b) => Some(Rvalue::ConstBool(b)),
        MirValue::Local(id) => Some(Rvalue::Move(id)),
        MirValue::Unit => Some(Rvalue::ConstInt(0)),
    }
}

pub(crate) fn rvalue_to_value(rv: Rvalue) -> Option<MirValue> {
    match rv {
        Rvalue::ConstInt(v) => Some(MirValue::Int(v)),
        Rvalue::ConstFloat(v) => Some(MirValue::Float(v)),
        Rvalue::ConstBool(v) => Some(MirValue::Bool(v)),
        Rvalue::Move(local) => Some(MirValue::Local(local)),
        _ => None,
    }
}

pub(crate) fn bool_rvalue(value: MirValue) -> Rvalue {
    match value {
        MirValue::Bool(b) => Rvalue::ConstBool(b),
        MirValue::Local(id) => Rvalue::Move(id),
        MirValue::Int(i) => Rvalue::ConstBool(i != 0),
        _ => Rvalue::ConstBool(false),
    }
}

pub(crate) fn coerce_to_bool(value: MirValue) -> MirValue {
    match value {
        MirValue::Bool(_) | MirValue::Local(_) => value,
        MirValue::Int(i) => MirValue::Bool(i != 0),
        _ => MirValue::Bool(false),
    }
}

pub(crate) fn infer_value_type(value: &MirValue, ctx: &LowerCtx) -> Option<Type> {
    match value {
        MirValue::Int(_) => Some(Type::I32),
        MirValue::Float(_) => Some(Type::F64),
        MirValue::Bool(_) => Some(Type::Bool),
        MirValue::Unit => Some(Type::Void),
        MirValue::Local(local_id) => ctx
            .locals
            .get(local_id.0 as usize)
            .and_then(|local| local.ty.clone()),
    }
}

pub(crate) fn infer_numeric_result_type(
    lhs: &MirValue,
    rhs: &MirValue,
    ctx: &LowerCtx,
) -> Option<Type> {
    #[derive(PartialEq)]
    enum Lit {
        No,
        Int,
        Float,
    }

    fn type_for_value(v: &MirValue, ctx: &LowerCtx) -> (Option<Type>, Lit) {
        match v {
            MirValue::Local(id) => (
                ctx.locals.get(id.0 as usize).and_then(|l| l.ty.clone()),
                Lit::No,
            ),
            MirValue::Int(_) => (None, Lit::Int), // untyped literal: defer to the other operand
            MirValue::Float(_) => (None, Lit::Float), // bare float literal defaults to f64
            MirValue::Bool(_) | MirValue::Unit => (None, Lit::No),
        }
    }

    fn width(ty: &Type) -> Option<u32> {
        match ty {
            Type::I8 | Type::U8 => Some(8),
            Type::I32 | Type::U32 | Type::Char => Some(32),
            Type::I64 | Type::U64 | Type::Usize => Some(64),
            _ => None,
        }
    }

    let (lt, lhs_lit) = type_for_value(lhs, ctx);
    let (rt, rhs_lit) = type_for_value(rhs, ctx);

    // Float operands dominate: mixed int/float arithmetic promotes to the float
    // side, and f64 wins over f32. A bare float literal counts as f64.
    let has_f64 = matches!(lt, Some(Type::F64))
        || matches!(rt, Some(Type::F64))
        || lhs_lit == Lit::Float
        || rhs_lit == Lit::Float;
    let has_f32 = matches!(lt, Some(Type::F32)) || matches!(rt, Some(Type::F32));
    if has_f64 {
        return Some(Type::F64);
    }
    if has_f32 {
        return Some(Type::F32);
    }

    match (lt.as_ref(), rt.as_ref()) {
        (Some(l), Some(r)) => {
            let lw = width(l)?;
            let rw = width(r)?;
            if lw >= rw {
                Some(l.clone())
            } else {
                Some(r.clone())
            }
        }
        (Some(l), None) => Some(l.clone()),
        (None, Some(r)) => Some(r.clone()),
        _ => {
            // Both are untyped int literals - default to i32
            if lhs_lit == Lit::Int || rhs_lit == Lit::Int {
                Some(Type::I32)
            } else {
                None
            }
        }
    }
}

pub(crate) fn update_local_type_from_rvalue(ctx: &mut LowerCtx, local: LocalId, rv: &mut Rvalue) {
    let existing = ctx.locals[local.0 as usize].ty.clone();

    // If the local is already typed (e.g. from an annotation), keep that type and
    // adjust builtin container constructors to match it.
    if let Some(existing_ty) = &existing {
        match (existing_ty, rv) {
            (
                Type::App { base, args },
                Rvalue::VecNew { elem_type }
                | Rvalue::VecWithCapacity { elem_type, .. }
                | Rvalue::VecPush { elem_type, .. },
            ) if base == "Vec" && args.len() == 1 => {
                *elem_type = args[0].clone();
            }
            (
                Type::App { base, args },
                Rvalue::MapNew {
                    key_type,
                    value_type,
                }
                | Rvalue::MapWithCapacity {
                    key_type,
                    value_type,
                    ..
                },
            ) if base == "Map" && args.len() == 2 => {
                *key_type = args[0].clone();
                *value_type = args[1].clone();
            }
            _ => {}
        }

        return;
    }

    if let Some(inferred) = infer_rvalue_type(rv, ctx) {
        ctx.locals[local.0 as usize].ty = Some(inferred);
    }
}

pub(crate) fn infer_rvalue_type(rv: &Rvalue, ctx: &LowerCtx) -> Option<Type> {
    match rv {
        Rvalue::ConstFloat(_) => Some(Type::F64),
        Rvalue::Cast { to, .. } => Some(to.clone()),
        Rvalue::StructLit { struct_name, .. } => Some(Type::Named(struct_name.clone())),
        Rvalue::Move(local) => ctx
            .locals
            .get(local.0 as usize)
            .and_then(|l| l.ty.as_ref().cloned()),
        Rvalue::StringLit { .. } => Some(Type::Str),
        Rvalue::StringClone { .. } => Some(Type::String),
        Rvalue::Ref { base, mutability } => ctx
            .locals
            .get(base.0 as usize)
            .and_then(|l| l.ty.as_ref().cloned())
            .map(|inner| Type::Ref(Box::new(inner), *mutability)),
        Rvalue::FieldRef {
            base,
            field_index,
            mutability,
            ..
        } => {
            let base_ty = ctx
                .locals
                .get(base.0 as usize)
                .and_then(|l| l.ty.as_ref())?;
            let mut inner = base_ty;
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

            let field_ty = match inner {
                Type::Tuple(elem_types) => elem_types.get(*field_index as usize).cloned(),
                _ => {
                    let struct_name = struct_name_from_type(base_ty)?;
                    let struct_type = ctx.resolver.get_struct(&struct_name)?;
                    struct_type
                        .fields
                        .get(*field_index as usize)
                        .map(|(_, ty)| ty.clone())
                }
            }?;

            Some(Type::Ref(Box::new(field_ty), *mutability))
        }
        Rvalue::ArrayLit {
            elem_type,
            elements,
        } => Some(Type::Array(Box::new(elem_type.clone()), elements.len())),
        Rvalue::ArrayIndex { base, .. } => ctx
            .locals
            .get(base.0 as usize)
            .and_then(|l| l.ty.as_ref())
            .and_then(|ty| match ty {
                Type::Array(elem_ty, _) => Some(elem_ty.as_ref().clone()),
                _ => None,
            }),
        Rvalue::ArrayLen { .. } => Some(Type::I32),
        Rvalue::OwnNew { elem_type, .. } => Some(Type::Own(Box::new(elem_type.clone()))),
        Rvalue::OwnIntoRaw { elem_type, .. } => Some(Type::RawPtr(Box::new(elem_type.clone()))),
        Rvalue::OwnFromRaw { elem_type, .. } => Some(Type::Own(Box::new(elem_type.clone()))),
        Rvalue::RawPtrNull { elem_type } => Some(Type::RawPtr(Box::new(elem_type.clone()))),
        Rvalue::SharedNew { elem_type, .. } => Some(Type::Shared(Box::new(elem_type.clone()))),
        Rvalue::SharedClone { elem_type, .. } => Some(Type::Shared(Box::new(elem_type.clone()))),
        Rvalue::FunctionRef { signature, .. } => Some(signature.clone()),
        Rvalue::CallIndirect { signature, .. } => {
            signature.function_signature().map(|(_, ret)| ret.clone())
        }
        Rvalue::EnumConstruct { enum_name, .. } => Some(Type::Enum(enum_name.clone())),
        Rvalue::EnumTag { .. } => Some(Type::I32),
        Rvalue::EnumPayload { payload_type, .. } => Some(payload_type.clone()),
        Rvalue::VecNew { elem_type }
        | Rvalue::VecWithCapacity { elem_type, .. }
        | Rvalue::VecPush { elem_type, .. } => Some(Type::App {
            base: "Vec".into(),
            args: vec![elem_type.clone()],
        }),
        Rvalue::VecPop { elem_type, .. } => Some(Type::App {
            base: "Option".into(),
            args: vec![elem_type.clone()],
        }),
        Rvalue::VecIndex { elem_type, .. } => Some(elem_type.clone()),
        Rvalue::VecIndexRef {
            elem_type,
            mutability,
            ..
        } => Some(Type::Ref(Box::new(elem_type.clone()), *mutability)),
        Rvalue::VecLen { .. } => Some(Type::Usize),
        _ => None,
    }
}

pub(crate) fn local_struct_name(ctx: &LowerCtx, local: LocalId) -> Option<String> {
    ctx.locals
        .get(local.0 as usize)
        .and_then(|l| l.ty.as_ref())
        .and_then(struct_name_from_type)
}

pub(crate) fn expr_span(expr: &Expr) -> Option<Span> {
    match expr {
        Expr::Lit(_, span) => Some(*span),
        Expr::Ident(_, span) => Some(*span),
        Expr::Unary { span, .. } => Some(*span),
        Expr::Binary { span, .. } => Some(*span),
        Expr::Call { span, .. } => Some(*span),
        Expr::If { span, .. } => Some(*span),
        Expr::While { span, .. } => Some(*span),
        Expr::For { span, .. } => Some(*span),
        Expr::Block(block) => Some(block.span),
        Expr::StructLit { span, .. } => Some(*span),
        Expr::FieldAccess { span, .. } => Some(*span),
        Expr::Ref { span, .. } => Some(*span),
        Expr::ArrayLit { span, .. } => Some(*span),
        Expr::Index { span, .. } => Some(*span),
        Expr::MethodCall { span, .. } => Some(*span),
        Expr::Match { span, .. } => Some(*span),
        Expr::InterpString { span, .. } => Some(*span),
        Expr::Tuple { span, .. } => Some(*span),
        Expr::Try { span, .. } => Some(*span),
        Expr::Cast { span, .. } => Some(*span),
        Expr::Closure { span, .. } => Some(*span),
        Expr::ForIn { span, .. } => Some(*span),
    }
}
