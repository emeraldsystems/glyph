#![cfg(feature = "codegen")]

use glyph_backend::codegen::CodegenContext;
use glyph_core::atomic::{AtomicOrdering, AtomicRmwOp, AtomicScalar};
use glyph_core::mir::{
    BlockId, Local, LocalId, MirBlock, MirFunction, MirInst, MirModule, MirValue, Rvalue,
};
use glyph_core::types::{Mutability, StructType, Type};
use std::collections::HashMap;
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

#[test]
fn atomic_ir_uses_native_instructions_seqcst_and_natural_alignment() {
    let scalar = AtomicScalar::Usize;
    let function = MirFunction {
        name: "atomic_ir".into(),
        ret_type: Some(Type::Usize),
        params: Vec::new(),
        locals: vec![
            local(Type::Atomic(scalar)),
            local(Type::Usize),
            local(Type::Void),
            local(Type::Usize),
            local(Type::Usize),
            local(Type::Void),
        ],
        blocks: vec![MirBlock {
            insts: vec![
                MirInst::Assign {
                    local: LocalId(0),
                    value: Rvalue::AtomicNew {
                        value: MirValue::Int(1),
                        scalar,
                    },
                },
                MirInst::Assign {
                    local: LocalId(1),
                    value: Rvalue::AtomicRmw {
                        atomic: LocalId(0),
                        value: MirValue::Int(2),
                        scalar,
                        op: AtomicRmwOp::Add,
                        ordering: AtomicOrdering::SeqCst,
                    },
                },
                MirInst::Assign {
                    local: LocalId(2),
                    value: Rvalue::AtomicStore {
                        atomic: LocalId(0),
                        value: MirValue::Int(7),
                        scalar,
                        ordering: AtomicOrdering::SeqCst,
                    },
                },
                MirInst::Assign {
                    local: LocalId(3),
                    value: Rvalue::AtomicCompareExchange {
                        atomic: LocalId(0),
                        expected: MirValue::Int(7),
                        desired: MirValue::Int(9),
                        scalar,
                        success: AtomicOrdering::SeqCst,
                        failure: AtomicOrdering::SeqCst,
                    },
                },
                MirInst::Assign {
                    local: LocalId(4),
                    value: Rvalue::AtomicLoad {
                        atomic: LocalId(0),
                        scalar,
                        ordering: AtomicOrdering::SeqCst,
                    },
                },
                MirInst::Assign {
                    local: LocalId(5),
                    value: Rvalue::AtomicFence {
                        ordering: AtomicOrdering::Acquire,
                    },
                },
                MirInst::Return(Some(MirValue::Local(LocalId(4)))),
            ],
        }],
    };

    let mut ctx = CodegenContext::new("atomic_ir").unwrap();
    ctx.codegen_module(&module_with(vec![function])).unwrap();
    let ir = ctx.dump_ir();
    assert!(ir.contains("atomicrmw add ptr"), "{ir}");
    assert!(ir.contains("store atomic i64 7"), "{ir}");
    assert!(ir.contains("cmpxchg ptr"), "{ir}");
    assert!(ir.contains("seq_cst seq_cst"), "{ir}");
    assert!(ir.contains("load atomic i64"), "{ir}");
    assert!(ir.contains("seq_cst, align 8"), "{ir}");
    assert!(ir.contains("fence acquire"), "{ir}");
    assert!(
        !ir.contains("volatile"),
        "atomic operations must not be volatile\n{ir}"
    );
}

fn cas_function(name: &str, initial: i64, expected: i64, desired: i64) -> MirFunction {
    MirFunction {
        name: name.into(),
        ret_type: Some(Type::I32),
        params: Vec::new(),
        locals: vec![local(Type::Atomic(AtomicScalar::I32)), local(Type::I32)],
        blocks: vec![MirBlock {
            insts: vec![
                MirInst::Assign {
                    local: LocalId(0),
                    value: Rvalue::AtomicNew {
                        value: MirValue::Int(initial),
                        scalar: AtomicScalar::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(1),
                    value: Rvalue::AtomicCompareExchange {
                        atomic: LocalId(0),
                        expected: MirValue::Int(expected),
                        desired: MirValue::Int(desired),
                        scalar: AtomicScalar::I32,
                        success: AtomicOrdering::SeqCst,
                        failure: AtomicOrdering::SeqCst,
                    },
                },
                MirInst::Return(Some(MirValue::Local(LocalId(1)))),
            ],
        }],
    }
}

#[test]
fn compare_exchange_returns_the_observed_value_on_success_and_failure() {
    for (name, initial, expected, desired, observed) in
        [("cas_success", 4, 4, 8, 4), ("cas_failure", 7, 4, 8, 7)]
    {
        let mut ctx = CodegenContext::new(name).unwrap();
        ctx.codegen_module(&module_with(vec![cas_function(
            name, initial, expected, desired,
        )]))
        .unwrap();
        assert_eq!(ctx.jit_execute_i32(name).unwrap(), observed);
    }
}

#[test]
fn internal_acquire_release_orderings_lower_without_public_api_exposure() {
    let scalar = AtomicScalar::U32;
    let function = MirFunction {
        name: "internal_orderings".into(),
        ret_type: Some(Type::U32),
        params: Vec::new(),
        locals: vec![
            local(Type::Atomic(scalar)),
            local(Type::U32),
            local(Type::U32),
        ],
        blocks: vec![MirBlock {
            insts: vec![
                MirInst::Assign {
                    local: LocalId(0),
                    value: Rvalue::AtomicNew {
                        value: MirValue::Int(0),
                        scalar,
                    },
                },
                MirInst::Assign {
                    local: LocalId(2),
                    value: Rvalue::AtomicRmw {
                        atomic: LocalId(0),
                        value: MirValue::Int(1),
                        scalar,
                        op: AtomicRmwOp::Add,
                        ordering: AtomicOrdering::Release,
                    },
                },
                MirInst::Assign {
                    local: LocalId(1),
                    value: Rvalue::AtomicLoad {
                        atomic: LocalId(0),
                        scalar,
                        ordering: AtomicOrdering::Acquire,
                    },
                },
                MirInst::Return(Some(MirValue::Local(LocalId(1)))),
            ],
        }],
    };

    let mut ctx = CodegenContext::new("internal_orderings").unwrap();
    ctx.codegen_module(&module_with(vec![function])).unwrap();
    let ir = ctx.dump_ir();
    assert!(
        ir.contains("atomicrmw add ptr") && ir.contains("release, align 4"),
        "{ir}"
    );
    assert!(
        ir.contains("load atomic i32") && ir.contains("acquire, align 4"),
        "{ir}"
    );
}

#[test]
fn atomic_operations_preserve_identity_through_references_and_aggregate_fields() {
    let scalar = AtomicScalar::I32;
    let counter_type = StructType {
        name: "CounterCell".into(),
        fields: vec![("value".into(), Type::Atomic(scalar))],
    };
    let function = MirFunction {
        name: "aggregate_atomic".into(),
        ret_type: Some(Type::I32),
        params: Vec::new(),
        locals: vec![
            local(Type::Atomic(scalar)),
            local(Type::Named(counter_type.name.clone())),
            local(Type::Ref(
                Box::new(Type::Atomic(scalar)),
                Mutability::Immutable,
            )),
            local(Type::I32),
            local(Type::I32),
        ],
        blocks: vec![MirBlock {
            insts: vec![
                MirInst::Assign {
                    local: LocalId(0),
                    value: Rvalue::AtomicNew {
                        value: MirValue::Int(4),
                        scalar,
                    },
                },
                MirInst::Assign {
                    local: LocalId(1),
                    value: Rvalue::StructLit {
                        struct_name: counter_type.name.clone(),
                        field_values: vec![("value".into(), MirValue::Local(LocalId(0)))],
                    },
                },
                MirInst::Assign {
                    local: LocalId(2),
                    value: Rvalue::FieldRef {
                        base: LocalId(1),
                        field_name: "value".into(),
                        field_index: 0,
                        mutability: Mutability::Immutable,
                    },
                },
                MirInst::Assign {
                    local: LocalId(3),
                    value: Rvalue::AtomicRmw {
                        atomic: LocalId(2),
                        value: MirValue::Int(3),
                        scalar,
                        op: AtomicRmwOp::Add,
                        ordering: AtomicOrdering::SeqCst,
                    },
                },
                MirInst::Assign {
                    local: LocalId(4),
                    value: Rvalue::AtomicLoad {
                        atomic: LocalId(2),
                        scalar,
                        ordering: AtomicOrdering::SeqCst,
                    },
                },
                MirInst::Return(Some(MirValue::Local(LocalId(4)))),
            ],
        }],
    };
    let mut module = module_with(vec![function]);
    module
        .struct_types
        .insert(counter_type.name.clone(), counter_type);

    let mut ctx = CodegenContext::new("aggregate_atomic").unwrap();
    ctx.codegen_module(&module).unwrap();
    let ir = ctx.dump_ir();
    assert!(ir.contains("atomic.ref"), "{ir}");
    assert!(ir.contains("atomicrmw add ptr"), "{ir}");
    assert_eq!(ctx.jit_execute_i32("aggregate_atomic").unwrap(), 7);
}

#[test]
fn unsupported_target_is_rejected_by_the_backend_atomic_path() {
    let scalar = AtomicScalar::I32;
    let function = MirFunction {
        name: "unsupported_atomic".into(),
        ret_type: Some(Type::I32),
        params: Vec::new(),
        locals: vec![local(Type::Atomic(scalar)), local(Type::I32)],
        blocks: vec![MirBlock {
            insts: vec![
                MirInst::Assign {
                    local: LocalId(0),
                    value: Rvalue::AtomicNew {
                        value: MirValue::Int(0),
                        scalar,
                    },
                },
                MirInst::Assign {
                    local: LocalId(1),
                    value: Rvalue::AtomicLoad {
                        atomic: LocalId(0),
                        scalar,
                        ordering: AtomicOrdering::SeqCst,
                    },
                },
                MirInst::Return(Some(MirValue::Local(LocalId(1)))),
            ],
        }],
    };

    let mut ctx = CodegenContext::new_for_target("unsupported_atomic", "wasm32-unknown-unknown")
        .expect("construct target-specific backend");
    let error = ctx
        .codegen_module(&module_with(vec![function]))
        .expect_err("wasm32 has no Glyph native atomic guarantee");
    assert!(
        error
            .to_string()
            .contains("no Glyph native lock-free atomic guarantee"),
        "unexpected error: {error:#}"
    );
}

fn atomic_ref(scalar: AtomicScalar) -> Type {
    Type::Ref(Box::new(Type::Atomic(scalar)), Mutability::Immutable)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn compile_and_run_pthread_harness(name: &str, module: &MirModule, source: &str) {
    let prefix = format!("glyph_{name}_{}", std::process::id());
    let directory = std::env::temp_dir();
    let object = directory.join(format!("{prefix}.o"));
    let harness = directory.join(format!("{prefix}.c"));
    let executable = directory.join(&prefix);

    let mut context = CodegenContext::new(name).expect("create backend");
    context.codegen_module(module).expect("lower atomic module");
    context
        .emit_object_file(&object)
        .expect("emit atomic object");
    fs::write(&harness, source).expect("write pthread harness");

    let compiler = Command::new("cc")
        .args(["-std=c11", "-O2", "-pthread"])
        .arg(&harness)
        .arg(&object)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("run C compiler");
    assert!(
        compiler.status.success(),
        "C harness failed to compile:\n{}",
        String::from_utf8_lossy(&compiler.stderr)
    );

    let run = Command::new(&executable)
        .output()
        .expect("run pthread harness");
    assert!(
        run.status.success(),
        "pthread harness failed with {:?}:\nstdout:\n{}\nstderr:\n{}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    for path in [object, harness, executable] {
        let _ = fs::remove_file(path);
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn generated_fetch_add_is_atomic_under_real_thread_contention() {
    let scalar = AtomicScalar::Usize;
    let increment = MirFunction {
        name: "glyph_atomic_increment".into(),
        ret_type: Some(Type::Usize),
        params: vec![LocalId(0)],
        locals: vec![local(atomic_ref(scalar)), local(Type::Usize)],
        blocks: vec![MirBlock {
            insts: vec![
                MirInst::Assign {
                    local: LocalId(1),
                    value: Rvalue::AtomicRmw {
                        atomic: LocalId(0),
                        value: MirValue::Int(1),
                        scalar,
                        op: AtomicRmwOp::Add,
                        ordering: AtomicOrdering::SeqCst,
                    },
                },
                MirInst::Return(Some(MirValue::Local(LocalId(1)))),
            ],
        }],
    };

    compile_and_run_pthread_harness(
        "atomic_fetch_add_stress",
        &module_with(vec![increment]),
        r#"
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>

enum { WORKERS = 8, ITERATIONS = 50000 };
extern uint64_t glyph_atomic_increment(_Atomic uint64_t *counter);

static _Atomic uint64_t counter;

static void *increment_many(void *unused) {
    (void)unused;
    for (int i = 0; i < ITERATIONS; ++i) {
        glyph_atomic_increment(&counter);
    }
    return 0;
}

int main(void) {
    pthread_t workers[WORKERS];
    atomic_init(&counter, 0);
    for (int i = 0; i < WORKERS; ++i) {
        if (pthread_create(&workers[i], 0, increment_many, 0) != 0) return 2;
    }
    for (int i = 0; i < WORKERS; ++i) {
        if (pthread_join(workers[i], 0) != 0) return 3;
    }
    uint64_t actual = atomic_load_explicit(&counter, memory_order_seq_cst);
    uint64_t expected = (uint64_t)WORKERS * ITERATIONS;
    if (actual != expected) {
        fprintf(stderr, "lost atomic updates: expected %llu, got %llu\n",
                (unsigned long long)expected, (unsigned long long)actual);
        return 4;
    }
    return 0;
}
"#,
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn generated_seqcst_operations_publish_across_real_threads() {
    let data_scalar = AtomicScalar::Usize;
    let ready_scalar = AtomicScalar::Bool;
    let publish = MirFunction {
        name: "glyph_seqcst_publish".into(),
        ret_type: None,
        params: vec![LocalId(0), LocalId(1)],
        locals: vec![
            local(atomic_ref(data_scalar)),
            local(atomic_ref(ready_scalar)),
            local(Type::Void),
            local(Type::Void),
        ],
        blocks: vec![MirBlock {
            insts: vec![
                MirInst::Assign {
                    local: LocalId(2),
                    value: Rvalue::AtomicStore {
                        atomic: LocalId(0),
                        value: MirValue::Int(42),
                        scalar: data_scalar,
                        ordering: AtomicOrdering::SeqCst,
                    },
                },
                MirInst::Assign {
                    local: LocalId(3),
                    value: Rvalue::AtomicStore {
                        atomic: LocalId(1),
                        value: MirValue::Bool(true),
                        scalar: ready_scalar,
                        ordering: AtomicOrdering::SeqCst,
                    },
                },
                MirInst::Return(None),
            ],
        }],
    };
    let observe = MirFunction {
        name: "glyph_seqcst_observe".into(),
        ret_type: Some(Type::Usize),
        params: vec![LocalId(0), LocalId(1)],
        locals: vec![
            local(atomic_ref(data_scalar)),
            local(atomic_ref(ready_scalar)),
            local(Type::Bool),
            local(Type::Usize),
        ],
        blocks: vec![
            MirBlock {
                insts: vec![MirInst::Goto(BlockId(1))],
            },
            MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(2),
                        value: Rvalue::AtomicLoad {
                            atomic: LocalId(1),
                            scalar: ready_scalar,
                            ordering: AtomicOrdering::SeqCst,
                        },
                    },
                    MirInst::If {
                        cond: MirValue::Local(LocalId(2)),
                        then_bb: BlockId(2),
                        else_bb: BlockId(1),
                    },
                ],
            },
            MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(3),
                        value: Rvalue::AtomicLoad {
                            atomic: LocalId(0),
                            scalar: data_scalar,
                            ordering: AtomicOrdering::SeqCst,
                        },
                    },
                    MirInst::Return(Some(MirValue::Local(LocalId(3)))),
                ],
            },
        ],
    };

    compile_and_run_pthread_harness(
        "atomic_seqcst_publication",
        &module_with(vec![publish, observe]),
        r#"
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>

enum { ROUNDS = 512 };
extern void glyph_seqcst_publish(_Atomic uint64_t *data, _Atomic uint8_t *ready);
extern uint64_t glyph_seqcst_observe(_Atomic uint64_t *data, _Atomic uint8_t *ready);

struct round_state {
    _Atomic uint64_t data;
    _Atomic uint8_t ready;
    uint64_t observed;
};

static void *publish(void *raw) {
    struct round_state *state = raw;
    glyph_seqcst_publish(&state->data, &state->ready);
    return 0;
}

static void *observe(void *raw) {
    struct round_state *state = raw;
    state->observed = glyph_seqcst_observe(&state->data, &state->ready);
    return 0;
}

int main(void) {
    for (int round = 0; round < ROUNDS; ++round) {
        struct round_state state;
        pthread_t reader;
        pthread_t writer;
        atomic_init(&state.data, 0);
        atomic_init(&state.ready, 0);
        state.observed = 0;
        if (pthread_create(&reader, 0, observe, &state) != 0) return 2;
        if (pthread_create(&writer, 0, publish, &state) != 0) return 3;
        if (pthread_join(writer, 0) != 0) return 4;
        if (pthread_join(reader, 0) != 0) return 5;
        if (state.observed != 42) {
            fprintf(stderr, "publication failed in round %d: got %llu\n",
                    round, (unsigned long long)state.observed);
            return 6;
        }
    }
    return 0;
}
"#,
    );
}
