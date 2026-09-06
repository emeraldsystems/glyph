// Integration tests for the Apex Web Server
//
// Tests verify: compilation, HTTP request parsing, response building,
// and TCP loopback request handling.

use glyph_frontend::{FrontendOptions, compile_source};

#[cfg(all(feature = "codegen", unix))]
use glyph_backend::{
    codegen::CodegenContext,
    linker::{Linker, LinkerOptions},
};

#[cfg(all(feature = "codegen", unix))]
use std::collections::BTreeMap;
#[cfg(all(feature = "codegen", unix))]
use std::collections::BTreeSet;
#[cfg(all(feature = "codegen", unix))]
use std::os::unix::process::ExitStatusExt;

#[cfg(all(feature = "codegen", unix))]
use std::process::Command;

#[cfg(all(feature = "codegen", unix))]
use tempfile::TempDir;

#[cfg(all(feature = "codegen", unix))]
fn build_and_run_exit_code(source: &str) -> i32 {
    if std::env::var("GLYPH_SKIP_RUN_MAIN").is_ok() {
        return 0;
    }

    let frontend_output = compile_source(
        source,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );

    assert!(
        frontend_output.diagnostics.is_empty(),
        "Compilation failed with diagnostics: {:?}",
        frontend_output.diagnostics
    );

    let temp = TempDir::new().unwrap();
    let obj_path = temp.path().join("test.o");
    let exe_path = temp.path().join("test_exe");

    let mut ctx = CodegenContext::new("glyph_module").unwrap();
    ctx.codegen_module(&frontend_output.mir).unwrap();

    if std::env::var("GLYPH_SKIP_RUN").is_ok() {
        return 0;
    }
    ctx.emit_object_file(&obj_path).unwrap();

    let linker = Linker::new();
    let opts = LinkerOptions {
        output_path: exe_path.clone(),
        object_files: vec![obj_path],
        link_libs: Vec::new(),
        link_search_paths: Vec::new(),
        runtime_lib_path: Linker::get_runtime_lib_path(),
    };
    linker.link(&opts).unwrap();

    let status = Command::new(&exe_path).status().unwrap();
    if let Some(code) = status.code() {
        code
    } else if let Some(sig) = status.signal() {
        -sig
    } else {
        -1
    }
}

/// Combine multiple Glyph source fragments into a single compilable source.
/// - Strips inter-module imports (from http/server import ...)
/// - Merges std imports by module (deduplicating symbols)
/// - Places all imports at top, followed by code
#[cfg(all(feature = "codegen", unix))]
fn combine_sources(fragments: &[&str]) -> String {
    let mut module_imports: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut code_lines: Vec<&str> = Vec::new();

    for fragment in fragments {
        for line in fragment.lines() {
            let trimmed = line.trim();
            // Skip inter-module imports
            if trimmed.starts_with("from http import") || trimmed.starts_with("from server import")
            {
                continue;
            }
            // Skip extern "C" declarations — these come from std/net via include_std
            if trimmed.starts_with("extern \"C\"") {
                continue;
            }
            // Parse and merge std imports
            if trimmed.starts_with("from ") && trimmed.contains(" import ") {
                if let Some(rest) = trimmed.strip_prefix("from ") {
                    if let Some(idx) = rest.find(" import ") {
                        let module = rest[..idx].to_string();
                        let symbols = &rest[idx + 8..];
                        for sym in symbols.split(',') {
                            let s = sym.trim().to_string();
                            if !s.is_empty() {
                                module_imports.entry(module.clone()).or_default().insert(s);
                            }
                        }
                    }
                }
                continue;
            }
            code_lines.push(line);
        }
    }

    let mut result = String::new();
    for (module, syms) in &module_imports {
        let sym_list: Vec<_> = syms.iter().map(|s| s.as_str()).collect();
        result.push_str(&format!("from {} import {}\n", module, sym_list.join(", ")));
    }
    result.push('\n');
    result.push_str(&code_lines.join("\n"));
    result
}

// ---------------------------------------------------------------------------
// Test 1: All Apex modules compile together without errors
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn apex_source_compiles() {
    let http_src = include_str!("../../../examples/apex/src/http.glyph");
    let server_src = include_str!("../../../examples/apex/src/server.glyph");
    let test_main = "fn main() -> i32 { ret 0 }";
    let source = combine_sources(&[http_src, server_src, test_main]);
    assert_eq!(build_and_run_exit_code(&source), 0);
}

// ---------------------------------------------------------------------------
// Test 1b: The *actual* multi-module project (main.glyph + http.glyph +
// server.glyph, with real inter-module imports, not `combine_sources`'s
// stand-in) frontend-compiles with zero diagnostics.
//
// GLYPH-80 regression test: `apex_source_compiles` above never actually
// exercises main.glyph — it discards it and substitutes a trivial stub, so
// it stayed green while `examples/apex` failed to build with a real
// move-checker error ("use of moved value `stream`" at main.glyph:48,
// caused by main.glyph moving `stream` into `serve_request` and then still
// calling `stream.close()`). This test compiles the project the same way
// `glyph build`/`glyph-cli` do (`compile_modules` over the module graph,
// keyed by module id, with real `from http import ...` / `from server
// import ...` edges) so a regression here fails `cargo test`, not just the
// release matrix.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn apex_full_project_compiles_with_no_diagnostics() {
    use glyph_frontend::{ParseOutput, compile_modules, lex, parse};
    use std::collections::HashMap;

    let sources: [(&str, &str); 3] = [
        ("main", include_str!("../../../examples/apex/src/main.glyph")),
        ("http", include_str!("../../../examples/apex/src/http.glyph")),
        ("server", include_str!("../../../examples/apex/src/server.glyph")),
    ];

    let mut modules = HashMap::new();
    for (id, src) in sources {
        let lex_out = lex(src);
        assert!(
            lex_out.diagnostics.is_empty(),
            "{id}: lex diagnostics: {:?}",
            lex_out.diagnostics
        );
        let ParseOutput { module, diagnostics } = parse(&lex_out.tokens, src);
        assert!(
            diagnostics.is_empty(),
            "{id}: parse diagnostics: {:?}",
            diagnostics
        );
        modules.insert(id.to_string(), module);
    }

    let output = compile_modules(
        modules,
        "main",
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );

    assert!(
        output.diagnostics.is_empty(),
        "examples/apex failed to compile as the real multi-module project \
         `glyph build` compiles (a TcpStream moved into a helper function \
         and not reused by the caller is the GLYPH-80 shape). Diagnostics: {:?}",
        output.diagnostics
    );
}

// ---------------------------------------------------------------------------
// Test 2: Parse a well-formed GET request
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn apex_parse_get_request() {
    let http_src = include_str!("../../../examples/apex/src/http.glyph");
    let test_main = r#"
fn main() -> i32 {
  let raw = String::from_str("GET /index.html HTTP/1.1\r\nHost: localhost\r\n\r\n")
  let req = parse_request(raw)
  if req.valid != 1 { ret 1 }

  let m: str = req.method
  if m != "GET" { ret 2 }

  let p: str = req.path
  if p != "/index.html" { ret 3 }

  let h: str = req.host
  if h != "localhost" { ret 4 }

  let v: str = req.version
  if v != "HTTP/1.1" { ret 5 }

  ret 0
}
"#;
    let source = combine_sources(&[http_src, test_main]);
    assert_eq!(build_and_run_exit_code(&source), 0);
}

// ---------------------------------------------------------------------------
// Test 3: Parse a well-formed HEAD request
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn apex_parse_head_request() {
    let http_src = include_str!("../../../examples/apex/src/http.glyph");
    let test_main = r#"
fn main() -> i32 {
  let raw = String::from_str("HEAD / HTTP/1.1\r\nHost: example.com\r\n\r\n")
  let req = parse_request(raw)
  if req.valid != 1 { ret 1 }

  let m: str = req.method
  if m != "HEAD" { ret 2 }

  let p: str = req.path
  if p != "/" { ret 3 }

  ret 0
}
"#;
    let source = combine_sources(&[http_src, test_main]);
    assert_eq!(build_and_run_exit_code(&source), 0);
}

// ---------------------------------------------------------------------------
// Test 4: Malformed requests produce valid=0
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn apex_parse_malformed_request() {
    let http_src = include_str!("../../../examples/apex/src/http.glyph");
    let test_main = r#"
fn main() -> i32 {
  // Empty string -> invalid
  let empty = String::from_str("")
  let req1 = parse_request(empty)
  if req1.valid != 0 { ret 1 }

  // Single word, no path -> invalid (parts_len < 2)
  let no_path = String::from_str("GET\r\n\r\n")
  let req2 = parse_request(no_path)
  if req2.valid != 0 { ret 2 }

  ret 0
}
"#;
    let source = combine_sources(&[http_src, test_main]);
    assert_eq!(build_and_run_exit_code(&source), 0);
}

// ---------------------------------------------------------------------------
// Test 5: build_response produces valid HTTP/1.1 200 OK response
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn apex_build_response_200() {
    let http_src = include_str!("../../../examples/apex/src/http.glyph");
    let test_main = r#"
from std/string import string_index_of

fn main() -> i32 {
  let resp = build_response(200, "OK", "text/html", "<h1>Hi</h1>")
  let rs: str = resp

  // Must contain status line
  let idx1: i64 = string_index_of(rs, "HTTP/1.1 200 OK")
  if idx1 < 0 { ret 1 }

  // Must contain Content-Type header
  let idx2: i64 = string_index_of(rs, "Content-Type: text/html")
  if idx2 < 0 { ret 2 }

  // Must contain Server header
  let idx3: i64 = string_index_of(rs, "Server: Apex")
  if idx3 < 0 { ret 3 }

  // Must contain Connection: close
  let idx4: i64 = string_index_of(rs, "Connection: close")
  if idx4 < 0 { ret 4 }

  // Must contain the body
  let idx5: i64 = string_index_of(rs, "<h1>Hi</h1>")
  if idx5 < 0 { ret 5 }

  // Content-Length should be 11 (len of "<h1>Hi</h1>")
  let idx6: i64 = string_index_of(rs, "Content-Length: 11")
  if idx6 < 0 { ret 6 }

  ret 0
}
"#;
    let source = combine_sources(&[http_src, test_main]);
    assert_eq!(build_and_run_exit_code(&source), 0);
}

// ---------------------------------------------------------------------------
// Test 6: build_error_response produces 404 with HTML body
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn apex_build_error_response_404() {
    let http_src = include_str!("../../../examples/apex/src/http.glyph");
    let test_main = r#"
from std/string import string_index_of

fn main() -> i32 {
  let resp = build_error_response(404, "Not Found")
  let rs: str = resp

  // Must contain 404 status line
  let idx1: i64 = string_index_of(rs, "HTTP/1.1 404 Not Found")
  if idx1 < 0 { ret 1 }

  // Must contain HTML error body
  let idx2: i64 = string_index_of(rs, "<h1>404 Not Found</h1>")
  if idx2 < 0 { ret 2 }

  // Must contain Content-Type: text/html
  let idx3: i64 = string_index_of(rs, "Content-Type: text/html")
  if idx3 < 0 { ret 3 }

  ret 0
}
"#;
    let source = combine_sources(&[http_src, test_main]);
    assert_eq!(build_and_run_exit_code(&source), 0);
}

// ---------------------------------------------------------------------------
// Test 7: TCP loopback — send HTTP request, receive HTTP response
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn apex_loopback_get() {
    let http_src = include_str!("../../../examples/apex/src/http.glyph");
    let test_main = r#"
from std/net import tcp_listen, tcp_connect, TcpListener, TcpStream, NetError
from std/string import string_index_of

fn main() -> i32 {
  // Bind listener on ephemeral port
  let lr: Result<TcpListener, NetError> = tcp_listen("127.0.0.1", 0, 1)
  let mut listener = match lr {
    Ok(l) => l,
    Err(_e) => { ret 1 },
  }
  let port = listener.local_port()
  if port == 0 { ret 2 }

  // Connect client
  let cr: Result<TcpStream, NetError> = tcp_connect("127.0.0.1", port)
  let mut client = match cr {
    Ok(c) => c,
    Err(_e) => { ret 3 },
  }

  // Accept server-side stream
  let ar: Result<TcpStream, NetError> = listener.accept()
  let mut server = match ar {
    Ok(s) => s,
    Err(_e) => { ret 4 },
  }

  // Client sends HTTP GET request
  let send_result = client.send("GET /test.html HTTP/1.1\r\nHost: localhost\r\n\r\n")
  match send_result {
    Ok(_n) => {},
    Err(_e) => { ret 5 },
  }

  // Server receives request
  let recv_result: Result<String, NetError> = server.recv(8192)
  let raw = match recv_result {
    Ok(d) => d,
    Err(_e) => { ret 6 },
  }

  // Server parses request
  let req = parse_request(raw)
  if req.valid != 1 { ret 7 }

  let m: str = req.method
  if m != "GET" { ret 8 }

  // Server builds and sends response
  let response = build_response(200, "OK", "text/html", "<p>test</p>")
  let resp_str: str = response
  let sr = server.send(resp_str)
  match sr {
    Ok(_n) => {},
    Err(_e) => { ret 9 },
  }

  // Close server side so client recv doesn't block
  let _ = server.close()

  // Client receives response
  let client_recv: Result<String, NetError> = client.recv(8192)
  let resp_data = match client_recv {
    Ok(d) => d,
    Err(_e) => { ret 10 },
  }

  // Verify response contains expected status and body
  let rd: str = resp_data
  let idx1: i64 = string_index_of(rd, "HTTP/1.1 200 OK")
  if idx1 < 0 { ret 11 }

  let idx2: i64 = string_index_of(rd, "<p>test</p>")
  if idx2 < 0 { ret 12 }

  // Cleanup
  let _ = client.close()
  let _ = listener.close()
  ret 0
}
"#;
    let source = combine_sources(&[http_src, test_main]);
    assert_eq!(build_and_run_exit_code(&source), 0);
}

// ===========================================================================
// Path Resolution Tests (server.glyph :: resolve_path)
// ===========================================================================

// ---------------------------------------------------------------------------
// Test 8: resolve_path maps normal paths correctly
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn apex_resolve_path_normal() {
    let http_src = include_str!("../../../examples/apex/src/http.glyph");
    let server_src = include_str!("../../../examples/apex/src/server.glyph");
    let test_main = r#"
fn main() -> i32 {
  // "/index.html" with doc_root "www" -> "www/index.html"
  let r1 = resolve_path("/index.html", "www")
  let s1: str = r1
  if s1 != "www/index.html" { ret 1 }

  // "/css/style.css" -> "www/css/style.css"
  let r2 = resolve_path("/css/style.css", "www")
  let s2: str = r2
  if s2 != "www/css/style.css" { ret 2 }

  ret 0
}
"#;
    let source = combine_sources(&[http_src, server_src, test_main]);
    assert_eq!(build_and_run_exit_code(&source), 0);
}

// ---------------------------------------------------------------------------
// Test 9: resolve_path rejects path traversal (..)
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn apex_resolve_path_traversal() {
    let http_src = include_str!("../../../examples/apex/src/http.glyph");
    let server_src = include_str!("../../../examples/apex/src/server.glyph");
    let test_main = r#"
fn main() -> i32 {
  // ".." in path -> empty string (rejected)
  let r1 = resolve_path("/../etc/passwd", "www")
  let s1: str = r1
  if s1.len() != 0 { ret 1 }

  let r2 = resolve_path("/foo/../bar", "www")
  let s2: str = r2
  if s2.len() != 0 { ret 2 }

  let r3 = resolve_path("..", "www")
  let s3: str = r3
  if s3.len() != 0 { ret 3 }

  ret 0
}
"#;
    let source = combine_sources(&[http_src, server_src, test_main]);
    assert_eq!(build_and_run_exit_code(&source), 0);
}

// ---------------------------------------------------------------------------
// Test 10: resolve_path defaults "/" and "" to index.html
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn apex_resolve_path_default_index() {
    let http_src = include_str!("../../../examples/apex/src/http.glyph");
    let server_src = include_str!("../../../examples/apex/src/server.glyph");
    let test_main = r#"
fn main() -> i32 {
  // "/" -> "www/index.html"
  let r1 = resolve_path("/", "www")
  let s1: str = r1
  if s1 != "www/index.html" { ret 1 }

  ret 0
}
"#;
    let source = combine_sources(&[http_src, server_src, test_main]);
    assert_eq!(build_and_run_exit_code(&source), 0);
}

// ===========================================================================
// MIME Type Tests (server.glyph :: guess_content_type)
// ===========================================================================

// ---------------------------------------------------------------------------
// Test 11: guess_content_type returns correct MIME types
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn apex_guess_content_type() {
    let http_src = include_str!("../../../examples/apex/src/http.glyph");
    let server_src = include_str!("../../../examples/apex/src/server.glyph");
    let test_main = r#"
fn main() -> i32 {
  if guess_content_type("page.html") != "text/html" { ret 1 }
  if guess_content_type("style.css") != "text/css" { ret 2 }
  if guess_content_type("app.js") != "application/javascript" { ret 3 }
  if guess_content_type("data.json") != "application/json" { ret 4 }
  if guess_content_type("readme.txt") != "text/plain" { ret 5 }
  if guess_content_type("icon.svg") != "image/svg+xml" { ret 6 }
  if guess_content_type("feed.xml") != "application/xml" { ret 7 }

  // Unknown extension -> octet-stream
  if guess_content_type("archive.tar") != "application/octet-stream" { ret 8 }
  if guess_content_type("noext") != "application/octet-stream" { ret 9 }

  ret 0
}
"#;
    let source = combine_sources(&[http_src, server_src, test_main]);
    assert_eq!(build_and_run_exit_code(&source), 0);
}

// ===========================================================================
// handle_request Tests (server.glyph — bypass parse_request by constructing
// HttpRequest manually with owned Strings)
// ===========================================================================

// ---------------------------------------------------------------------------
// Test 12: handle_request returns 400 for invalid request (valid=0)
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn apex_handle_bad_request() {
    let http_src = include_str!("../../../examples/apex/src/http.glyph");
    let server_src = include_str!("../../../examples/apex/src/server.glyph");
    let test_main = r#"
from std/string import string_index_of

fn main() -> i32 {
  let req = HttpRequest {
    method: String::from_str(""),
    path: String::from_str(""),
    version: String::from_str("HTTP/1.1"),
    host: String::from_str(""),
    valid: 0,
  }
  let resp = handle_request(req, "www")
  let rs: str = resp

  let idx: i64 = string_index_of(rs, "400 Bad Request")
  if idx < 0 { ret 1 }

  ret 0
}
"#;
    let source = combine_sources(&[http_src, server_src, test_main]);
    assert_eq!(build_and_run_exit_code(&source), 0);
}

// ---------------------------------------------------------------------------
// Test 13: handle_request returns 405 for unsupported method (POST)
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn apex_handle_method_not_allowed() {
    let http_src = include_str!("../../../examples/apex/src/http.glyph");
    let server_src = include_str!("../../../examples/apex/src/server.glyph");
    let test_main = r#"
from std/string import string_index_of

fn main() -> i32 {
  let req = HttpRequest {
    method: String::from_str("POST"),
    path: String::from_str("/index.html"),
    version: String::from_str("HTTP/1.1"),
    host: String::from_str("localhost"),
    valid: 1,
  }
  let resp = handle_request(req, "www")
  let rs: str = resp

  let idx: i64 = string_index_of(rs, "405 Method Not Allowed")
  if idx < 0 { ret 1 }

  ret 0
}
"#;
    let source = combine_sources(&[http_src, server_src, test_main]);
    assert_eq!(build_and_run_exit_code(&source), 0);
}

// ---------------------------------------------------------------------------
// Test 14: handle_request returns 404 for missing file
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn apex_handle_file_not_found() {
    let http_src = include_str!("../../../examples/apex/src/http.glyph");
    let server_src = include_str!("../../../examples/apex/src/server.glyph");
    let test_main = r#"
from std/string import string_index_of

fn main() -> i32 {
  let req = HttpRequest {
    method: String::from_str("GET"),
    path: String::from_str("/nonexistent.html"),
    version: String::from_str("HTTP/1.1"),
    host: String::from_str(""),
    valid: 1,
  }
  let resp = handle_request(req, "/tmp/apex_no_such_dir_9999")
  let rs: str = resp

  let idx: i64 = string_index_of(rs, "404 Not Found")
  if idx < 0 { ret 1 }

  ret 0
}
"#;
    let source = combine_sources(&[http_src, server_src, test_main]);
    assert_eq!(build_and_run_exit_code(&source), 0);
}

// ---------------------------------------------------------------------------
// Test 15: handle_request returns 403 for path traversal attempt
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn apex_handle_forbidden_traversal() {
    let http_src = include_str!("../../../examples/apex/src/http.glyph");
    let server_src = include_str!("../../../examples/apex/src/server.glyph");
    let test_main = r#"
from std/string import string_index_of

fn main() -> i32 {
  let req = HttpRequest {
    method: String::from_str("GET"),
    path: String::from_str("/../../../etc/passwd"),
    version: String::from_str("HTTP/1.1"),
    host: String::from_str(""),
    valid: 1,
  }
  let resp = handle_request(req, "www")
  let rs: str = resp

  let idx: i64 = string_index_of(rs, "403 Forbidden")
  if idx < 0 { ret 1 }

  ret 0
}
"#;
    let source = combine_sources(&[http_src, server_src, test_main]);
    assert_eq!(build_and_run_exit_code(&source), 0);
}

// ===========================================================================
// Binary File Serving Tests (Task #14)
// Tests for serve_request(), send_file(), file_size(), and binary MIME types.
// ===========================================================================

// ---------------------------------------------------------------------------
// Test 16: guess_content_type returns correct MIME types for binary files
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn apex_guess_content_type_binary() {
    let http_src = include_str!("../../../examples/apex/src/http.glyph");
    let server_src = include_str!("../../../examples/apex/src/server.glyph");
    let test_main = r#"
fn main() -> i32 {
  if guess_content_type("photo.png") != "image/png" { ret 1 }
  if guess_content_type("photo.jpg") != "image/jpeg" { ret 2 }
  if guess_content_type("photo.jpeg") != "image/jpeg" { ret 3 }
  if guess_content_type("anim.gif") != "image/gif" { ret 4 }
  if guess_content_type("favicon.ico") != "image/x-icon" { ret 5 }
  if guess_content_type("font.woff") != "font/woff" { ret 6 }
  if guess_content_type("font.woff2") != "font/woff2" { ret 7 }
  if guess_content_type("doc.pdf") != "application/pdf" { ret 8 }
  if guess_content_type("image.webp") != "image/webp" { ret 9 }
  ret 0
}
"#;
    let source = combine_sources(&[http_src, server_src, test_main]);
    assert_eq!(build_and_run_exit_code(&source), 0);
}

// ---------------------------------------------------------------------------
// Test 17: file_size returns correct size for existing file, -1 for missing
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn apex_file_size() {
    // Create a temp file with known content
    let temp = TempDir::new().unwrap();
    let test_file = temp.path().join("test_size.txt");
    std::fs::write(&test_file, "hello world").unwrap(); // 11 bytes

    let file_path = test_file.to_str().unwrap();
    let http_src = include_str!("../../../examples/apex/src/http.glyph");
    let server_src = include_str!("../../../examples/apex/src/server.glyph");
    let test_main = format!(
        r#"
from std/net import file_size

fn main() -> i32 {{
  // Known file: should be 11 bytes
  let sz = file_size("{file_path}")
  if sz != 11 {{ ret 1 }}

  // Nonexistent file: should return -1
  let sz2 = file_size("/tmp/glyph_no_such_file_99999.txt")
  if sz2 != -1 {{ ret 2 }}

  ret 0
}}
"#
    );
    let source = combine_sources(&[http_src, server_src, &test_main]);
    assert_eq!(build_and_run_exit_code(&source), 0);
}

// ---------------------------------------------------------------------------
// Test 18: build_file_headers produces correct headers with i64 content length
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn apex_build_file_headers() {
    let http_src = include_str!("../../../examples/apex/src/http.glyph");
    let test_main = r#"
from std/string import string_index_of

fn main() -> i32 {
  let headers = build_file_headers(200, "OK", "image/png", 4096)
  let hs: str = headers

  // Must contain status line
  let idx1: i64 = string_index_of(hs, "HTTP/1.1 200 OK")
  if idx1 < 0 { ret 1 }

  // Must contain Content-Type
  let idx2: i64 = string_index_of(hs, "Content-Type: image/png")
  if idx2 < 0 { ret 2 }

  // Must contain Content-Length: 4096
  let idx3: i64 = string_index_of(hs, "Content-Length: 4096")
  if idx3 < 0 { ret 3 }

  // Must contain Connection: close
  let idx4: i64 = string_index_of(hs, "Connection: close")
  if idx4 < 0 { ret 4 }

  // Must contain Server header
  let idx5: i64 = string_index_of(hs, "Server: Apex")
  if idx5 < 0 { ret 5 }

  // Must end with double CRLF (header/body separator)
  if hs.ends_with("\r\n\r\n") {
    ret 0
  }
  ret 6
}
"#;
    let source = combine_sources(&[http_src, test_main]);
    assert_eq!(build_and_run_exit_code(&source), 0);
}

// ---------------------------------------------------------------------------
// Test 19: serve_request streams binary file over TCP (loopback with send_file)
// Writes a binary file with null bytes, serves it via serve_request, verifies
// the client receives correct headers and the full binary content.
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn apex_serve_binary_file_loopback() {
    // Create a temp directory with a binary file containing null bytes
    let temp = TempDir::new().unwrap();
    let www_dir = temp.path().join("www");
    std::fs::create_dir(&www_dir).unwrap();
    let bin_file = www_dir.join("test.bin");
    // 16 bytes: includes null bytes to verify no truncation
    let bin_content: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x01,
        0x00,
    ];
    std::fs::write(&bin_file, &bin_content).unwrap();

    let doc_root = www_dir.to_str().unwrap();
    let http_src = include_str!("../../../examples/apex/src/http.glyph");
    let server_src = include_str!("../../../examples/apex/src/server.glyph");
    let test_main = format!(
        r#"
from std/net import tcp_listen, tcp_connect, TcpListener, TcpStream, NetError

fn main() -> i32 {{
  // Bind listener on ephemeral port
  let lr: Result<TcpListener, NetError> = tcp_listen("127.0.0.1", 0, 1)
  let mut listener = match lr {{
    Ok(l) => l,
    Err(_e) => {{ ret 1 }},
  }}
  let port = listener.local_port()

  // Connect client
  let cr: Result<TcpStream, NetError> = tcp_connect("127.0.0.1", port)
  let mut client = match cr {{
    Ok(c) => c,
    Err(_e) => {{ ret 2 }},
  }}

  // Accept server-side stream
  let ar: Result<TcpStream, NetError> = listener.accept()
  let mut server = match ar {{
    Ok(s) => s,
    Err(_e) => {{ ret 3 }},
  }}

  // Client sends GET request for binary file
  let _ = client.send("GET /test.bin HTTP/1.1\r\nHost: localhost\r\n\r\n")

  // Server receives and parses request
  let recv_result: Result<String, NetError> = server.recv(8192)
  let raw = match recv_result {{
    Ok(d) => d,
    Err(_e) => {{ ret 4 }},
  }}
  let request = parse_request(raw)
  if request.valid != 1 {{ ret 5 }}

  // Server serves the request (streams headers + binary body)
  let status = serve_request(request, "{doc_root}", server)
  if status != 200 {{ ret 6 }}

  // Client reads response (headers + binary body)
  // Read enough to get headers — binary portion may not be valid UTF-8
  // but recv returns raw bytes as String
  let client_recv: Result<String, NetError> = client.recv(8192)
  let resp_data = match client_recv {{
    Ok(d) => d,
    Err(_e) => {{ ret 7 }},
  }}

  // Check that we got back data (headers + 16 bytes body)
  let rd: str = resp_data
  // response must be non-empty: headers are at least ~100 bytes + 16 bytes body
  if rd.len() < 50 {{ ret 8 }}

  // Cleanup
  let _ = client.close()
  let _ = listener.close()
  ret 0
}}
"#
    );
    let source = combine_sources(&[http_src, server_src, &test_main]);
    assert_eq!(build_and_run_exit_code(&source), 0);
}

// ---------------------------------------------------------------------------
// Test 20: serve_request returns 405 for unsupported method over TCP
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn apex_serve_request_method_not_allowed() {
    let http_src = include_str!("../../../examples/apex/src/http.glyph");
    let server_src = include_str!("../../../examples/apex/src/server.glyph");
    let test_main = r#"
from std/net import tcp_listen, tcp_connect, TcpListener, TcpStream, NetError
from std/string import string_index_of

fn main() -> i32 {
  // Bind listener
  let lr: Result<TcpListener, NetError> = tcp_listen("127.0.0.1", 0, 1)
  let mut listener = match lr {
    Ok(l) => l,
    Err(_e) => { ret 1 },
  }
  let port = listener.local_port()

  // Connect client
  let cr: Result<TcpStream, NetError> = tcp_connect("127.0.0.1", port)
  let mut client = match cr {
    Ok(c) => c,
    Err(_e) => { ret 2 },
  }

  // Accept server-side stream
  let ar: Result<TcpStream, NetError> = listener.accept()
  let mut server = match ar {
    Ok(s) => s,
    Err(_e) => { ret 3 },
  }

  // Client sends DELETE request (unsupported method)
  let _ = client.send("DELETE /file.txt HTTP/1.1\r\nHost: localhost\r\n\r\n")

  // Server receives and parses
  let recv_result: Result<String, NetError> = server.recv(8192)
  let raw = match recv_result {
    Ok(d) => d,
    Err(_e) => { ret 4 },
  }
  let request = parse_request(raw)

  // Server serves the request
  let status = serve_request(request, "/tmp", server)
  if status != 405 { ret 5 }

  // Close server side so client can read
  // (serve_request already sent response over server stream)

  // Client reads error response
  let client_recv: Result<String, NetError> = client.recv(8192)
  let resp_data = match client_recv {
    Ok(d) => d,
    Err(_e) => { ret 6 },
  }
  let rd: str = resp_data
  let idx: i64 = string_index_of(rd, "405 Method Not Allowed")
  if idx < 0 { ret 7 }

  // Cleanup
  let _ = client.close()
  let _ = listener.close()
  ret 0
}
"#;
    let source = combine_sources(&[http_src, server_src, test_main]);
    assert_eq!(build_and_run_exit_code(&source), 0);
}

// ---------------------------------------------------------------------------
// Test 21: serve_request returns 404 for nonexistent file over TCP
// ---------------------------------------------------------------------------
#[cfg(all(feature = "codegen", unix))]
#[test]
fn apex_serve_request_file_not_found() {
    let http_src = include_str!("../../../examples/apex/src/http.glyph");
    let server_src = include_str!("../../../examples/apex/src/server.glyph");
    let test_main = r#"
from std/net import tcp_listen, tcp_connect, TcpListener, TcpStream, NetError
from std/string import string_index_of

fn main() -> i32 {
  let lr: Result<TcpListener, NetError> = tcp_listen("127.0.0.1", 0, 1)
  let mut listener = match lr {
    Ok(l) => l,
    Err(_e) => { ret 1 },
  }
  let port = listener.local_port()

  let cr: Result<TcpStream, NetError> = tcp_connect("127.0.0.1", port)
  let mut client = match cr {
    Ok(c) => c,
    Err(_e) => { ret 2 },
  }

  let ar: Result<TcpStream, NetError> = listener.accept()
  let mut server = match ar {
    Ok(s) => s,
    Err(_e) => { ret 3 },
  }

  // Request a file that doesn't exist
  let _ = client.send("GET /nope.html HTTP/1.1\r\nHost: localhost\r\n\r\n")

  let recv_result: Result<String, NetError> = server.recv(8192)
  let raw = match recv_result {
    Ok(d) => d,
    Err(_e) => { ret 4 },
  }
  let request = parse_request(raw)

  // Serve with a doc_root that doesn't have this file
  let status = serve_request(request, "/tmp/glyph_no_such_dir_99999", server)
  if status != 404 { ret 5 }

  // Client reads 404 response
  let client_recv: Result<String, NetError> = client.recv(8192)
  let resp_data = match client_recv {
    Ok(d) => d,
    Err(_e) => { ret 6 },
  }
  let rd: str = resp_data
  let idx: i64 = string_index_of(rd, "404 Not Found")
  if idx < 0 { ret 7 }

  let _ = client.close()
  let _ = listener.close()
  ret 0
}
"#;
    let source = combine_sources(&[http_src, server_src, test_main]);
    assert_eq!(build_and_run_exit_code(&source), 0);
}
