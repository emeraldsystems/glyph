use std::collections::HashSet;

use glyph_core::{
    ast::{BinaryOp, ConstDef, Expr, Literal, Module},
    diag::Diagnostic,
    span::Span,
    types::Type,
};

use super::{ResolverContext, expr_span, resolve_type_expr_to_type, type_expr_to_string};

/// Represents a constant value that can be evaluated at compile-time
#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    Int(i64),
    Bool(bool),
    Char(char),
    Str(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConstBinding {
    pub ty: Type,
    pub value: ConstValue,
}

pub fn validate_consts(
    module: &Module,
    ctx: &mut ResolverContext,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut value_names = HashSet::new();
    for item in &module.items {
        match item {
            glyph_core::ast::Item::Function(func) => {
                value_names.insert(func.name.0.clone());
            }
            glyph_core::ast::Item::ExternFunction(func) => {
                value_names.insert(func.name.0.clone());
            }
            _ => {}
        }
    }

    let module_id = ctx
        .current_module
        .clone()
        .unwrap_or_else(|| "<unknown>".to_string());
    let mut stack: Vec<(String, String)> = Vec::new();

    for item in &module.items {
        let glyph_core::ast::Item::Const(def) = item else {
            continue;
        };

        if value_names.contains(&def.name.0) || ctx.consts.contains_key(&def.name.0) {
            diagnostics.push(Diagnostic::error(
                format!("constant '{}' is defined multiple times", def.name.0),
                Some(def.span),
            ));
            continue;
        }

        let ty = resolve_type_expr_to_type(&def.ty, ctx)
            .unwrap_or_else(|| Type::Named(type_expr_to_string(&def.ty)));
        let value = eval_const_def(&module_id, def, module, ctx, &mut stack, diagnostics);
        if let Some(value) = value {
            if !const_value_matches_type(&value, &ty) {
                diagnostics.push(Diagnostic::error(
                    format!(
                        "const '{}' expects type '{}' but initializer is '{}'",
                        def.name.0,
                        type_expr_to_string(&def.ty),
                        const_value_type_label(&value)
                    ),
                    Some(def.span),
                ));
                continue;
            }

            ctx.consts
                .insert(def.name.0.clone(), ConstBinding { ty, value });
            value_names.insert(def.name.0.clone());
        }
    }
}

fn eval_const_def(
    module_id: &str,
    def: &ConstDef,
    module: &Module,
    ctx: &ResolverContext,
    stack: &mut Vec<(String, String)>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ConstValue> {
    let key = (module_id.to_string(), def.name.0.clone());
    if let Some(pos) = stack.iter().position(|k| *k == key) {
        let mut cycle: Vec<String> = stack[pos..]
            .iter()
            .map(|(m, n)| format!("{}::{}", m.replace('/', "::"), n))
            .collect();
        cycle.push(format!("{}::{}", module_id.replace('/', "::"), def.name.0));
        diagnostics.push(Diagnostic::error(
            format!("const cycle detected: {}", cycle.join(" -> ")),
            Some(def.span),
        ));
        return None;
    }

    stack.push(key);
    let value = eval_const_expr(&def.value, module_id, module, ctx, stack, diagnostics);
    stack.pop();
    value
}

fn eval_const_expr(
    expr: &Expr,
    module_id: &str,
    module: &Module,
    ctx: &ResolverContext,
    stack: &mut Vec<(String, String)>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ConstValue> {
    match expr {
        Expr::Lit(lit, span) => {
            if let Some(value) = const_from_literal(lit) {
                Some(value)
            } else {
                diagnostics.push(Diagnostic::error(
                    "const literals must be int, bool, char, or str",
                    Some(*span),
                ));
                None
            }
        }
        Expr::InterpString { segments, span } => {
            let mut out = String::new();
            for seg in segments {
                match seg {
                    glyph_core::ast::InterpSegment::Literal(s) => out.push_str(s),
                    glyph_core::ast::InterpSegment::Expr(_) => {
                        diagnostics.push(Diagnostic::error(
                            "const interpolated strings cannot contain expressions",
                            Some(*span),
                        ));
                        return None;
                    }
                }
            }
            Some(ConstValue::Str(out))
        }
        Expr::Ident(ident, span) => resolve_const_reference(
            ident.0.as_str(),
            *span,
            module_id,
            module,
            ctx,
            stack,
            diagnostics,
        ),
        Expr::Unary { op, expr, span } => match op {
            glyph_core::ast::UnaryOp::Not => {
                let value = eval_const_expr(expr, module_id, module, ctx, stack, diagnostics)?;
                match value {
                    ConstValue::Bool(b) => Some(ConstValue::Bool(!b)),
                    _ => {
                        diagnostics.push(Diagnostic::error(
                            "const unary 'not' expects a boolean",
                            Some(*span),
                        ));
                        None
                    }
                }
            }
            glyph_core::ast::UnaryOp::Neg => {
                let value = eval_const_expr(expr, module_id, module, ctx, stack, diagnostics)?;
                match value {
                    ConstValue::Int(n) => Some(ConstValue::Int(-n)),
                    _ => {
                        diagnostics.push(Diagnostic::error(
                            "const unary '-' expects an integer",
                            Some(*span),
                        ));
                        None
                    }
                }
            }
        },
        Expr::Binary { op, lhs, rhs, span } => {
            let left = eval_const_expr(lhs, module_id, module, ctx, stack, diagnostics)?;
            let right = eval_const_expr(rhs, module_id, module, ctx, stack, diagnostics)?;
            eval_const_binary(op, left, right, *span, diagnostics)
        }
        _ => {
            diagnostics.push(Diagnostic::error(
                "const initializer must be a compile-time constant",
                Some(expr_span(expr)),
            ));
            None
        }
    }
}

fn eval_const_binary(
    op: &BinaryOp,
    left: ConstValue,
    right: ConstValue,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ConstValue> {
    match op {
        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => {
            let (l, r) = match (left, right) {
                (ConstValue::Int(l), ConstValue::Int(r)) => (l, r),
                _ => {
                    diagnostics.push(Diagnostic::error(
                        "const arithmetic expects integer operands",
                        Some(span),
                    ));
                    return None;
                }
            };
            if matches!(op, BinaryOp::Div | BinaryOp::Mod) && r == 0 {
                diagnostics.push(Diagnostic::error("const division by zero", Some(span)));
                return None;
            }
            let value = match op {
                BinaryOp::Add => l + r,
                BinaryOp::Sub => l - r,
                BinaryOp::Mul => l * r,
                BinaryOp::Div => l / r,
                BinaryOp::Mod => l % r,
                _ => l,
            };
            Some(ConstValue::Int(value))
        }
        BinaryOp::And | BinaryOp::Or => {
            let (l, r) = match (left, right) {
                (ConstValue::Bool(l), ConstValue::Bool(r)) => (l, r),
                _ => {
                    diagnostics.push(Diagnostic::error(
                        "const logical operators expect boolean operands",
                        Some(span),
                    ));
                    return None;
                }
            };
            let value = match op {
                BinaryOp::And => l && r,
                BinaryOp::Or => l || r,
                _ => l,
            };
            Some(ConstValue::Bool(value))
        }
        BinaryOp::Eq | BinaryOp::Ne => {
            let eq = match (left, right) {
                (ConstValue::Int(l), ConstValue::Int(r)) => l == r,
                (ConstValue::Bool(l), ConstValue::Bool(r)) => l == r,
                (ConstValue::Char(l), ConstValue::Char(r)) => l == r,
                (ConstValue::Str(l), ConstValue::Str(r)) => l == r,
                _ => {
                    diagnostics.push(Diagnostic::error(
                        "const equality requires matching operand types",
                        Some(span),
                    ));
                    return None;
                }
            };
            Some(ConstValue::Bool(if matches!(op, BinaryOp::Eq) {
                eq
            } else {
                !eq
            }))
        }
        BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
            let (l, r) = match (left, right) {
                (ConstValue::Int(l), ConstValue::Int(r)) => (l, r),
                (ConstValue::Char(l), ConstValue::Char(r)) => (l as i64, r as i64),
                _ => {
                    diagnostics.push(Diagnostic::error(
                        "const comparisons require integer operands",
                        Some(span),
                    ));
                    return None;
                }
            };
            let value = match op {
                BinaryOp::Lt => l < r,
                BinaryOp::Le => l <= r,
                BinaryOp::Gt => l > r,
                BinaryOp::Ge => l >= r,
                _ => false,
            };
            Some(ConstValue::Bool(value))
        }
    }
}

fn const_from_literal(lit: &Literal) -> Option<ConstValue> {
    match lit {
        Literal::Int(i) => Some(ConstValue::Int(*i)),
        Literal::Bool(b) => Some(ConstValue::Bool(*b)),
        Literal::Char(c) => Some(ConstValue::Char(*c)),
        Literal::Str(s) => Some(ConstValue::Str(s.clone())),
        Literal::Float(_) => None,
    }
}

fn resolve_const_reference(
    name: &str,
    span: Span,
    module_id: &str,
    module: &Module,
    ctx: &ResolverContext,
    stack: &mut Vec<(String, String)>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ConstValue> {
    if let Some(def) = find_const_def(module, name) {
        return eval_const_def(module_id, def, module, ctx, stack, diagnostics);
    }

    if let Some(crate::resolver::ResolvedSymbol::Const(target_module, const_name)) =
        ctx.resolve_symbol(name)
    {
        let Some(all_modules) = &ctx.all_modules else {
            diagnostics.push(Diagnostic::error(
                format!("unknown const '{}'", name),
                Some(span),
            ));
            return None;
        };
        let Some(target_module_def) = all_modules.modules.get(&target_module) else {
            diagnostics.push(Diagnostic::error(
                format!(
                    "module '{}' not found for const '{}'",
                    target_module, const_name
                ),
                Some(span),
            ));
            return None;
        };
        if let Some(def) = find_const_def(target_module_def, &const_name) {
            return eval_const_def(
                &target_module,
                def,
                target_module_def,
                ctx,
                stack,
                diagnostics,
            );
        }
        diagnostics.push(Diagnostic::error(
            format!(
                "const '{}' not found in module '{}'",
                const_name, target_module
            ),
            Some(span),
        ));
        return None;
    }

    diagnostics.push(Diagnostic::error(
        format!("unknown const '{}'", name),
        Some(span),
    ));
    None
}

pub fn resolve_const_value(
    name: &str,
    span: Span,
    module: &Module,
    ctx: &ResolverContext,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<ConstValue> {
    let module_id = ctx
        .current_module
        .clone()
        .unwrap_or_else(|| "<unknown>".to_string());
    let mut stack = Vec::new();
    resolve_const_reference(name, span, &module_id, module, ctx, &mut stack, diagnostics)
}

fn find_const_def<'a>(module: &'a Module, name: &str) -> Option<&'a ConstDef> {
    module.items.iter().find_map(|item| match item {
        glyph_core::ast::Item::Const(def) if def.name.0 == name => Some(def),
        _ => None,
    })
}

fn const_value_matches_type(value: &ConstValue, ty: &Type) -> bool {
    match (value, ty) {
        (ConstValue::Int(_), Type::I8)
        | (ConstValue::Int(_), Type::I16)
        | (ConstValue::Int(_), Type::I32)
        | (ConstValue::Int(_), Type::I64)
        | (ConstValue::Int(_), Type::U8)
        | (ConstValue::Int(_), Type::U16)
        | (ConstValue::Int(_), Type::U32)
        | (ConstValue::Int(_), Type::U64)
        | (ConstValue::Int(_), Type::Usize) => true,
        (ConstValue::Bool(_), Type::Bool) => true,
        (ConstValue::Char(_), Type::Char) => true,
        (ConstValue::Str(_), Type::Str) => true,
        (ConstValue::Str(_), Type::Ref(inner, _)) => matches!(inner.as_ref(), Type::Str),
        _ => false,
    }
}

fn const_value_type_label(value: &ConstValue) -> &'static str {
    match value {
        ConstValue::Int(_) => "int",
        ConstValue::Bool(_) => "bool",
        ConstValue::Char(_) => "char",
        ConstValue::Str(_) => "str",
    }
}
