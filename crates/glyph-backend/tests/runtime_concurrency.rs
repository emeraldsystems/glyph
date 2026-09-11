#![cfg(any(target_os = "macos", target_os = "linux"))]

// Link glyph-backend even when no backend API is used here: its build
// script links libglyph_runtime.a whole into every binary that depends
// on the crate, and the `extern "C"` block below resolves against that
// (see glyph-backend/build.rs). A bare `#[link]` on the extern block
// would add a second, plain copy of the archive and duplicate symbols
// on ELF linkers.
use glyph_backend as _;

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fs;
use std::sync::{Arc, Barrier, mpsc};
use std::thread;

#[repr(C)]
struct GlyphVec {
    data: *mut c_void,
    len: i64,
    cap: i64,
}

unsafe extern "C" {
    fn glyph_time_to_human_readable(timestamp: u64) -> *const c_char;

    fn glyph_fmt_write_str(fd: c_int, value: *const c_char) -> c_int;
    fn glyph_string_from_i32(value: c_int) -> *mut c_char;

    fn glyph_net_tcp_recv(fd: c_int, max_bytes: u32) -> *mut c_char;
    fn glyph_net_get_last_error() -> c_int;

    fn glyph_term_enter_ui_session(term_id: c_int) -> c_int;
    fn glyph_term_session_end(term_id: c_int) -> c_int;

    fn glyph_audio_wav_open(path: *const c_char, sample_rate: u32, channels: u32) -> c_int;
    fn glyph_audio_wav_write(handle: c_int, samples: *const GlyphVec) -> c_int;
    fn glyph_audio_wav_close(handle: c_int) -> c_int;

    fn glyph_process_run(command: *const c_char, args: *const GlyphVec) -> c_int;
}

fn c_string(pointer: *const c_char) -> String {
    assert!(!pointer.is_null());
    unsafe { CStr::from_ptr(pointer) }
        .to_str()
        .expect("runtime returned UTF-8 ASCII")
        .to_owned()
}

#[test]
fn human_readable_time_buffers_are_isolated_between_threads() {
    let first_call_ready = Arc::new(Barrier::new(2));
    let second_call_done = Arc::new(Barrier::new(2));

    let first = {
        let first_call_ready = Arc::clone(&first_call_ready);
        let second_call_done = Arc::clone(&second_call_done);
        thread::spawn(move || {
            let pointer = unsafe { glyph_time_to_human_readable(0) } as usize;
            assert_eq!(c_string(pointer as *const c_char), "01/01/1970 00:00:00");
            first_call_ready.wait();
            second_call_done.wait();
            c_string(pointer as *const c_char)
        })
    };

    let second = thread::spawn(move || {
        first_call_ready.wait();
        let formatted = c_string(unsafe { glyph_time_to_human_readable(86_400) });
        second_call_done.wait();
        formatted
    });

    assert_eq!(
        second.join().expect("second formatter thread"),
        "02/01/1970 00:00:00"
    );
    assert_eq!(
        first.join().expect("first formatter thread"),
        "01/01/1970 00:00:00"
    );
}

#[test]
fn receive_error_cache_is_isolated_between_threads() {
    let mut sockets = [-1; 2];
    assert_eq!(
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sockets.as_mut_ptr()) },
        0
    );
    assert_eq!(
        unsafe { libc::write(sockets[0], b"x".as_ptr().cast(), 1) },
        1
    );

    let error_recorded = Arc::new(Barrier::new(2));
    let success_recorded = Arc::new(Barrier::new(2));

    let failing = {
        let error_recorded = Arc::clone(&error_recorded);
        let success_recorded = Arc::clone(&success_recorded);
        thread::spawn(move || {
            let result = unsafe { glyph_net_tcp_recv(-1, 1) };
            assert!(!result.is_null());
            unsafe { libc::free(result.cast()) };
            assert_eq!(unsafe { glyph_net_get_last_error() }, libc::EBADF);
            error_recorded.wait();
            success_recorded.wait();
            unsafe { glyph_net_get_last_error() }
        })
    };

    let receiving = thread::spawn(move || {
        error_recorded.wait();
        let result = unsafe { glyph_net_tcp_recv(sockets[1], 1) };
        assert_eq!(c_string(result), "x");
        unsafe { libc::free(result.cast()) };
        let error = unsafe { glyph_net_get_last_error() };
        success_recorded.wait();
        assert_eq!(unsafe { libc::close(sockets[1]) }, 0);
        error
    });

    assert_eq!(receiving.join().expect("receiving thread"), 0);
    assert_eq!(failing.join().expect("failing thread"), libc::EBADF);
    assert_eq!(unsafe { libc::close(sockets[0]) }, 0);
}

#[test]
fn terminal_session_admission_has_exactly_one_winner() {
    const CONTENDERS: usize = 8;
    for _ in 0..64 {
        let start = Arc::new(Barrier::new(CONTENDERS));
        let contenders: Vec<_> = (0..CONTENDERS)
            .map(|_| {
                let start = Arc::clone(&start);
                thread::spawn(move || {
                    start.wait();
                    unsafe { glyph_term_enter_ui_session(1) }
                })
            })
            .collect();
        let results: Vec<_> = contenders
            .into_iter()
            .map(|contender| contender.join().expect("terminal contender"))
            .collect();

        assert_eq!(results.iter().filter(|result| **result == 0).count(), 1);
        assert!(results.iter().all(|result| *result == 0 || *result == -1));
        assert_eq!(unsafe { glyph_term_session_end(1) }, 0);
    }
}

#[test]
fn concurrent_formatting_writes_remain_whole() {
    const WRITERS: usize = 16;
    let mut pipe_fds = [-1; 2];
    assert_eq!(unsafe { libc::pipe(pipe_fds.as_mut_ptr()) }, 0);
    let start = Arc::new(Barrier::new(WRITERS));
    let writers: Vec<_> = (0..WRITERS)
        .map(|_| {
            let start = Arc::clone(&start);
            let write_fd = pipe_fds[1];
            thread::spawn(move || {
                start.wait();
                let converted = unsafe { glyph_string_from_i32(42) };
                assert_eq!(c_string(converted), "42");
                unsafe { libc::free(converted.cast()) };
                unsafe { glyph_fmt_write_str(write_fd, c"glyph\n".as_ptr()) }
            })
        })
        .collect();
    for writer in writers {
        assert_eq!(writer.join().expect("formatting writer"), 6);
    }
    assert_eq!(unsafe { libc::close(pipe_fds[1]) }, 0);

    let mut output = [0_u8; WRITERS * 6];
    let mut read = 0;
    while read < output.len() {
        let count = unsafe {
            libc::read(
                pipe_fds[0],
                output[read..].as_mut_ptr().cast(),
                output.len() - read,
            )
        };
        assert!(
            count >= 0,
            "pipe read failed: {}",
            std::io::Error::last_os_error()
        );
        if count == 0 {
            break;
        }
        read += count as usize;
    }
    assert_eq!(unsafe { libc::close(pipe_fds[0]) }, 0);
    assert_eq!(read, output.len());
    assert!(output.chunks_exact(6).all(|line| line == b"glyph\n"));
}

#[test]
fn synchronous_process_launches_are_independent() {
    const LAUNCHES: usize = 8;
    let start = Arc::new(Barrier::new(LAUNCHES));
    let launches: Vec<_> = (0..LAUNCHES)
        .map(|_| {
            let start = Arc::clone(&start);
            thread::spawn(move || {
                let args = GlyphVec {
                    data: std::ptr::null_mut(),
                    len: 0,
                    cap: 0,
                };
                start.wait();
                unsafe { glyph_process_run(c"/usr/bin/true".as_ptr(), &args) }
            })
        })
        .collect();
    for launch in launches {
        assert_eq!(launch.join().expect("process launcher"), 0);
    }
}

#[test]
fn independent_file_handles_do_not_share_stream_state() {
    const FILES: usize = 8;
    let all_open = Arc::new(Barrier::new(FILES + 1));
    let unique = format!("{}-file-concurrency", std::process::id());
    let paths: Vec<_> = (0..FILES)
        .map(|index| std::env::temp_dir().join(format!("glyph-{unique}-{index}.bin")))
        .collect();
    let writers: Vec<_> = paths
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, path)| {
            let all_open = Arc::clone(&all_open);
            thread::spawn(move || {
                let path = CString::new(path.to_string_lossy().as_bytes()).expect("file path");
                let file = unsafe { libc::fopen(path.as_ptr(), c"wb".as_ptr()) };
                all_open.wait();
                if file.is_null() {
                    return -1;
                }
                let payload = [index as u8; 32];
                let written =
                    unsafe { libc::fwrite(payload.as_ptr().cast(), 1, payload.len(), file) };
                if written != payload.len() || unsafe { libc::fclose(file) } != 0 {
                    return -1;
                }
                0
            })
        })
        .collect();
    all_open.wait();
    for writer in writers {
        assert_eq!(writer.join().expect("file writer"), 0);
    }
    for (index, path) in paths.into_iter().enumerate() {
        assert_eq!(fs::read(&path).expect("file output"), vec![index as u8; 32]);
        fs::remove_file(path).expect("remove file output");
    }
}

#[test]
fn concurrent_wav_writers_receive_unique_live_handles() {
    const WRITERS: usize = 8;
    let all_open = Arc::new(Barrier::new(WRITERS + 1));
    let (handle_tx, handle_rx) = mpsc::channel();
    let unique = format!("{}-runtime-concurrency", std::process::id());
    let paths: Vec<_> = (0..WRITERS)
        .map(|index| std::env::temp_dir().join(format!("glyph-{unique}-{index}.wav")))
        .collect();

    let writers: Vec<_> = paths
        .iter()
        .cloned()
        .map(|path| {
            let all_open = Arc::clone(&all_open);
            let handle_tx = handle_tx.clone();
            thread::spawn(move || {
                let path = CString::new(path.to_string_lossy().as_bytes()).expect("WAV path");
                let handle = unsafe { glyph_audio_wav_open(path.as_ptr(), 48_000, 1) };
                handle_tx.send(handle).expect("send WAV handle");
                all_open.wait();
                if handle < 0 {
                    return handle;
                }

                let mut samples = [-0.5_f64, 0.5_f64];
                let samples = GlyphVec {
                    data: samples.as_mut_ptr().cast(),
                    len: samples.len() as i64,
                    cap: samples.len() as i64,
                };
                assert_eq!(unsafe { glyph_audio_wav_write(handle, &samples) }, 2);
                unsafe { glyph_audio_wav_close(handle) }
            })
        })
        .collect();
    drop(handle_tx);

    let handles: Vec<_> = (0..WRITERS)
        .map(|_| handle_rx.recv().expect("receive WAV handle"))
        .collect();
    all_open.wait();

    assert!(handles.iter().all(|handle| *handle >= 0));
    let mut sorted_handles = handles;
    sorted_handles.sort_unstable();
    sorted_handles.dedup();
    assert_eq!(sorted_handles.len(), WRITERS);
    for writer in writers {
        assert_eq!(writer.join().expect("WAV writer"), 0);
    }

    for path in paths {
        assert_eq!(fs::metadata(&path).expect("WAV output").len(), 48);
        fs::remove_file(path).expect("remove WAV output");
    }
}
