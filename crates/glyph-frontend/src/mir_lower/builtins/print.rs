use glyph_core::ast::Expr;
use glyph_core::mir::{MirInst, MirValue, Rvalue};
use glyph_core::span::Span;
use glyph_core::types::{Mutability, Type};

use super::super::context::{LocalState, LowerCtx};
use super::super::expr::lower_value;
use super::super::value::infer_value_type;

#[derive(Clone)]
enum PrintSegment<'a> {
    Literal(String),
    Expr(&'a Expr, Span),
}

fn resolve_builtin_target(ctx: &mut LowerCtx<'_>, key: &str, span: Span) -> Option<String> {
    if let Some(sig) = ctx.fn_sigs.get(key) {
        Some(sig.target_name.clone())
    } else {
        ctx.error(format!("unknown function '{}'", key), Some(span));
        None
    }
}

fn lower_print_value<'a>(ctx: &mut LowerCtx<'a>, expr: &'a Expr) -> Option<MirValue> {
    match expr {
        Expr::Ident(name, ident_span) => {
            let Some(local) = ctx.bindings.get(name.0.as_str()).copied() else {
                ctx.error(
                    format!("unknown identifier '{}'", name.0),
                    Some(*ident_span),
                );
                return None;
            };
            if ctx.local_uses_ownership_tracking(local) {
                match ctx.local_states.get(local.0 as usize) {
                    Some(LocalState::Moved) => {
                        ctx.emit_use_of_moved_local(local, Some(*ident_span));
                        return None;
                    }
                    Some(LocalState::Uninitialized) => {
                        ctx.emit_use_of_uninitialized_local(local, Some(*ident_span));
                        return None;
                    }
                    _ => {}
                }
            }
            Some(MirValue::Local(local))
        }
        _ => lower_value(ctx, expr),
    }
}

pub(crate) fn lower_print_builtin<'a>(
    ctx: &mut LowerCtx<'a>,
    args: &'a [Expr],
    span: Span,
    add_newline: bool,
    is_err: bool,
) -> Option<Rvalue> {
    if args.len() != 1 {
        ctx.error("print/println expects exactly one argument", Some(span));
        return None;
    }

    let fd = if is_err { 2 } else { 1 };
    let arg = &args[0];

    let mut segments: Vec<PrintSegment<'a>> = Vec::new();
    let mut only_strings = false;
    match arg {
        Expr::Lit(glyph_core::ast::Literal::Str(s), _) => {
            segments.push(PrintSegment::Literal(s.clone()));
        }
        Expr::InterpString {
            segments: segs,
            span: interp_span,
        } => {
            for seg in segs {
                match seg {
                    glyph_core::ast::InterpSegment::Literal(s) => {
                        segments.push(PrintSegment::Literal(s.clone()))
                    }
                    glyph_core::ast::InterpSegment::Expr(e) => {
                        segments.push(PrintSegment::Expr(e, *interp_span))
                    }
                }
            }
        }
        _ => {
            segments.push(PrintSegment::Expr(arg, span));
            only_strings = true;
        }
    }

    if segments.is_empty() {
        segments.push(PrintSegment::Literal(String::new()));
    }

    if add_newline {
        segments.push(PrintSegment::Literal("\n".into()));
    }

    // Build writer: Stdout or Stderr
    let writer_struct = if is_err { "Stderr" } else { "Stdout" };
    let writer_local = ctx.fresh_local(None);
    ctx.locals[writer_local.0 as usize].ty = Some(Type::Named(writer_struct.to_string()));
    ctx.push_inst(MirInst::Assign {
        local: writer_local,
        value: Rvalue::StructLit {
            struct_name: writer_struct.to_string(),
            field_values: vec![("fd".into(), MirValue::Int(fd))],
        },
    });

    let mut last = None;
    for seg in segments {
        match seg {
            PrintSegment::Literal(s) => {
                let raw_write = resolve_builtin_target(ctx, "std::io::raw_write", span)?;
                let str_local = ctx.fresh_local(None);
                ctx.locals[str_local.0 as usize].ty = Some(Type::Str);
                let global = ctx.fresh_string_global();
                ctx.push_inst(MirInst::Assign {
                    local: str_local,
                    value: Rvalue::StringLit {
                        content: s.clone(),
                        global_name: global,
                    },
                });
                let out = ctx.fresh_local(None);
                ctx.locals[out.0 as usize].ty = Some(Type::I32);
                ctx.push_inst(MirInst::Assign {
                    local: out,
                    value: Rvalue::Call {
                        name: raw_write,
                        args: vec![
                            MirValue::Int(fd),
                            MirValue::Local(str_local),
                            MirValue::Int(s.len() as i64),
                        ],
                    },
                });
                last = Some(out);
            }
            PrintSegment::Expr(expr, seg_span) => {
                let val = lower_print_value(ctx, expr)?;
                let val_ty = infer_value_type(&val, ctx);

                if let Some(Type::Ref(inner, _)) = val_ty.as_ref() {
                    if matches!(inner.as_ref(), Type::Str | Type::String) {
                        ctx.error(
                            "print/println do not accept references; pass a str/String value (or clone the String)",
                            Some(seg_span),
                        );
                        return None;
                    }
                }

                if only_strings && !matches!(val_ty, Some(Type::Str | Type::String)) {
                    ctx.error(
                        "print/println require a string literal, interpolated string, or str/String value",
                        Some(seg_span),
                    );
                    return None;
                }

                // Scalar and string segments call directly into the C formatting
                // runtime (runtime/glyph_fmt.c); the externs are declared in
                // mir_lower::lower_module whenever std/io is available.
                let fmt_extern: Option<&'static str> = match val_ty.as_ref() {
                    Some(Type::I32) | Some(Type::I8) => Some("glyph_fmt_write_i32"),
                    Some(Type::U32) | Some(Type::U8) => Some("glyph_fmt_write_u32"),
                    Some(Type::I64) => Some("glyph_fmt_write_i64"),
                    Some(Type::U64) | Some(Type::Usize) => Some("glyph_fmt_write_u64"),
                    Some(Type::Bool) => Some("glyph_fmt_write_bool"),
                    Some(Type::Char) => Some("glyph_fmt_write_char"),
                    Some(Type::F32) => Some("glyph_fmt_write_f32"),
                    Some(Type::F64) => Some("glyph_fmt_write_f64"),
                    Some(Type::Str) | Some(Type::String) => Some("glyph_fmt_write_str"),
                    _ => None,
                };

                if let Some(fmt_fn) = fmt_extern {
                    let out = ctx.fresh_local(None);
                    ctx.locals[out.0 as usize].ty = Some(Type::I32);
                    ctx.push_inst(MirInst::Assign {
                        local: out,
                        value: Rvalue::Call {
                            name: fmt_fn.to_string(),
                            args: vec![MirValue::Int(fd), val.clone()],
                        },
                    });
                    last = Some(out);
                } else if let Some(Type::Named(struct_name)) = val_ty.as_ref() {
                    // Struct segments call a user-provided free function
                    // fmt_<type> taking (value, &mut Stdout).
                    let writer_ref = ctx.fresh_local(None);
                    ctx.locals[writer_ref.0 as usize].ty = Some(Type::Ref(
                        Box::new(Type::Named(writer_struct.to_string())),
                        Mutability::Mutable,
                    ));
                    ctx.push_inst(MirInst::Assign {
                        local: writer_ref,
                        value: Rvalue::Ref {
                            base: writer_local,
                            mutability: Mutability::Mutable,
                        },
                    });

                    let fmt_fn_key = format!("std::fmt::fmt_{}", struct_name);
                    let fmt_fn = resolve_builtin_target(ctx, &fmt_fn_key, seg_span)?;
                    let out = ctx.fresh_local(None);
                    ctx.locals[out.0 as usize].ty = Some(Type::Void);

                    ctx.push_inst(MirInst::Assign {
                        local: out,
                        value: Rvalue::Call {
                            name: fmt_fn,
                            args: vec![val.clone(), MirValue::Local(writer_ref)],
                        },
                    });
                    last = Some(out);
                } else {
                    ctx.error(
                        "Format not implemented for this type; supported: i8, i32, u8, u32, i64, u64, usize, f32, f64, bool, char, str/String, and types with fmt_<type> in std::fmt",
                        Some(seg_span),
                    );
                    return None;
                }
            }
        }
    }

    last.map(Rvalue::Move)
}
