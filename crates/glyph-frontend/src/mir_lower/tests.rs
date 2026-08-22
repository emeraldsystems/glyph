use super::*;
use crate::resolver::{ResolverContext, resolve_types};
use crate::{FrontendOptions, compile_source};
use glyph_core::ast::{
    BinaryOp, Block, EnumDef, EnumVariantDef, Expr, FieldDef, Function, Ident, Item, Literal,
    MatchArm, MatchPattern, Module, Param, Stmt, StructDef, TypeExpr,
};
use glyph_core::diag::Severity;
use glyph_core::mir::{LocalId, MirInst, MirValue, Rvalue};
use glyph_core::span::Span;
use glyph_core::types::{Mutability, Type};

fn path_ty(name: &str, span: Span) -> TypeExpr {
    TypeExpr::Path {
        segments: vec![name.to_string()],
        span,
    }
}

#[test]
fn lowers_reference_expression() {
    let span = Span::new(0, 5);
    let func = Function {
        name: Ident("main".into()),
        params: vec![],
        ret_type: Some(path_ty("i32", span)),

        body: Block {
            span,
            stmts: vec![
                Stmt::Let {
                    name: Ident("p".into()),
                    ty: Some(path_ty("Point", span)),
                    mutable: false,
                    value: Some(Expr::StructLit {
                        name: Ident("Point".into()),
                        fields: vec![
                            (Ident("x".into()), Expr::Lit(Literal::Int(1), span)),
                            (Ident("y".into()), Expr::Lit(Literal::Int(2), span)),
                        ],
                        span,
                    }),
                    span,
                },
                Stmt::Let {
                    name: Ident("pref".into()),
                    ty: None,
                    mutable: false,
                    value: Some(Expr::Ref {
                        expr: Box::new(Expr::Ident(Ident("p".into()), span)),
                        mutability: Mutability::Immutable,
                        span,
                    }),
                    span,
                },
                Stmt::Ret(
                    Some(Expr::FieldAccess {
                        base: Box::new(Expr::Ident(Ident("pref".into()), span)),
                        field: Ident("x".into()),
                        span,
                    }),
                    span,
                ),
            ],
        },

        span,
    };

    let point_struct = Item::Struct(StructDef {
        name: Ident("Point".into()),
        generic_params: Vec::new(),
        fields: vec![
            FieldDef {
                name: Ident("x".into()),
                ty: path_ty("i32", span),
                span,
            },
            FieldDef {
                name: Ident("y".into()),
                ty: path_ty("i32", span),
                span,
            },
        ],
        interfaces: Vec::new(),
        methods: Vec::new(),
        inline_impls: Vec::new(),
        span,
    });

    let _module = Module {
        imports: vec![],
        items: vec![point_struct.clone(), Item::Function(func.clone())],
    };

    let module = Module {
        imports: vec![],
        items: vec![point_struct, Item::Function(func)],
    };

    let (resolver, diags) = resolve_types(&module);
    assert!(diags.is_empty());

    let (_mir, lower_diags) = lower_module(&module, &resolver);
    assert!(
        lower_diags.is_empty(),
        "unexpected diagnostics: {:?}",
        lower_diags
    );
}

#[test]
fn lowers_function_to_mir_with_return() {
    let func = Function {
        name: Ident("main".into()),
        params: vec![],
        ret_type: None,
        body: Block {
            span: Span::new(0, 4),
            stmts: vec![Stmt::Ret(None, Span::new(0, 4))],
        },
        span: Span::new(0, 4),
    };
    let module = Module {
        imports: vec![],
        items: vec![Item::Function(func)],
    };
    let (mir, diags) = lower_module(&module, &ResolverContext::default());
    assert!(diags.is_empty(), "unexpected diagnostics: {:?}", diags);
    assert_eq!(mir.functions.len(), 1);
    assert!(
        mir.functions[0]
            .blocks
            .iter()
            .any(|b| matches!(b.insts.last(), Some(MirInst::Return(_))))
    );
}

#[test]
fn lowers_binary_expression_to_temp_and_return() {
    let func = Function {
        name: Ident("main".into()),
        params: vec![],
        ret_type: None,
        body: Block {
            span: Span::new(0, 10),
            stmts: vec![Stmt::Ret(
                Some(Expr::Binary {
                    op: BinaryOp::Add,
                    lhs: Box::new(Expr::Lit(glyph_core::ast::Literal::Int(1), Span::new(0, 1))),
                    rhs: Box::new(Expr::Lit(glyph_core::ast::Literal::Int(2), Span::new(2, 3))),
                    span: Span::new(0, 3),
                }),
                Span::new(0, 3),
            )],
        },
        span: Span::new(0, 10),
    };
    let module = Module {
        imports: vec![],
        items: vec![Item::Function(func)],
    };
    let (mir, diags) = lower_module(&module, &ResolverContext::default());
    assert!(diags.is_empty(), "unexpected diagnostics: {:?}", diags);
    let insts = &mir.functions[0].blocks[0].insts;
    assert!(insts.iter().any(|i| matches!(i, MirInst::Assign { .. })));
    assert!(matches!(
        insts.last(),
        Some(MirInst::Return(Some(MirValue::Local(_))))
    ));
}

#[test]
fn lowers_struct_literal_and_field_access() {
    let span = Span::new(0, 5);
    let point_struct = Item::Struct(StructDef {
        name: Ident("Point".into()),
        generic_params: Vec::new(),
        fields: vec![
            FieldDef {
                name: Ident("x".into()),
                ty: path_ty("i32", span),
                span,
            },
            FieldDef {
                name: Ident("y".into()),
                ty: path_ty("i32", span),
                span,
            },
        ],
        interfaces: Vec::new(),
        methods: Vec::new(),
        inline_impls: Vec::new(),
        span,
    });

    let func = Function {
        name: Ident("main".into()),
        params: vec![],
        ret_type: Some(path_ty("i32", span)),
        body: Block {
            span,
            stmts: vec![
                Stmt::Let {
                    name: Ident("p".into()),
                    ty: Some(path_ty("Point", span)),
                    mutable: false,
                    value: Some(Expr::StructLit {
                        name: Ident("Point".into()),
                        fields: vec![
                            (Ident("x".into()), Expr::Lit(Literal::Int(1), span)),
                            (Ident("y".into()), Expr::Lit(Literal::Int(2), span)),
                        ],
                        span,
                    }),
                    span,
                },
                Stmt::Ret(
                    Some(Expr::FieldAccess {
                        base: Box::new(Expr::Ident(Ident("p".into()), span)),
                        field: Ident("x".into()),
                        span,
                    }),
                    span,
                ),
            ],
        },
        span,
    };

    let module = Module {
        imports: vec![],
        items: vec![point_struct, Item::Function(func)],
    };

    let (resolver, diags) = resolve_types(&module);
    assert!(diags.is_empty());

    let (mir, lower_diags) = lower_module(&module, &resolver);
    assert!(
        lower_diags.is_empty(),
        "unexpected diagnostics: {:?}",
        lower_diags
    );

    assert_eq!(mir.struct_types.len(), 1);
    let insts = &mir.functions[0].blocks[0].insts;
    assert!(insts.iter().any(|inst| matches!(
        inst,
        MirInst::Assign {
            value: Rvalue::StructLit { .. },
            ..
        }
    )));
    assert!(insts.iter().any(|inst| matches!(
        inst,
        MirInst::Assign {
            value: Rvalue::FieldAccess { field_name, .. },
            ..
        } if field_name == "x"
    )));
}

#[test]
fn lowers_match_arm_block_expression_values() {
    let span = Span::new(0, 5);

    let enum_def = Item::Enum(EnumDef {
        name: Ident("Maybe".into()),
        generic_params: Vec::new(),
        variants: vec![
            EnumVariantDef {
                name: Ident("Some".into()),
                payload: Some(path_ty("i32", span)),
                span,
            },
            EnumVariantDef {
                name: Ident("None".into()),
                payload: None,
                span,
            },
        ],
        span,
    });

    let match_expr = Expr::Match {
        scrutinee: Box::new(Expr::Ident(Ident("v".into()), span)),
        arms: vec![
            MatchArm {
                pattern: MatchPattern::Variant {
                    name: Ident("Some".into()),
                    binding: Some(Ident("n".into())),
                },
                expr: Expr::Block(Block {
                    span,
                    stmts: vec![Stmt::Expr(Expr::Ident(Ident("n".into()), span), span)],
                }),
                span,
            },
            MatchArm {
                pattern: MatchPattern::Variant {
                    name: Ident("None".into()),
                    binding: None,
                },
                expr: Expr::Block(Block {
                    span,
                    stmts: vec![Stmt::Expr(Expr::Lit(Literal::Int(0), span), span)],
                }),
                span,
            },
        ],
        span,
    };

    let func = Function {
        name: Ident("main".into()),
        params: vec![],
        ret_type: Some(path_ty("i32", span)),
        body: Block {
            span,
            stmts: vec![
                Stmt::Let {
                    name: Ident("v".into()),
                    ty: None,
                    mutable: false,
                    value: Some(Expr::Call {
                        callee: Box::new(Expr::Ident(Ident("Some".into()), span)),
                        args: vec![Expr::Lit(Literal::Int(1), span)],
                        span,
                    }),
                    span,
                },
                Stmt::Ret(Some(match_expr), span),
            ],
        },
        span,
    };

    let module = Module {
        imports: vec![],
        items: vec![enum_def, Item::Function(func)],
    };

    let (resolver, diags) = resolve_types(&module);
    assert!(diags.is_empty());

    let (mir, lower_diags) = lower_module(&module, &resolver);
    assert!(
        lower_diags.is_empty(),
        "unexpected diagnostics: {:?}",
        lower_diags
    );

    let main = mir
        .functions
        .iter()
        .find(|f| f.name == "main")
        .expect("main exists");

    let ret_local = main
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .find_map(|inst| match inst {
            MirInst::Return(Some(MirValue::Local(id))) => Some(*id),
            _ => None,
        })
        .expect("return local");

    let has_assign = main
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .any(|inst| matches!(inst, MirInst::Assign { local, .. } if *local == ret_local));

    assert!(has_assign, "expected match arms to assign to result local");
}

#[test]
fn return_temporary_string_is_not_dropped() {
    let span = Span::new(0, 5);
    let func = Function {
        name: Ident("main".into()),
        params: vec![],
        ret_type: Some(path_ty("String", span)),
        body: Block {
            span,
            stmts: vec![Stmt::Ret(
                Some(Expr::Call {
                    callee: Box::new(Expr::Ident(Ident("String::from_str".into()), span)),
                    args: vec![Expr::Lit(Literal::Str("hi".into()), span)],
                    span,
                }),
                span,
            )],
        },
        span,
    };

    let module = Module {
        imports: vec![],
        items: vec![Item::Function(func)],
    };

    let (resolver, diags) = resolve_types(&module);
    assert!(diags.is_empty());

    let (mir, lower_diags) = lower_module(&module, &resolver);
    assert!(
        lower_diags.is_empty(),
        "unexpected diagnostics: {:?}",
        lower_diags
    );

    let main = mir
        .functions
        .iter()
        .find(|f| f.name == "main")
        .expect("main exists");

    let ret_local = main
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .find_map(|inst| match inst {
            MirInst::Return(Some(MirValue::Local(id))) => Some(*id),
            _ => None,
        })
        .expect("return local");

    let has_drop = main
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .any(|inst| matches!(inst, MirInst::Drop(local) if *local == ret_local));

    assert!(
        !has_drop,
        "return local should not be dropped before return"
    );
}

#[test]
fn match_merge_ignores_terminated_arms_for_drops() {
    let span = Span::new(0, 5);

    let enum_def = Item::Enum(EnumDef {
        name: Ident("Flag".into()),
        generic_params: Vec::new(),
        variants: vec![
            EnumVariantDef {
                name: Ident("Yes".into()),
                payload: None,
                span,
            },
            EnumVariantDef {
                name: Ident("No".into()),
                payload: None,
                span,
            },
        ],
        span,
    });

    let consume_fn = Item::Function(Function {
        name: Ident("consume".into()),
        params: vec![Param {
            name: Ident("s".into()),
            ty: Some(path_ty("String", span)),
            span,
        }],
        ret_type: Some(path_ty("i32", span)),
        body: Block {
            span,
            stmts: vec![Stmt::Ret(Some(Expr::Lit(Literal::Int(0), span)), span)],
        },
        span,
    });

    let match_expr = Expr::Match {
        scrutinee: Box::new(Expr::Ident(Ident("flag".into()), span)),
        arms: vec![
            MatchArm {
                pattern: MatchPattern::Variant {
                    name: Ident("Yes".into()),
                    binding: None,
                },
                expr: Expr::Block(Block {
                    span,
                    stmts: vec![
                        Stmt::Expr(
                            Expr::Call {
                                callee: Box::new(Expr::Ident(Ident("consume".into()), span)),
                                args: vec![Expr::Ident(Ident("s".into()), span)],
                                span,
                            },
                            span,
                        ),
                        Stmt::Ret(Some(Expr::Lit(Literal::Int(1), span)), span),
                    ],
                }),
                span,
            },
            MatchArm {
                pattern: MatchPattern::Variant {
                    name: Ident("No".into()),
                    binding: None,
                },
                expr: Expr::Lit(Literal::Int(0), span),
                span,
            },
        ],
        span,
    };

    let main_fn = Item::Function(Function {
        name: Ident("main".into()),
        params: vec![],
        ret_type: Some(path_ty("i32", span)),
        body: Block {
            span,
            stmts: vec![
                Stmt::Let {
                    name: Ident("s".into()),
                    ty: Some(path_ty("String", span)),
                    mutable: false,
                    value: Some(Expr::Call {
                        callee: Box::new(Expr::Ident(Ident("String::from_str".into()), span)),
                        args: vec![Expr::Lit(Literal::Str("hi".into()), span)],
                        span,
                    }),
                    span,
                },
                Stmt::Let {
                    name: Ident("flag".into()),
                    ty: None,
                    mutable: false,
                    value: Some(Expr::Call {
                        callee: Box::new(Expr::Ident(Ident("Yes".into()), span)),
                        args: vec![],
                        span,
                    }),
                    span,
                },
                Stmt::Let {
                    name: Ident("v".into()),
                    ty: None,
                    mutable: false,
                    value: Some(match_expr),
                    span,
                },
                Stmt::Ret(Some(Expr::Ident(Ident("v".into()), span)), span),
            ],
        },
        span,
    });

    let module = Module {
        imports: vec![],
        items: vec![enum_def, consume_fn, main_fn],
    };

    let (resolver, diags) = resolve_types(&module);
    assert!(diags.is_empty());

    let (mir, lower_diags) = lower_module(&module, &resolver);
    assert!(
        lower_diags.is_empty(),
        "unexpected diagnostics: {:?}",
        lower_diags
    );

    let main = mir
        .functions
        .iter()
        .find(|f| f.name == "main")
        .expect("main exists");

    let s_local = main
        .locals
        .iter()
        .enumerate()
        .find_map(|(idx, local)| {
            if local.name.as_deref() == Some("s") {
                Some(glyph_core::mir::LocalId(idx as u32))
            } else {
                None
            }
        })
        .expect("s local");

    let has_drop = main
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .any(|inst| matches!(inst, MirInst::Drop(local) if *local == s_local));

    assert!(has_drop, "expected drop for s on reachable join path");
}

#[test]
fn if_return_does_not_poison_move_state() {
    let span = Span::new(0, 5);
    let if_expr = Expr::If {
        cond: Box::new(Expr::Lit(Literal::Bool(true), span)),
        then_block: Block {
            span,
            stmts: vec![Stmt::Ret(Some(Expr::Lit(Literal::Int(1), span)), span)],
        },
        else_block: None,
        span,
    };

    let func = Function {
        name: Ident("main".into()),
        params: vec![],
        ret_type: Some(path_ty("i32", span)),
        body: Block {
            span,
            stmts: vec![
                Stmt::Let {
                    name: Ident("s".into()),
                    ty: Some(path_ty("String", span)),
                    mutable: false,
                    value: Some(Expr::Call {
                        callee: Box::new(Expr::Ident(Ident("String::from_str".into()), span)),
                        args: vec![Expr::Lit(Literal::Str("hi".into()), span)],
                        span,
                    }),
                    span,
                },
                Stmt::Expr(if_expr, span),
                Stmt::Let {
                    name: Ident("t".into()),
                    ty: None,
                    mutable: false,
                    value: Some(Expr::Ident(Ident("s".into()), span)),
                    span,
                },
                Stmt::Ret(Some(Expr::Lit(Literal::Int(0), span)), span),
            ],
        },
        span,
    };

    let module = Module {
        imports: vec![],
        items: vec![Item::Function(func)],
    };

    let (resolver, diags) = resolve_types(&module);
    assert!(diags.is_empty());

    let (_mir, lower_diags) = lower_module(&module, &resolver);
    assert!(
        lower_diags.is_empty(),
        "unexpected diagnostics: {:?}",
        lower_diags
    );
}

#[test]
fn struct_literal_missing_field_reports_error() {
    let span = Span::new(0, 5);
    let point_struct = Item::Struct(StructDef {
        name: Ident("Point".into()),
        generic_params: Vec::new(),
        fields: vec![
            FieldDef {
                name: Ident("x".into()),
                ty: path_ty("i32", span),
                span,
            },
            FieldDef {
                name: Ident("y".into()),
                ty: path_ty("i32", span),
                span,
            },
        ],
        interfaces: Vec::new(),
        methods: Vec::new(),
        inline_impls: Vec::new(),
        span,
    });

    let func = Function {
        name: Ident("main".into()),
        params: vec![],
        ret_type: Some(path_ty("i32", span)),
        body: Block {
            span,
            stmts: vec![Stmt::Ret(
                Some(Expr::StructLit {
                    name: Ident("Point".into()),
                    fields: vec![(Ident("x".into()), Expr::Lit(Literal::Int(1), span))],
                    span,
                }),
                span,
            )],
        },
        span,
    };

    let module = Module {
        imports: vec![],
        items: vec![point_struct, Item::Function(func)],
    };

    let (resolver, diags) = resolve_types(&module);
    assert!(diags.is_empty());

    let (_mir, lower_diags) = lower_module(&module, &resolver);
    assert!(
        lower_diags
            .iter()
            .any(|d| d.message.contains("missing field")),
        "expected missing-field diagnostic, got: {:?}",
        lower_diags
    );
}

#[test]
fn lowers_assignment_statements_to_mir() {
    let span = Span::new(0, 5);
    let func = Function {
        name: Ident("main".into()),
        params: vec![],
        ret_type: None,
        body: Block {
            span,
            stmts: vec![
                Stmt::Let {
                    name: Ident("x".into()),
                    ty: Some(path_ty("i32", span)),
                    mutable: true,
                    value: Some(Expr::Lit(Literal::Int(1), span)),
                    span,
                },
                Stmt::Assign {
                    target: Expr::Ident(Ident("x".into()), span),
                    value: Expr::Lit(Literal::Int(2), span),
                    span,
                },
                Stmt::Ret(Some(Expr::Ident(Ident("x".into()), span)), span),
            ],
        },
        span,
    };

    let module = Module {
        imports: vec![],
        items: vec![Item::Function(func)],
    };

    let (mir, diags) = lower_module(&module, &ResolverContext::default());
    assert!(diags.is_empty(), "unexpected diagnostics: {:?}", diags);

    let assigns: Vec<_> = mir.functions[0].blocks[0]
        .insts
        .iter()
        .filter_map(|inst| match inst {
            MirInst::Assign { local, value } => Some((*local, value.clone())),
            _ => None,
        })
        .collect();

    assert!(
        assigns
            .iter()
            .any(|(local, value)| { local.0 == 0 && matches!(value, Rvalue::ConstInt(2)) })
    );
}

#[test]
fn lowers_struct_field_assignment() {
    let src = r#"
struct Run {
  max_iterations: i32
}

struct Session {
  run: Run
}

fn main() {
  let mut session = Session { run: Run { max_iterations: 1 } }
  session.run.max_iterations = 2
}
"#;

    let out = compile_source(
        src,
        FrontendOptions {
            emit_mir: true,
            include_std: false,
        },
    );

    assert!(
        out.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        out.diagnostics
    );

    let main_fn = out
        .mir
        .functions
        .iter()
        .find(|f| f.name == "main")
        .expect("main function present");

    let has_assign = main_fn.blocks.iter().flat_map(|b| &b.insts).any(|inst| {
        matches!(
            inst,
            MirInst::AssignField { field_name, .. } if field_name == "max_iterations"
        )
    });

    assert!(has_assign, "expected field assignment in MIR");
}

#[test]
fn lowers_interface_method_call_via_impl() {
    let src = r#"
interface Drawable {
  fn draw(self: &Point) -> i32;
}

struct Point {
  x: i32
  impl Drawable {
    fn draw(self: &Point) -> i32 { ret self.x }
  }
}

fn main() -> i32 {
  let p = Point { x: 5 }
  ret p.draw()
}
"#;

    let out = compile_source(
        src,
        FrontendOptions {
            emit_mir: true,
            include_std: false,
        },
    );
    assert!(
        out.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        out.diagnostics
    );

    let main_fn = out
        .mir
        .functions
        .iter()
        .find(|f| f.name == "main")
        .expect("main function present");

    let (call_name, args) = main_fn
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .find_map(|inst| match inst {
            MirInst::Assign {
                value: Rvalue::Call { name, args },
                ..
            } => Some((name.clone(), args.clone())),
            _ => None,
        })
        .expect("call lowered");

    assert_eq!(call_name, "Point::Drawable::draw");
    assert_eq!(args.len(), 1);

    if let MirValue::Local(local) = &args[0] {
        let ty = main_fn.locals[local.0 as usize]
            .ty
            .as_ref()
            .expect("receiver type");
        assert_eq!(
            ty,
            &Type::Ref(Box::new(Type::Named("Point".into())), Mutability::Immutable)
        );
    } else {
        panic!("expected receiver argument to be a local");
    }
}

#[test]
fn lowers_public_atomic_api_to_seqcst_mir() {
    use glyph_core::atomic::{AtomicOrdering, AtomicRmwOp, AtomicScalar};

    let src = r#"
fn main() -> usize {
  let counter: AtomicUsize = AtomicUsize::new(1)
  let before = counter.fetch_add(2)
  counter.store(9)
  let observed = counter.compare_exchange(9, 12)
  let current = counter.load()
  ret before + observed + current
}
"#;
    let out = compile_source(
        src,
        FrontendOptions {
            emit_mir: true,
            include_std: false,
        },
    );
    assert!(
        out.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        out.diagnostics
    );

    let main = out.mir.functions.iter().find(|f| f.name == "main").unwrap();
    let rvalues: Vec<&Rvalue> = main
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter_map(|inst| match inst {
            MirInst::Assign { value, .. } => Some(value),
            _ => None,
        })
        .collect();

    assert!(rvalues.iter().any(|value| matches!(
        value,
        Rvalue::AtomicNew {
            scalar: AtomicScalar::Usize,
            ..
        }
    )));
    assert!(rvalues.iter().any(|value| matches!(
        value,
        Rvalue::AtomicRmw {
            scalar: AtomicScalar::Usize,
            op: AtomicRmwOp::Add,
            ordering: AtomicOrdering::SeqCst,
            ..
        }
    )));
    assert!(rvalues.iter().any(|value| matches!(
        value,
        Rvalue::AtomicStore {
            ordering: AtomicOrdering::SeqCst,
            ..
        }
    )));
    assert!(rvalues.iter().any(|value| matches!(
        value,
        Rvalue::AtomicCompareExchange {
            success: AtomicOrdering::SeqCst,
            failure: AtomicOrdering::SeqCst,
            ..
        }
    )));
    assert!(rvalues.iter().any(|value| matches!(
        value,
        Rvalue::AtomicLoad {
            ordering: AtomicOrdering::SeqCst,
            ..
        }
    )));
}

#[test]
fn atomic_bool_rejects_integer_only_fetch_operations() {
    let out = compile_source(
        r#"
fn main() {
  let flag = AtomicBool::new(false)
  flag.fetch_add(true)
}
"#,
        FrontendOptions {
            emit_mir: true,
            include_std: false,
        },
    );
    assert!(out.diagnostics.iter().any(|diag| {
        diag.message
            .contains("AtomicBool does not support .fetch_add()")
    }));
}

#[test]
fn lowers_extern_function_call() {
    let module = Module {
        imports: vec![],
        items: vec![
            Item::ExternFunction(glyph_core::ast::ExternFunctionDecl {
                abi: Some("C".into()),
                name: Ident("puts".into()),
                params: vec![Param {
                    name: Ident("msg".into()),
                    ty: Some(path_ty("i32", Span::new(0, 0))),
                    span: Span::new(0, 0),
                }],
                ret_type: None,
                link_name: None,
                span: Span::new(0, 0),
            }),
            Item::Function(Function {
                name: Ident("main".into()),
                params: vec![],
                ret_type: Some(path_ty("i32", Span::new(0, 0))),
                body: Block {
                    span: Span::new(0, 0),
                    stmts: vec![
                        Stmt::Expr(
                            Expr::Call {
                                callee: Box::new(Expr::Ident(
                                    Ident("puts".into()),
                                    Span::new(0, 0),
                                )),
                                args: vec![Expr::Lit(Literal::Int(1), Span::new(0, 0))],
                                span: Span::new(0, 0),
                            },
                            Span::new(0, 0),
                        ),
                        Stmt::Ret(
                            Some(Expr::Lit(Literal::Int(0), Span::new(0, 0))),
                            Span::new(0, 0),
                        ),
                    ],
                },
                span: Span::new(0, 0),
            }),
        ],
    };

    let resolver = ResolverContext::default();
    let (mir, diags) = lower_module(&module, &resolver);
    assert!(diags.is_empty(), "unexpected diagnostics: {:?}", diags);
    assert_eq!(mir.extern_functions.len(), 1);
    assert_eq!(mir.extern_functions[0].name, "puts");

    let main = mir
        .functions
        .iter()
        .find(|f| f.name == "main")
        .expect("main exists");
    let mut found_call = false;
    for block in &main.blocks {
        for inst in &block.insts {
            if let MirInst::Assign {
                value: Rvalue::Call { name, .. },
                ..
            } = inst
            {
                assert_eq!(name, "puts");
                found_call = true;
            }
        }
    }
    assert!(found_call, "call to extern function not lowered");
}

#[test]
fn extern_function_wrong_arity_reports_error() {
    let module = Module {
        imports: vec![],
        items: vec![
            Item::ExternFunction(glyph_core::ast::ExternFunctionDecl {
                abi: Some("C".into()),
                name: Ident("puts".into()),
                params: vec![Param {
                    name: Ident("msg".into()),
                    ty: Some(path_ty("i32", Span::new(0, 0))),
                    span: Span::new(0, 0),
                }],
                ret_type: None,
                link_name: None,
                span: Span::new(0, 0),
            }),
            Item::Function(Function {
                name: Ident("main".into()),
                params: vec![],
                ret_type: Some(path_ty("i32", Span::new(0, 0))),
                body: Block {
                    span: Span::new(0, 0),
                    stmts: vec![
                        Stmt::Expr(
                            Expr::Call {
                                callee: Box::new(Expr::Ident(
                                    Ident("puts".into()),
                                    Span::new(0, 0),
                                )),
                                args: vec![],
                                span: Span::new(0, 0),
                            },
                            Span::new(0, 0),
                        ),
                        Stmt::Ret(
                            Some(Expr::Lit(Literal::Int(0), Span::new(0, 0))),
                            Span::new(0, 0),
                        ),
                    ],
                },
                span: Span::new(0, 0),
            }),
        ],
    };

    let resolver = ResolverContext::default();
    let (_mir, diags) = lower_module(&module, &resolver);
    assert!(
        diags
            .iter()
            .any(|d| d.message.contains("expects 1 arguments but got 0")),
        "expected arity diagnostic, got {:?}",
        diags
    );
}

#[test]
fn lowers_usize_add_literal_to_usize_temp() {
    let span = Span::new(0, 0);
    let func = Function {
        name: Ident("main".into()),
        params: vec![],
        ret_type: Some(path_ty("usize", span)),
        body: Block {
            span,
            stmts: vec![
                Stmt::Let {
                    name: Ident("pos".into()),
                    ty: Some(path_ty("usize", span)),
                    mutable: false,
                    value: Some(Expr::Lit(Literal::Int(0), span)),
                    span,
                },
                Stmt::Ret(
                    Some(Expr::Binary {
                        op: BinaryOp::Add,
                        lhs: Box::new(Expr::Ident(Ident("pos".into()), span)),
                        rhs: Box::new(Expr::Lit(Literal::Int(1), span)),
                        span,
                    }),
                    span,
                ),
            ],
        },
        span,
    };

    let module = Module {
        imports: vec![],
        items: vec![Item::Function(func)],
    };

    let (mir, diags) = lower_module(&module, &ResolverContext::default());
    assert!(diags.is_empty(), "unexpected diagnostics: {:?}", diags);

    let main = mir
        .functions
        .iter()
        .find(|f| f.name == "main")
        .expect("main exists");

    let mut found = false;
    for block in &main.blocks {
        for inst in &block.insts {
            if let MirInst::Assign {
                local,
                value: Rvalue::Binary {
                    op: BinaryOp::Add, ..
                },
            } = inst
            {
                let ty = main.locals[local.0 as usize].ty.as_ref();
                assert_eq!(ty, Some(&Type::Usize));
                found = true;
            }
        }
    }
    assert!(found, "expected an Add binary temp assignment");
}

fn compile_ok(src: &str) {
    let out = compile_source(
        src,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );
    assert!(
        out.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        out.diagnostics
    );
}

#[test]
fn closure_lowering_reports_the_staged_feature_boundary() {
    let out = compile_source(
        "fn main() { let callback = (x: i32) -> x + 1 }",
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );

    assert!(out.diagnostics.iter().any(|diag| {
        diag.message
            .contains("closure conversion is not implemented yet")
    }));
}

#[test]
fn borrow_string_field_is_ok() {
    let src = r#"
        struct Session {
          phase: String
        }

        fn takes_str(s: str) -> i32 { ret s.len() }

        fn main() -> i32 {
          let session = Session { phase: String::from_str("hi") }
          let phase: str = session.phase
          let _n = takes_str(session.phase)
          let _m = phase.len()
          ret 0
        }
    "#;

    compile_ok(src);
}

#[test]
fn move_string_field_errors() {
    let src = r#"
        struct Session {
          phase: String
        }

        fn takes_string(_s: String) -> i32 { ret 0 }

        fn main() -> i32 {
          let session = Session { phase: String::from_str("hi") }
          let _phase = session.phase
          let _ = takes_string(session.phase)
          ret 0
        }
    "#;

    let out = compile_source(
        src,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );

    assert!(
        out.diagnostics
            .iter()
            .any(|d| d.message.contains("cannot move field")),
        "expected field-move diagnostic, got: {:?}",
        out.diagnostics
    );
}

#[test]
fn if_return_then_allows_use_after() {
    let src = r#"
        fn main() -> i32 {
          let s = String::from_str("hi")
          if false { ret 1 }
          let t = s
          let t_ref: str = t
          if t_ref.len() != 2 { ret 2 }
          ret 0
        }
    "#;

    compile_ok(src);
}

#[test]
fn if_return_else_allows_use_after() {
    let src = r#"
        fn main() -> i32 {
          let s = String::from_str("hi")
          if true { } else { ret 1 }
          let t = s
          let t_ref: str = t
          if t_ref.len() != 2 { ret 2 }
          ret 0
        }
    "#;

    compile_ok(src);
}

#[test]
fn nested_if_return_allows_use_after() {
    let src = r#"
        fn main() -> i32 {
          let s = String::from_str("hi")
          if true {
            if false { ret 1 }
          }
          let t = s
          let t_ref: str = t
          if t_ref.len() != 2 { ret 2 }
          ret 0
        }
    "#;

    compile_ok(src);
}

#[test]
fn if_expression_allows_diverging_then() {
    let src = r#"
        fn main() -> i32 {
          let v = if false { ret 1 } else { 2 }
          ret v
        }
    "#;

    compile_ok(src);
}

#[test]
fn if_expression_allows_diverging_else() {
    let src = r#"
        fn main() -> i32 {
          let v = if true { 2 } else { ret 1 }
          ret v
        }
    "#;

    compile_ok(src);
}

#[test]
fn match_expression_allows_diverging_arm() {
    let src = r#"
        enum Flag { A, B }

        fn main() -> i32 {
          let s = String::from_str("hi")
          let flag = B()
          let v = match flag {
            A => { ret 1 }
            B => 2
          }
          let t = s
          let t_ref: str = t
          if t_ref.len() != 2 { ret 3 }
          ret v
        }
    "#;

    compile_ok(src);
}

#[test]
fn if_branch_move_without_reinit_reports_moved() {
    let src = r#"
        fn main() -> i32 {
          let s = String::from_str("hi")
          if true {
            let moved = s
            let moved_ref: str = moved
            if moved_ref.len() != 2 { ret 1 }
          }
          let again = s
          let again_ref: str = again
          ret again_ref.len()
        }
    "#;

    let out = compile_with_std(src);
    assert!(
        out.diagnostics
            .iter()
            .any(|d| d.message.contains("use of moved value `s`")),
        "expected moved-value diagnostic, got: {:?}",
        out.diagnostics
    );
}

#[test]
fn if_branch_move_and_reinitialize_on_all_paths_allows_join_use() {
    let src = r#"
        fn main() -> i32 {
          let mut s = String::from_str("seed")
          if true {
            let consumed = s
            let _consumed_ref: str = consumed
            s = String::from_str("left")
          } else {
            s = String::from_str("right")
          }
          let final_s = s
          let final_ref: str = final_s
          if final_ref.len() < 4 { ret 1 }
          ret 0
        }
    "#;

    compile_ok(src);
}

#[test]
fn match_arm_move_without_reinit_reports_moved() {
    let src = r#"
        enum Flag { A, B }

        fn main() -> i32 {
          let s = String::from_str("hi")
          let flag = A()
          let _code = match flag {
            A => {
              let taken = s
              let taken_ref: str = taken
              if taken_ref.len() != 2 { ret 1 }
              0
            },
            B => 0,
          }
          let again = s
          let again_ref: str = again
          ret again_ref.len()
        }
    "#;

    let out = compile_with_std(src);
    assert!(
        out.diagnostics
            .iter()
            .any(|d| d.message.contains("use of moved value `s`")),
        "expected moved-value diagnostic, got: {:?}",
        out.diagnostics
    );
}

#[test]
fn match_arm_move_and_reinitialize_allows_join_use() {
    let src = r#"
        enum Flag { A, B }

        fn main() -> i32 {
          let mut s = String::from_str("seed")
          let flag = A()
          let _code = match flag {
            A => {
              let moved = s
              let _moved_ref: str = moved
              s = String::from_str("left")
              0
            },
            B => {
              s = String::from_str("right")
              0
            },
          }
          let final_s = s
          let final_ref: str = final_s
          if final_ref.len() < 4 { ret 1 }
          ret 0
        }
    "#;

    compile_ok(src);
}

#[test]
fn control_flow_ownership_matrix_cases() {
    enum Expected {
        Ok,
        DiagContains(&'static str),
    }

    struct Case {
        name: &'static str,
        src: &'static str,
        expected: Expected,
    }

    let cases = [
        Case {
            name: "if_diverging_then_join_use_ok",
            src: r#"
                fn main() -> i32 {
                  let s = String::from_str("hi")
                  if false { ret 1 }
                  let t = s
                  let t_ref: str = t
                  if t_ref.len() != 2 { ret 2 }
                  ret 0
                }
            "#,
            expected: Expected::Ok,
        },
        Case {
            name: "if_move_no_else_diag_moved",
            src: r#"
                fn main() -> i32 {
                  let s = String::from_str("hi")
                  if true {
                    let moved = s
                    let _moved_ref: str = moved
                  }
                  let t = s
                  let t_ref: str = t
                  ret t_ref.len()
                }
            "#,
            expected: Expected::DiagContains("use of moved value `s`"),
        },
        Case {
            name: "if_move_reinit_both_paths_ok",
            src: r#"
                fn main() -> i32 {
                  let mut s = String::from_str("seed")
                  if true {
                    let moved = s
                    let _moved_ref: str = moved
                    s = String::from_str("left")
                  } else {
                    s = String::from_str("right")
                  }
                  let t = s
                  let t_ref: str = t
                  if t_ref.len() < 4 { ret 1 }
                  ret 0
                }
            "#,
            expected: Expected::Ok,
        },
        Case {
            name: "if_conditional_init_no_else_diag_uninitialized",
            src: r#"
                fn main() -> i32 {
                  let mut s: String
                  if true {
                    s = String::from_str("init")
                  }
                  let t = s
                  let t_ref: str = t
                  ret t_ref.len()
                }
            "#,
            expected: Expected::DiagContains("use of uninitialized value `s`"),
        },
        Case {
            name: "match_diverging_arm_join_use_ok",
            src: r#"
                enum Flag { A, B }

                fn main() -> i32 {
                  let s = String::from_str("hi")
                  let flag = B()
                  let v = match flag {
                    A => { ret 1 }
                    B => 2
                  }
                  let t = s
                  let t_ref: str = t
                  if t_ref.len() != 2 { ret 3 }
                  ret v
                }
            "#,
            expected: Expected::Ok,
        },
        Case {
            name: "match_move_one_arm_diag_moved",
            src: r#"
                enum Flag { A, B }

                fn main() -> i32 {
                  let s = String::from_str("hi")
                  let flag = A()
                  let _v = match flag {
                    A => {
                      let moved = s
                      let _moved_ref: str = moved
                      0
                    }
                    B => 1
                  }
                  let t = s
                  let t_ref: str = t
                  ret t_ref.len()
                }
            "#,
            expected: Expected::DiagContains("use of moved value `s`"),
        },
        Case {
            name: "match_reinit_both_arms_ok",
            src: r#"
                enum Flag { A, B }

                fn main() -> i32 {
                  let mut s = String::from_str("seed")
                  let flag = A()
                  let _v = match flag {
                    A => {
                      let moved = s
                      let _moved_ref: str = moved
                      s = String::from_str("left")
                      0
                    }
                    B => {
                      s = String::from_str("right")
                      1
                    }
                  }
                  let t = s
                  let t_ref: str = t
                  if t_ref.len() < 4 { ret 1 }
                  ret 0
                }
            "#,
            expected: Expected::Ok,
        },
        Case {
            name: "nested_if_inside_match_reinit_ok",
            src: r#"
                enum Side { Left, Right }

                fn main() -> i32 {
                  let mut s = String::from_str("seed")
                  let side = Left()
                  let _v = match side {
                    Left => {
                      if true {
                        s = String::from_str("left")
                      } else {
                        s = String::from_str("left_alt")
                      }
                      0
                    }
                    Right => {
                      s = String::from_str("right")
                      1
                    }
                  }
                  let t = s
                  let t_ref: str = t
                  if t_ref.len() < 4 { ret 1 }
                  ret 0
                }
            "#,
            expected: Expected::Ok,
        },
    ];

    for case in cases {
        let out = compile_with_std(case.src);
        match case.expected {
            Expected::Ok => assert!(
                out.diagnostics.is_empty(),
                "case {} expected success, got diagnostics: {:?}",
                case.name,
                out.diagnostics
            ),
            Expected::DiagContains(msg) => assert!(
                out.diagnostics.iter().any(|d| d.message.contains(msg)),
                "case {} expected diagnostic containing {:?}, got {:?}",
                case.name,
                msg,
                out.diagnostics
            ),
        }
    }
}

fn compile_with_std(src: &str) -> crate::FrontendOutput {
    compile_source(
        src,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    )
}

fn main_fn_from_output(out: &crate::FrontendOutput) -> &glyph_core::mir::MirFunction {
    out.mir
        .functions
        .iter()
        .find(|f| f.name == "main")
        .expect("main function should exist")
}

fn local_id_by_name(func: &glyph_core::mir::MirFunction, name: &str) -> LocalId {
    func.locals
        .iter()
        .enumerate()
        .find_map(|(idx, local)| {
            (local.name.as_deref() == Some(name)).then_some(LocalId(idx as u32))
        })
        .unwrap_or_else(|| panic!("missing local named '{}'", name))
}

fn has_drop_followed_by_goto(func: &glyph_core::mir::MirFunction, local: LocalId) -> bool {
    func.blocks.iter().any(|block| {
        block.insts.windows(2).any(|pair| {
            matches!(pair[0], MirInst::Drop(id) if id == local)
                && matches!(pair[1], MirInst::Goto(_))
        })
    })
}

fn count_drop_in_goto_terminated_blocks(
    func: &glyph_core::mir::MirFunction,
    local: LocalId,
) -> usize {
    func.blocks
        .iter()
        .filter(|block| {
            matches!(block.insts.last(), Some(MirInst::Goto(_)))
                && block
                    .insts
                    .iter()
                    .any(|inst| matches!(inst, MirInst::Drop(id) if *id == local))
        })
        .count()
}

fn max_drop_count_per_block(func: &glyph_core::mir::MirFunction, local: LocalId) -> usize {
    func.blocks
        .iter()
        .map(|block| {
            block
                .insts
                .iter()
                .filter(|inst| matches!(inst, MirInst::Drop(id) if *id == local))
                .count()
        })
        .max()
        .unwrap_or(0)
}

fn function_by_name<'a>(
    out: &'a crate::FrontendOutput,
    name: &str,
) -> &'a glyph_core::mir::MirFunction {
    out.mir
        .functions
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("missing MIR function '{}'", name))
}

fn has_tag_compare_against(func: &glyph_core::mir::MirFunction, variant_index: i64) -> bool {
    func.blocks.iter().any(|block| {
        block.insts.iter().any(|inst| {
            matches!(
                inst,
                MirInst::Assign {
                    value:
                        Rvalue::Binary {
                            op: BinaryOp::Eq,
                            rhs: MirValue::Int(idx),
                            ..
                        },
                    ..
                } if *idx == variant_index
            )
        })
    })
}

fn has_enum_construct(
    func: &glyph_core::mir::MirFunction,
    enum_name: &str,
    variant_index: u32,
    payload_present: bool,
) -> bool {
    let monomorphized_prefix = format!("{}$", enum_name);
    func.blocks.iter().any(|block| {
        block.insts.iter().any(|inst| {
            matches!(
                inst,
                MirInst::Assign {
                    value:
                        Rvalue::EnumConstruct {
                            enum_name: actual_enum,
                            variant_index: actual_index,
                            payload,
                        },
                    ..
                } if (actual_enum == enum_name || actual_enum.starts_with(&monomorphized_prefix))
                    && *actual_index == variant_index
                    && payload.is_some() == payload_present
            )
        })
    })
}

#[test]
fn try_option_uses_std_variant_indices() {
    let src = r#"
        import std

        fn maybe_some() -> Option<i32> {
          ret Some(7)
        }

        fn unwrap_some() -> Option<i32> {
          let value = maybe_some()?
          ret Some(value)
        }
    "#;

    let out = compile_with_std(src);
    assert!(
        out.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        out.diagnostics
    );

    let func = function_by_name(&out, "unwrap_some");
    assert!(
        has_tag_compare_against(func, 1),
        "Option ? should branch on the Some variant index"
    );
    assert!(
        has_enum_construct(func, "Option", 0, false),
        "Option ? should synthesize None using the std Option variant index"
    );
}

#[test]
fn try_result_keeps_std_variant_indices() {
    let src = r#"
        import std

        fn make_ok() -> Result<i32, i32> {
          ret Ok(7)
        }

        fn unwrap_ok() -> Result<i32, i32> {
          let value = make_ok()?
          ret Ok(value)
        }
    "#;

    let out = compile_with_std(src);
    assert!(
        out.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        out.diagnostics
    );

    let func = function_by_name(&out, "unwrap_ok");
    assert!(
        has_tag_compare_against(func, 0),
        "Result ? should branch on the Ok variant index"
    );
    assert!(
        has_enum_construct(func, "Result", 1, true),
        "Result ? should synthesize Err using the std Result variant index"
    );
}

#[test]
fn by_value_struct_call_consumes_argument() {
    let src = r#"
        struct TwoStrings { a: String, b: String }

        fn consume(_ts: TwoStrings) -> i32 {
          ret 0
        }

        fn main() -> i32 {
          let ts = TwoStrings { a: String::from_str("hello"), b: String::from_str("world") }
          let _first = consume(ts)
          let _second = consume(ts)
          ret 0
        }
    "#;

    let out = compile_with_std(src);
    assert!(
        out.diagnostics
            .iter()
            .any(|d| d.message.contains("use of moved value `ts`")),
        "expected moved-value diagnostic for consumed struct arg, got: {:?}",
        out.diagnostics
    );
}

#[test]
fn by_value_vec_call_consumes_argument() {
    let src = r#"
        fn consume_vec(v: Vec<String>) -> i32 {
          ret v.len()
        }

        fn main() -> i32 {
          let mut v: Vec<String> = Vec::new()
          v.push(String::from_str("hello"))
          let _first = consume_vec(v)
          let _second = consume_vec(v)
          ret 0
        }
    "#;

    let out = compile_with_std(src);
    assert!(
        out.diagnostics
            .iter()
            .any(|d| d.message.contains("use of moved value `v`")),
        "expected moved-value diagnostic for consumed Vec arg, got: {:?}",
        out.diagnostics
    );
}

#[test]
fn by_value_shared_call_consumes_handle_clone_shares() {
    let moved_src = r#"
        fn consume_shared(_s: Shared<i32>) -> i32 {
          ret 0
        }

        fn main() -> i32 {
          let s = Shared::new(7)
          let _first = consume_shared(s)
          let _second = consume_shared(s)
          ret 0
        }
    "#;

    let moved = compile_with_std(moved_src);
    assert!(
        moved
            .diagnostics
            .iter()
            .any(|d| d.message.contains("use of moved value `s`")),
        "expected moved-value diagnostic for consumed Shared handle, got: {:?}",
        moved.diagnostics
    );

    let clone_src = r#"
        fn consume_shared(_s: Shared<i32>) -> i32 {
          ret 0
        }

        fn main() -> i32 {
          let s = Shared::new(7)
          let s2 = s.clone()
          let _first = consume_shared(s)
          let _second = consume_shared(s2)
          ret 0
        }
    "#;

    let cloned = compile_with_std(clone_src);
    assert!(
        cloned.diagnostics.is_empty(),
        "unexpected diagnostics for cloned Shared handles: {:?}",
        cloned.diagnostics
    );
}

#[test]
fn by_ref_call_does_not_consume_argument() {
    let src = r#"
        fn borrow_string(_s: &String) -> i32 {
          ret 0
        }

        fn main() -> i32 {
          let s = String::from_str("hello")
          let _borrowed = borrow_string(&s)
          let owned = s
          let view: str = owned
          ret view.len()
        }
    "#;

    let out = compile_with_std(src);
    assert!(
        out.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        out.diagnostics
    );
}

#[test]
fn if_else_reassign_drops_old_value_in_both_branches() {
    let src = r#"
        fn main() -> i32 {
          let mut s = String::from_str("seed")
          if true {
            s = String::from_str("left")
          } else {
            s = String::from_str("right")
          }
          ret 0
        }
    "#;

    let out = compile_with_std(src);
    assert!(
        out.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        out.diagnostics
    );

    let main = main_fn_from_output(&out);
    let s_local = local_id_by_name(main, "s");
    assert_eq!(
        count_drop_in_goto_terminated_blocks(main, s_local),
        2,
        "expected one branch-local drop in each if/else arm"
    );
    assert!(
        max_drop_count_per_block(main, s_local) <= 1,
        "did not expect duplicate drops of `s` in a single block"
    );
}

#[test]
fn match_reassign_drops_old_value_in_reachable_arms() {
    let src = r#"
        enum Flag { A, B }

        fn main() -> i32 {
          let mut s = String::from_str("seed")
          let flag = A()
          let _code = match flag {
            A => {
              s = String::from_str("left")
              0
            },
            B => {
              s = String::from_str("right")
              1
            },
          }
          ret 0
        }
    "#;

    let out = compile_with_std(src);
    assert!(
        out.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        out.diagnostics
    );

    let main = main_fn_from_output(&out);
    let s_local = local_id_by_name(main, "s");
    assert_eq!(
        count_drop_in_goto_terminated_blocks(main, s_local),
        2,
        "expected one branch-local drop in each match arm that reaches join"
    );
    assert!(
        max_drop_count_per_block(main, s_local) <= 1,
        "did not expect duplicate drops of `s` in a single block"
    );
}

#[test]
fn nested_if_inside_match_does_not_double_drop_in_same_block() {
    let src = r#"
        enum Side { Left, Right }

        fn main() -> i32 {
          let mut s = String::from_str("seed")
          let mut i: i32 = 0
          while i < 2 {
            let side = if i == 0 { Left() } else { Right() }
            let _code = match side {
              Left => {
                if true {
                  s = String::from_str("left")
                } else {
                  s = String::from_str("left_alt")
                }
                0
              },
              Right => {
                s = String::from_str("right")
                1
              },
            }
            i = i + 1
          }
          ret 0
        }
    "#;

    let out = compile_with_std(src);
    assert!(
        out.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        out.diagnostics
    );

    let main = main_fn_from_output(&out);
    let s_local = local_id_by_name(main, "s");
    assert!(
        max_drop_count_per_block(main, s_local) <= 1,
        "did not expect duplicate drops of `s` in a single block under nested control flow"
    );
}

#[test]
fn guard_drops_before_return_in_reverse_order() {
    let src = r#"
        struct UiSessionGuard { token: String }

        fn main() -> i32 {
          let first = UiSessionGuard { token: String::from_str("a") }
          let second = UiSessionGuard { token: String::from_str("b") }
          ret 0
        }
    "#;

    let out = compile_with_std(src);
    assert!(
        out.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        out.diagnostics
    );

    let main = main_fn_from_output(&out);
    let first_local = local_id_by_name(main, "first");
    let second_local = local_id_by_name(main, "second");
    let insts = &main.blocks[0].insts;

    let second_drop_idx = insts
        .iter()
        .position(|inst| matches!(inst, MirInst::Drop(id) if *id == second_local))
        .expect("missing drop for `second`");
    let first_drop_idx = insts
        .iter()
        .position(|inst| matches!(inst, MirInst::Drop(id) if *id == first_local))
        .expect("missing drop for `first`");
    let ret_idx = insts
        .iter()
        .position(|inst| matches!(inst, MirInst::Return(_)))
        .expect("missing return");

    assert!(
        second_drop_idx < first_drop_idx,
        "guards must drop in reverse declaration order"
    );
    assert!(
        first_drop_idx < ret_idx,
        "guard drops must be emitted before return"
    );
}

#[test]
fn guard_drops_before_break() {
    let src = r#"
        struct UiSessionGuard { token: String }

        fn main() -> i32 {
          let mut i: i32 = 0
          while i < 3 {
            let session = UiSessionGuard { token: String::from_str("loop") }
            if i == 1 { break }
            i = i + 1
          }
          ret 0
        }
    "#;

    let out = compile_with_std(src);
    assert!(
        out.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        out.diagnostics
    );

    let main = main_fn_from_output(&out);
    let session_local = local_id_by_name(main, "session");
    assert!(
        has_drop_followed_by_goto(main, session_local),
        "expected guard drop before break goto"
    );
}

#[test]
fn guard_drops_before_continue() {
    let src = r#"
        struct UiSessionGuard { token: String }

        fn main() -> i32 {
          let mut i: i32 = 0
          while i < 4 {
            let session = UiSessionGuard { token: String::from_str("loop") }
            if i == 1 {
              i = i + 1
              cont
            }
            i = i + 1
          }
          ret 0
        }
    "#;

    let out = compile_with_std(src);
    assert!(
        out.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        out.diagnostics
    );

    let main = main_fn_from_output(&out);
    let session_local = local_id_by_name(main, "session");
    assert!(
        has_drop_followed_by_goto(main, session_local),
        "expected guard drop before continue goto"
    );
}

#[test]
fn guard_double_move_reports_clear_error() {
    let src = r#"
        struct UiSessionGuard { token: String }

        fn main() -> i32 {
          let session = UiSessionGuard { token: String::from_str("ui") }
          let _moved = session
          let _moved_again = session
          ret 0
        }
    "#;

    let out = compile_with_std(src);
    assert!(
        out.diagnostics
            .iter()
            .any(|d| d.message.contains("use of moved guard `session`")),
        "expected moved-guard diagnostic, got: {:?}",
        out.diagnostics
    );
    assert!(
        out.diagnostics.iter().any(|d| {
            d.severity == Severity::Note && d.message.contains("single-owner move semantics")
        }),
        "expected ownership note diagnostic, got: {:?}",
        out.diagnostics
    );
    assert!(
        out.diagnostics.iter().any(|d| {
            d.severity == Severity::Help && d.message.contains("reinitialize the value")
        }),
        "expected ownership help diagnostic, got: {:?}",
        out.diagnostics
    );
}

#[test]
fn uninitialized_value_reports_note_and_help() {
    let src = r#"
        fn main() -> i32 {
          let mut s: String
          if true {
            s = String::from_str("hello")
          }

          let t = s
          let t_ref: str = t
          ret t_ref.len()
        }
    "#;

    let out = compile_with_std(src);
    assert!(
        out.diagnostics
            .iter()
            .any(|d| d.message.contains("use of uninitialized value `s`")),
        "expected uninitialized-value diagnostic, got: {:?}",
        out.diagnostics
    );
    assert!(
        out.diagnostics.iter().any(|d| {
            d.severity == Severity::Note
                && d.message
                    .contains("not definitely initialized on all reachable paths")
        }),
        "expected uninitialized note diagnostic, got: {:?}",
        out.diagnostics
    );
    assert!(
        out.diagnostics.iter().any(|d| {
            d.severity == Severity::Help
                && d.message
                    .contains("initialize the value on every reachable branch before use")
        }),
        "expected uninitialized help diagnostic, got: {:?}",
        out.diagnostics
    );
}

#[test]
fn std_term_api_smoke_compiles() {
    let src = r#"
        from std/term import KeyCode, Terminal
        from std/term import TermError
        from std/enums import Option, Result

        fn run() -> Result<i32, TermError> {
          let mut t = Terminal::stdout()
          let _session = t.enter_ui_session()?
          let _move = t.move_to(1, 2)?
          let _clear = t.clear_line()?
          let _write = t.write_str("hello")?
          let _flush = t.flush()?

          let event = t.poll_event(0)
          let _code = match event {
            Some(_ev) => 1
            None => 0
          }

          ret Ok(0)
        }

        fn main() -> i32 {
          ret match run() {
            Ok(v) => v
            Err(_e) => 1
          }
        }
    "#;

    compile_ok(src);
}
