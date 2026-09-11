#![cfg(all(feature = "codegen", any(target_os = "macos", target_os = "linux")))]

use glyph_backend::codegen::CodegenContext;
use glyph_core::mir::{
    Local, LocalId, MirBlock, MirExternFunction, MirFunction, MirInst, MirModule, MirValue, Rvalue,
};
use glyph_core::types::{Mutability, Type};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

#[repr(C)]
struct GlyphMutex {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn glyph_mutex_create(out: *mut *mut GlyphMutex) -> i32;
    fn glyph_mutex_lock(mutex: *mut GlyphMutex) -> i32;
    fn glyph_mutex_try_lock(mutex: *mut GlyphMutex) -> i32;
    fn glyph_mutex_unlock(mutex: *mut GlyphMutex) -> i32;
    fn glyph_mutex_destroy(mutex: *mut *mut GlyphMutex) -> i32;
}

unsafe extern "C" fn increment_i32(value: *mut i32) {
    unsafe { *value += 1 };
}

static FREE_CALLS: AtomicUsize = AtomicUsize::new(0);
static UNLOCK_CALLS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn tracked_free(pointer: *mut c_void) {
    FREE_CALLS.fetch_add(1, Ordering::SeqCst);
    unsafe { libc::free(pointer) };
}

unsafe extern "C" fn tracked_mutex_unlock(mutex: *mut GlyphMutex) -> i32 {
    UNLOCK_CALLS.fetch_add(1, Ordering::SeqCst);
    unsafe { glyph_mutex_unlock(mutex) }
}

fn runtime_symbols() -> HashMap<String, u64> {
    HashMap::from([
        (
            "glyph_mutex_create".into(),
            glyph_mutex_create as *const () as usize as u64,
        ),
        (
            "glyph_mutex_lock".into(),
            glyph_mutex_lock as *const () as usize as u64,
        ),
        (
            "glyph_mutex_try_lock".into(),
            glyph_mutex_try_lock as *const () as usize as u64,
        ),
        (
            "glyph_mutex_unlock".into(),
            glyph_mutex_unlock as *const () as usize as u64,
        ),
        (
            "glyph_mutex_destroy".into(),
            glyph_mutex_destroy as *const () as usize as u64,
        ),
        (
            "increment_i32".into(),
            increment_i32 as *const () as usize as u64,
        ),
    ])
}

fn local(ty: Type) -> Local {
    Local {
        name: None,
        ty: Some(ty),
        mutable: false,
        skip_drop: false,
    }
}

fn identity_i32() -> MirFunction {
    MirFunction {
        name: "identity_i32".into(),
        ret_type: Some(Type::I32),
        params: vec![LocalId(0)],
        locals: vec![local(Type::I32)],
        blocks: vec![MirBlock {
            insts: vec![MirInst::Return(Some(MirValue::Local(LocalId(0))))],
        }],
    }
}

fn increment_extern() -> MirExternFunction {
    MirExternFunction {
        name: "increment_i32".into(),
        ret_type: Some(Type::Void),
        params: vec![Type::Ref(Box::new(Type::I32), Mutability::Mutable)],
        abi: Some("C".into()),
        link_name: None,
    }
}

#[test]
fn lock_borrow_and_guard_drop_unlock_the_typed_payload() {
    let mutex = Type::mutex(Type::I32);
    let guard = Type::mutex_guard(Type::I32);
    let value_ref = Type::Ref(Box::new(Type::I32), Mutability::Mutable);
    let main = MirFunction {
        name: "main".into(),
        ret_type: Some(Type::I32),
        params: vec![],
        locals: vec![
            local(mutex),
            local(guard.clone()),
            local(value_ref.clone()),
            local(Type::Void),
            local(guard),
            local(Type::Bool),
            local(value_ref),
            local(Type::I32),
        ],
        blocks: vec![MirBlock {
            insts: vec![
                MirInst::Assign {
                    local: LocalId(0),
                    value: Rvalue::MutexNew {
                        value: MirValue::Int(41),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(1),
                    value: Rvalue::MutexLock {
                        base: LocalId(0),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(2),
                    value: Rvalue::MutexGuardBorrow {
                        guard: LocalId(1),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(3),
                    value: Rvalue::Call {
                        name: "increment_i32".into(),
                        args: vec![MirValue::Local(LocalId(2))],
                    },
                },
                MirInst::Drop(LocalId(1)),
                MirInst::Assign {
                    local: LocalId(4),
                    value: Rvalue::MutexTryLock {
                        base: LocalId(0),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(5),
                    value: Rvalue::MutexGuardIsAcquired {
                        guard: LocalId(4),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(6),
                    value: Rvalue::MutexGuardBorrow {
                        guard: LocalId(4),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(7),
                    value: Rvalue::Call {
                        name: "identity_i32".into(),
                        args: vec![MirValue::Local(LocalId(6))],
                    },
                },
                MirInst::Drop(LocalId(4)),
                MirInst::Drop(LocalId(0)),
                MirInst::Return(Some(MirValue::Local(LocalId(7)))),
            ],
        }],
    };
    let module = MirModule {
        functions: vec![identity_i32(), main],
        extern_functions: vec![increment_extern()],
        ..MirModule::default()
    };
    let mut context = CodegenContext::new("mutex_lock").unwrap();
    context.codegen_module(&module).unwrap();
    assert_eq!(
        context
            .jit_execute_i32_with_symbols("main", &runtime_symbols())
            .unwrap(),
        42
    );
    let ir = context.dump_ir();
    assert!(ir.contains("call i32 @glyph_mutex_lock"), "{ir}");
    assert!(ir.contains("call i32 @glyph_mutex_try_lock"), "{ir}");
    assert!(ir.contains("call i32 @glyph_mutex_unlock"), "{ir}");
    assert!(ir.contains("call i32 @glyph_mutex_destroy"), "{ir}");
    assert!(ir.contains("mutex.guard.drop.isnull"), "{ir}");
}

#[test]
fn duplicate_guard_drop_is_idempotent_and_unlocks_only_once() {
    let mutex = Type::mutex(Type::I32);
    let guard = Type::mutex_guard(Type::I32);
    let main = MirFunction {
        name: "main".into(),
        ret_type: Some(Type::I32),
        params: vec![],
        locals: vec![local(mutex), local(guard)],
        blocks: vec![MirBlock {
            insts: vec![
                MirInst::Assign {
                    local: LocalId(0),
                    value: Rvalue::MutexNew {
                        value: MirValue::Int(7),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(1),
                    value: Rvalue::MutexLock {
                        base: LocalId(0),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Drop(LocalId(1)),
                MirInst::Drop(LocalId(1)),
                MirInst::Drop(LocalId(0)),
                MirInst::Return(Some(MirValue::Int(42))),
            ],
        }],
    };
    let mut context = CodegenContext::new("mutex_duplicate_guard_drop").unwrap();
    context
        .codegen_module(&MirModule {
            functions: vec![main],
            ..MirModule::default()
        })
        .unwrap();
    UNLOCK_CALLS.store(0, Ordering::SeqCst);
    let mut symbols = runtime_symbols();
    symbols.insert(
        "glyph_mutex_unlock".into(),
        tracked_mutex_unlock as *const () as usize as u64,
    );
    assert_eq!(
        context
            .jit_execute_i32_with_symbols("main", &symbols)
            .unwrap(),
        42
    );
    assert_eq!(
        UNLOCK_CALLS.load(Ordering::SeqCst),
        1,
        "the first drop nulls the guard slot, so duplicate cleanup must not unlock twice"
    );
}

#[test]
fn try_lock_contention_returns_a_null_guard_that_is_safe_to_drop() {
    let mutex = Type::mutex(Type::I32);
    let guard = Type::mutex_guard(Type::I32);
    let main = MirFunction {
        name: "main".into(),
        ret_type: Some(Type::I32),
        params: vec![],
        locals: vec![
            local(mutex),
            local(guard.clone()),
            local(guard.clone()),
            local(Type::Bool),
            local(guard),
            local(Type::Bool),
            local(Type::I32),
        ],
        blocks: vec![MirBlock {
            insts: vec![
                MirInst::Assign {
                    local: LocalId(0),
                    value: Rvalue::MutexNew {
                        value: MirValue::Int(1),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(1),
                    value: Rvalue::MutexLock {
                        base: LocalId(0),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(2),
                    value: Rvalue::MutexTryLock {
                        base: LocalId(0),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(3),
                    value: Rvalue::MutexGuardIsAcquired {
                        guard: LocalId(2),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Drop(LocalId(2)),
                MirInst::Drop(LocalId(1)),
                MirInst::Assign {
                    local: LocalId(4),
                    value: Rvalue::MutexTryLock {
                        base: LocalId(0),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(5),
                    value: Rvalue::MutexGuardIsAcquired {
                        guard: LocalId(4),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(6),
                    value: Rvalue::Cast {
                        value: MirValue::Local(LocalId(5)),
                        from: Type::Bool,
                        to: Type::I32,
                    },
                },
                MirInst::Drop(LocalId(4)),
                MirInst::Drop(LocalId(0)),
                MirInst::Return(Some(MirValue::Local(LocalId(6)))),
            ],
        }],
    };
    let mut context = CodegenContext::new("mutex_try").unwrap();
    context
        .codegen_module(&MirModule {
            functions: vec![main],
            ..MirModule::default()
        })
        .unwrap();
    assert_eq!(
        context
            .jit_execute_i32_with_symbols("main", &runtime_symbols())
            .unwrap(),
        1
    );
    assert!(context.dump_ir().contains("mutex.try.busy"));
}

#[test]
fn arc_mutex_keeps_a_stable_address_and_drops_the_payload_exactly_once() {
    let owned = Type::Own(Box::new(Type::I32));
    let mutex = Type::mutex(owned.clone());
    let arc = Type::arc(mutex.clone());
    let main = MirFunction {
        name: "main".into(),
        ret_type: Some(Type::I32),
        params: vec![],
        locals: vec![
            local(owned.clone()),
            local(mutex.clone()),
            local(arc.clone()),
            local(arc.clone()),
            local(Type::Ref(Box::new(mutex.clone()), Mutability::Immutable)),
            local(Type::mutex_guard(owned.clone())),
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
                    value: Rvalue::MutexNew {
                        value: MirValue::Local(LocalId(0)),
                        elem_type: owned.clone(),
                    },
                },
                MirInst::Assign {
                    local: LocalId(2),
                    value: Rvalue::ArcNew {
                        value: MirValue::Local(LocalId(1)),
                        elem_type: mutex.clone(),
                    },
                },
                MirInst::Assign {
                    local: LocalId(3),
                    value: Rvalue::ArcClone {
                        base: LocalId(2),
                        elem_type: mutex.clone(),
                    },
                },
                MirInst::Assign {
                    local: LocalId(4),
                    value: Rvalue::ArcBorrow {
                        base: LocalId(3),
                        elem_type: mutex,
                    },
                },
                MirInst::Assign {
                    local: LocalId(5),
                    value: Rvalue::MutexLock {
                        base: LocalId(4),
                        elem_type: owned,
                    },
                },
                MirInst::Drop(LocalId(5)),
                MirInst::Drop(LocalId(2)),
                MirInst::Drop(LocalId(3)),
                MirInst::Return(Some(MirValue::Int(0))),
            ],
        }],
    };
    FREE_CALLS.store(0, Ordering::SeqCst);
    let mut context = CodegenContext::new("arc_mutex").unwrap();
    context
        .codegen_module(&MirModule {
            functions: vec![main],
            ..MirModule::default()
        })
        .unwrap();
    let mut symbols = runtime_symbols();
    symbols.insert("free".into(), tracked_free as *const () as usize as u64);
    assert_eq!(
        context
            .jit_execute_i32_with_symbols("main", &symbols)
            .unwrap(),
        0
    );
    assert_eq!(
        FREE_CALLS.load(Ordering::SeqCst),
        3,
        "Own payload, Mutex block, and Arc block must each free exactly once"
    );
}

#[test]
fn forged_mutex_and_guard_applications_are_rejected_by_codegen() {
    let forged = Type::App {
        base: "Mutex".into(),
        args: vec![Type::I32],
    };
    let function = MirFunction {
        name: "main".into(),
        ret_type: Some(Type::I32),
        params: vec![],
        locals: vec![local(forged)],
        blocks: vec![MirBlock {
            insts: vec![MirInst::Return(Some(MirValue::Int(0)))],
        }],
    };
    let mut context = CodegenContext::new("forged_mutex").unwrap();
    let error = context
        .codegen_module(&MirModule {
            functions: vec![function],
            ..MirModule::default()
        })
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("generic types must be monomorphized"),
        "{error:#}"
    );
}
