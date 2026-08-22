#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::ffi::{c_int, c_void};
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(feature = "codegen")]
use glyph_backend::{
    codegen::CodegenContext,
    linker::{Linker, LinkerOptions},
};
#[cfg(feature = "codegen")]
use glyph_core::mir::{
    Local, LocalId, MirBlock, MirExternFunction, MirFunction, MirInst, MirModule, MirValue, Rvalue,
};
#[cfg(feature = "codegen")]
use glyph_core::types::Type;

const WAIT_MS: u32 = 2_000;
const TEST_FAIL_ALLOC: c_int = 1;
const TEST_FAIL_CREATE: c_int = 2;
const TEST_FAIL_JOIN: c_int = 3;
const TEST_FAIL_DETACH: c_int = 4;

#[repr(C)]
struct GlyphThread {
    _private: [u8; 0],
}

#[repr(C)]
struct GlyphThreadTestLatch {
    _private: [u8; 0],
}

type ThreadEntry = unsafe extern "C" fn(*mut c_void);
type DropUnstarted = unsafe extern "C" fn(*mut c_void);

#[link(name = "glyph_runtime", kind = "static")]
unsafe extern "C" {
    fn glyph_thread_spawn(
        out: *mut *mut GlyphThread,
        entry: Option<ThreadEntry>,
        env: *mut c_void,
        drop_unstarted: Option<DropUnstarted>,
    ) -> c_int;
    fn glyph_thread_join(handle: *mut *mut GlyphThread) -> c_int;
    fn glyph_thread_detach(handle: *mut *mut GlyphThread) -> c_int;

    fn glyph_thread_test_fail_next(operation: c_int, error_code: c_int) -> c_int;
    fn glyph_thread_test_latch_create(
        initial_count: u32,
        out: *mut *mut GlyphThreadTestLatch,
    ) -> c_int;
    fn glyph_thread_test_latch_count_down(latch: *mut GlyphThreadTestLatch) -> c_int;
    fn glyph_thread_test_latch_wait(latch: *mut GlyphThreadTestLatch, timeout_ms: u32) -> c_int;
    fn glyph_thread_test_latch_destroy(latch: *mut *mut GlyphThreadTestLatch) -> c_int;
}

static INVOCATIONS: AtomicUsize = AtomicUsize::new(0);
static FAILURE_DROPS: AtomicUsize = AtomicUsize::new(0);
static TEST_LOCK: Mutex<()> = Mutex::new(());

struct WorkerEnv {
    gate: *mut GlyphThreadTestLatch,
    completion: *mut GlyphThreadTestLatch,
}

unsafe extern "C" fn count_and_finish(raw: *mut c_void) {
    let env = unsafe { Box::from_raw(raw.cast::<WorkerEnv>()) };
    INVOCATIONS.fetch_add(1, Ordering::SeqCst);
    if !env.gate.is_null() {
        let _ = unsafe { glyph_thread_test_latch_wait(env.gate, WAIT_MS) };
    }
    let _ = unsafe { glyph_thread_test_latch_count_down(env.completion) };
}

unsafe extern "C" fn drop_unstarted(raw: *mut c_void) {
    if !raw.is_null() {
        drop(unsafe { Box::from_raw(raw.cast::<WorkerEnv>()) });
    }
    FAILURE_DROPS.fetch_add(1, Ordering::SeqCst);
}

fn latch(initial_count: u32) -> *mut GlyphThreadTestLatch {
    let mut latch = ptr::null_mut();
    assert_eq!(
        unsafe { glyph_thread_test_latch_create(initial_count, &mut latch) },
        0
    );
    assert!(!latch.is_null());
    latch
}

fn destroy_latch(mut latch: *mut GlyphThreadTestLatch) {
    assert_eq!(unsafe { glyph_thread_test_latch_destroy(&mut latch) }, 0);
    assert!(latch.is_null());
}

fn worker_env(
    gate: *mut GlyphThreadTestLatch,
    completion: *mut GlyphThreadTestLatch,
) -> *mut c_void {
    Box::into_raw(Box::new(WorkerEnv { gate, completion })).cast()
}

#[test]
fn spawn_and_join_transfer_the_environment_once() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    INVOCATIONS.store(0, Ordering::SeqCst);
    let completion = latch(1);
    let mut handle = ptr::null_mut();

    assert_eq!(
        unsafe {
            glyph_thread_spawn(
                &mut handle,
                Some(count_and_finish),
                worker_env(ptr::null_mut(), completion),
                Some(drop_unstarted),
            )
        },
        0
    );
    assert!(!handle.is_null());
    assert_eq!(
        unsafe { glyph_thread_test_latch_wait(completion, WAIT_MS) },
        0
    );
    assert_eq!(unsafe { glyph_thread_join(&mut handle) }, 0);
    assert!(handle.is_null());
    assert_eq!(INVOCATIONS.load(Ordering::SeqCst), 1);
    assert_eq!(unsafe { glyph_thread_join(&mut handle) }, -libc::EINVAL);

    destroy_latch(completion);
}

#[test]
fn create_and_allocation_failures_drop_the_unstarted_environment() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    FAILURE_DROPS.store(0, Ordering::SeqCst);
    for (operation, error) in [
        (TEST_FAIL_ALLOC, libc::ENOMEM),
        (TEST_FAIL_CREATE, libc::EAGAIN),
    ] {
        let completion = latch(1);
        let mut handle = ptr::null_mut();
        assert_eq!(unsafe { glyph_thread_test_fail_next(operation, error) }, 0);
        assert_eq!(
            unsafe {
                glyph_thread_spawn(
                    &mut handle,
                    Some(count_and_finish),
                    worker_env(ptr::null_mut(), completion),
                    Some(drop_unstarted),
                )
            },
            -error
        );
        assert!(handle.is_null());
        destroy_latch(completion);
    }
    assert_eq!(FAILURE_DROPS.load(Ordering::SeqCst), 2);
}

#[test]
fn failed_join_is_retryable_and_never_loses_the_handle() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let completion = latch(1);
    let mut handle = ptr::null_mut();
    assert_eq!(
        unsafe {
            glyph_thread_spawn(
                &mut handle,
                Some(count_and_finish),
                worker_env(ptr::null_mut(), completion),
                Some(drop_unstarted),
            )
        },
        0
    );
    assert_eq!(
        unsafe { glyph_thread_test_latch_wait(completion, WAIT_MS) },
        0
    );

    assert_eq!(
        unsafe { glyph_thread_test_fail_next(TEST_FAIL_JOIN, libc::EBUSY) },
        0
    );
    assert_eq!(unsafe { glyph_thread_join(&mut handle) }, -libc::EBUSY);
    assert!(!handle.is_null());
    assert_eq!(unsafe { glyph_thread_join(&mut handle) }, 0);
    assert!(handle.is_null());

    destroy_latch(completion);
}

#[test]
fn detach_is_nonblocking_and_failed_detach_is_retryable() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let gate = latch(1);
    let completion = latch(1);
    let mut handle = ptr::null_mut();
    assert_eq!(
        unsafe {
            glyph_thread_spawn(
                &mut handle,
                Some(count_and_finish),
                worker_env(gate, completion),
                Some(drop_unstarted),
            )
        },
        0
    );

    assert_eq!(
        unsafe { glyph_thread_test_fail_next(TEST_FAIL_DETACH, libc::EBUSY) },
        0
    );
    assert_eq!(unsafe { glyph_thread_detach(&mut handle) }, -libc::EBUSY);
    assert!(!handle.is_null());
    assert_eq!(unsafe { glyph_thread_detach(&mut handle) }, 0);
    assert!(handle.is_null());
    assert_eq!(unsafe { glyph_thread_detach(&mut handle) }, -libc::EINVAL);

    assert_eq!(unsafe { glyph_thread_test_latch_count_down(gate) }, 0);
    assert_eq!(
        unsafe { glyph_thread_test_latch_wait(completion, WAIT_MS) },
        0
    );
    destroy_latch(gate);
    destroy_latch(completion);
}

#[test]
fn invalid_spawn_arguments_still_release_owned_environment() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    FAILURE_DROPS.store(0, Ordering::SeqCst);
    let completion = latch(1);
    assert_eq!(
        unsafe {
            glyph_thread_spawn(
                ptr::null_mut(),
                Some(count_and_finish),
                worker_env(ptr::null_mut(), completion),
                Some(drop_unstarted),
            )
        },
        -libc::EINVAL
    );
    assert_eq!(FAILURE_DROPS.load(Ordering::SeqCst), 1);
    destroy_latch(completion);
}

#[test]
fn repeated_join_and_detach_lifecycles_complete_with_bounded_waits() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    for detach in [false, true] {
        for _ in 0..32 {
            let completion = latch(1);
            let mut handle = ptr::null_mut();
            assert_eq!(
                unsafe {
                    glyph_thread_spawn(
                        &mut handle,
                        Some(count_and_finish),
                        worker_env(ptr::null_mut(), completion),
                        Some(drop_unstarted),
                    )
                },
                0
            );
            if detach {
                assert_eq!(unsafe { glyph_thread_detach(&mut handle) }, 0);
            }
            assert_eq!(
                unsafe { glyph_thread_test_latch_wait(completion, WAIT_MS) },
                0
            );
            if !detach {
                assert_eq!(unsafe { glyph_thread_join(&mut handle) }, 0);
            }
            destroy_latch(completion);
        }
    }
}

#[cfg(feature = "codegen")]
#[test]
fn aot_linker_resolves_and_executes_native_thread_runtime_symbol() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let handle_ptr = Type::RawPtr(Box::new(Type::RawPtr(Box::new(Type::U8))));
    let mir = MirModule {
        struct_types: Default::default(),
        enum_types: Default::default(),
        extern_functions: vec![MirExternFunction {
            name: "glyph_thread_join".into(),
            ret_type: Some(Type::I32),
            params: vec![handle_ptr.clone()],
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
                    ty: Some(handle_ptr),
                    mutable: false,
                    skip_drop: true,
                },
                Local {
                    name: None,
                    ty: Some(Type::I32),
                    mutable: false,
                    skip_drop: true,
                },
            ],
            blocks: vec![MirBlock {
                insts: vec![
                    MirInst::Assign {
                        local: LocalId(0),
                        value: Rvalue::RawPtrNull {
                            elem_type: Type::RawPtr(Box::new(Type::U8)),
                        },
                    },
                    MirInst::Assign {
                        local: LocalId(1),
                        value: Rvalue::Call {
                            name: "glyph_thread_join".into(),
                            args: vec![MirValue::Local(LocalId(0))],
                        },
                    },
                    MirInst::Return(Some(MirValue::Local(LocalId(1)))),
                ],
            }],
        }],
    };

    let unique = format!("glyph_thread_aot_{}", std::process::id());
    let object = std::env::temp_dir().join(format!("{unique}.o"));
    let executable = std::env::temp_dir().join(unique);
    let mut ctx = CodegenContext::new("thread_runtime_aot").unwrap();
    ctx.codegen_module(&mir).unwrap();
    ctx.emit_object_file(&object).unwrap();
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
    assert_eq!(status.code(), Some(256 - libc::EINVAL));
    let _ = std::fs::remove_file(object);
    let _ = std::fs::remove_file(executable);
}
