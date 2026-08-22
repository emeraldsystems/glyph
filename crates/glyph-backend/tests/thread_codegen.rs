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
use glyph_core::thread::{private_unit_handle_type, task_type, unit_task_type};
use glyph_core::types::{StructType, Type};

#[repr(C)]
struct GlyphThread {
    _private: [u8; 0],
}

type ThreadEntry = unsafe extern "C" fn(*mut c_void);
type DropUnstarted = unsafe extern "C" fn(*mut c_void);
type ThreadResultEntry = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void);
type DropResult = unsafe extern "C" fn(*mut c_void);

#[link(name = "glyph_runtime", kind = "static")]
unsafe extern "C" {
    fn glyph_thread_spawn(
        out: *mut *mut GlyphThread,
        entry: Option<ThreadEntry>,
        env: *mut c_void,
        drop_unstarted: Option<DropUnstarted>,
    ) -> i32;
    fn glyph_thread_spawn_result(
        out: *mut *mut GlyphThread,
        entry: Option<ThreadResultEntry>,
        invoke: *mut c_void,
        env: *mut c_void,
        drop_unstarted: Option<DropUnstarted>,
        result_size: usize,
        drop_result: Option<DropResult>,
    ) -> i32;
    fn glyph_thread_join(handle: *mut *mut GlyphThread) -> i32;
    fn glyph_thread_join_result(handle: *mut *mut GlyphThread, out_result: *mut c_void) -> i32;
    fn glyph_thread_detach(handle: *mut *mut GlyphThread) -> i32;
    fn glyph_thread_test_fail_next(operation: i32, error_code: i32) -> i32;
}

const TEST_FAIL_CREATE: i32 = 2;
static TEST_LOCK: Mutex<()> = Mutex::new(());
static INVOCATIONS: AtomicUsize = AtomicUsize::new(0);
static DETACH_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
static RESULT_FREES: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn record_invocation() {
    INVOCATIONS.fetch_add(1, Ordering::SeqCst);
}

unsafe extern "C" fn fake_handle() -> *mut c_void {
    // The fake detach callback never dereferences this sentinel.
    std::ptr::dangling_mut::<u8>().cast()
}

unsafe extern "C" fn transient_fake_detach(handle: *mut *mut c_void) -> i32 {
    let attempt = DETACH_ATTEMPTS.fetch_add(1, Ordering::SeqCst);
    if attempt == 0 {
        -libc::EBUSY
    } else {
        unsafe { *handle = std::ptr::null_mut() };
        0
    }
}

unsafe extern "C" fn counting_result_free(pointer: *mut c_void) {
    RESULT_FREES.fetch_add(1, Ordering::SeqCst);
    unsafe { libc::free(pointer) };
}

fn runtime_symbols() -> HashMap<String, u64> {
    HashMap::from([
        (
            "glyph_thread_spawn".into(),
            glyph_thread_spawn as *const () as usize as u64,
        ),
        (
            "glyph_thread_join".into(),
            glyph_thread_join as *const () as usize as u64,
        ),
        (
            "glyph_thread_spawn_result".into(),
            glyph_thread_spawn_result as *const () as usize as u64,
        ),
        (
            "glyph_thread_join_result".into(),
            glyph_thread_join_result as *const () as usize as u64,
        ),
        (
            "glyph_thread_detach".into(),
            glyph_thread_detach as *const () as usize as u64,
        ),
        (
            "test_record_invocation".into(),
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

fn worker(name: &str, records_invocation: bool) -> MirFunction {
    let mut locals = Vec::new();
    let mut insts = Vec::new();
    if records_invocation {
        locals.push(local(Type::Void, "recorded"));
        insts.push(MirInst::Assign {
            local: LocalId(0),
            value: Rvalue::Call {
                name: "test_record_invocation".into(),
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
        name: "test_record_invocation".into(),
        ret_type: Some(Type::Void),
        params: vec![],
        abi: Some("C".into()),
        link_name: None,
    }
}

fn spawn_join_main(worker_name: &str) -> MirFunction {
    MirFunction {
        name: "main".into(),
        ret_type: Some(Type::I32),
        params: vec![],
        locals: vec![
            local(unit_task_type(), "task"),
            local(private_unit_handle_type(), "handle"),
            local(Type::I32, "status"),
        ],
        blocks: vec![MirBlock {
            insts: vec![
                MirInst::Assign {
                    local: LocalId(0),
                    value: Rvalue::FunctionRef {
                        name: worker_name.into(),
                        signature: unit_task_type(),
                    },
                },
                MirInst::Assign {
                    local: LocalId(2),
                    value: Rvalue::ThreadSpawnUnit {
                        task: LocalId(0),
                        out_handle: LocalId(1),
                    },
                },
                MirInst::Assign {
                    local: LocalId(2),
                    value: Rvalue::ThreadJoinUnit { handle: LocalId(1) },
                },
                MirInst::DropThreadHandle(LocalId(1)),
                MirInst::Return(Some(MirValue::Local(LocalId(2)))),
            ],
        }],
    }
}

#[test]
fn unit_task_spawns_and_joins_through_the_callable_carrier() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    INVOCATIONS.store(0, Ordering::SeqCst);
    let module = MirModule {
        functions: vec![worker("worker", true), spawn_join_main("worker")],
        extern_functions: vec![record_extern()],
        ..MirModule::default()
    };
    let mut context = CodegenContext::new("thread_spawn_join").unwrap();
    context.codegen_module(&module).unwrap();

    assert_eq!(
        context
            .jit_execute_i32_with_symbols("main", &runtime_symbols())
            .unwrap(),
        0
    );
    assert_eq!(INVOCATIONS.load(Ordering::SeqCst), 1);
    let ir = context.dump_ir();
    assert!(ir.contains("call i32 @glyph_thread_spawn"));
    assert!(ir.contains("call i32 @glyph_thread_join"));
    assert!(ir.contains("thread.drop.isnull"));
}

#[test]
fn many_unit_tasks_run_and_join_without_timing_sleeps() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    const COUNT: usize = 32;
    INVOCATIONS.store(0, Ordering::SeqCst);
    let mut locals = Vec::new();
    let mut insts = Vec::new();
    for index in 0..COUNT {
        let task = LocalId(locals.len() as u32);
        locals.push(local(unit_task_type(), &format!("task_{index}")));
        let handle = LocalId(locals.len() as u32);
        locals.push(local(
            private_unit_handle_type(),
            &format!("handle_{index}"),
        ));
        let status = LocalId(locals.len() as u32);
        locals.push(local(Type::I32, &format!("status_{index}")));
        insts.push(MirInst::Assign {
            local: task,
            value: Rvalue::FunctionRef {
                name: "worker".into(),
                signature: unit_task_type(),
            },
        });
        insts.push(MirInst::Assign {
            local: status,
            value: Rvalue::ThreadSpawnUnit {
                task,
                out_handle: handle,
            },
        });
    }
    for index in 0..COUNT {
        let handle = LocalId((index * 3 + 1) as u32);
        let status = LocalId((index * 3 + 2) as u32);
        insts.push(MirInst::Assign {
            local: status,
            value: Rvalue::ThreadJoinUnit { handle },
        });
        insts.push(MirInst::DropThreadHandle(handle));
    }
    let last_status = LocalId(((COUNT - 1) * 3 + 2) as u32);
    insts.push(MirInst::Return(Some(MirValue::Local(last_status))));
    let module = MirModule {
        functions: vec![
            worker("worker", true),
            MirFunction {
                name: "main".into(),
                ret_type: Some(Type::I32),
                params: vec![],
                locals,
                blocks: vec![MirBlock { insts }],
            },
        ],
        extern_functions: vec![record_extern()],
        ..MirModule::default()
    };
    let mut context = CodegenContext::new("thread_many").unwrap();
    context.codegen_module(&module).unwrap();

    assert_eq!(
        context
            .jit_execute_i32_with_symbols("main", &runtime_symbols())
            .unwrap(),
        0
    );
    assert_eq!(INVOCATIONS.load(Ordering::SeqCst), COUNT);
}

#[test]
fn dropping_a_live_handle_emits_nonblocking_detach_cleanup() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let mut main = spawn_join_main("worker");
    main.blocks[0].insts = vec![
        main.blocks[0].insts[0].clone(),
        main.blocks[0].insts[1].clone(),
        MirInst::DropThreadHandle(LocalId(1)),
        MirInst::Return(Some(MirValue::Local(LocalId(2)))),
    ];
    let module = MirModule {
        functions: vec![worker("worker", true), main],
        extern_functions: vec![record_extern()],
        ..MirModule::default()
    };
    let mut context = CodegenContext::new("thread_drop_detaches").unwrap();
    context.codegen_module(&module).unwrap();
    let ir = context.dump_ir();
    let null_check = ir.find("thread.drop.isnull").unwrap();
    let detach = ir.find("call i32 @glyph_thread_detach").unwrap();
    assert!(null_check < detach);
    assert!(!ir.contains("call i32 @glyph_thread_join"));
}

#[test]
fn creation_failure_consumes_the_callable_and_publishes_no_handle() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let module = MirModule {
        functions: vec![worker("worker", false), spawn_join_main("worker")],
        ..MirModule::default()
    };
    let mut context = CodegenContext::new("thread_create_failure").unwrap();
    context.codegen_module(&module).unwrap();
    assert_eq!(
        unsafe { glyph_thread_test_fail_next(TEST_FAIL_CREATE, libc::EAGAIN) },
        0
    );

    // `main` attempts join after the failed spawn. A null out-handle yields
    // EINVAL; importantly, cleanup sees null and never owns the task again.
    assert_eq!(
        context
            .jit_execute_i32_with_symbols("main", &runtime_symbols())
            .unwrap(),
        -libc::EINVAL
    );
    let ir = context.dump_ir();
    let clear = ir.find("store { ptr, ptr, ptr } zeroinitializer").unwrap();
    let spawn = ir.find("call i32 @glyph_thread_spawn").unwrap();
    assert!(
        clear < spawn,
        "task carrier must clear before runtime transfer"
    );
}

#[test]
fn join_error_path_keeps_nonblocking_drop_cleanup_in_generated_code() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let module = MirModule {
        functions: vec![worker("worker", false), spawn_join_main("worker")],
        ..MirModule::default()
    };
    let mut context = CodegenContext::new("thread_retry_cleanup").unwrap();
    context.codegen_module(&module).unwrap();
    let ir = context.dump_ir();
    let join = ir.find("call i32 @glyph_thread_join").unwrap();
    let cleanup = ir.find("call i32 @glyph_thread_detach").unwrap();
    assert!(join < cleanup);
    assert!(ir.contains("thread.drop.isnull"));
    assert!(ir.contains("thread.drop.retry"));
    assert!(ir.contains("thread.drop.fatal"));
    assert!(ir.contains("call void @abort"));
}

#[test]
fn drop_retries_one_transient_detach_failure_without_losing_the_handle() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    DETACH_ATTEMPTS.store(0, Ordering::SeqCst);
    let module = MirModule {
        functions: vec![MirFunction {
            name: "main".into(),
            ret_type: Some(Type::I32),
            params: vec![],
            locals: vec![
                local(private_unit_handle_type(), "handle"),
                local(Type::I32, "status"),
            ],
            blocks: vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(0),
                        value: Rvalue::Call {
                            name: "test_fake_handle".into(),
                            args: vec![],
                        },
                    },
                    MirInst::DropThreadHandle(LocalId(0)),
                    MirInst::Assign {
                        local: LocalId(1),
                        value: Rvalue::ConstInt(0),
                    },
                    MirInst::Return(Some(MirValue::Local(LocalId(1)))),
                ],
            }],
        }],
        extern_functions: vec![MirExternFunction {
            name: "test_fake_handle".into(),
            ret_type: Some(private_unit_handle_type()),
            params: vec![],
            abi: Some("C".into()),
            link_name: None,
        }],
        ..MirModule::default()
    };
    let mut context = CodegenContext::new("thread_drop_retry").unwrap();
    context.codegen_module(&module).unwrap();
    let mut symbols = HashMap::from([
        (
            "test_fake_handle".into(),
            fake_handle as *const () as usize as u64,
        ),
        (
            "glyph_thread_detach".into(),
            transient_fake_detach as *const () as usize as u64,
        ),
    ]);
    // `abort` is not reached, but explicitly resolving it makes this test
    // independent of execution-engine dynamic-symbol lookup behavior.
    symbols.insert("abort".into(), libc::abort as *const () as usize as u64);

    assert_eq!(
        context
            .jit_execute_i32_with_symbols("main", &symbols)
            .unwrap(),
        0
    );
    assert_eq!(DETACH_ATTEMPTS.load(Ordering::SeqCst), 2);
}

#[test]
fn backend_rejects_non_unit_tasks_and_forged_handle_storage() {
    let bad_task = Type::Function {
        params: vec![Type::I32],
        ret: Box::new(Type::Void),
    };
    for (task_ty, handle_ty, expected) in [
        (
            bad_task,
            private_unit_handle_type(),
            "task must be FnOnce() -> ()",
        ),
        (unit_task_type(), Type::I64, "private RawPtr<I8>"),
    ] {
        let module = MirModule {
            functions: vec![MirFunction {
                name: "main".into(),
                ret_type: Some(Type::I32),
                params: vec![],
                locals: vec![
                    local(task_ty, "task"),
                    local(handle_ty, "forged"),
                    local(Type::I32, "status"),
                ],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(2),
                            value: Rvalue::ThreadSpawnUnit {
                                task: LocalId(0),
                                out_handle: LocalId(1),
                            },
                        },
                        MirInst::Return(Some(MirValue::Local(LocalId(2)))),
                    ],
                }],
            }],
            ..MirModule::default()
        };
        let mut context = CodegenContext::new("thread_bad_contract").unwrap();
        let error = context.codegen_module(&module).unwrap_err();
        assert!(
            error.to_string().contains(expected),
            "unexpected diagnostic: {error:#}"
        );
    }
}

#[test]
fn typed_scalar_task_spawns_joins_and_moves_its_result() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let signature = task_type(Type::I32);
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
                    local(signature.clone(), "task"),
                    local(private_unit_handle_type(), "handle"),
                    local(Type::I32, "status"),
                    local(Type::I32, "result"),
                ],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(0),
                            value: Rvalue::FunctionRef {
                                name: "answer".into(),
                                signature: signature.clone(),
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(2),
                            value: Rvalue::ThreadSpawnResult {
                                task: LocalId(0),
                                out_handle: LocalId(1),
                                result_type: Type::I32,
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(2),
                            value: Rvalue::ThreadJoinResult {
                                handle: LocalId(1),
                                out_result: LocalId(3),
                                result_type: Type::I32,
                            },
                        },
                        MirInst::DropThreadHandle(LocalId(1)),
                        MirInst::Return(Some(MirValue::Local(LocalId(3)))),
                    ],
                }],
            },
        ],
        ..MirModule::default()
    };
    let mut context = CodegenContext::new("thread_typed_scalar").unwrap();
    context.codegen_module(&module).unwrap();

    assert_eq!(
        context
            .jit_execute_i32_with_symbols("main", &runtime_symbols())
            .unwrap(),
        42
    );
    let ir = context.dump_ir();
    assert!(ir.contains("@glyph_thread_spawn_result"));
    assert!(ir.contains("@glyph_thread_join_result"));
    assert!(ir.contains("__glyph_thread_result_entry_i32"));
    assert!(ir.contains("__glyph_thread_result_drop_i32"));

    let unique = format!("glyph_thread_typed_aot_{}", std::process::id());
    let object = std::env::temp_dir().join(format!("{unique}.o"));
    let executable = std::env::temp_dir().join(unique);
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
    let status = std::process::Command::new(&executable).status().unwrap();
    assert_eq!(status.code(), Some(42));
    let _ = std::fs::remove_file(object);
    let _ = std::fs::remove_file(executable);
}

#[test]
fn typed_large_result_uses_callable_sret_and_preserves_layout() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let big = Type::Named("BigThreadResult".into());
    let signature = task_type(big.clone());
    let mut struct_types = HashMap::new();
    struct_types.insert(
        "BigThreadResult".into(),
        StructType {
            name: "BigThreadResult".into(),
            fields: (0..8)
                .map(|index| (format!("f{index}"), Type::I32))
                .collect(),
        },
    );
    let module = MirModule {
        struct_types,
        functions: vec![
            MirFunction {
                name: "large_answer".into(),
                ret_type: Some(big.clone()),
                params: vec![],
                locals: vec![local(big.clone(), "result")],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(0),
                            value: Rvalue::StructLit {
                                struct_name: "BigThreadResult".into(),
                                field_values: (0..8)
                                    .map(|index| (format!("f{index}"), MirValue::Int(index + 40)))
                                    .collect(),
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
                locals: vec![
                    local(signature.clone(), "task"),
                    local(private_unit_handle_type(), "handle"),
                    local(Type::I32, "status"),
                    local(big.clone(), "result"),
                    local(Type::I32, "field"),
                ],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(0),
                            value: Rvalue::FunctionRef {
                                name: "large_answer".into(),
                                signature: signature.clone(),
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(2),
                            value: Rvalue::ThreadSpawnResult {
                                task: LocalId(0),
                                out_handle: LocalId(1),
                                result_type: big.clone(),
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(2),
                            value: Rvalue::ThreadJoinResult {
                                handle: LocalId(1),
                                out_result: LocalId(3),
                                result_type: big.clone(),
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(4),
                            value: Rvalue::FieldAccess {
                                base: LocalId(3),
                                field_name: "f7".into(),
                                field_index: 7,
                            },
                        },
                        MirInst::DropThreadHandle(LocalId(1)),
                        MirInst::Return(Some(MirValue::Local(LocalId(4)))),
                    ],
                }],
            },
        ],
        ..MirModule::default()
    };
    let mut context = CodegenContext::new("thread_typed_sret").unwrap();
    context.codegen_module(&module).unwrap();

    assert_eq!(
        context
            .jit_execute_i32_with_symbols("main", &runtime_symbols())
            .unwrap(),
        47
    );
    let ir = context.dump_ir();
    assert!(ir.contains("sret(%BigThreadResult)"));
    assert!(ir.contains("__glyph_thread_result_entry_BigThreadResult"));
}

#[test]
fn joined_owned_result_is_destroyed_once_by_the_joiner() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    RESULT_FREES.store(0, Ordering::SeqCst);
    let owned = Type::Own(Box::new(Type::I32));
    let signature = task_type(owned.clone());
    let module = MirModule {
        functions: vec![
            MirFunction {
                name: "owned_answer".into(),
                ret_type: Some(owned.clone()),
                params: vec![],
                locals: vec![local(owned.clone(), "result")],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(0),
                            value: Rvalue::OwnNew {
                                value: MirValue::Int(77),
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
                locals: vec![
                    local(signature.clone(), "task"),
                    local(private_unit_handle_type(), "handle"),
                    local(Type::I32, "status"),
                    local(owned.clone(), "result"),
                ],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(0),
                            value: Rvalue::FunctionRef {
                                name: "owned_answer".into(),
                                signature: signature.clone(),
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(2),
                            value: Rvalue::ThreadSpawnResult {
                                task: LocalId(0),
                                out_handle: LocalId(1),
                                result_type: owned.clone(),
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(2),
                            value: Rvalue::ThreadJoinResult {
                                handle: LocalId(1),
                                out_result: LocalId(3),
                                result_type: owned,
                            },
                        },
                        MirInst::Drop(LocalId(3)),
                        MirInst::DropThreadHandle(LocalId(1)),
                        MirInst::Return(Some(MirValue::Local(LocalId(2)))),
                    ],
                }],
            },
        ],
        ..MirModule::default()
    };
    let mut context = CodegenContext::new("thread_typed_owned").unwrap();
    context.codegen_module(&module).unwrap();
    let mut symbols = runtime_symbols();
    symbols.insert(
        "free".into(),
        counting_result_free as *const () as usize as u64,
    );

    assert_eq!(
        context
            .jit_execute_i32_with_symbols("main", &symbols)
            .unwrap(),
        0
    );
    assert_eq!(RESULT_FREES.load(Ordering::SeqCst), 1);
    assert!(
        context
            .dump_ir()
            .contains("__glyph_thread_result_drop_own_i32")
    );
}

#[test]
fn backend_rejects_typed_task_and_result_slot_mismatches() {
    for (task_ty, out_ty, expected) in [
        (
            task_type(Type::I64),
            Type::I32,
            "task must be FnOnce() -> I32",
        ),
        (
            task_type(Type::I32),
            Type::I64,
            "result local has type I64, expected I32",
        ),
    ] {
        let module = MirModule {
            functions: vec![MirFunction {
                name: "main".into(),
                ret_type: Some(Type::I32),
                params: vec![],
                locals: vec![
                    local(task_ty, "task"),
                    local(private_unit_handle_type(), "handle"),
                    local(Type::I32, "status"),
                    local(out_ty, "result"),
                ],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(2),
                            value: Rvalue::ThreadSpawnResult {
                                task: LocalId(0),
                                out_handle: LocalId(1),
                                result_type: Type::I32,
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(2),
                            value: Rvalue::ThreadJoinResult {
                                handle: LocalId(1),
                                out_result: LocalId(3),
                                result_type: Type::I32,
                            },
                        },
                        MirInst::Return(Some(MirValue::Local(LocalId(2)))),
                    ],
                }],
            }],
            ..MirModule::default()
        };
        let mut context = CodegenContext::new("thread_typed_bad_contract").unwrap();
        let error = context.codegen_module(&module).unwrap_err();
        assert!(
            error.to_string().contains(expected),
            "unexpected diagnostic: {error:#}"
        );
    }
}
