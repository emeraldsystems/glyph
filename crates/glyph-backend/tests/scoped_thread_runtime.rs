#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::ffi::{c_int, c_void};
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const TEST_FAIL_CREATE: c_int = 2;

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

#[link(name = "glyph_runtime", kind = "static")]
unsafe extern "C" {
    fn glyph_thread_scope_create(out: *mut *mut GlyphThreadScope) -> c_int;
    fn glyph_thread_scope_spawn(
        scope: *mut GlyphThreadScope,
        out: *mut *mut GlyphScopedThread,
        entry: Option<ThreadEntry>,
        env: *mut c_void,
    ) -> c_int;
    fn glyph_thread_scope_spawn_result(
        scope: *mut GlyphThreadScope,
        out: *mut *mut GlyphScopedThread,
        entry: Option<ThreadResultEntry>,
        invoke: *mut c_void,
        env: *mut c_void,
        result_size: usize,
        drop_result: Option<DropResult>,
    ) -> c_int;
    fn glyph_thread_scope_join_result(
        child: *mut *mut GlyphScopedThread,
        out_result: *mut c_void,
    ) -> c_int;
    fn glyph_thread_scope_join_all(scope: *mut *mut GlyphThreadScope) -> c_int;
    fn glyph_thread_scope_drain(scope: *mut GlyphThreadScope) -> c_int;
    fn glyph_thread_test_fail_next(operation: c_int, error_code: c_int) -> c_int;
}

static TEST_LOCK: Mutex<()> = Mutex::new(());
static UNCLAIMED_RESULT_DROPS: AtomicUsize = AtomicUsize::new(0);

fn new_scope() -> *mut GlyphThreadScope {
    let mut scope = ptr::null_mut();
    assert_eq!(unsafe { glyph_thread_scope_create(&mut scope) }, 0);
    assert!(!scope.is_null());
    scope
}

unsafe extern "C" fn set_borrowed_flag(raw: *mut c_void) {
    let flag = unsafe { &*raw.cast::<AtomicBool>() };
    flag.store(true, Ordering::Release);
}

#[test]
fn join_all_waits_for_unjoined_children_without_consuming_borrowed_environments() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let first = AtomicBool::new(false);
    let second = AtomicBool::new(false);
    let mut scope = new_scope();

    for flag in [&first, &second] {
        let mut child = ptr::null_mut();
        assert_eq!(
            unsafe {
                glyph_thread_scope_spawn(
                    scope,
                    &mut child,
                    Some(set_borrowed_flag),
                    (flag as *const AtomicBool).cast_mut().cast(),
                )
            },
            0
        );
        assert!(!child.is_null());
        // Losing the token is permitted: the scope still owns the child.
        child = ptr::null_mut();
        assert!(child.is_null());
    }

    assert_eq!(unsafe { glyph_thread_scope_join_all(&mut scope) }, 0);
    assert!(scope.is_null());
    assert!(first.load(Ordering::Acquire));
    assert!(second.load(Ordering::Acquire));
}

#[test]
fn callback_frame_drain_joins_children_without_consuming_scope_owner() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let flag = AtomicBool::new(false);
    let mut scope = new_scope();
    let mut child = ptr::null_mut();
    assert_eq!(
        unsafe {
            glyph_thread_scope_spawn(
                scope,
                &mut child,
                Some(set_borrowed_flag),
                (&flag as *const AtomicBool).cast_mut().cast(),
            )
        },
        0
    );
    child = ptr::null_mut();
    assert!(child.is_null());
    assert_eq!(unsafe { glyph_thread_scope_drain(scope) }, 0);
    assert!(flag.load(Ordering::Acquire));
    assert!(!scope.is_null());
    assert_eq!(unsafe { glyph_thread_scope_join_all(&mut scope) }, 0);
}

struct TypedBorrow {
    calls: AtomicUsize,
    value: u64,
}

unsafe extern "C" fn write_typed_borrowed_result(
    _invoke: *mut c_void,
    raw: *mut c_void,
    out: *mut c_void,
) {
    let borrowed = unsafe { &*raw.cast::<TypedBorrow>() };
    borrowed.calls.fetch_add(1, Ordering::SeqCst);
    unsafe { out.cast::<u64>().write(borrowed.value) };
}

unsafe extern "C" fn drop_unclaimed_result(_raw: *mut c_void) {
    UNCLAIMED_RESULT_DROPS.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn explicit_typed_join_unregisters_the_child_and_transfers_once() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let borrowed = TypedBorrow {
        calls: AtomicUsize::new(0),
        value: 0x5c0bed,
    };
    let mut scope = new_scope();
    let mut child = ptr::null_mut();
    assert_eq!(
        unsafe {
            glyph_thread_scope_spawn_result(
                scope,
                &mut child,
                Some(write_typed_borrowed_result),
                ptr::null_mut(),
                (&borrowed as *const TypedBorrow).cast_mut().cast(),
                std::mem::size_of::<u64>(),
                None,
            )
        },
        0
    );
    let mut result = 0_u64;
    assert_eq!(
        unsafe { glyph_thread_scope_join_result(&mut child, (&mut result as *mut u64).cast()) },
        0
    );
    assert!(child.is_null());
    assert_eq!(result, borrowed.value);
    assert_eq!(borrowed.calls.load(Ordering::SeqCst), 1);
    assert_eq!(unsafe { glyph_thread_scope_join_all(&mut scope) }, 0);
}

#[test]
fn join_all_drops_an_unclaimed_typed_result_exactly_once() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    UNCLAIMED_RESULT_DROPS.store(0, Ordering::SeqCst);
    let borrowed = TypedBorrow {
        calls: AtomicUsize::new(0),
        value: 99,
    };
    let mut scope = new_scope();
    let mut child = ptr::null_mut();
    assert_eq!(
        unsafe {
            glyph_thread_scope_spawn_result(
                scope,
                &mut child,
                Some(write_typed_borrowed_result),
                ptr::null_mut(),
                (&borrowed as *const TypedBorrow).cast_mut().cast(),
                std::mem::size_of::<u64>(),
                Some(drop_unclaimed_result),
            )
        },
        0
    );
    child = ptr::null_mut();
    assert!(child.is_null());
    assert_eq!(unsafe { glyph_thread_scope_join_all(&mut scope) }, 0);
    assert_eq!(borrowed.calls.load(Ordering::SeqCst), 1);
    assert_eq!(UNCLAIMED_RESULT_DROPS.load(Ordering::SeqCst), 1);
}

#[test]
fn spawn_failure_leaves_the_borrowed_environment_with_the_caller() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let flag = AtomicBool::new(false);
    let mut scope = new_scope();
    let mut child = ptr::null_mut();
    assert_eq!(
        unsafe { glyph_thread_test_fail_next(TEST_FAIL_CREATE, libc::EAGAIN) },
        0
    );
    assert_eq!(
        unsafe {
            glyph_thread_scope_spawn(
                scope,
                &mut child,
                Some(set_borrowed_flag),
                (&flag as *const AtomicBool).cast_mut().cast(),
            )
        },
        -libc::EAGAIN
    );
    assert!(child.is_null());
    assert!(!flag.load(Ordering::Acquire));
    assert_eq!(unsafe { glyph_thread_scope_join_all(&mut scope) }, 0);
    // The stack value remains valid and exclusively owned by this frame.
    flag.store(true, Ordering::Release);
    assert!(flag.load(Ordering::Acquire));
}
