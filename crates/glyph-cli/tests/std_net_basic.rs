use glyph_frontend::{FrontendOptions, compile_source};

#[cfg(all(feature = "codegen", unix))]
use glyph_backend::{
    codegen::CodegenContext,
    linker::{Linker, LinkerOptions},
};

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

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_net_import_compiles() {
    let source = r#"
        from std/net import TcpStream, TcpListener, UdpSocket, NetError
        from std/enums import Result

        fn main() -> i32 {
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_net_tcp_listen_close() {
    let source = r#"
        from std/net import tcp_listen, TcpListener, NetError
        from std/enums import Result

        fn main() -> i32 {
          let result: Result<TcpListener, NetError> = tcp_listen("127.0.0.1", 0, 1)
          ret match result {
            Ok(listener) => {
              let mut active_listener: TcpListener = listener
              let port = active_listener.local_port()
              if port == 0 { ret 10 }
              let _ = active_listener.close()
              ret 0
            },
            Err(_e) => 1,
          }
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_net_udp_bind_close() {
    let source = r#"
        from std/net import udp_bind, UdpSocket, NetError
        from std/enums import Result

        fn main() -> i32 {
          let result: Result<UdpSocket, NetError> = udp_bind("127.0.0.1", 0)
          ret match result {
            Ok(sock) => {
              let mut active_socket: UdpSocket = sock
              let port = active_socket.local_port()
              if port == 0 { ret 10 }
              let _ = active_socket.close()
              ret 0
            },
            Err(_e) => 1,
          }
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_net_tcp_connect_error() {
    let source = r#"
        from std/net import tcp_connect, TcpStream, NetError
        from std/enums import Result

        fn main() -> i32 {
          let result: Result<TcpStream, NetError> = tcp_connect("127.0.0.1", 1)
          ret match result {
            Ok(_stream) => 1,
            Err(_e) => 0,
          }
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_net_tcp_loopback_echo() {
    let source = r#"
        from std/net import tcp_listen, tcp_connect, TcpListener, TcpStream, NetError
        from std/enums import Result

        fn main() -> i32 {
          let lr: Result<TcpListener, NetError> = tcp_listen("127.0.0.1", 0, 1)
          let mut listener = match lr {
            Ok(l) => l,
            Err(_e) => { ret 1 },
          }
          let port = listener.local_port()
          if port == 0 { ret 2 }

          let cr: Result<TcpStream, NetError> = tcp_connect("127.0.0.1", port)
          let mut client = match cr {
            Ok(c) => c,
            Err(_e) => { ret 3 },
          }

          let ar: Result<TcpStream, NetError> = listener.accept()
          let mut server = match ar {
            Ok(s) => s,
            Err(_e) => { ret 4 },
          }

          let sr = client.send("hello")
          match sr {
            Ok(_n) => {},
            Err(_e) => { ret 5 },
          }

          let rr: Result<String, NetError> = server.recv(1024)
          let data = match rr {
            Ok(d) => d,
            Err(_e) => { ret 6 },
          }

          let ds: str = data
          if ds.len() != 5 { ret 7 }

          let _ = client.close()
          let _ = server.close()
          let _ = listener.close()
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}

#[cfg(all(feature = "codegen", unix))]
#[test]
fn std_net_udp_loopback() {
    let source = r#"
        from std/net import udp_bind, UdpSocket, NetError
        from std/enums import Result

        fn main() -> i32 {
          let br: Result<UdpSocket, NetError> = udp_bind("127.0.0.1", 0)
          let mut sock = match br {
            Ok(s) => s,
            Err(_e) => { ret 1 },
          }
          let port = sock.local_port()
          if port == 0 { ret 2 }

          let sr = sock.send_to("hello udp", "127.0.0.1", port)
          match sr {
            Ok(_n) => {},
            Err(_e) => { ret 3 },
          }

          let rr: Result<String, NetError> = sock.recv(1024)
          let data = match rr {
            Ok(d) => d,
            Err(_e) => { ret 4 },
          }

          let ds: str = data
          if ds.len() != 9 { ret 5 }

          let _ = sock.close()
          ret 0
        }
    "#;

    assert_eq!(build_and_run_exit_code(source), 0);
}
