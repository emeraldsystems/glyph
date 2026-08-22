#![cfg(feature = "codegen")]

use glyph_backend::codegen::CodegenContext;
use glyph_core::mir::{
    Local, LocalId, MirBlock, MirFunction, MirInst, MirModule, MirValue, Rvalue,
};
use glyph_core::types::{Mutability, Type};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::{
    fs,
    process::{Child, Command, ExitStatus},
    time::{Duration, Instant},
};

#[cfg(any(target_os = "macos", target_os = "linux"))]
const SPSC_STRESS_TIMEOUT: Duration = Duration::from_secs(60);

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn wait_for_child(child: &mut Child, timeout: Duration, description: &str) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("failed to poll child process") {
            return status;
        }
        if Instant::now() >= deadline {
            child
                .kill()
                .expect("failed to terminate timed-out child process");
            child
                .wait()
                .expect("failed to reap timed-out child process");
            panic!("{description} did not complete within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

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

fn sequential_ring() -> MirFunction {
    let sender = Type::spsc_sender(Type::I32);
    let receiver = Type::spsc_receiver(Type::I32);
    MirFunction {
        name: "spsc_sequential".into(),
        ret_type: Some(Type::I32),
        params: Vec::new(),
        locals: vec![
            local(receiver), // 0
            local(sender),   // 1
            local(Type::I32),
            local(Type::I32),
            local(Type::I32),
            local(Type::I32),
            local(Type::I32),
        ],
        blocks: vec![MirBlock {
            insts: vec![
                MirInst::Assign {
                    local: LocalId(1),
                    value: Rvalue::SpscChannelNew {
                        capacity: MirValue::Int(2),
                        out_receiver: LocalId(0),
                        elem_type: Type::I32,
                    },
                },
                // Empty before the first publication.
                MirInst::Assign {
                    local: LocalId(6),
                    value: Rvalue::SpscTryRecv {
                        receiver: LocalId(0),
                        out_value: LocalId(5),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(2),
                    value: Rvalue::ConstInt(10),
                },
                MirInst::Assign {
                    local: LocalId(6),
                    value: Rvalue::SpscTrySend {
                        sender: LocalId(1),
                        value: LocalId(2),
                        out_unsent: LocalId(3),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(2),
                    value: Rvalue::ConstInt(20),
                },
                MirInst::Assign {
                    local: LocalId(6),
                    value: Rvalue::SpscTrySend {
                        sender: LocalId(1),
                        value: LocalId(2),
                        out_unsent: LocalId(3),
                        elem_type: Type::I32,
                    },
                },
                // Full: 30 is returned in local 3.
                MirInst::Assign {
                    local: LocalId(2),
                    value: Rvalue::ConstInt(30),
                },
                MirInst::Assign {
                    local: LocalId(6),
                    value: Rvalue::SpscTrySend {
                        sender: LocalId(1),
                        value: LocalId(2),
                        out_unsent: LocalId(3),
                        elem_type: Type::I32,
                    },
                },
                // Receive 10, then publish the returned 30 into the wrapped slot.
                MirInst::Assign {
                    local: LocalId(6),
                    value: Rvalue::SpscTryRecv {
                        receiver: LocalId(0),
                        out_value: LocalId(5),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(6),
                    value: Rvalue::SpscTrySend {
                        sender: LocalId(1),
                        value: LocalId(3),
                        out_unsent: LocalId(4),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(6),
                    value: Rvalue::SpscTryRecv {
                        receiver: LocalId(0),
                        out_value: LocalId(5),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(6),
                    value: Rvalue::SpscTryRecv {
                        receiver: LocalId(0),
                        out_value: LocalId(5),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Drop(LocalId(1)),
                // Once drained, a closed producer is reported as disconnected.
                MirInst::Assign {
                    local: LocalId(6),
                    value: Rvalue::SpscTryRecv {
                        receiver: LocalId(0),
                        out_value: LocalId(4),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Drop(LocalId(0)),
                MirInst::Return(Some(MirValue::Local(LocalId(5)))),
            ],
        }],
    }
}

#[test]
fn spsc_ir_and_jit_cover_empty_full_wrap_order_and_disconnect() {
    let mut context = CodegenContext::new("spsc_sequential").unwrap();
    context
        .codegen_module(&module_with(vec![sequential_ring()]))
        .unwrap();
    let ir = context.dump_ir();
    assert_eq!(context.jit_execute_i32("spsc_sequential").unwrap(), 30);
    assert!(ir.contains("spsc.send.full"), "{ir}");
    assert!(ir.contains("spsc.recv.disconnected"), "{ir}");
    assert!(ir.contains("load atomic i64"), "{ir}");
    assert!(ir.contains("acquire"), "{ir}");
    assert!(ir.contains("store atomic i64"), "{ir}");
    assert!(ir.contains("release"), "{ir}");
    assert_eq!(
        ir.matches("call ptr @malloc").count(),
        1,
        "only channel construction may allocate\n{ir}"
    );
    assert!(
        !ir.contains("pthread_mutex"),
        "SPSC must not use locks\n{ir}"
    );
}

static FREE_CALLS: AtomicUsize = AtomicUsize::new(0);
static FREE_COUNTER_TEST_LOCK: Mutex<()> = Mutex::new(());

unsafe extern "C" fn tracked_free(ptr: *mut c_void) {
    FREE_CALLS.fetch_add(1, Ordering::SeqCst);
    unsafe { libc::free(ptr) };
}

fn one_queued_payload_drop(name: &str, sender_first: bool) -> MirFunction {
    let elem = Type::Own(Box::new(Type::I32));
    let mut drops = if sender_first {
        vec![MirInst::Drop(LocalId(1)), MirInst::Drop(LocalId(0))]
    } else {
        vec![MirInst::Drop(LocalId(0)), MirInst::Drop(LocalId(1))]
    };
    let mut insts = vec![
        MirInst::Assign {
            local: LocalId(1),
            value: Rvalue::SpscChannelNew {
                capacity: MirValue::Int(1),
                out_receiver: LocalId(0),
                elem_type: elem.clone(),
            },
        },
        MirInst::Assign {
            local: LocalId(2),
            value: Rvalue::OwnNew {
                value: MirValue::Int(7),
                elem_type: Type::I32,
            },
        },
        MirInst::Assign {
            local: LocalId(4),
            value: Rvalue::SpscTrySend {
                sender: LocalId(1),
                value: LocalId(2),
                out_unsent: LocalId(3),
                elem_type: elem.clone(),
            },
        },
    ];
    insts.append(&mut drops);
    insts.push(MirInst::Return(Some(MirValue::Int(0))));
    MirFunction {
        name: name.into(),
        ret_type: Some(Type::I32),
        params: Vec::new(),
        locals: vec![
            local(Type::spsc_receiver(elem.clone())),
            local(Type::spsc_sender(elem.clone())),
            local(elem.clone()),
            local(elem),
            local(Type::I32),
        ],
        blocks: vec![MirBlock { insts }],
    }
}

#[test]
fn either_final_endpoint_drains_the_queued_payload_before_freeing() {
    let _counter_guard = FREE_COUNTER_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let symbols = HashMap::from([(
        "free".to_string(),
        tracked_free as *const () as usize as u64,
    )]);
    for (name, sender_first) in [
        ("receiver_final_drain", true),
        ("sender_final_drain", false),
    ] {
        FREE_CALLS.store(0, Ordering::SeqCst);
        let mut context = CodegenContext::new(name).unwrap();
        context
            .codegen_module(&module_with(vec![one_queued_payload_drop(
                name,
                sender_first,
            )]))
            .unwrap();
        assert_eq!(
            context
                .jit_execute_i32_with_symbols(name, &symbols)
                .unwrap(),
            0
        );
        assert_eq!(
            FREE_CALLS.load(Ordering::SeqCst),
            2,
            "{name}: one Own payload plus one ring allocation"
        );
    }
}

#[test]
fn final_endpoint_drains_droppable_payloads_exactly_once() {
    let _counter_guard = FREE_COUNTER_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let elem = Type::Own(Box::new(Type::I32));
    let sender = Type::spsc_sender(elem.clone());
    let receiver = Type::spsc_receiver(elem.clone());
    let function = MirFunction {
        name: "spsc_drop".into(),
        ret_type: Some(Type::I32),
        params: Vec::new(),
        locals: vec![
            local(receiver),
            local(sender),
            local(elem.clone()),
            local(elem.clone()),
            local(elem.clone()),
            local(Type::I32),
            local(elem.clone()),
        ],
        blocks: vec![MirBlock {
            insts: vec![
                MirInst::Assign {
                    local: LocalId(1),
                    value: Rvalue::SpscChannelNew {
                        capacity: MirValue::Int(1),
                        out_receiver: LocalId(0),
                        elem_type: elem.clone(),
                    },
                },
                MirInst::Assign {
                    local: LocalId(2),
                    value: Rvalue::OwnNew {
                        value: MirValue::Int(1),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(5),
                    value: Rvalue::SpscTrySend {
                        sender: LocalId(1),
                        value: LocalId(2),
                        out_unsent: LocalId(4),
                        elem_type: elem.clone(),
                    },
                },
                // A full send returns ownership; dropping local 4 must free it.
                MirInst::Assign {
                    local: LocalId(3),
                    value: Rvalue::OwnNew {
                        value: MirValue::Int(2),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(5),
                    value: Rvalue::SpscTrySend {
                        sender: LocalId(1),
                        value: LocalId(3),
                        out_unsent: LocalId(4),
                        elem_type: elem,
                    },
                },
                MirInst::Drop(LocalId(4)),
                // Receiver closes first. A subsequent send returns ownership
                // as disconnected rather than losing the payload.
                MirInst::Drop(LocalId(0)),
                MirInst::Assign {
                    local: LocalId(6),
                    value: Rvalue::OwnNew {
                        value: MirValue::Int(3),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Assign {
                    local: LocalId(5),
                    value: Rvalue::SpscTrySend {
                        sender: LocalId(1),
                        value: LocalId(6),
                        out_unsent: LocalId(4),
                        elem_type: Type::Own(Box::new(Type::I32)),
                    },
                },
                MirInst::Drop(LocalId(4)),
                // Sender becomes final owner and drains the queued first
                // allocation before freeing the ring.
                MirInst::Drop(LocalId(1)),
                MirInst::Return(Some(MirValue::Int(0))),
            ],
        }],
    };
    FREE_CALLS.store(0, Ordering::SeqCst);
    let mut context = CodegenContext::new("spsc_drop").unwrap();
    context
        .codegen_module(&module_with(vec![function]))
        .unwrap();
    let symbols = HashMap::from([(
        "free".to_string(),
        tracked_free as *const () as usize as u64,
    )]);
    assert_eq!(
        context
            .jit_execute_i32_with_symbols("spsc_drop", &symbols)
            .unwrap(),
        0
    );
    assert_eq!(
        FREE_CALLS.load(Ordering::SeqCst),
        4,
        "three Own payloads and the one ring allocation must each free once"
    );
}

fn exported_i32_ring() -> Vec<MirFunction> {
    let sender = Type::spsc_sender(Type::I32);
    let receiver = Type::spsc_receiver(Type::I32);
    vec![
        MirFunction {
            name: "spsc_make".into(),
            ret_type: Some(sender.clone()),
            params: vec![LocalId(0), LocalId(1)],
            locals: vec![
                local(Type::Usize),
                local(Type::Ref(Box::new(receiver.clone()), Mutability::Mutable)),
                local(sender.clone()),
            ],
            blocks: vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(2),
                        value: Rvalue::SpscChannelNew {
                            capacity: MirValue::Local(LocalId(0)),
                            out_receiver: LocalId(1),
                            elem_type: Type::I32,
                        },
                    },
                    MirInst::Return(Some(MirValue::Local(LocalId(2)))),
                ],
            }],
        },
        MirFunction {
            name: "spsc_send".into(),
            ret_type: Some(Type::I32),
            params: vec![LocalId(0), LocalId(1), LocalId(2)],
            locals: vec![
                local(sender.clone()),
                local(Type::I32),
                local(Type::Ref(Box::new(Type::I32), Mutability::Mutable)),
                local(Type::I32),
            ],
            blocks: vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(3),
                        value: Rvalue::SpscTrySend {
                            sender: LocalId(0),
                            value: LocalId(1),
                            out_unsent: LocalId(2),
                            elem_type: Type::I32,
                        },
                    },
                    MirInst::Return(Some(MirValue::Local(LocalId(3)))),
                ],
            }],
        },
        MirFunction {
            name: "spsc_recv".into(),
            ret_type: Some(Type::I32),
            params: vec![LocalId(0), LocalId(1)],
            locals: vec![
                local(receiver.clone()),
                local(Type::Ref(Box::new(Type::I32), Mutability::Mutable)),
                local(Type::I32),
            ],
            blocks: vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(2),
                        value: Rvalue::SpscTryRecv {
                            receiver: LocalId(0),
                            out_value: LocalId(1),
                            elem_type: Type::I32,
                        },
                    },
                    MirInst::Return(Some(MirValue::Local(LocalId(2)))),
                ],
            }],
        },
        MirFunction {
            name: "spsc_close_sender".into(),
            ret_type: None,
            params: vec![LocalId(0)],
            locals: vec![local(sender)],
            blocks: vec![MirBlock {
                insts: vec![MirInst::Drop(LocalId(0)), MirInst::Return(None)],
            }],
        },
        MirFunction {
            name: "spsc_close_receiver".into(),
            ret_type: None,
            params: vec![LocalId(0)],
            locals: vec![local(receiver)],
            blocks: vec![MirBlock {
                insts: vec![MirInst::Drop(LocalId(0)), MirInst::Return(None)],
            }],
        },
    ]
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn aot_spsc_preserves_order_under_high_iteration_contention() {
    let unique = format!("glyph_spsc_aot_{}", std::process::id());
    let directory = std::env::temp_dir();
    let object = directory.join(format!("{unique}.o"));
    let source = directory.join(format!("{unique}.c"));
    let executable = directory.join(&unique);
    let mut context = CodegenContext::new("spsc_aot").unwrap();
    context
        .codegen_module(&module_with(exported_i32_ring()))
        .unwrap();
    let ir = context.dump_ir();
    for function in ["spsc_send", "spsc_recv"] {
        let body = ir
            .split(&format!("define i32 @{function}("))
            .nth(1)
            .unwrap_or_else(|| panic!("missing {function} in IR\n{ir}"))
            .split("\n}")
            .next()
            .unwrap();
        assert!(
            !body.contains("@malloc") && !body.contains("pthread_mutex"),
            "{function} hot path must contain neither allocation nor locks\n{body}"
        );
    }
    assert_eq!(
        ir.matches("call ptr @malloc").count(),
        1,
        "only spsc_make may allocate\n{ir}"
    );
    context.emit_object_file(&object).unwrap();
    fs::write(
        &source,
        r#"
#include <assert.h>
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <sched.h>

extern void *spsc_make(uint64_t capacity, void **receiver);
extern int32_t spsc_send(void *sender, int32_t value, int32_t *unsent);
extern int32_t spsc_recv(void *receiver, int32_t *value);
extern void spsc_close_sender(void *sender);
extern void spsc_close_receiver(void *receiver);

enum { ITERATIONS = 200000 };
static void *sender;
static void *receiver;

static void *produce(void *unused) {
    (void)unused;
    for (int32_t value = 0; value < ITERATIONS; value++) {
        int32_t owned = value;
        int32_t status;
        while ((status = spsc_send(sender, owned, &owned)) == 1)
            sched_yield();
        assert(status == 0);
    }
    spsc_close_sender(sender);
    return NULL;
}

static void *consume(void *unused) {
    (void)unused;
    for (int32_t expected = 0; expected < ITERATIONS; expected++) {
        int32_t value = -1;
        int32_t status;
        while ((status = spsc_recv(receiver, &value)) == 1)
            sched_yield();
        assert(status == 0);
        assert(value == expected);
    }
    int32_t ignored;
    while (spsc_recv(receiver, &ignored) == 1)
        sched_yield();
    assert(spsc_recv(receiver, &ignored) == 2);
    spsc_close_receiver(receiver);
    return NULL;
}

int main(void) {
    sender = spsc_make(64, &receiver);
    assert(sender != NULL && receiver != NULL);
    pthread_t producer, consumer;
    assert(pthread_create(&producer, NULL, produce, NULL) == 0);
    assert(pthread_create(&consumer, NULL, consume, NULL) == 0);
    assert(pthread_join(producer, NULL) == 0);
    assert(pthread_join(consumer, NULL) == 0);
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
    let mut stress = Command::new(&executable)
        .spawn()
        .expect("failed to spawn SPSC stress executable");
    let status = wait_for_child(&mut stress, SPSC_STRESS_TIMEOUT, "SPSC AOT stress test");
    assert!(status.success(), "SPSC stress executable failed: {status}");
    let _ = fs::remove_file(object);
    let _ = fs::remove_file(source);
    let _ = fs::remove_file(executable);
}

#[test]
fn spsc_rejects_noncanonical_endpoints_and_unsupported_targets() {
    let forged = MirFunction {
        name: "forged_spsc".into(),
        ret_type: Some(Type::I32),
        params: vec![LocalId(0), LocalId(1), LocalId(2)],
        locals: vec![
            local(Type::App {
                base: "Sender".into(),
                args: vec![Type::I32],
            }),
            local(Type::I32),
            local(Type::I32),
            local(Type::I32),
        ],
        blocks: vec![MirBlock {
            insts: vec![
                MirInst::Assign {
                    local: LocalId(3),
                    value: Rvalue::SpscTrySend {
                        sender: LocalId(0),
                        value: LocalId(1),
                        out_unsent: LocalId(2),
                        elem_type: Type::I32,
                    },
                },
                MirInst::Return(Some(MirValue::Local(LocalId(3)))),
            ],
        }],
    };
    let mut context = CodegenContext::new("forged_spsc").unwrap();
    let error = context
        .codegen_module(&module_with(vec![forged]))
        .expect_err("a user-defined Sender must not select the SPSC intrinsic");
    assert!(
        error
            .to_string()
            .contains("generic types must be monomorphized")
    );

    let mut context =
        CodegenContext::new_for_target("spsc_wasm", "wasm32-unknown-unknown").unwrap();
    let error = context
        .codegen_module(&module_with(vec![sequential_ring()]))
        .expect_err("SPSC requires native AtomicUsize");
    assert!(
        error
            .to_string()
            .contains("no Glyph native lock-free atomic guarantee"),
        "unexpected error: {error:#}"
    );
}
