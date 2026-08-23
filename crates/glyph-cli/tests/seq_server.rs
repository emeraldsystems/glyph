//! GLYPH-69 acceptance: the sequencer as a JSON-over-stdio server.
//!
//! This is the process the GlyphAudio daemon spawns, so the tests drive it
//! the way a supervisor does — write JSON lines to stdin, read replies and
//! reports from stdout, close stdin to shut it down — rather than through
//! any in-process API.
//!
//! Replies are checked by substring rather than by parsing, matching the
//! other sequencer suites and keeping the dependency set unchanged. The
//! server's own serializer is fixed-format, so the exact strings asserted
//! here are stable.

#![cfg(all(feature = "codegen", unix))]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use tempfile::TempDir;

const CANONICAL_SONG: &str = include_str!("../../../examples/sequencer/song.json");

/// Same value the GLYPH-61 gate pins. Driving the engine over stdio must
/// produce exactly the audio the demo and the acceptance fixture produce —
/// if this and `seq_acceptance.rs` ever disagree, the transport has started
/// changing the render, which is precisely what must never happen.
const EXPECTED_FNV1A: u64 = 0x9baf_8ecd_b86d_b9b7;

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Collapses a JSON document to one line so it can be sent as a single
/// command. Whitespace inside string literals is preserved.
fn compact_json(json: &str) -> String {
    let mut out = String::new();
    let mut in_string = false;
    let mut escaped = false;
    for c in json.chars() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            ' ' | '\n' | '\r' | '\t' => {}
            _ => out.push(c),
        }
    }
    out
}

fn load_song_command() -> String {
    format!(
        "{{\"cmd\":\"load_song\",\"song\":{}}}",
        compact_json(CANONICAL_SONG)
    )
}

/// Copies the shipped example verbatim and builds the server binary.
fn build_server() -> Option<(TempDir, PathBuf)> {
    if std::env::var("GLYPH_SKIP_RUN").is_ok() || std::env::var("GLYPH_SKIP_RUN_MAIN").is_ok() {
        return None;
    }

    let example_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/sequencer");
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    fs::create_dir_all(root.join("src")).unwrap();
    fs::copy(example_dir.join("glyph.toml"), root.join("glyph.toml")).unwrap();
    fs::copy(example_dir.join("song.json"), root.join("song.json")).unwrap();
    for entry in fs::read_dir(example_dir.join("src")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) == Some("glyph") {
            fs::copy(&path, root.join("src").join(path.file_name().unwrap())).unwrap();
        }
    }

    let build = Command::new(env!("CARGO_BIN_EXE_glyph"))
        .arg("build")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "the sequencer example failed to build:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let exe = root.join("target/debug/sequencer");
    assert!(exe.exists(), "expected {}", exe.display());
    Some((temp, exe))
}

/// The server is the demo binary in `--server` mode: one executable to
/// build, ship and bundle.
fn spawn_server(exe: &Path, cwd: &Path, args: &[&str]) -> Child {
    Command::new(exe)
        .arg("--server")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap()
}

fn exit_code(status: ExitStatus, label: &str) -> i32 {
    if let Some(sig) = status.signal() {
        panic!("{} was killed by signal {}", label, sig);
    }
    status.code().unwrap_or(-1)
}

/// The headline: a whole render driven over stdio produces byte-identical
/// audio to the canonical gate.
#[test]
fn stdio_session_renders_the_canonical_song() {
    let Some((temp, exe)) = build_server() else {
        return;
    };
    let root = temp.path();
    let mut child = spawn_server(&exe, root, &["--wav", "server.wav"]);

    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "{}", load_song_command()).unwrap();
    writeln!(stdin, "{{\"cmd\":\"render_to\",\"frame\":384000}}").unwrap();
    stdin.flush().unwrap();

    // Read as we go: this both collects the transcript and keeps the
    // child's stdout pipe drained, which is what a supervisor does.
    let reader = BufReader::new(child.stdout.take().unwrap());
    let mut replies = Vec::new();
    let mut saw_position = false;
    let mut saw_block = false;
    let mut stopped_line = None;
    for line in reader.lines() {
        let line = line.unwrap();
        assert!(
            line.starts_with('{') && line.ends_with('}'),
            "server emitted a non-JSON line: {:?}",
            line
        );
        if line.starts_with("{\"ev\":") {
            if line.starts_with("{\"ev\":\"position\"") {
                saw_position = true;
            }
            if line.starts_with("{\"ev\":\"block\"") {
                saw_block = true;
            }
            if line.starts_with("{\"ev\":\"stopped\"") {
                stopped_line = Some(line);
                break;
            }
        } else {
            replies.push(line);
        }
    }

    assert_eq!(
        stopped_line.as_deref(),
        Some("{\"ev\":\"stopped\",\"frame\":384000}"),
        "expected the render to stop exactly on target"
    );
    assert_eq!(replies.len(), 2, "expected one reply per command");
    assert!(
        replies[0].contains("\"ok\":true") && replies[0].contains("\"notes\":14"),
        "load_song reply was wrong: {}",
        replies[0]
    );
    assert!(
        replies[1].contains("\"ok\":true"),
        "render_to was rejected: {}",
        replies[1]
    );
    assert!(
        saw_position && saw_block,
        "expected both position and block telemetry"
    );

    // Closing stdin is how a supervisor says goodbye.
    drop(stdin);
    assert_eq!(
        exit_code(child.wait().unwrap(), "server"),
        0,
        "server did not exit cleanly on EOF"
    );

    let bytes = fs::read(root.join("server.wav")).unwrap();
    assert_eq!(bytes.len(), 44 + 384_000 * 2, "wrong render length");
    let hash = fnv1a64(&bytes);
    assert_eq!(
        hash, EXPECTED_FNV1A,
        "a render driven over stdio ({:#018x}) no longer matches the canonical \
         gate ({:#018x}) — the transport is changing the audio",
        hash, EXPECTED_FNV1A
    );
}

/// EOF is how the engine learns its parent is gone. It must finalize the
/// sink rather than die holding it — an unpatched RIFF header would leave
/// an unreadable file.
#[test]
fn eof_shuts_down_cleanly_and_finalizes_the_sink() {
    let Some((temp, exe)) = build_server() else {
        return;
    };
    let root = temp.path();

    // No commands at all: straight to EOF.
    let mut child = spawn_server(&exe, root, &["--wav", "empty.wav"]);
    drop(child.stdin.take().unwrap());
    assert_eq!(
        exit_code(child.wait().unwrap(), "server"),
        0,
        "immediate EOF did not exit cleanly"
    );
    let bytes = fs::read(root.join("empty.wav")).unwrap();
    assert_eq!(bytes.len(), 44, "expected a header-only WAV");
    assert_eq!(&bytes[0..4], b"RIFF");

    // EOF part-way through a very long render, with the parent never
    // draining stdout — the case that used to deadlock, because the report
    // thread blocks in write(2) on a full pipe and can never notice the
    // engine finishing. The report thread is detached for exactly this.
    let mut child = spawn_server(&exe, root, &["--wav", "cut.wav"]);
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "{}", load_song_command()).unwrap();
    writeln!(stdin, "{{\"cmd\":\"render_to\",\"frame\":48000000}}").unwrap();
    stdin.flush().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(150));
    drop(stdin);

    assert_eq!(
        exit_code(child.wait().unwrap(), "server"),
        0,
        "EOF mid-render did not exit cleanly"
    );

    let bytes = fs::read(root.join("cut.wav")).unwrap();
    assert_eq!(&bytes[0..4], b"RIFF", "sink was not finalized");
    let frames = (bytes.len() - 44) / 2;
    assert!(
        frames > 0 && frames < 48_000_000,
        "expected a truncated but complete WAV, got {} frames",
        frames
    );
}

/// Bad input must be answered, not fatal. A supervisor keeps the process
/// for a whole session, so one malformed line cannot take the engine out.
#[test]
fn malformed_input_is_answered_and_survivable() {
    let Some((temp, exe)) = build_server() else {
        return;
    };
    let root = temp.path();
    let mut child = spawn_server(&exe, root, &["--wav", "bad.wav"]);

    let mut stdin = child.stdin.take().unwrap();
    // The blank line is skipped; every other line gets a structured reply.
    write!(
        stdin,
        "{{not json\n\n{{\"cmd\":\"fly\"}}\n\
         {{\"cmd\":\"note_on\",\"track\":0,\"pitch\":999,\"velocity\":0.5}}\n\
         {{\"cmd\":\"play\"}}\n"
    )
    .unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    let reader = BufReader::new(child.stdout.take().unwrap());
    let replies: Vec<String> = reader
        .lines()
        .map(|l| l.unwrap())
        .filter(|l| !l.starts_with("{\"ev\":"))
        .collect();

    assert_eq!(
        exit_code(child.wait().unwrap(), "server"),
        0,
        "server died on malformed input"
    );

    assert_eq!(replies.len(), 4, "expected one reply per non-blank line");
    assert!(
        replies[0].contains("\"ok\":false"),
        "malformed JSON should be rejected: {}",
        replies[0]
    );
    assert!(
        replies[1].contains("\"ok\":false"),
        "unknown verb should be rejected: {}",
        replies[1]
    );
    assert!(
        replies[2].contains("\"ok\":false"),
        "out-of-range pitch should be rejected: {}",
        replies[2]
    );
    assert!(
        replies[3].contains("\"ok\":true"),
        "a valid command after bad ones must still work: {}",
        replies[3]
    );
}

/// The sink is chosen at startup, so a missing or malformed choice has to
/// fail loudly rather than start an engine writing nowhere.
#[test]
fn sink_arguments_are_validated() {
    let Some((temp, exe)) = build_server() else {
        return;
    };
    let root = temp.path();

    for (args, expected) in [
        (vec![], "expected --wav <path> or --live"),
        (vec!["--wav"], "--wav needs a path"),
    ] {
        let mut child = spawn_server(&exe, root, &args);
        drop(child.stdin.take().unwrap());
        let out = child.wait_with_output().unwrap();
        assert_eq!(
            exit_code(out.status, "server"),
            2,
            "expected a usage failure for {:?}",
            args
        );
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains("\"ok\":false") && text.contains(expected),
            "usage error for {:?} was {:?}",
            args,
            text
        );
    }
}
