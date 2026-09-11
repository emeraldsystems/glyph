#![cfg(all(feature = "codegen", any(target_os = "macos", target_os = "linux")))]

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use glyph_backend::{
    codegen::CodegenContext,
    linker::{Linker, LinkerOptions},
};
use glyph_core::mir::{
    Local, LocalId, MirBlock, MirExternFunction, MirFunction, MirInst, MirModule, MirValue, Rvalue,
};
use glyph_core::thread::{
    private_scoped_thread_handle_type, private_thread_scope_type, scoped_task_type,
};
use glyph_core::types::{BorrowedCallableKind, StructType, Type};

#[repr(C)]
struct GlyphThreadScope {
    _private: [u8; 0],
}

#[repr(C)]
struct GlyphScopedThread {
    _private: [u8; 0],
}

type ThreadEntry = unsafe extern "C" fn(*mut c_void);
type ThreadResultEntry = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void);
type DropResult = unsafe extern "C" fn(*mut c_void);

unsafe extern "C" {
    fn glyph_thread_scope_create(out: *mut *mut GlyphThreadScope) -> i32;
    fn glyph_thread_scope_spawn(
        scope: *mut GlyphThreadScope,
        out: *mut *mut GlyphScopedThread,
        entry: Option<ThreadEntry>,
        env: *mut c_void,
    ) -> i32;
    fn glyph_thread_scope_spawn_result(
        scope: *mut GlyphThreadScope,
        out: *mut *mut GlyphScopedThread,
        entry: Option<ThreadResultEntry>,
        invoke: *mut c_void,
        env: *mut c_void,
        result_size: usize,
        drop_result: Option<DropResult>,
    ) -> i32;
    fn glyph_thread_scope_join(child: *mut *mut GlyphScopedThread) -> i32;
    fn glyph_thread_scope_join_result(
        child: *mut *mut GlyphScopedThread,
        out_result: *mut c_void,
    ) -> i32;
    fn glyph_thread_scope_join_all(scope: *mut *mut GlyphThreadScope) -> i32;
    fn glyph_thread_scope_drain_or_abort(scope: *mut GlyphThreadScope);
    fn glyph_thread_test_fail_next(operation: i32, error_code: i32) -> i32;
}

const TEST_FAIL_CREATE: i32 = 2;
static TEST_LOCK: Mutex<()> = Mutex::new(());
static INVOCATIONS: AtomicUsize = AtomicUsize::new(0);
static OWN_RESULT_FREES: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn record_invocation() {
    INVOCATIONS.fetch_add(1, Ordering::SeqCst);
}

unsafe extern "C" fn counting_result_free(pointer: *mut c_void) {
    OWN_RESULT_FREES.fetch_add(1, Ordering::SeqCst);
    unsafe { libc::free(pointer) };
}

fn runtime_symbols() -> HashMap<String, u64> {
    HashMap::from([
        (
            "glyph_thread_scope_create".into(),
            glyph_thread_scope_create as *const () as usize as u64,
        ),
        (
            "glyph_thread_scope_spawn".into(),
            glyph_thread_scope_spawn as *const () as usize as u64,
        ),
        (
            "glyph_thread_scope_spawn_result".into(),
            glyph_thread_scope_spawn_result as *const () as usize as u64,
        ),
        (
            "glyph_thread_scope_join".into(),
            glyph_thread_scope_join as *const () as usize as u64,
        ),
        (
            "glyph_thread_scope_join_result".into(),
            glyph_thread_scope_join_result as *const () as usize as u64,
        ),
        (
            "glyph_thread_scope_join_all".into(),
            glyph_thread_scope_join_all as *const () as usize as u64,
        ),
        (
            "glyph_thread_scope_drain_or_abort".into(),
            glyph_thread_scope_drain_or_abort as *const () as usize as u64,
        ),
        (
            "test_record_scoped_invocation".into(),
            record_invocation as *const () as usize as u64,
        ),
    ])
}

fn local(ty: Type, name: &str) -> Local {
    Local {
        name: Some(name.into()),
        ty: Some(ty),
        mutable: false,
        skip_drop: true,
    }
}

fn unit_worker(name: &str, records: bool) -> MirFunction {
    let mut locals = vec![];
    let mut insts = vec![];
    if records {
        locals.push(local(Type::Void, "recorded"));
        insts.push(MirInst::Assign {
            local: LocalId(0),
            value: Rvalue::Call {
                name: "test_record_scoped_invocation".into(),
                args: vec![],
            },
        });
    }
    insts.push(MirInst::Return(None));
    MirFunction {
        name: name.into(),
        ret_type: Some(Type::Void),
        params: vec![],
        locals,
        blocks: vec![MirBlock { insts }],
    }
}

fn record_extern() -> MirExternFunction {
    MirExternFunction {
        name: "test_record_scoped_invocation".into(),
        ret_type: Some(Type::Void),
        params: vec![],
        abi: Some("C".into()),
        link_name: None,
    }
}

fn unjoined_main(worker: &str) -> MirFunction {
    let signature = scoped_task_type(BorrowedCallableKind::Fn, Type::Void);
    MirFunction {
        name: "main".into(),
        ret_type: Some(Type::I32),
        params: vec![],
        locals: vec![
            local(private_thread_scope_type(), "scope"),
            local(signature.clone(), "task"),
            local(private_scoped_thread_handle_type(Type::Void), "child"),
            local(Type::I32, "status"),
        ],
        blocks: vec![MirBlock {
            insts: vec![
                MirInst::Assign {
                    local: LocalId(3),
                    value: Rvalue::ThreadScopeCreate {
                        out_scope: LocalId(0),
                    },
                },
                MirInst::Assign {
                    local: LocalId(1),
                    value: Rvalue::FunctionRef {
                        name: worker.into(),
                        signature,
                    },
                },
                MirInst::Assign {
                    local: LocalId(3),
                    value: Rvalue::ScopedThreadSpawnUnit {
                        scope: LocalId(0),
                        task: LocalId(1),
                        out_handle: LocalId(2),
                    },
                },
                MirInst::DropScopedThreadHandle(LocalId(2)),
                MirInst::DropThreadScope(LocalId(0)),
                MirInst::Return(Some(MirValue::Local(LocalId(3)))),
            ],
        }],
    }
}

#[test]
fn jit_scope_cleanup_joins_unjoined_child_and_never_emits_detach() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    INVOCATIONS.store(0, Ordering::SeqCst);
    let module = MirModule {
        functions: vec![unit_worker("worker", true), unjoined_main("worker")],
        extern_functions: vec![record_extern()],
        ..MirModule::default()
    };
    let mut context = CodegenContext::new("scoped_join_all").unwrap();
    context.codegen_module(&module).unwrap();
    assert_eq!(
        context
            .jit_execute_i32_with_symbols("main", &runtime_symbols())
            .unwrap(),
        0
    );
    assert_eq!(INVOCATIONS.load(Ordering::SeqCst), 1);
    let ir = context.dump_ir();
    assert!(ir.contains("@glyph_thread_scope_join_all"));
    assert!(!ir.contains("@glyph_thread_detach"));
    assert!(!ir.contains("store zeroinitializer, ptr %task"));
}

#[test]
fn callback_body_drains_before_returning_to_the_scope_owner() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    INVOCATIONS.store(0, Ordering::SeqCst);
    let signature = scoped_task_type(BorrowedCallableKind::Fn, Type::Void);
    let body = MirFunction {
        name: "scope_body".into(),
        ret_type: Some(Type::I32),
        params: vec![LocalId(0)],
        locals: vec![
            local(glyph_core::thread::canonical_thread_scope_type(), "scope"),
            local(signature.clone(), "task"),
            local(private_scoped_thread_handle_type(Type::Void), "child"),
            local(Type::I32, "status"),
        ],
        blocks: vec![MirBlock {
            insts: vec![
                MirInst::Assign {
                    local: LocalId(1),
                    value: Rvalue::FunctionRef {
                        name: "worker".into(),
                        signature,
                    },
                },
                MirInst::Assign {
                    local: LocalId(3),
                    value: Rvalue::ScopedThreadSpawnUnit {
                        scope: LocalId(0),
                        task: LocalId(1),
                        out_handle: LocalId(2),
                    },
                },
                MirInst::DropScopedThreadHandle(LocalId(2)),
                MirInst::DrainThreadScope(LocalId(0)),
                MirInst::Return(Some(MirValue::Local(LocalId(3)))),
            ],
        }],
    };
    let main = MirFunction {
        name: "main".into(),
        ret_type: Some(Type::I32),
        params: vec![],
        locals: vec![
            local(private_thread_scope_type(), "owner"),
            local(glyph_core::thread::canonical_thread_scope_type(), "view"),
            local(Type::I32, "status"),
        ],
        blocks: vec![MirBlock {
            insts: vec![
                MirInst::Assign {
                    local: LocalId(2),
                    value: Rvalue::ThreadScopeCreate {
                        out_scope: LocalId(0),
                    },
                },
                MirInst::Assign {
                    local: LocalId(1),
                    value: Rvalue::ThreadScopeFromRaw { raw: LocalId(0) },
                },
                MirInst::Assign {
                    local: LocalId(2),
                    value: Rvalue::Call {
                        name: "scope_body".into(),
                        args: vec![MirValue::Local(LocalId(1))],
                    },
                },
                MirInst::DropThreadScope(LocalId(0)),
                MirInst::Return(Some(MirValue::Local(LocalId(2)))),
            ],
        }],
    };
    let module = MirModule {
        functions: vec![unit_worker("worker", true), body, main],
        extern_functions: vec![record_extern()],
        ..MirModule::default()
    };
    let mut context = CodegenContext::new("scoped_callback_drain").unwrap();
    context.codegen_module(&module).unwrap();
    assert_eq!(
        context
            .jit_execute_i32_with_symbols("main", &runtime_symbols())
            .unwrap(),
        0
    );
    assert_eq!(INVOCATIONS.load(Ordering::SeqCst), 1);
    let ir = context.dump_ir();
    assert!(ir.contains("@glyph_thread_scope_drain_or_abort"));
    assert!(!ir.contains("@glyph_thread_detach"));
}

#[test]
fn jit_spawn_failure_keeps_borrowed_carrier_owned_by_the_scope_frame() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let module = MirModule {
        functions: vec![unit_worker("worker", false), unjoined_main("worker")],
        ..MirModule::default()
    };
    assert_eq!(
        unsafe { glyph_thread_test_fail_next(TEST_FAIL_CREATE, libc::EAGAIN) },
        0
    );
    let mut context = CodegenContext::new("scoped_spawn_failure").unwrap();
    context.codegen_module(&module).unwrap();
    assert_eq!(
        context
            .jit_execute_i32_with_symbols("main", &runtime_symbols())
            .unwrap(),
        -libc::EAGAIN
    );
    let ir = context.dump_ir();
    assert!(ir.contains("@glyph_thread_scope_spawn"));
    assert!(!ir.contains("@glyph_thread_detach"));
    assert!(!ir.contains("store zeroinitializer, ptr %task"));
}

#[test]
fn jit_explicit_typed_join_moves_result_then_scope_exits_empty() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let signature = scoped_task_type(BorrowedCallableKind::Fn, Type::I32);
    let module = MirModule {
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
                locals: vec![
                    local(private_thread_scope_type(), "scope"),
                    local(signature.clone(), "task"),
                    local(private_scoped_thread_handle_type(Type::I32), "child"),
                    local(Type::I32, "status"),
                    local(Type::I32, "result"),
                ],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(3),
                            value: Rvalue::ThreadScopeCreate {
                                out_scope: LocalId(0),
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(1),
                            value: Rvalue::FunctionRef {
                                name: "answer".into(),
                                signature,
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(3),
                            value: Rvalue::ScopedThreadSpawnResult {
                                scope: LocalId(0),
                                task: LocalId(1),
                                out_handle: LocalId(2),
                                result_type: Type::I32,
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(3),
                            value: Rvalue::ScopedThreadJoinResult {
                                handle: LocalId(2),
                                out_result: LocalId(4),
                                result_type: Type::I32,
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(3),
                            value: Rvalue::ThreadScopeExit { scope: LocalId(0) },
                        },
                        MirInst::DropThreadScope(LocalId(0)),
                        MirInst::Return(Some(MirValue::Local(LocalId(4)))),
                    ],
                }],
            },
        ],
        ..MirModule::default()
    };
    let mut context = CodegenContext::new("scoped_typed_join").unwrap();
    context.codegen_module(&module).unwrap();
    assert_eq!(
        context
            .jit_execute_i32_with_symbols("main", &runtime_symbols())
            .unwrap(),
        42
    );
    let ir = context.dump_ir();
    assert!(ir.contains("@glyph_thread_scope_spawn_result"));
    assert!(ir.contains("@glyph_thread_scope_join_result"));
}

fn owned_result_module(explicit_join: bool) -> MirModule {
    let owned = Type::Own(Box::new(Type::I32));
    let signature = scoped_task_type(BorrowedCallableKind::Fn, owned.clone());
    let mut main_locals = vec![
        local(private_thread_scope_type(), "scope"),
        local(signature.clone(), "task"),
        local(private_scoped_thread_handle_type(owned.clone()), "child"),
        local(Type::I32, "status"),
    ];
    let mut main_insts = vec![
        MirInst::Assign {
            local: LocalId(3),
            value: Rvalue::ThreadScopeCreate {
                out_scope: LocalId(0),
            },
        },
        MirInst::Assign {
            local: LocalId(1),
            value: Rvalue::FunctionRef {
                name: "owned_answer".into(),
                signature,
            },
        },
        MirInst::Assign {
            local: LocalId(3),
            value: Rvalue::ScopedThreadSpawnResult {
                scope: LocalId(0),
                task: LocalId(1),
                out_handle: LocalId(2),
                result_type: owned.clone(),
            },
        },
    ];
    if explicit_join {
        main_locals.push(local(owned.clone(), "result"));
        main_insts.push(MirInst::Assign {
            local: LocalId(3),
            value: Rvalue::ScopedThreadJoinResult {
                handle: LocalId(2),
                out_result: LocalId(4),
                result_type: owned.clone(),
            },
        });
    } else {
        main_insts.push(MirInst::DropScopedThreadHandle(LocalId(2)));
    }
    main_insts.extend([
        MirInst::Assign {
            local: LocalId(3),
            value: Rvalue::ThreadScopeExit { scope: LocalId(0) },
        },
        MirInst::DropThreadScope(LocalId(0)),
    ]);
    if explicit_join {
        main_insts.push(MirInst::Drop(LocalId(4)));
    }
    main_insts.push(MirInst::Return(Some(MirValue::Local(LocalId(3)))));

    MirModule {
        functions: vec![
            MirFunction {
                name: "owned_answer".into(),
                ret_type: Some(owned.clone()),
                params: vec![],
                locals: vec![local(owned, "answer")],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(0),
                            value: Rvalue::OwnNew {
                                value: MirValue::Int(42),
                                elem_type: Type::I32,
                            },
                        },
                        MirInst::Return(Some(MirValue::Local(LocalId(0)))),
                    ],
                }],
            },
            MirFunction {
                name: "main".into(),
                ret_type: Some(Type::I32),
                params: vec![],
                locals: main_locals,
                blocks: vec![MirBlock { insts: main_insts }],
            },
        ],
        ..MirModule::default()
    }
}

#[test]
fn scoped_owned_result_is_dropped_once_after_join_or_unclaimed_scope_exit() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    for (name, explicit_join) in [
        ("scoped_owned_join", true),
        ("scoped_owned_unclaimed", false),
    ] {
        OWN_RESULT_FREES.store(0, Ordering::SeqCst);
        let mut context = CodegenContext::new(name).unwrap();
        context
            .codegen_module(&owned_result_module(explicit_join))
            .unwrap();
        let mut symbols = runtime_symbols();
        symbols.insert(
            "free".into(),
            counting_result_free as *const () as usize as u64,
        );
        assert_eq!(
            context
                .jit_execute_i32_with_symbols("main", &symbols)
                .unwrap(),
            0,
            "{name}"
        );
        assert_eq!(
            OWN_RESULT_FREES.load(Ordering::SeqCst),
            1,
            "{name}: owned payload must be freed exactly once"
        );
        assert!(
            context
                .dump_ir()
                .contains("__glyph_thread_result_drop_own_i32"),
            "{name}: missing generated result drop thunk"
        );
    }
}

#[test]
fn aot_scope_cleanup_links_and_joins_before_main_returns() {
    let module = MirModule {
        functions: vec![unit_worker("worker", false), unjoined_main("worker")],
        ..MirModule::default()
    };
    let unique = format!("glyph_scoped_aot_{}", std::process::id());
    let object = std::env::temp_dir().join(format!("{unique}.o"));
    let executable = std::env::temp_dir().join(unique);
    let mut context = CodegenContext::new("scoped_aot").unwrap();
    context.codegen_module(&module).unwrap();
    context.emit_object_file(&object).unwrap();
    Linker::new()
        .link(&LinkerOptions {
            output_path: executable.clone(),
            object_files: vec![object.clone()],
            link_libs: vec![],
            link_search_paths: vec![],
            runtime_lib_path: Linker::get_runtime_lib_path(),
        })
        .unwrap();
    assert_eq!(
        std::process::Command::new(&executable)
            .status()
            .unwrap()
            .code(),
        Some(0)
    );
    std::fs::remove_file(object).unwrap();
    std::fs::remove_file(executable).unwrap();
}

#[test]
fn forged_owned_task_is_rejected_before_runtime_ir_is_emitted() {
    let owned = Type::Function {
        params: vec![],
        ret: Box::new(Type::Void),
    };
    let module = MirModule {
        functions: vec![
            unit_worker("worker", false),
            MirFunction {
                name: "main".into(),
                ret_type: Some(Type::I32),
                params: vec![],
                locals: vec![
                    local(private_thread_scope_type(), "scope"),
                    local(owned.clone(), "task"),
                    local(private_scoped_thread_handle_type(Type::Void), "child"),
                    local(Type::I32, "status"),
                ],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(1),
                            value: Rvalue::FunctionRef {
                                name: "worker".into(),
                                signature: owned,
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(3),
                            value: Rvalue::ScopedThreadSpawnUnit {
                                scope: LocalId(0),
                                task: LocalId(1),
                                out_handle: LocalId(2),
                            },
                        },
                        MirInst::Return(Some(MirValue::Int(0))),
                    ],
                }],
            },
        ],
        ..MirModule::default()
    };
    let mut context = CodegenContext::new("scoped_forged_owned").unwrap();
    let error = context.codegen_module(&module).unwrap_err();
    assert!(error.to_string().contains("borrowed Fn/FnMut"));
    assert!(!context.dump_ir().contains("@glyph_thread_scope_spawn"));
}

#[test]
fn forged_result_relabel_is_rejected_before_typed_join() {
    let module = MirModule {
        functions: vec![MirFunction {
            name: "main".into(),
            ret_type: Some(Type::I32),
            params: vec![],
            locals: vec![
                local(private_thread_scope_type(), "scope"),
                local(
                    private_scoped_thread_handle_type(Type::String),
                    "string_child",
                ),
                local(Type::I32, "out"),
                local(Type::I32, "status"),
            ],
            blocks: vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(3),
                        value: Rvalue::ScopedThreadJoinResult {
                            handle: LocalId(1),
                            out_result: LocalId(2),
                            result_type: Type::I32,
                        },
                    },
                    MirInst::Return(Some(MirValue::Int(0))),
                ],
            }],
        }],
        ..MirModule::default()
    };
    let mut context = CodegenContext::new("scoped_forged_result").unwrap();
    let error = context.codegen_module(&module).unwrap_err();
    assert!(error.to_string().contains("wrong result provenance"));
    assert!(
        !context
            .dump_ir()
            .contains("@glyph_thread_scope_join_result")
    );
}

#[test]
fn duplicate_fnmut_spawn_is_rejected_by_scoped_mir_validation() {
    let signature = scoped_task_type(BorrowedCallableKind::FnMut, Type::Void);
    let module = MirModule {
        functions: vec![MirFunction {
            name: "main".into(),
            ret_type: Some(Type::I32),
            params: vec![],
            locals: vec![
                local(private_thread_scope_type(), "scope"),
                local(signature, "task"),
                local(private_scoped_thread_handle_type(Type::Void), "first"),
                local(private_scoped_thread_handle_type(Type::Void), "second"),
                local(Type::I32, "status"),
            ],
            blocks: vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(4),
                        value: Rvalue::ThreadScopeCreate {
                            out_scope: LocalId(0),
                        },
                    },
                    MirInst::Assign {
                        local: LocalId(4),
                        value: Rvalue::ScopedThreadSpawnUnit {
                            scope: LocalId(0),
                            task: LocalId(1),
                            out_handle: LocalId(2),
                        },
                    },
                    MirInst::Assign {
                        local: LocalId(4),
                        value: Rvalue::ScopedThreadSpawnUnit {
                            scope: LocalId(0),
                            task: LocalId(1),
                            out_handle: LocalId(3),
                        },
                    },
                    MirInst::DropThreadScope(LocalId(0)),
                    MirInst::Return(Some(MirValue::Int(0))),
                ],
            }],
        }],
        ..MirModule::default()
    };
    let mut context = CodegenContext::new("scoped_duplicate_fnmut").unwrap();
    let error = context.codegen_module(&module).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("more than one live scoped spawn")
    );
    assert!(!context.dump_ir().contains("@glyph_thread_scope_spawn"));
}

#[test]
fn return_with_an_active_scope_is_rejected_before_codegen() {
    let module = MirModule {
        functions: vec![MirFunction {
            name: "main".into(),
            ret_type: Some(Type::I32),
            params: vec![],
            locals: vec![
                local(private_thread_scope_type(), "scope"),
                local(Type::I32, "status"),
            ],
            blocks: vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(1),
                        value: Rvalue::ThreadScopeCreate {
                            out_scope: LocalId(0),
                        },
                    },
                    MirInst::Return(Some(MirValue::Int(0))),
                ],
            }],
        }],
        ..MirModule::default()
    };
    let mut context = CodegenContext::new("scoped_missing_cleanup").unwrap();
    let error = context.codegen_module(&module).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cleanup is missing before return")
    );
    assert!(!context.dump_ir().contains("@glyph_thread_scope_create"));
}

#[test]
fn forged_borrowed_callable_storage_is_rejected_before_codegen() {
    let signature = scoped_task_type(BorrowedCallableKind::Fn, Type::Void);
    let module = MirModule {
        struct_types: HashMap::from([(
            "Holder".into(),
            StructType {
                name: "Holder".into(),
                fields: vec![("callback".into(), signature.clone())],
            },
        )]),
        functions: vec![
            unit_worker("worker", false),
            MirFunction {
                name: "main".into(),
                ret_type: Some(Type::I32),
                params: vec![],
                locals: vec![
                    local(signature.clone(), "callback"),
                    local(Type::Named("Holder".into()), "holder"),
                ],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(0),
                            value: Rvalue::FunctionRef {
                                name: "worker".into(),
                                signature,
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(1),
                            value: Rvalue::StructLit {
                                struct_name: "Holder".into(),
                                field_values: vec![(
                                    "callback".into(),
                                    MirValue::Local(LocalId(0)),
                                )],
                            },
                        },
                        MirInst::Return(Some(MirValue::Int(0))),
                    ],
                }],
            },
        ],
        ..MirModule::default()
    };
    let mut context = CodegenContext::new("borrowed_storage_forgery").unwrap();
    let error = context.codegen_module(&module).unwrap_err();
    assert!(error.to_string().contains("cannot be stored"));
}

#[test]
fn forged_borrowed_callable_return_is_rejected_before_codegen() {
    let signature = scoped_task_type(BorrowedCallableKind::Fn, Type::Void);
    let module = MirModule {
        functions: vec![
            unit_worker("worker", false),
            MirFunction {
                name: "leak".into(),
                ret_type: Some(signature.clone()),
                params: vec![],
                locals: vec![local(signature.clone(), "callback")],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(0),
                            value: Rvalue::FunctionRef {
                                name: "worker".into(),
                                signature,
                            },
                        },
                        MirInst::Return(Some(MirValue::Local(LocalId(0)))),
                    ],
                }],
            },
        ],
        ..MirModule::default()
    };
    let mut context = CodegenContext::new("borrowed_return_forgery").unwrap();
    let error = context.codegen_module(&module).unwrap_err();
    assert!(error.to_string().contains("cannot escape through return"));
}
