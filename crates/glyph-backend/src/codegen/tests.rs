use super::*;
use glyph_core::mir::{
    BlockId, BorrowKind, CaptureTransfer, Local, LocalId, MirBlock, MirBorrowCapture, MirCapture,
    MirExternFunction, MirFunction, MirInst, MirModule, MirValue, Rvalue,
};
use glyph_core::types::BorrowedCallableKind;
use std::collections::HashMap;
use std::ffi::c_void;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};

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
    symbols.insert(
        "test_add_ten".to_string(),
        test_add_ten as *const () as usize as u64,
    );

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
    symbols.insert(
        "putchar".to_string(),
        putchar_wrapper as *const () as usize as u64,
    );

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
    symbols.insert(
        "puts".to_string(),
        puts_wrapper as *const () as usize as u64,
    );

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

static CLOSURE_FREE_COUNT: AtomicUsize = AtomicUsize::new(0);
static INVOKED_CLOSURE_FREE_COUNT: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn counting_closure_free(pointer: *mut c_void) {
    CLOSURE_FREE_COUNT.fetch_add(1, Ordering::SeqCst);
    unsafe { libc::free(pointer) };
}

unsafe extern "C" fn counting_invoked_closure_free(pointer: *mut c_void) {
    INVOKED_CLOSURE_FREE_COUNT.fetch_add(1, Ordering::SeqCst);
    unsafe { libc::free(pointer) };
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

/// GLYPH-73: `codegen_callable_argument` (codegen/callable.rs) must derive
/// sign/zero extension from the ARGUMENT's own Glyph type, not the
/// callable's declared parameter type. The frontend resolver requires an
/// exact type match at a `function(value)` invocation site, so this
/// width-mismatched argument can't be produced by compiling Glyph source
/// (see `calling_a_callable_value_directly_requires_an_exact_type_match` in
/// `crates/glyph-cli/tests/codegen_int_widening.rs`); this test constructs
/// the MIR directly to exercise the coercion in isolation and guard against
/// a future resolver relaxation reintroducing the sign-extension bug.
#[test]
fn jit_calls_function_value_zero_extends_unsigned_argument() {
    let signature = Type::Function {
        params: vec![Type::I64],
        ret: Box::new(Type::I64),
    };
    let mut ctx = CodegenContext::new("callable_unsigned_widen").unwrap();
    let mir = MirModule {
        struct_types: HashMap::new(),
        enum_types: HashMap::new(),
        extern_functions: vec![],
        functions: vec![
            MirFunction {
                name: "identity64".into(),
                ret_type: Some(Type::I64),
                params: vec![LocalId(0)],
                locals: vec![typed_local(Type::I64)],
                blocks: vec![MirBlock {
                    insts: vec![MirInst::Return(Some(MirValue::Local(LocalId(0))))],
                }],
            },
            MirFunction {
                name: "main".into(),
                ret_type: Some(Type::I32),
                params: vec![],
                // 0: the callable value; 1: a u32 arg holding all bits set
                // (4294967295); 2: the i64 call result.
                locals: vec![
                    typed_local(signature.clone()),
                    typed_local(Type::U32),
                    typed_local(Type::I64),
                ],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(0),
                            value: Rvalue::FunctionRef {
                                name: "identity64".into(),
                                signature: signature.clone(),
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(1),
                            value: Rvalue::ConstInt(4294967295),
                        },
                        MirInst::Assign {
                            local: LocalId(2),
                            value: Rvalue::CallIndirect {
                                callee: LocalId(0),
                                signature,
                                args: vec![MirValue::Local(LocalId(1))],
                            },
                        },
                        MirInst::Return(Some(MirValue::Int(0))),
                    ],
                }],
            },
        ],
    };

    ctx.codegen_module(&mir).unwrap();
    let ir = ctx.dump_ir();
    assert!(
        ir.contains("zext i32") && ir.contains("to i64"),
        "u32 argument to an indirect call must zero-extend:\n{}",
        ir
    );
    assert!(
        !ir.contains("sext i32"),
        "u32 argument to an indirect call must not sign-extend:\n{}",
        ir
    );
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

#[test]
fn jit_reuses_shared_borrowed_closure_without_consuming_or_freeing_it() {
    let signature = Type::BorrowedFunction {
        kind: BorrowedCallableKind::Fn,
        params: vec![Type::I32],
        ret: Box::new(Type::I32),
    };
    let capture_ref = Type::Ref(Box::new(Type::I32), Mutability::Immutable);
    let mut ctx = CodegenContext::new("borrowed_fn_repeat").unwrap();
    let mir = MirModule {
        struct_types: HashMap::new(),
        enum_types: HashMap::new(),
        extern_functions: vec![],
        functions: vec![
            MirFunction {
                name: "add".into(),
                ret_type: Some(Type::I32),
                params: vec![LocalId(0), LocalId(1)],
                locals: vec![
                    typed_local(Type::I32),
                    typed_local(Type::I32),
                    typed_local(Type::I32),
                ],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(2),
                            value: Rvalue::Binary {
                                op: glyph_core::ast::BinaryOp::Add,
                                lhs: MirValue::Local(LocalId(0)),
                                rhs: MirValue::Local(LocalId(1)),
                            },
                        },
                        MirInst::Return(Some(MirValue::Local(LocalId(2)))),
                    ],
                }],
            },
            MirFunction {
                name: "main::__closure_borrowed".into(),
                ret_type: Some(Type::I32),
                params: vec![LocalId(0), LocalId(1)],
                locals: vec![
                    typed_local(capture_ref),
                    typed_local(Type::I32),
                    typed_local(Type::I32),
                ],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(2),
                            value: Rvalue::Call {
                                name: "add".into(),
                                args: vec![
                                    MirValue::Local(LocalId(0)),
                                    MirValue::Local(LocalId(1)),
                                ],
                            },
                        },
                        MirInst::Return(Some(MirValue::Local(LocalId(2)))),
                    ],
                }],
            },
            MirFunction {
                name: "main".into(),
                ret_type: Some(Type::I32),
                params: vec![],
                locals: vec![
                    typed_local(Type::I32),
                    typed_local(signature.clone()),
                    typed_local(Type::I32),
                    typed_local(Type::I32),
                    typed_local(Type::I32),
                ],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(0),
                            value: Rvalue::ConstInt(40),
                        },
                        MirInst::Assign {
                            local: LocalId(1),
                            value: Rvalue::MakeBorrowedClosure {
                                function: "main::__closure_borrowed".into(),
                                signature: signature.clone(),
                                captures: vec![MirBorrowCapture {
                                    name: "offset".into(),
                                    local: LocalId(0),
                                    ty: Type::I32,
                                    borrow: BorrowKind::Shared,
                                    source: glyph_core::mir::BorrowCaptureSource::Local,
                                }],
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(2),
                            value: Rvalue::CallIndirectShared {
                                callee: LocalId(1),
                                signature: signature.clone(),
                                args: vec![MirValue::Int(1)],
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(3),
                            value: Rvalue::CallIndirectShared {
                                callee: LocalId(1),
                                signature,
                                args: vec![MirValue::Int(2)],
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(4),
                            value: Rvalue::Binary {
                                op: glyph_core::ast::BinaryOp::Add,
                                lhs: MirValue::Local(LocalId(2)),
                                rhs: MirValue::Local(LocalId(3)),
                            },
                        },
                        MirInst::Return(Some(MirValue::Local(LocalId(4)))),
                    ],
                }],
            },
        ],
    };

    ctx.codegen_module(&mir).unwrap();
    assert_eq!(ctx.jit_execute_i32("main").unwrap(), 83);
    let ir = ctx.dump_ir();
    assert!(ir.contains("borrowed.closure.env"));
    assert!(ir.contains("__glyph_borrowed_closure_invoke_"));
    assert!(!ir.contains("borrowed.closure.env.free"));
}

#[test]
fn jit_reuses_fnmut_carrier_without_consuming_it() {
    let signature = Type::BorrowedFunction {
        kind: BorrowedCallableKind::FnMut,
        params: vec![],
        ret: Box::new(Type::I32),
    };
    let capture_ref = Type::Ref(Box::new(Type::I32), Mutability::Mutable);
    let mut ctx = CodegenContext::new("borrowed_fnmut_repeat").unwrap();
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
                name: "main::__closure_borrowed_mut".into(),
                ret_type: Some(Type::I32),
                params: vec![LocalId(0)],
                locals: vec![typed_local(capture_ref), typed_local(Type::I32)],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(1),
                            value: Rvalue::Call {
                                name: "identity".into(),
                                args: vec![MirValue::Local(LocalId(0))],
                            },
                        },
                        MirInst::Return(Some(MirValue::Local(LocalId(1)))),
                    ],
                }],
            },
            MirFunction {
                name: "main".into(),
                ret_type: Some(Type::I32),
                params: vec![],
                locals: vec![
                    typed_local(Type::I32),
                    typed_local(signature.clone()),
                    typed_local(Type::I32),
                    typed_local(Type::I32),
                    typed_local(Type::I32),
                ],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(0),
                            value: Rvalue::ConstInt(7),
                        },
                        MirInst::Assign {
                            local: LocalId(1),
                            value: Rvalue::MakeBorrowedClosure {
                                function: "main::__closure_borrowed_mut".into(),
                                signature: signature.clone(),
                                captures: vec![MirBorrowCapture {
                                    name: "state".into(),
                                    local: LocalId(0),
                                    ty: Type::I32,
                                    borrow: BorrowKind::Mutable,
                                    source: glyph_core::mir::BorrowCaptureSource::Local,
                                }],
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(2),
                            value: Rvalue::CallIndirectMut {
                                callee: LocalId(1),
                                signature: signature.clone(),
                                args: vec![],
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(3),
                            value: Rvalue::CallIndirectMut {
                                callee: LocalId(1),
                                signature,
                                args: vec![],
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(4),
                            value: Rvalue::Binary {
                                op: glyph_core::ast::BinaryOp::Add,
                                lhs: MirValue::Local(LocalId(2)),
                                rhs: MirValue::Local(LocalId(3)),
                            },
                        },
                        MirInst::Return(Some(MirValue::Local(LocalId(4)))),
                    ],
                }],
            },
        ],
    };

    ctx.codegen_module(&mir).unwrap();
    assert_eq!(ctx.jit_execute_i32("main").unwrap(), 14);
}

#[test]
fn consuming_and_repeatable_indirect_call_mir_cannot_be_interchanged() {
    let owned = Type::Function {
        params: vec![],
        ret: Box::new(Type::I32),
    };
    let borrowed = Type::BorrowedFunction {
        kind: BorrowedCallableKind::Fn,
        params: vec![],
        ret: Box::new(Type::I32),
    };

    let make_module = |local_ty: Type, call: Rvalue| MirModule {
        struct_types: HashMap::new(),
        enum_types: HashMap::new(),
        extern_functions: vec![],
        functions: vec![MirFunction {
            name: "main".into(),
            ret_type: Some(Type::I32),
            params: vec![],
            locals: vec![typed_local(local_ty), typed_local(Type::I32)],
            blocks: vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(1),
                        value: call,
                    },
                    MirInst::Return(Some(MirValue::Local(LocalId(1)))),
                ],
            }],
        }],
    };

    let mut consuming = CodegenContext::new("bad_consuming_borrowed").unwrap();
    let error = consuming
        .codegen_module(&make_module(
            borrowed.clone(),
            Rvalue::CallIndirect {
                callee: LocalId(0),
                signature: borrowed,
                args: vec![],
            },
        ))
        .unwrap_err();
    assert!(error.to_string().contains("owned FnOnce"));

    let mut repeatable = CodegenContext::new("bad_repeatable_owned").unwrap();
    let error = repeatable
        .codegen_module(&make_module(
            owned.clone(),
            Rvalue::CallIndirectShared {
                callee: LocalId(0),
                signature: owned,
                args: vec![],
            },
        ))
        .unwrap_err();
    assert!(error.to_string().contains("borrowed Fn"));

    let make_function_module = |signature: Type, construction: Rvalue| MirModule {
        struct_types: HashMap::new(),
        enum_types: HashMap::new(),
        extern_functions: vec![],
        functions: vec![
            MirFunction {
                name: "target".into(),
                ret_type: Some(Type::I32),
                params: vec![],
                locals: vec![],
                blocks: vec![MirBlock {
                    insts: vec![MirInst::Return(Some(MirValue::Int(1)))],
                }],
            },
            MirFunction {
                name: "main".into(),
                ret_type: Some(Type::I32),
                params: vec![],
                locals: vec![typed_local(signature)],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(0),
                            value: construction,
                        },
                        MirInst::Return(Some(MirValue::Int(0))),
                    ],
                }],
            },
        ],
    };

    let borrowed = Type::BorrowedFunction {
        kind: BorrowedCallableKind::Fn,
        params: vec![],
        ret: Box::new(Type::I32),
    };
    let mut owned_constructor = CodegenContext::new("bad_owned_constructor").unwrap();
    let error = owned_constructor
        .codegen_module(&make_function_module(
            borrowed.clone(),
            Rvalue::MakeClosure {
                function: "target".into(),
                signature: borrowed,
                captures: vec![],
            },
        ))
        .unwrap_err();
    assert!(error.to_string().contains("owned FnOnce"));

    let owned = Type::Function {
        params: vec![],
        ret: Box::new(Type::I32),
    };
    let mut borrowed_constructor = CodegenContext::new("bad_borrowed_constructor").unwrap();
    let error = borrowed_constructor
        .codegen_module(&make_function_module(
            owned.clone(),
            Rvalue::MakeBorrowedClosure {
                function: "target".into(),
                signature: owned.clone(),
                captures: vec![],
            },
        ))
        .unwrap_err();
    assert!(error.to_string().contains("Fn/FnMut"));

    let borrowed = Type::BorrowedFunction {
        kind: BorrowedCallableKind::Fn,
        params: vec![],
        ret: Box::new(Type::I32),
    };
    let mut relabeled_borrowed = CodegenContext::new("relabeled_borrowed_as_owned").unwrap();
    let error = relabeled_borrowed
        .codegen_module(&make_function_module(
            owned.clone(),
            Rvalue::MakeBorrowedClosure {
                function: "target".into(),
                signature: borrowed.clone(),
                captures: vec![],
            },
        ))
        .unwrap_err();
    assert!(error.to_string().contains("destination type"));

    let mut relabeled_owned = CodegenContext::new("relabeled_owned_as_borrowed").unwrap();
    let error = relabeled_owned
        .codegen_module(&make_function_module(
            borrowed,
            Rvalue::MakeClosure {
                function: "target".into(),
                signature: owned,
                captures: vec![],
            },
        ))
        .unwrap_err();
    assert!(error.to_string().contains("destination type"));
}

#[test]
fn jit_invokes_closure_with_copied_scalar_capture() {
    let signature = Type::Function {
        params: vec![Type::I32],
        ret: Box::new(Type::I32),
    };
    let mut ctx = CodegenContext::new("closure_scalar_capture").unwrap();
    let mir = MirModule {
        struct_types: HashMap::new(),
        enum_types: HashMap::new(),
        extern_functions: vec![],
        functions: vec![
            MirFunction {
                name: "main::__closure_0".into(),
                ret_type: Some(Type::I32),
                params: vec![LocalId(0), LocalId(1)],
                locals: vec![
                    typed_local(Type::I32),
                    typed_local(Type::I32),
                    typed_local(Type::I32),
                ],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(2),
                            value: Rvalue::Binary {
                                op: glyph_core::ast::BinaryOp::Add,
                                lhs: MirValue::Local(LocalId(0)),
                                rhs: MirValue::Local(LocalId(1)),
                            },
                        },
                        MirInst::Return(Some(MirValue::Local(LocalId(2)))),
                    ],
                }],
            },
            MirFunction {
                name: "main".into(),
                ret_type: Some(Type::I32),
                params: vec![],
                locals: vec![
                    typed_local(Type::I32),
                    typed_local(signature.clone()),
                    typed_local(Type::I32),
                ],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(0),
                            value: Rvalue::ConstInt(40),
                        },
                        MirInst::Assign {
                            local: LocalId(1),
                            value: Rvalue::MakeClosure {
                                function: "main::__closure_0".into(),
                                signature: signature.clone(),
                                captures: vec![MirCapture {
                                    name: "offset".into(),
                                    local: LocalId(0),
                                    ty: Type::I32,
                                    transfer: CaptureTransfer::Copy,
                                }],
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(2),
                            value: Rvalue::CallIndirect {
                                callee: LocalId(1),
                                signature,
                                args: vec![MirValue::Int(2)],
                            },
                        },
                        MirInst::Return(Some(MirValue::Local(LocalId(2)))),
                    ],
                }],
            },
        ],
    };

    ctx.codegen_module(&mir).unwrap();
    assert_eq!(ctx.jit_execute_i32("main").unwrap(), 42);
    let ir = ctx.dump_ir();
    assert!(ir.contains("__glyph_closure_invoke_"));
    assert!(ir.contains("__glyph_closure_drop_"));
    assert!(ir.contains("closure.env.malloc"));
    assert!(ir.contains("call void @free"));
}

#[test]
fn jit_drops_uncalled_closure_owned_capture_exactly_once() {
    let signature = Type::Function {
        params: vec![],
        ret: Box::new(Type::Void),
    };
    let mut ctx = CodegenContext::new("closure_owned_drop").unwrap();
    let owned_i32 = Type::Own(Box::new(Type::I32));
    let mir = MirModule {
        struct_types: HashMap::new(),
        enum_types: HashMap::new(),
        extern_functions: vec![],
        functions: vec![
            MirFunction {
                name: "main::__closure_0".into(),
                ret_type: None,
                params: vec![LocalId(0)],
                locals: vec![typed_local(owned_i32.clone())],
                blocks: vec![MirBlock {
                    insts: vec![MirInst::Drop(LocalId(0)), MirInst::Return(None)],
                }],
            },
            MirFunction {
                name: "main".into(),
                ret_type: Some(Type::I32),
                params: vec![],
                locals: vec![
                    typed_local(owned_i32.clone()),
                    typed_local(signature.clone()),
                ],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(0),
                            value: Rvalue::OwnNew {
                                value: MirValue::Int(7),
                                elem_type: Type::I32,
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(1),
                            value: Rvalue::MakeClosure {
                                function: "main::__closure_0".into(),
                                signature,
                                captures: vec![MirCapture {
                                    name: "message".into(),
                                    local: LocalId(0),
                                    ty: owned_i32,
                                    transfer: CaptureTransfer::Move,
                                }],
                            },
                        },
                        MirInst::Drop(LocalId(1)),
                        MirInst::Return(Some(MirValue::Int(9))),
                    ],
                }],
            },
        ],
    };

    ctx.codegen_module(&mir).unwrap();
    CLOSURE_FREE_COUNT.store(0, Ordering::SeqCst);
    let symbols = HashMap::from([(
        "free".to_string(),
        counting_closure_free as *const () as u64,
    )]);
    assert_eq!(
        ctx.jit_execute_i32_with_symbols("main", &symbols).unwrap(),
        9
    );
    assert_eq!(
        CLOSURE_FREE_COUNT.load(Ordering::SeqCst),
        2,
        "owned payload and closure environment must each be freed exactly once"
    );
    let ir = ctx.dump_ir();
    assert!(ir.contains("closure.drop.capture.0"));
    assert!(ir.contains("callable.drop.call"));
}

#[test]
fn jit_invoked_closure_drops_owned_capture_and_environment_exactly_once() {
    let signature = Type::Function {
        params: vec![],
        ret: Box::new(Type::I32),
    };
    let mut ctx = CodegenContext::new("closure_invoked_owned_drop").unwrap();
    let owned_i32 = Type::Own(Box::new(Type::I32));
    let mir = MirModule {
        struct_types: HashMap::new(),
        enum_types: HashMap::new(),
        extern_functions: vec![],
        functions: vec![
            MirFunction {
                name: "main::__closure_0".into(),
                ret_type: Some(Type::I32),
                params: vec![LocalId(0)],
                locals: vec![typed_local(owned_i32.clone())],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Drop(LocalId(0)),
                        MirInst::Return(Some(MirValue::Int(42))),
                    ],
                }],
            },
            MirFunction {
                name: "main".into(),
                ret_type: Some(Type::I32),
                params: vec![],
                locals: vec![
                    typed_local(owned_i32.clone()),
                    typed_local(signature.clone()),
                    typed_local(Type::I32),
                ],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(0),
                            value: Rvalue::OwnNew {
                                value: MirValue::Int(7),
                                elem_type: Type::I32,
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(1),
                            value: Rvalue::MakeClosure {
                                function: "main::__closure_0".into(),
                                signature: signature.clone(),
                                captures: vec![MirCapture {
                                    name: "payload".into(),
                                    local: LocalId(0),
                                    ty: owned_i32,
                                    transfer: CaptureTransfer::Move,
                                }],
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(2),
                            value: Rvalue::CallIndirect {
                                callee: LocalId(1),
                                signature,
                                args: vec![],
                            },
                        },
                        // Scope cleanup still visits the consumed callable. The
                        // consuming call must have cleared its carrier so this
                        // post-call drop cannot free the environment twice.
                        MirInst::Drop(LocalId(1)),
                        MirInst::Return(Some(MirValue::Local(LocalId(2)))),
                    ],
                }],
            },
        ],
    };

    ctx.codegen_module(&mir).unwrap();
    INVOKED_CLOSURE_FREE_COUNT.store(0, Ordering::SeqCst);
    let symbols = HashMap::from([(
        "free".to_string(),
        counting_invoked_closure_free as *const () as u64,
    )]);
    assert_eq!(
        ctx.jit_execute_i32_with_symbols("main", &symbols).unwrap(),
        42,
        "the owned FnOnce body must run before its environment is released"
    );
    assert_eq!(
        INVOKED_CLOSURE_FREE_COUNT.load(Ordering::SeqCst),
        2,
        "the invoked closure must free its owned payload and environment exactly once"
    );
}

#[test]
fn jit_invokes_capturing_closure_with_large_sret_result() {
    let big_ty = Type::Named("Big".into());
    let signature = Type::Function {
        params: vec![],
        ret: Box::new(big_ty.clone()),
    };
    let mut struct_types = HashMap::new();
    struct_types.insert(
        "Big".into(),
        StructType {
            name: "Big".into(),
            fields: (0..5)
                .map(|index| (format!("f{index}"), Type::I32))
                .collect(),
        },
    );
    let mut ctx = CodegenContext::new("closure_sret").unwrap();
    let mir = MirModule {
        struct_types,
        enum_types: HashMap::new(),
        extern_functions: vec![],
        functions: vec![
            MirFunction {
                name: "main::__closure_0".into(),
                ret_type: Some(big_ty.clone()),
                params: vec![LocalId(0)],
                locals: vec![typed_local(Type::I32), typed_local(big_ty.clone())],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(1),
                            value: Rvalue::StructLit {
                                struct_name: "Big".into(),
                                field_values: (0..5)
                                    .map(|index| {
                                        let value = if index == 4 {
                                            MirValue::Local(LocalId(0))
                                        } else {
                                            MirValue::Int(index + 1)
                                        };
                                        (format!("f{index}"), value)
                                    })
                                    .collect(),
                            },
                        },
                        MirInst::Return(Some(MirValue::Local(LocalId(1)))),
                    ],
                }],
            },
            MirFunction {
                name: "main".into(),
                ret_type: Some(Type::I32),
                params: vec![],
                locals: vec![
                    typed_local(Type::I32),
                    typed_local(signature.clone()),
                    typed_local(big_ty),
                    typed_local(Type::I32),
                ],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(0),
                            value: Rvalue::ConstInt(42),
                        },
                        MirInst::Assign {
                            local: LocalId(1),
                            value: Rvalue::MakeClosure {
                                function: "main::__closure_0".into(),
                                signature: signature.clone(),
                                captures: vec![MirCapture {
                                    name: "answer".into(),
                                    local: LocalId(0),
                                    ty: Type::I32,
                                    transfer: CaptureTransfer::Copy,
                                }],
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(2),
                            value: Rvalue::CallIndirect {
                                callee: LocalId(1),
                                signature,
                                args: vec![],
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
    assert_eq!(ctx.jit_execute_i32("main").unwrap(), 42);
    assert!(ctx.dump_ir().contains("sret(%Big)"));
}

#[test]
fn closure_rejects_bitwise_copy_of_ownership_bearing_capture() {
    let owned_i32 = Type::Own(Box::new(Type::I32));
    let signature = Type::Function {
        params: vec![],
        ret: Box::new(Type::Void),
    };
    let mut ctx = CodegenContext::new("closure_invalid_copy").unwrap();
    let mir = MirModule {
        struct_types: HashMap::new(),
        enum_types: HashMap::new(),
        extern_functions: vec![],
        functions: vec![
            MirFunction {
                name: "main::__closure_0".into(),
                ret_type: None,
                params: vec![LocalId(0)],
                locals: vec![typed_local(owned_i32.clone())],
                blocks: vec![MirBlock {
                    insts: vec![MirInst::Drop(LocalId(0)), MirInst::Return(None)],
                }],
            },
            MirFunction {
                name: "main".into(),
                ret_type: Some(Type::I32),
                params: vec![],
                locals: vec![
                    typed_local(owned_i32.clone()),
                    typed_local(signature.clone()),
                ],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(0),
                            value: Rvalue::OwnNew {
                                value: MirValue::Int(7),
                                elem_type: Type::I32,
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(1),
                            value: Rvalue::MakeClosure {
                                function: "main::__closure_0".into(),
                                signature,
                                captures: vec![MirCapture {
                                    name: "owned".into(),
                                    local: LocalId(0),
                                    ty: owned_i32,
                                    transfer: CaptureTransfer::Copy,
                                }],
                            },
                        },
                        MirInst::Return(Some(MirValue::Int(0))),
                    ],
                }],
            },
        ],
    };

    let error = ctx.codegen_module(&mir).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cannot copy ownership-bearing type")
    );
}

// ---------------------------------------------------------------------------
// GLYPH-3: `codegen_module` must run the MIR verifier before any LLVM
// lowering. `glyph_core::mir_verify` has its own exhaustive unit tests for
// the check logic itself; this test is only about the wiring — that
// `codegen_module` (the single chokepoint every LLVM path, including this
// test file's own `CodegenContext::new` + `codegen_module` pattern, goes
// through) actually calls it and fails closed before touching LLVM.
// ---------------------------------------------------------------------------

#[test]
fn codegen_module_rejects_malformed_mir_before_any_llvm_lowering() {
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
                // `Goto` to a block that does not exist: verifier must catch
                // this before `create_named_types`/`codegen_function_body`
                // ever run, so no partial LLVM module is left behind either.
                insts: vec![MirInst::Goto(BlockId(41))],
            }],
        }],
        extern_functions: Vec::new(),
    };

    let error = ctx.codegen_module(&mir).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("MIR verification failed"),
        "expected the verifier's bail message, got: {}",
        message
    );
    assert!(
        message.contains("out-of-range block"),
        "expected the specific verifier error to be included, got: {}",
        message
    );

    // Nothing should have been emitted into the module on the failing path.
    let ir = ctx.dump_ir();
    assert!(
        !ir.contains("define"),
        "codegen must not lower anything once verification fails:\n{}",
        ir
    );
}
