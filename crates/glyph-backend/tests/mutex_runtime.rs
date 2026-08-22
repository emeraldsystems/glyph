#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::process::{Child, Command, ExitStatus};
use std::ptr;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

const CONTENTION_CHILD_ENV: &str = "GLYPH_MUTEX_RUNTIME_CONTENTION_CHILD";
const CONTENTION_TIMEOUT: Duration = Duration::from_secs(60);

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

#[repr(C)]
struct GlyphMutex {
    _private: [u8; 0],
}

#[link(name = "glyph_runtime", kind = "static")]
unsafe extern "C" {
    fn glyph_mutex_create(out: *mut *mut GlyphMutex) -> i32;
    fn glyph_mutex_lock(mutex: *mut GlyphMutex) -> i32;
    fn glyph_mutex_try_lock(mutex: *mut GlyphMutex) -> i32;
    fn glyph_mutex_unlock(mutex: *mut GlyphMutex) -> i32;
    fn glyph_mutex_destroy(mutex: *mut *mut GlyphMutex) -> i32;
}

#[test]
fn runtime_mutex_protects_a_contended_counter_and_publishes_visibility() {
    if std::env::var_os(CONTENTION_CHILD_ENV).is_none() {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "runtime_mutex_protects_a_contended_counter_and_publishes_visibility",
                "--nocapture",
            ])
            .env(CONTENTION_CHILD_ENV, "1")
            .spawn()
            .expect("failed to spawn isolated mutex contention test");
        let status = wait_for_child(&mut child, CONTENTION_TIMEOUT, "mutex contention test");
        assert!(status.success(), "mutex contention child failed: {status}");
        return;
    }

    const THREADS: usize = 8;
    const INCREMENTS: usize = 20_000;
    let mut mutex = ptr::null_mut();
    assert_eq!(unsafe { glyph_mutex_create(&mut mutex) }, 0);
    let counter = Box::into_raw(Box::new(0usize));
    let published = Box::into_raw(Box::new(0usize));
    let start = Arc::new(Barrier::new(THREADS));
    let mut workers = Vec::new();
    for worker in 0..THREADS {
        let mutex_address = mutex as usize;
        let counter_address = counter as usize;
        let published_address = published as usize;
        let start = start.clone();
        workers.push(std::thread::spawn(move || {
            start.wait();
            for _ in 0..INCREMENTS {
                let mutex = mutex_address as *mut GlyphMutex;
                assert_eq!(unsafe { glyph_mutex_lock(mutex) }, 0);
                unsafe { *(counter_address as *mut usize) += 1 };
                if worker == THREADS - 1 {
                    unsafe { *(published_address as *mut usize) = 0x51_51 };
                }
                assert_eq!(unsafe { glyph_mutex_unlock(mutex) }, 0);
            }
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(unsafe { glyph_mutex_lock(mutex) }, 0);
    assert_eq!(unsafe { *counter }, THREADS * INCREMENTS);
    assert_eq!(unsafe { *published }, 0x51_51);
    assert_eq!(unsafe { glyph_mutex_unlock(mutex) }, 0);
    assert_eq!(unsafe { glyph_mutex_destroy(&mut mutex) }, 0);
    assert!(mutex.is_null());
    unsafe {
        drop(Box::from_raw(counter));
        drop(Box::from_raw(published));
    }
}

#[test]
fn runtime_try_lock_reports_contention_without_blocking() {
    let mut mutex = ptr::null_mut();
    assert_eq!(unsafe { glyph_mutex_create(&mut mutex) }, 0);
    assert_eq!(unsafe { glyph_mutex_lock(mutex) }, 0);

    let barrier = Arc::new(Barrier::new(2));
    let mutex_address = mutex as usize;
    let worker_barrier = barrier.clone();
    let worker = std::thread::spawn(move || {
        worker_barrier.wait();
        unsafe { glyph_mutex_try_lock(mutex_address as *mut GlyphMutex) }
    });
    barrier.wait();
    assert_eq!(worker.join().unwrap(), 1);

    assert_eq!(unsafe { glyph_mutex_unlock(mutex) }, 0);
    assert_eq!(unsafe { glyph_mutex_try_lock(mutex) }, 0);
    assert_eq!(unsafe { glyph_mutex_unlock(mutex) }, 0);
    assert_eq!(unsafe { glyph_mutex_destroy(&mut mutex) }, 0);
}

#[test]
fn runtime_rejects_destroy_while_locked_and_leaves_owner_retryable() {
    let mut mutex = ptr::null_mut();
    assert_eq!(unsafe { glyph_mutex_create(&mut mutex) }, 0);
    assert_eq!(unsafe { glyph_mutex_lock(mutex) }, 0);
    assert_eq!(unsafe { glyph_mutex_destroy(&mut mutex) }, -libc::EBUSY);
    assert!(!mutex.is_null());
    assert_eq!(unsafe { glyph_mutex_unlock(mutex) }, 0);
    assert_eq!(unsafe { glyph_mutex_destroy(&mut mutex) }, 0);
}

#[test]
fn runtime_rejects_null_arguments() {
    assert_eq!(
        unsafe { glyph_mutex_create(ptr::null_mut()) },
        -libc::EINVAL
    );
    assert_eq!(unsafe { glyph_mutex_lock(ptr::null_mut()) }, -libc::EINVAL);
    assert_eq!(
        unsafe { glyph_mutex_try_lock(ptr::null_mut()) },
        -libc::EINVAL
    );
    assert_eq!(
        unsafe { glyph_mutex_unlock(ptr::null_mut()) },
        -libc::EINVAL
    );
    assert_eq!(
        unsafe { glyph_mutex_destroy(ptr::null_mut()) },
        -libc::EINVAL
    );
}
