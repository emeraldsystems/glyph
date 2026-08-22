use super::*;
use glyph_core::mir::{
    Local, LocalId, MirBlock, MirExternFunction, MirFunction, MirInst, MirModule, MirValue, Rvalue,
};
use std::collections::HashMap;
use std::fs;

#[test]
fn creates_empty_module() {
    let ctx = CodegenContext::new("test").unwrap();
    let ir = ctx.dump_ir();
    assert!(ir.contains("test"));
}

#[test]
fn codegens_simple_function() {
    let mut ctx = CodegenContext::new("test").unwrap();
    let mir = MirModule {
        struct_types: HashMap::new(),
        enum_types: HashMap::new(),
        functions: vec![MirFunction {
            name: "main".into(),
            ret_type: Some(Type::I32),
            params: vec![],
            locals: vec![],
            blocks: vec![MirBlock {
                insts: vec![MirInst::Return(Some(MirValue::Int(42)))],
            }],
        }],
        extern_functions: Vec::new(),
    };
    ctx.codegen_module(&mir).unwrap();
    let ir = ctx.dump_ir();
    assert!(ir.contains("define i32 @main"));
    assert!(ir.contains("ret i32 42"));
}

#[test]
fn jit_executes_simple_function() {
    let mut ctx = CodegenContext::new("test").unwrap();
    let mir = MirModule {
        struct_types: HashMap::new(),
        enum_types: HashMap::new(),
        functions: vec![MirFunction {
            name: "main".into(),
            ret_type: Some(Type::I32),
            params: vec![],
            locals: vec![],
            blocks: vec![MirBlock {
                insts: vec![MirInst::Return(Some(MirValue::Int(42)))],
            }],
        }],
        extern_functions: Vec::new(),
    };
    ctx.codegen_module(&mir).unwrap();
    let result = ctx.jit_execute_i32("main").unwrap();
    assert_eq!(result, 42);
}

#[test]
fn codegens_struct_literal() {
    let mut ctx = CodegenContext::new("test").unwrap();
    let mut struct_types = HashMap::new();
    struct_types.insert(
        "Point".into(),
        StructType {
            name: "Point".into(),
            fields: vec![("x".into(), Type::I32), ("y".into(), Type::I32)],
        },
    );

    let mir = MirModule {
        struct_types,
        enum_types: HashMap::new(),
        functions: vec![MirFunction {
            name: "main".into(),
            ret_type: Some(Type::Named("Point".into())),
            params: vec![],
            locals: vec![
                Local {
                    name: Some("p".into()),
                    ty: Some(Type::Named("Point".into())),
                    mutable: false,
                    skip_drop: false,
                },
                Local {
                    name: None,
                    ty: Some(Type::Named("Point".into())),
                    mutable: false,
                    skip_drop: false,
                },
            ],
            blocks: vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(1),
                        value: Rvalue::StructLit {
                            struct_name: "Point".into(),
                            field_values: vec![
                                ("x".into(), MirValue::Int(1)),
                                ("y".into(), MirValue::Int(2)),
                            ],
                        },
                    },
                    MirInst::Assign {
                        local: LocalId(0),
                        value: Rvalue::Move(LocalId(1)),
                    },
                    MirInst::Return(Some(MirValue::Local(LocalId(0)))),
                ],
            }],
        }],
        extern_functions: Vec::new(),
    };

    ctx.codegen_module(&mir).unwrap();
    let ir = ctx.dump_ir();
    assert!(ir.contains("%Point = type { i32, i32 }"));
    assert!(ir.contains("getelementptr inbounds"));
}

#[test]
fn codegens_extern_declare_and_call() {
    let mut ctx = CodegenContext::new("test").unwrap();
    let mir = MirModule {
        struct_types: HashMap::new(),
        enum_types: HashMap::new(),
        extern_functions: vec![MirExternFunction {
            name: "foo".into(),
            ret_type: Some(Type::I32),
            params: vec![Type::I32],
            abi: Some("C".into()),
            link_name: None,
        }],
        functions: vec![MirFunction {
            name: "main".into(),
            ret_type: Some(Type::I32),
            params: vec![],
            locals: vec![Local {
                name: None,
                ty: Some(Type::I32),
                mutable: false,
                skip_drop: false,
            }],
            blocks: vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(0),
                        value: Rvalue::Call {
                            name: "foo".into(),
                            args: vec![MirValue::Int(1)],
                        },
                    },
                    MirInst::Return(Some(MirValue::Local(LocalId(0)))),
                ],
            }],
        }],
    };

    ctx.codegen_module(&mir).unwrap();
    let ir = ctx.dump_ir();
    assert!(ir.contains("declare i32 @foo(i32)"));
    assert!(ir.contains("call i32 @foo"));
}

#[test]
fn codegens_field_access() {
    let mut ctx = CodegenContext::new("test").unwrap();
    let mut struct_types = HashMap::new();
    struct_types.insert(
        "Point".into(),
        StructType {
            name: "Point".into(),
            fields: vec![("x".into(), Type::I32), ("y".into(), Type::I32)],
        },
    );

    let mir = MirModule {
        struct_types,
        enum_types: HashMap::new(),
        functions: vec![MirFunction {
            name: "main".into(),
            ret_type: Some(Type::I32),
            params: vec![],
            locals: vec![
                Local {
                    name: Some("p".into()),
                    ty: Some(Type::Named("Point".into())),
                    mutable: false,
                    skip_drop: false,
                },
                Local {
                    name: None,
                    ty: Some(Type::Named("Point".into())),
                    mutable: false,
                    skip_drop: false,
                },
                Local {
                    name: None,
                    ty: Some(Type::I32),
                    mutable: false,
                    skip_drop: false,
                },
            ],
            blocks: vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(1),
                        value: Rvalue::StructLit {
                            struct_name: "Point".into(),
                            field_values: vec![
                                ("x".into(), MirValue::Int(10)),
                                ("y".into(), MirValue::Int(20)),
                            ],
                        },
                    },
                    MirInst::Assign {
                        local: LocalId(0),
                        value: Rvalue::Move(LocalId(1)),
                    },
                    MirInst::Assign {
                        local: LocalId(2),
                        value: Rvalue::FieldAccess {
                            base: LocalId(0),
                            field_name: "y".into(),
                            field_index: 1,
                        },
                    },
                    MirInst::Return(Some(MirValue::Local(LocalId(2)))),
                ],
            }],
        }],
        extern_functions: Vec::new(),
    };

    ctx.codegen_module(&mir).unwrap();
    let ir = ctx.dump_ir();
    assert!(ir.contains("getelementptr inbounds"));
    assert!(ir.contains("ret i32"));
}

#[test]
fn emit_sets_module_datalayout() {
    let mut ctx = CodegenContext::new("test").unwrap();
    let mir = MirModule {
        struct_types: HashMap::new(),
        enum_types: HashMap::new(),
        functions: vec![],
        extern_functions: Vec::new(),
    };
    ctx.codegen_module(&mir).unwrap();

    let obj_path = std::env::temp_dir().join(format!("glyph_layout_test_{}.o", std::process::id()));
    let result = ctx.emit_object_file(&obj_path);
    let _ = fs::remove_file(&obj_path);
    result.unwrap();

    let ir = ctx.dump_ir();
    assert!(ir.contains("target datalayout"));
    assert!(ir.contains("target triple"));
}

#[test]
fn jit_resolves_extern_symbol_from_host() {
    // Define a test host function that will be called from JIT code
    extern "C" fn test_add_ten(x: i32) -> i32 {
        x + 10
    }

    let mut ctx = CodegenContext::new("test").unwrap();
    let mir = MirModule {
        struct_types: HashMap::new(),
        enum_types: HashMap::new(),
        extern_functions: vec![MirExternFunction {
            name: "test_add_ten".into(),
            ret_type: Some(Type::I32),
            params: vec![Type::I32],
            abi: Some("C".into()),
            link_name: None,
        }],
        functions: vec![MirFunction {
            name: "main".into(),
            ret_type: Some(Type::I32),
            params: vec![],
            locals: vec![Local {
                name: None,
                ty: Some(Type::I32),
                mutable: false,
                skip_drop: false,
            }],
            blocks: vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(0),
                        value: Rvalue::Call {
                            name: "test_add_ten".into(),
                            args: vec![MirValue::Int(5)],
                        },
                    },
                    MirInst::Return(Some(MirValue::Local(LocalId(0)))),
                ],
            }],
        }],
    };

    ctx.codegen_module(&mir).unwrap();

    // Create symbol map with the address of our test function
    let mut symbols = HashMap::new();
    symbols.insert("test_add_ten".to_string(), test_add_ten as u64);

    // Execute and verify the result
    let result = ctx.jit_execute_i32_with_symbols("main", &symbols).unwrap();
    assert_eq!(result, 15);
}

#[test]
fn jit_extern_symbol_codegen_without_execution() {
    // This test verifies that extern functions are declared in LLVM IR
    // but does NOT attempt to execute code with missing symbols (which would crash)
    let mut ctx = CodegenContext::new("test").unwrap();
    let mir = MirModule {
        struct_types: HashMap::new(),
        enum_types: HashMap::new(),
        extern_functions: vec![MirExternFunction {
            name: "missing_function".into(),
            ret_type: Some(Type::I32),
            params: vec![],
            abi: Some("C".into()),
            link_name: None,
        }],
        functions: vec![MirFunction {
            name: "main".into(),
            ret_type: Some(Type::I32),
            params: vec![],
            locals: vec![Local {
                name: None,
                ty: Some(Type::I32),
                mutable: false,
                skip_drop: false,
            }],
            blocks: vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(0),
                        value: Rvalue::Call {
                            name: "missing_function".into(),
                            args: vec![],
                        },
                    },
                    MirInst::Return(Some(MirValue::Local(LocalId(0)))),
                ],
            }],
        }],
    };

    ctx.codegen_module(&mir).unwrap();
    let ir = ctx.dump_ir();

    // Verify the extern function is declared
    assert!(ir.contains("declare i32 @missing_function()"));
    // Verify it's called
    assert!(ir.contains("call i32 @missing_function"));

    // Note: Actually executing this code would crash due to missing symbol.
    // In a production JIT, you'd want symbol resolution validation before execution.
}

#[test]
fn jit_hello_world_with_putchar() {
    // First "Hello World" - calling libc putchar to print 'H'
    extern "C" fn putchar_wrapper(c: i32) -> i32 {
        // Mock putchar for testing (real one would write to stdout)
        c // Just return the character code
    }

    let mut ctx = CodegenContext::new("test").unwrap();
    let mir = MirModule {
        struct_types: HashMap::new(),
        enum_types: HashMap::new(),
        extern_functions: vec![MirExternFunction {
            name: "putchar".into(),
            ret_type: Some(Type::I32),
            params: vec![Type::I32],
            abi: Some("C".into()),
            link_name: None,
        }],
        functions: vec![MirFunction {
            name: "main".into(),
            ret_type: Some(Type::I32),
            params: vec![],
            locals: vec![Local {
                name: None,
                ty: Some(Type::I32),
                mutable: false,
                skip_drop: false,
            }],
            blocks: vec![MirBlock {
                insts: vec![
                    // Call putchar('H') - ASCII 72
                    MirInst::Assign {
                        local: LocalId(0),
                        value: Rvalue::Call {
                            name: "putchar".into(),
                            args: vec![MirValue::Int(72)], // 'H'
                        },
                    },
                    MirInst::Return(Some(MirValue::Local(LocalId(0)))),
                ],
            }],
        }],
    };

    ctx.codegen_module(&mir).unwrap();

    // Register putchar symbol
    let mut symbols = HashMap::new();
    symbols.insert("putchar".to_string(), putchar_wrapper as u64);

    // Execute - should "print" 'H' and return 72
    let result = ctx.jit_execute_i32_with_symbols("main", &symbols).unwrap();
    assert_eq!(result, 72); // putchar returns the character it printed
}

#[test]
fn jit_hello_world_with_puts_literal() {
    extern "C" fn puts_wrapper(ptr: *const i8) -> i32 {
        unsafe { CStr::from_ptr(ptr).to_bytes().len() as i32 }
    }

    let mut ctx = CodegenContext::new("test").unwrap();
    let mir = MirModule {
        struct_types: HashMap::new(),
        enum_types: HashMap::new(),
        extern_functions: vec![MirExternFunction {
            name: "puts".into(),
            ret_type: Some(Type::I32),
            params: vec![Type::Str],
            abi: Some("C".into()),
            link_name: None,
        }],
        functions: vec![MirFunction {
            name: "main".into(),
            ret_type: Some(Type::I32),
            params: vec![],
            locals: vec![
                Local {
                    name: None,
                    ty: Some(Type::Str),
                    mutable: false,
                    skip_drop: false,
                },
                Local {
                    name: None,
                    ty: Some(Type::I32),
                    mutable: false,
                    skip_drop: false,
                },
            ],
            blocks: vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(0),
                        value: Rvalue::StringLit {
                            content: "Hello".into(),
                            global_name: ".str.main.0".into(),
                        },
                    },
                    MirInst::Assign {
                        local: LocalId(1),
                        value: Rvalue::Call {
                            name: "puts".into(),
                            args: vec![MirValue::Local(LocalId(0))],
                        },
                    },
                    MirInst::Return(Some(MirValue::Local(LocalId(1)))),
                ],
            }],
        }],
    };

    ctx.codegen_module(&mir).unwrap();

    let mut symbols = HashMap::new();
    symbols.insert("puts".to_string(), puts_wrapper as u64);

    let result = ctx.jit_execute_i32_with_symbols("main", &symbols).unwrap();
    assert_eq!(result, 5);
}

fn typed_local(ty: Type) -> Local {
    Local {
        name: None,
        ty: Some(ty),
        mutable: false,
        skip_drop: false,
    }
}

#[test]
fn jit_calls_non_capturing_function_value_with_scalar_result() {
    let signature = Type::Function {
        params: vec![Type::I32],
        ret: Box::new(Type::I32),
    };
    let mut ctx = CodegenContext::new("callable_scalar").unwrap();
    let mir = MirModule {
        struct_types: HashMap::new(),
        enum_types: HashMap::new(),
        extern_functions: vec![],
        functions: vec![
            MirFunction {
                name: "identity".into(),
                ret_type: Some(Type::I32),
                params: vec![LocalId(0)],
                locals: vec![typed_local(Type::I32)],
                blocks: vec![MirBlock {
                    insts: vec![MirInst::Return(Some(MirValue::Local(LocalId(0))))],
                }],
            },
            MirFunction {
                name: "main".into(),
                ret_type: Some(Type::I32),
                params: vec![],
                locals: vec![typed_local(signature.clone()), typed_local(Type::I32)],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(0),
                            value: Rvalue::FunctionRef {
                                name: "identity".into(),
                                signature: signature.clone(),
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(1),
                            value: Rvalue::CallIndirect {
                                callee: LocalId(0),
                                signature,
                                args: vec![MirValue::Int(42)],
                            },
                        },
                        MirInst::Return(Some(MirValue::Local(LocalId(1)))),
                    ],
                }],
            },
        ],
    };

    ctx.codegen_module(&mir).unwrap();
    assert_eq!(ctx.jit_execute_i32("main").unwrap(), 42);
    let ir = ctx.dump_ir();
    assert!(ir.contains("__glyph_fnref_identity_"));
    assert!(ir.contains("call.indirect"));
}

#[test]
fn jit_calls_zero_argument_unit_function_value() {
    let signature = Type::Function {
        params: vec![],
        ret: Box::new(Type::Void),
    };
    let mut ctx = CodegenContext::new("callable_unit").unwrap();
    let mir = MirModule {
        struct_types: HashMap::new(),
        enum_types: HashMap::new(),
        extern_functions: vec![],
        functions: vec![
            MirFunction {
                name: "ping".into(),
                ret_type: None,
                params: vec![],
                locals: vec![],
                blocks: vec![MirBlock {
                    insts: vec![MirInst::Return(None)],
                }],
            },
            MirFunction {
                name: "main".into(),
                ret_type: Some(Type::I32),
                params: vec![],
                locals: vec![typed_local(signature.clone()), typed_local(Type::Void)],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(0),
                            value: Rvalue::FunctionRef {
                                name: "ping".into(),
                                signature: signature.clone(),
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(1),
                            value: Rvalue::CallIndirect {
                                callee: LocalId(0),
                                signature,
                                args: vec![],
                            },
                        },
                        MirInst::Return(Some(MirValue::Int(7))),
                    ],
                }],
            },
        ],
    };

    ctx.codegen_module(&mir).unwrap();
    assert_eq!(ctx.jit_execute_i32("main").unwrap(), 7);
}

#[test]
fn jit_calls_function_value_with_large_sret_result() {
    let big_ty = Type::Named("Big".into());
    let signature = Type::Function {
        params: vec![big_ty.clone()],
        ret: Box::new(big_ty.clone()),
    };
    let mut struct_types = HashMap::new();
    struct_types.insert(
        "Big".into(),
        StructType {
            name: "Big".into(),
            fields: (0..5)
                .map(|index| (format!("f{}", index), Type::I32))
                .collect(),
        },
    );
    let mut ctx = CodegenContext::new("callable_sret").unwrap();
    let mir = MirModule {
        struct_types,
        enum_types: HashMap::new(),
        extern_functions: vec![],
        functions: vec![
            MirFunction {
                name: "identity_big".into(),
                ret_type: Some(big_ty.clone()),
                params: vec![LocalId(0)],
                locals: vec![typed_local(big_ty.clone())],
                blocks: vec![MirBlock {
                    insts: vec![MirInst::Return(Some(MirValue::Local(LocalId(0))))],
                }],
            },
            MirFunction {
                name: "main".into(),
                ret_type: Some(Type::I32),
                params: vec![],
                locals: vec![
                    typed_local(signature.clone()),
                    typed_local(big_ty.clone()),
                    typed_local(big_ty),
                    typed_local(Type::I32),
                ],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(0),
                            value: Rvalue::FunctionRef {
                                name: "identity_big".into(),
                                signature: signature.clone(),
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(1),
                            value: Rvalue::StructLit {
                                struct_name: "Big".into(),
                                field_values: (0..5)
                                    .map(|index| (format!("f{}", index), MirValue::Int(index + 1)))
                                    .collect(),
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(2),
                            value: Rvalue::CallIndirect {
                                callee: LocalId(0),
                                signature,
                                args: vec![MirValue::Local(LocalId(1))],
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(3),
                            value: Rvalue::FieldAccess {
                                base: LocalId(2),
                                field_name: "f4".into(),
                                field_index: 4,
                            },
                        },
                        MirInst::Return(Some(MirValue::Local(LocalId(3)))),
                    ],
                }],
            },
        ],
    };

    ctx.codegen_module(&mir).unwrap();
    assert_eq!(ctx.jit_execute_i32("main").unwrap(), 5);
    let ir = ctx.dump_ir();
    assert!(ir.contains("sret(%Big)"));
}

#[test]
fn dropping_non_capturing_function_value_is_a_safe_noop() {
    let signature = Type::Function {
        params: vec![],
        ret: Box::new(Type::I32),
    };
    let mut ctx = CodegenContext::new("callable_drop").unwrap();
    let mir = MirModule {
        struct_types: HashMap::new(),
        enum_types: HashMap::new(),
        extern_functions: vec![],
        functions: vec![
            MirFunction {
                name: "answer".into(),
                ret_type: Some(Type::I32),
                params: vec![],
                locals: vec![],
                blocks: vec![MirBlock {
                    insts: vec![MirInst::Return(Some(MirValue::Int(42)))],
                }],
            },
            MirFunction {
                name: "main".into(),
                ret_type: Some(Type::I32),
                params: vec![],
                locals: vec![typed_local(signature.clone())],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(0),
                            value: Rvalue::FunctionRef {
                                name: "answer".into(),
                                signature,
                            },
                        },
                        MirInst::Drop(LocalId(0)),
                        MirInst::Return(Some(MirValue::Int(9))),
                    ],
                }],
            },
        ],
    };

    ctx.codegen_module(&mir).unwrap();
    assert_eq!(ctx.jit_execute_i32("main").unwrap(), 9);
    assert!(ctx.dump_ir().contains("callable.drop.isnull"));
}

#[test]
fn callable_views_are_never_silently_cloned() {
    let signature = Type::Function {
        params: vec![Type::I32],
        ret: Box::new(Type::I32),
    };
    let mut ctx = CodegenContext::new("callable_clone_rejection").unwrap();

    assert!(CodegenContext::type_needs_clone(&signature));
    let error = ctx
        .codegen_deep_clone_value(&signature, std::ptr::null_mut())
        .unwrap_err();
    assert!(error.to_string().contains("cannot be cloned"));
}
