#![cfg(feature = "codegen")]

use glyph_backend::codegen::CodegenContext;
use glyph_core::mir::{
    Local, LocalId, MirBlock, MirFunction, MirInst, MirModule, MirValue, Rvalue,
};
use glyph_core::types::{Mutability, Type};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::{fs, process::Command};

fn local(ty: Type) -> Local {
    Local {
        name: None,
        ty: Some(ty),
        mutable: false,
        skip_drop: false,
    }
}

fn module_with(functions: Vec<MirFunction>) -> MirModule {
    MirModule {
        struct_types: HashMap::new(),
        enum_types: HashMap::new(),
        functions,
        extern_functions: Vec::new(),
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

fn scalar_arc_main(name: &str) -> MirFunction {
    let arc = Type::arc(Type::I32);
    MirFunction {
        name: name.into(),
        ret_type: Some(Type::I32),
        params: Vec::new(),
        locals: vec![
            local(Type::I32),
            local(arc.clone()),
            local(arc),
            local(Type::Ref(Box::new(Type::I32), Mutability::Immutable)),
            local(Type::I32),
        ],
        blocks: vec![MirBlock {
            insts: vec![
                MirInst::Assign {
                    local: LocalId(0),
                    value: Rvalue::ConstInt(42),
                },
                MirInst::Assign {
                    local: LocalId(1),
                    value: Rvalue::ArcNew {
                        value: MirValue::Local(LocalId(0)),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(2),
                    value: Rvalue::ArcClone {
                        base: LocalId(1),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Drop(LocalId(1)),
                MirInst::Assign {
                    local: LocalId(3),
                    value: Rvalue::ArcBorrow {
                        base: LocalId(2),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(4),
                    value: Rvalue::Call {
                        name: "identity_i32".into(),
                        args: vec![MirValue::Local(LocalId(3))],
                    },
                },
                MirInst::Drop(LocalId(2)),
                MirInst::Return(Some(MirValue::Local(LocalId(4)))),
            ],
        }],
    }
}

#[test]
fn arc_ir_uses_atomic_refcounting_and_immutable_access() {
    let mut context = CodegenContext::new("arc_ir").unwrap();
    context
        .codegen_module(&module_with(vec![identity_i32(), scalar_arc_main("main")]))
        .unwrap();
    let ir = context.dump_ir();

    assert!(ir.contains("atomicrmw add ptr"), "{ir}");
    assert!(ir.contains("monotonic"), "{ir}");
    assert!(ir.contains("atomicrmw sub ptr"), "{ir}");
    assert!(ir.contains("release"), "{ir}");
    assert!(ir.contains("fence acquire"), "{ir}");
    assert!(ir.contains("arc.clone.overflow"), "{ir}");
    assert!(ir.contains("call void @abort"), "{ir}");
    assert!(ir.contains("arc.borrow.value"), "{ir}");
    assert!(
        !ir.contains("shared.rc"),
        "Arc must not reuse Shared<T>'s non-atomic refcount path\n{ir}"
    );
    assert_eq!(context.jit_execute_i32("main").unwrap(), 42);
}

static FREE_CALLS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn tracked_free(ptr: *mut c_void) {
    FREE_CALLS.fetch_add(1, Ordering::SeqCst);
    unsafe { libc::free(ptr) };
}

#[test]
fn nested_arc_drops_droppable_payload_once_at_the_last_owner() {
    let owned = Type::Own(Box::new(Type::I32));
    let inner = Type::arc(owned.clone());
    let outer = Type::arc(inner.clone());
    let function = MirFunction {
        name: "nested_arc".into(),
        ret_type: Some(Type::I32),
        params: Vec::new(),
        locals: vec![
            local(owned.clone()),
            local(inner.clone()),
            local(outer.clone()),
            local(outer),
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
                    value: Rvalue::ArcNew {
                        value: MirValue::Local(LocalId(0)),
                        elem_type: owned,
                    },
                },
                MirInst::Assign {
                    local: LocalId(2),
                    value: Rvalue::ArcNew {
                        value: MirValue::Local(LocalId(1)),
                        elem_type: inner,
                    },
                },
                MirInst::Assign {
                    local: LocalId(3),
                    value: Rvalue::ArcClone {
                        base: LocalId(2),
                        elem_type: Type::arc(Type::Own(Box::new(Type::I32))),
                    },
                },
                MirInst::Drop(LocalId(2)),
                MirInst::Drop(LocalId(3)),
                MirInst::Return(Some(MirValue::Int(0))),
            ],
        }],
    };

    FREE_CALLS.store(0, Ordering::SeqCst);
    let mut context = CodegenContext::new("nested_arc").unwrap();
    context
        .codegen_module(&module_with(vec![function]))
        .unwrap();
    let symbols = HashMap::from([(
        "free".to_string(),
        tracked_free as *const () as usize as u64,
    )]);
    assert_eq!(
        context
            .jit_execute_i32_with_symbols("nested_arc", &symbols)
            .unwrap(),
        0
    );
    // Own<i32> allocation + inner Arc control block + outer Arc control block.
    assert_eq!(FREE_CALLS.load(Ordering::SeqCst), 3);
}

#[test]
fn repeated_clone_drop_keeps_the_original_alive() {
    let arc = Type::arc(Type::I32);
    let mut insts = vec![
        MirInst::Assign {
            local: LocalId(0),
            value: Rvalue::ArcNew {
                value: MirValue::Int(9),
                elem_type: Type::I32,
            },
        },
        MirInst::Assign {
            local: LocalId(2),
            value: Rvalue::ArcBorrow {
                base: LocalId(0),
                elem_type: Type::I32,
            },
        },
        MirInst::Assign {
            local: LocalId(3),
            value: Rvalue::Call {
                name: "identity_i32".into(),
                args: vec![MirValue::Local(LocalId(2))],
            },
        },
    ];
    for _ in 0..512 {
        insts.push(MirInst::Assign {
            local: LocalId(1),
            value: Rvalue::ArcClone {
                base: LocalId(0),
                elem_type: Type::I32,
            },
        });
        insts.push(MirInst::Drop(LocalId(1)));
    }
    insts.push(MirInst::Drop(LocalId(0)));
    insts.push(MirInst::Return(Some(MirValue::Local(LocalId(3)))));

    let stress = MirFunction {
        name: "arc_stress".into(),
        ret_type: Some(Type::I32),
        params: Vec::new(),
        locals: vec![
            local(arc.clone()),
            local(arc),
            local(Type::Ref(Box::new(Type::I32), Mutability::Immutable)),
            local(Type::I32),
        ],
        blocks: vec![MirBlock { insts }],
    };
    let mut context = CodegenContext::new("arc_stress").unwrap();
    context
        .codegen_module(&module_with(vec![identity_i32(), stress]))
        .unwrap();
    assert_eq!(context.jit_execute_i32("arc_stress").unwrap(), 9);
}

fn exported_arc_functions() -> Vec<MirFunction> {
    let arc = Type::arc(Type::I32);
    vec![
        identity_i32(),
        MirFunction {
            name: "arc_clone_drop".into(),
            ret_type: None,
            params: vec![LocalId(0)],
            locals: vec![local(arc.clone()), local(arc.clone())],
            blocks: vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(1),
                        value: Rvalue::ArcClone {
                            base: LocalId(0),
                            elem_type: Type::I32,
                        },
                    },
                    MirInst::Drop(LocalId(1)),
                    MirInst::Return(None),
                ],
            }],
        },
        MirFunction {
            name: "arc_read".into(),
            ret_type: Some(Type::I32),
            params: vec![LocalId(0)],
            locals: vec![
                local(arc.clone()),
                local(Type::Ref(Box::new(Type::I32), Mutability::Immutable)),
                local(Type::I32),
            ],
            blocks: vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(1),
                        value: Rvalue::ArcBorrow {
                            base: LocalId(0),
                            elem_type: Type::I32,
                        },
                    },
                    MirInst::Assign {
                        local: LocalId(2),
                        value: Rvalue::Call {
                            name: "identity_i32".into(),
                            args: vec![MirValue::Local(LocalId(1))],
                        },
                    },
                    MirInst::Return(Some(MirValue::Local(LocalId(2)))),
                ],
            }],
        },
        MirFunction {
            name: "arc_release".into(),
            ret_type: None,
            params: vec![LocalId(0)],
            locals: vec![local(arc)],
            blocks: vec![MirBlock {
                insts: vec![MirInst::Drop(LocalId(0)), MirInst::Return(None)],
            }],
        },
    ]
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn aot_arc_survives_threaded_clone_read_and_last_owner_races() {
    let unique = format!("glyph_arc_aot_{}", std::process::id());
    let directory = std::env::temp_dir();
    let object = directory.join(format!("{unique}.o"));
    let source = directory.join(format!("{unique}.c"));
    let executable = directory.join(&unique);

    let mut context = CodegenContext::new("arc_aot").unwrap();
    context
        .codegen_module(&module_with(exported_arc_functions()))
        .unwrap();
    context.emit_object_file(&object).unwrap();
    fs::write(
        &source,
        r#"
#include <assert.h>
#include <pthread.h>
#include <stdint.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

typedef struct ArcI32 {
    _Atomic uint64_t strong;
    int32_t value;
} ArcI32;

extern void arc_clone_drop(ArcI32 *arc);
extern int32_t arc_read(ArcI32 *arc);
extern void arc_release(ArcI32 *arc);

enum { THREADS = 8, ITERATIONS = 10000 };

static void *clone_reader(void *raw) {
    ArcI32 *arc = raw;
    for (int i = 0; i < ITERATIONS; i++) {
        arc_clone_drop(arc);
        assert(arc_read(arc) == 42);
    }
    return NULL;
}

static void *release_owner(void *raw) {
    arc_release(raw);
    return NULL;
}

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "overflow") == 0) {
        ArcI32 overflow = { .strong = UINT64_MAX, .value = 1 };
        arc_clone_drop(&overflow);
        return 99;
    }

    ArcI32 *shared = malloc(sizeof(*shared));
    assert(shared != NULL);
    atomic_init(&shared->strong, 1);
    shared->value = 42;
    pthread_t workers[THREADS];
    for (int i = 0; i < THREADS; i++)
        assert(pthread_create(&workers[i], NULL, clone_reader, shared) == 0);
    for (int i = 0; i < THREADS; i++)
        assert(pthread_join(workers[i], NULL) == 0);
    assert(atomic_load(&shared->strong) == 1);
    arc_release(shared);

    ArcI32 *last_race = malloc(sizeof(*last_race));
    assert(last_race != NULL);
    atomic_init(&last_race->strong, THREADS);
    last_race->value = 7;
    for (int i = 0; i < THREADS; i++)
        assert(pthread_create(&workers[i], NULL, release_owner, last_race) == 0);
    for (int i = 0; i < THREADS; i++)
        assert(pthread_join(workers[i], NULL) == 0);
    return 0;
}
"#,
    )
    .unwrap();

    let compile = Command::new("cc")
        .args(["-std=c11", "-O2", "-pthread"])
        .arg(&source)
        .arg(&object)
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "C harness failed:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    assert!(Command::new(&executable).status().unwrap().success());
    assert!(
        !Command::new(&executable)
            .arg("overflow")
            .status()
            .unwrap()
            .success(),
        "refcount overflow must abort or trap"
    );

    let _ = fs::remove_file(object);
    let _ = fs::remove_file(source);
    let _ = fs::remove_file(executable);
}

#[test]
fn arc_rejects_targets_without_native_usize_atomics() {
    let function = MirFunction {
        name: "unsupported_arc".into(),
        ret_type: None,
        params: vec![LocalId(0)],
        locals: vec![local(Type::arc(Type::I32))],
        blocks: vec![MirBlock {
            insts: vec![MirInst::Return(None)],
        }],
    };
    let mut context =
        CodegenContext::new_for_target("unsupported_arc", "wasm32-unknown-unknown").unwrap();
    let error = context
        .codegen_module(&module_with(vec![function]))
        .expect_err("Arc requires target-native AtomicUsize");
    assert!(
        error
            .to_string()
            .contains("no Glyph native lock-free atomic guarantee"),
        "unexpected error: {error:#}"
    );
}

#[test]
fn arc_operations_reject_mismatched_payload_types() {
    let function = MirFunction {
        name: "bad_arc".into(),
        ret_type: None,
        params: Vec::new(),
        locals: vec![local(Type::arc(Type::I32)), local(Type::arc(Type::U32))],
        blocks: vec![MirBlock {
            insts: vec![
                MirInst::Assign {
                    local: LocalId(0),
                    value: Rvalue::ArcNew {
                        value: MirValue::Int(1),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(1),
                    value: Rvalue::ArcClone {
                        base: LocalId(0),
                        elem_type: Type::U32,
                    },
                },
                MirInst::Return(None),
            ],
        }],
    };
    let mut context = CodegenContext::new("bad_arc").unwrap();
    let error = context
        .codegen_module(&module_with(vec![function]))
        .expect_err("mismatched Arc MIR must be rejected");
    assert!(error.to_string().contains("Arc MIR references local"));
}
