#![cfg(feature = "codegen")]

use glyph_backend::codegen::CodegenContext;
use glyph_core::atomic::{AtomicOrdering, AtomicRmwOp, AtomicScalar};
use glyph_core::mir::{
    Local, LocalId, MirBlock, MirFunction, MirInst, MirModule, MirValue, Rvalue,
};
use glyph_core::types::Type;
use std::collections::HashMap;

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
