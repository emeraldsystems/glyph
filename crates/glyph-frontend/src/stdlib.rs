use std::collections::HashMap;

use glyph_core::ast::{
    EnumDef, EnumVariantDef, ExternFunctionDecl, Ident, Import, ImportKind, ImportPath,
    InterfaceDef, Module, Param, StructDef, TypeExpr,
};
use glyph_core::span::Span;
use glyph_core::types::Mutability;

use crate::{lexer, parser};

fn tp(name: &str, span: Span) -> TypeExpr {
    TypeExpr::Path {
        segments: vec![name.to_string()],
        span,
    }
}

fn parse_std_source(module_id: &str, source: &str) -> Module {
    let lex_out = lexer::lex(source);
    if !lex_out.diagnostics.is_empty() {
        panic!(
            "failed to lex std module {}: {:?}",
            module_id, lex_out.diagnostics
        );
    }
    let parse_out = parser::parse(&lex_out.tokens, source);
    if !parse_out.diagnostics.is_empty() {
        panic!(
            "failed to parse std module {}: {:?}",
            module_id, parse_out.diagnostics
        );
    }
    parse_out.module
}

/// Return the built-in std modules shipped with the compiler.
/// Currently includes:
/// - std (re-exports std::io)
/// - std/io with puts/raw_write and Write interface
/// - std/enums with generic Option<T> and monomorphic Result<i32>
pub fn std_modules() -> HashMap<String, Module> {
    let mut modules = HashMap::new();
    let span = Span::new(0, 0);

    // std::io
    let stdout_struct = StructDef {
        name: Ident("Stdout".into()),
        generic_params: Vec::new(),
        fields: vec![glyph_core::ast::FieldDef {
            name: Ident("fd".into()),
            ty: tp("i32", span),
            span,
        }],
        interfaces: vec![],
        methods: Vec::new(),
        inline_impls: Vec::new(),
        span,
    };

    let puts_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("puts".into()),
        params: vec![Param {
            name: Ident("msg".into()),
            ty: Some(tp("str", span)),
            span,
        }],
        ret_type: Some(tp("i32", span)),
        link_name: None,
        span,
    };

    let print_str_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("print_str".into()),
        params: vec![Param {
            name: Ident("msg".into()),
            ty: Some(tp("str", span)),
            span,
        }],
        ret_type: Some(tp("i32", span)),
        link_name: Some("glyph_print".into()),
        span,
    };

    let raw_write_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("raw_write".into()),
        params: vec![
            Param {
                name: Ident("fd".into()),
                ty: Some(tp("i32", span)),
                span,
            },
            Param {
                name: Ident("bytes".into()),
                ty: Some(TypeExpr::App {
                    base: Box::new(tp("RawPtr", span)),
                    args: vec![tp("u8", span)],
                    span,
                }),
                span,
            },
            Param {
                name: Ident("len".into()),
                ty: Some(tp("u32", span)),
                span,
            },
        ],
        ret_type: Some(tp("i32", span)),
        link_name: Some("write".into()),
        span,
    };

    let fopen_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("fopen".into()),
        params: vec![
            Param {
                name: Ident("path".into()),
                ty: Some(tp("str", span)),
                span,
            },
            Param {
                name: Ident("mode".into()),
                ty: Some(tp("str", span)),
                span,
            },
        ],
        ret_type: Some(TypeExpr::App {
            base: Box::new(tp("RawPtr", span)),
            args: vec![tp("u8", span)],
            span,
        }),
        link_name: Some("fopen".into()),
        span,
    };

    let write_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("write".into()),
        params: vec![
            Param {
                name: Ident("buf".into()),
                ty: Some(tp("str", span)),
                span,
            },
            Param {
                name: Ident("size".into()),
                ty: Some(tp("u32", span)),
                span,
            },
            Param {
                name: Ident("count".into()),
                ty: Some(tp("u32", span)),
                span,
            },
            Param {
                name: Ident("file".into()),
                ty: Some(TypeExpr::App {
                    base: Box::new(tp("RawPtr", span)),
                    args: vec![tp("u8", span)],
                    span,
                }),
                span,
            },
        ],
        ret_type: Some(tp("u32", span)),
        link_name: Some("fwrite".into()),
        span,
    };

    let read_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("read".into()),
        params: vec![
            Param {
                name: Ident("fd".into()),
                ty: Some(tp("i32", span)),
                span,
            },
            Param {
                name: Ident("buf".into()),
                ty: Some(TypeExpr::App {
                    base: Box::new(tp("RawPtr", span)),
                    args: vec![tp("u8", span)],
                    span,
                }),
                span,
            },
            Param {
                name: Ident("len".into()),
                ty: Some(tp("u32", span)),
                span,
            },
        ],
        ret_type: Some(tp("i32", span)),
        link_name: Some("read".into()),
        span,
    };

    let fclose_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("fclose".into()),
        params: vec![Param {
            name: Ident("file".into()),
            ty: Some(TypeExpr::App {
                base: Box::new(tp("RawPtr", span)),
                args: vec![tp("u8", span)],
                span,
            }),
            span,
        }],
        ret_type: Some(tp("i32", span)),
        link_name: Some("fclose".into()),
        span,
    };

    let fread_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("fread".into()),
        params: vec![
            Param {
                name: Ident("buf".into()),
                ty: Some(TypeExpr::App {
                    base: Box::new(tp("RawPtr", span)),
                    args: vec![tp("u8", span)],
                    span,
                }),
                span,
            },
            Param {
                name: Ident("size".into()),
                ty: Some(tp("u32", span)),
                span,
            },
            Param {
                name: Ident("count".into()),
                ty: Some(tp("u32", span)),
                span,
            },
            Param {
                name: Ident("file".into()),
                ty: Some(TypeExpr::App {
                    base: Box::new(tp("RawPtr", span)),
                    args: vec![tp("u8", span)],
                    span,
                }),
                span,
            },
        ],
        ret_type: Some(tp("u32", span)),
        link_name: Some("fread".into()),
        span,
    };

    let fseek_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("fseek".into()),
        params: vec![
            Param {
                name: Ident("file".into()),
                ty: Some(TypeExpr::App {
                    base: Box::new(tp("RawPtr", span)),
                    args: vec![tp("u8", span)],
                    span,
                }),
                span,
            },
            Param {
                name: Ident("offset".into()),
                ty: Some(tp("i64", span)),
                span,
            },
            Param {
                name: Ident("origin".into()),
                ty: Some(tp("i32", span)),
                span,
            },
        ],
        ret_type: Some(tp("i32", span)),
        link_name: Some("fseek".into()),
        span,
    };

    let ftell_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("ftell".into()),
        params: vec![Param {
            name: Ident("file".into()),
            ty: Some(TypeExpr::App {
                base: Box::new(tp("RawPtr", span)),
                args: vec![tp("u8", span)],
                span,
            }),
            span,
        }],
        ret_type: Some(tp("i64", span)),
        link_name: Some("ftell".into()),
        span,
    };

    let rewind_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("rewind".into()),
        params: vec![Param {
            name: Ident("file".into()),
            ty: Some(TypeExpr::App {
                base: Box::new(tp("RawPtr", span)),
                args: vec![tp("u8", span)],
                span,
            }),
            span,
        }],
        ret_type: None,
        link_name: Some("rewind".into()),
        span,
    };

    let println_extern = ExternFunctionDecl {
        name: Ident("println".into()),
        abi: Some("C".into()),
        params: vec![Param {
            name: Ident("msg".into()),
            ty: Some(tp("str", span)),
            span,
        }],
        ret_type: Some(tp("i32", span)),
        link_name: None,
        span,
    };

    // Reads one line from stdin, INCLUDING its trailing newline.
    //
    // The newline is retained deliberately: it is what makes EOF
    // unambiguous without a second out-parameter, which Glyph cannot
    // express (there are no scalar `&mut` params). A blank line comes back
    // as "\n" (length 1); end of input comes back as "" (length 0).
    //
    // The returned view is valid until the next call ON THE SAME THREAD;
    // copy it with String::from_str to keep it.
    let read_line_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("read_line_raw".into()),
        params: vec![],
        ret_type: Some(tp("str", span)),
        link_name: Some("glyph_io_read_line".into()),
        span,
    };

    // Stops a closed stdout from killing the process: SIGPIPE's default
    // action is immediate termination, which gives a program streaming to a
    // pipe no chance to finalize anything it holds open. Call this once at
    // startup in a server; leave it alone in a filter that should die
    // quietly when its reader closes. No-op where SIGPIPE does not exist.
    let ignore_sigpipe_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("ignore_sigpipe".into()),
        params: vec![],
        ret_type: Some(tp("i32", span)),
        link_name: Some("glyph_io_ignore_sigpipe".into()),
        span,
    };

    // Reads raw bytes, which read_to_string cannot: it produces text, so
    // every binary format -- audio, images, archives, anything with a
    // header -- was previously unreachable from Glyph.
    //
    // THE CALLER SIZES THE BUFFER. `out` must already hold at least `max`
    // elements; push that many zeros, or ask std/net::file_size first. The
    // runtime writes only into memory Glyph already owns and never grows
    // the Vec, because nothing else in the runtime allocates into one and
    // doing so would mean C guessing at the allocator and the ownership
    // rules of a single-owner move language.
    //
    // Returns the count read (which may be short at end of file), or a
    // negative code: -1 null argument, -2 buffer smaller than max, -3 could
    // not open, -4 read error. Distinct on purpose -- a caller wants to
    // tell a missing file from a corrupt one.
    let read_file_bytes_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("read_file_bytes".into()),
        params: vec![
            Param {
                name: Ident("path".into()),
                ty: Some(tp("str", span)),
                span,
            },
            Param {
                name: Ident("out".into()),
                ty: Some(TypeExpr::Ref {
                    mutability: Mutability::Mutable,
                    inner: Box::new(TypeExpr::App {
                        base: Box::new(tp("Vec", span)),
                        args: vec![tp("u8", span)],
                        span,
                    }),
                    span,
                }),
                span,
            },
            Param {
                name: Ident("max".into()),
                ty: Some(tp("i64", span)),
                span,
            },
        ],
        ret_type: Some(tp("i64", span)),
        link_name: Some("glyph_io_read_file_bytes".into()),
        span,
    };

    let file_struct = StructDef {
        name: Ident("File".into()),
        generic_params: vec![],
        fields: vec![glyph_core::ast::FieldDef {
            name: Ident("handle".into()),
            ty: TypeExpr::App {
                base: Box::new(tp("RawPtr", span)),
                args: vec![tp("u8", span)],
                span,
            },
            span,
        }],
        interfaces: vec![],
        methods: Vec::new(),
        inline_impls: Vec::new(),
        span,
    };

    let std_io_module = Module {
        imports: vec![],
        items: vec![
            glyph_core::ast::Item::Struct(stdout_struct),
            glyph_core::ast::Item::Struct(file_struct),
            glyph_core::ast::Item::ExternFunction(puts_extern),
            glyph_core::ast::Item::ExternFunction(print_str_extern),
            glyph_core::ast::Item::ExternFunction(raw_write_extern),
            glyph_core::ast::Item::ExternFunction(fopen_extern),
            glyph_core::ast::Item::ExternFunction(write_extern),
            glyph_core::ast::Item::ExternFunction(read_extern),
            glyph_core::ast::Item::ExternFunction(fclose_extern),
            glyph_core::ast::Item::ExternFunction(fread_extern),
            glyph_core::ast::Item::ExternFunction(fseek_extern),
            glyph_core::ast::Item::ExternFunction(ftell_extern),
            glyph_core::ast::Item::ExternFunction(rewind_extern),
            glyph_core::ast::Item::ExternFunction(println_extern),
            glyph_core::ast::Item::ExternFunction(read_line_extern),
            glyph_core::ast::Item::ExternFunction(ignore_sigpipe_extern),
            glyph_core::ast::Item::ExternFunction(read_file_bytes_extern),
        ],
    };
    modules.insert("std/io".into(), std_io_module);

    let std_println_extern = ExternFunctionDecl {
        name: Ident("println".into()),
        abi: Some("C".into()),
        params: vec![Param {
            name: Ident("msg".into()),
            ty: Some(tp("str", span)),
            span,
        }],
        ret_type: Some(tp("i32", span)),
        link_name: None,
        span,
    };

    // std root re-exporting io
    let std_root = Module {
        imports: vec![
            Import {
                kind: ImportKind::Wildcard,
                path: ImportPath {
                    segments: vec!["std/io".into()],
                    span,
                },
                span,
            },
            Import {
                kind: ImportKind::Wildcard,
                path: ImportPath {
                    segments: vec!["std/vec".into()],
                    span,
                },
                span,
            },
            Import {
                kind: ImportKind::Wildcard,
                path: ImportPath {
                    segments: vec!["std/enums".into()],
                    span,
                },
                span,
            },
            Import {
                kind: ImportKind::Wildcard,
                path: ImportPath {
                    segments: vec!["std/string".into()],
                    span,
                },
                span,
            },
            Import {
                kind: ImportKind::Wildcard,
                path: ImportPath {
                    segments: vec!["std/sys".into()],
                    span,
                },
                span,
            },
            Import {
                kind: ImportKind::Wildcard,
                path: ImportPath {
                    segments: vec!["std/process".into()],
                    span,
                },
                span,
            },
            Import {
                kind: ImportKind::Wildcard,
                path: ImportPath {
                    segments: vec!["std/hashing".into()],
                    span,
                },
                span,
            },
            Import {
                kind: ImportKind::Wildcard,
                path: ImportPath {
                    segments: vec!["std/map".into()],
                    span,
                },
                span,
            },
            Import {
                kind: ImportKind::Wildcard,
                path: ImportPath {
                    segments: vec!["std/json".into()],
                    span,
                },
                span,
            },
            Import {
                kind: ImportKind::Wildcard,
                path: ImportPath {
                    segments: vec!["std/term".into()],
                    span,
                },
                span,
            },
            Import {
                kind: ImportKind::Wildcard,
                path: ImportPath {
                    segments: vec!["std/net".into()],
                    span,
                },
                span,
            },
            Import {
                kind: ImportKind::Wildcard,
                path: ImportPath {
                    segments: vec!["std/thread".into()],
                    span,
                },
                span,
            },
            Import {
                kind: ImportKind::Wildcard,
                path: ImportPath {
                    segments: vec!["std/sync".into()],
                    span,
                },
                span,
            },
        ],
        items: vec![glyph_core::ast::Item::ExternFunction(std_println_extern)],
    };
    modules.insert("std".into(), std_root);

    // std/vec
    let vec_struct = StructDef {
        name: Ident("Vec".into()),
        generic_params: vec![Ident("T".into())],
        fields: vec![
            glyph_core::ast::FieldDef {
                name: Ident("data".into()),
                ty: TypeExpr::App {
                    base: Box::new(tp("RawPtr", span)),
                    args: vec![tp("T", span)],
                    span,
                },
                span,
            },
            glyph_core::ast::FieldDef {
                name: Ident("len".into()),
                ty: tp("usize", span),
                span,
            },
            glyph_core::ast::FieldDef {
                name: Ident("cap".into()),
                ty: tp("usize", span),
                span,
            },
        ],
        interfaces: vec![],
        methods: Vec::new(),
        inline_impls: Vec::new(),
        span,
    };

    let std_vec_module = Module {
        imports: vec![],
        items: vec![glyph_core::ast::Item::Struct(vec_struct)],
    };
    modules.insert("std/vec".into(), std_vec_module);

    // std/enums with generic Option<T> and Result<T, E>
    let option_enum = EnumDef {
        name: Ident("Option".into()),
        generic_params: vec![Ident("T".into())],
        variants: vec![
            EnumVariantDef {
                name: Ident("None".into()),
                payload: None,
                span,
            },
            EnumVariantDef {
                name: Ident("Some".into()),
                payload: Some(tp("T", span)),
                span,
            },
        ],
        span,
    };

    let err_struct = StructDef {
        name: Ident("Err".into()),
        generic_params: vec![],
        fields: vec![glyph_core::ast::FieldDef {
            name: Ident("msg".into()),
            ty: tp("String", span),
            span,
        }],
        interfaces: vec![],
        methods: Vec::new(),
        inline_impls: Vec::new(),
        span,
    };

    let result_enum = EnumDef {
        name: Ident("Result".into()),
        generic_params: vec![Ident("T".into()), Ident("E".into())],
        variants: vec![
            EnumVariantDef {
                name: Ident("Ok".into()),
                payload: Some(tp("T", span)),
                span,
            },
            EnumVariantDef {
                name: Ident("Err".into()),
                payload: Some(tp("E", span)),
                span,
            },
        ],
        span,
    };

    let std_enums = Module {
        imports: vec![],
        items: vec![
            glyph_core::ast::Item::Enum(option_enum),
            glyph_core::ast::Item::Struct(err_struct),
            glyph_core::ast::Item::Enum(result_enum),
        ],
    };
    modules.insert("std/enums".into(), std_enums);

    // std/thread contains resolver-only canonical declarations. Its callable
    // and opaque result types are lowered by compiler intrinsics.
    let std_thread_module = parse_std_source("std/thread", include_str!("stdlib/thread.glyph"));
    modules.insert("std/thread".into(), std_thread_module);
    let std_sync_module = parse_std_source("std/sync", include_str!("stdlib/sync.glyph"));
    modules.insert("std/sync".into(), std_sync_module);
    let std_spsc_module = parse_std_source("std/sync/spsc", include_str!("stdlib/sync/spsc.glyph"));
    modules.insert("std/sync/spsc".into(), std_spsc_module);

    // std/string
    let strdup_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("strdup".into()),
        params: vec![Param {
            name: Ident("input".into()),
            ty: Some(tp("str", span)),
            span,
        }],
        ret_type: Some(tp("String", span)),
        link_name: Some("strdup".into()),
        span,
    };

    let strlen_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("strlen".into()),
        params: vec![Param {
            name: Ident("input".into()),
            ty: Some(tp("str", span)),
            span,
        }],
        ret_type: Some(tp("usize", span)),
        link_name: Some("strlen".into()),
        span,
    };

    let memcmp_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("memcmp".into()),
        params: vec![
            Param {
                name: Ident("lhs".into()),
                ty: Some(TypeExpr::App {
                    base: Box::new(tp("RawPtr", span)),
                    args: vec![tp("u8", span)],
                    span,
                }),
                span,
            },
            Param {
                name: Ident("rhs".into()),
                ty: Some(TypeExpr::App {
                    base: Box::new(tp("RawPtr", span)),
                    args: vec![tp("u8", span)],
                    span,
                }),
                span,
            },
            Param {
                name: Ident("len".into()),
                ty: Some(tp("usize", span)),
                span,
            },
        ],
        ret_type: Some(tp("i32", span)),
        link_name: Some("memcmp".into()),
        span,
    };

    let memcpy_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("memcpy".into()),
        params: vec![
            Param {
                name: Ident("dst".into()),
                ty: Some(TypeExpr::App {
                    base: Box::new(tp("RawPtr", span)),
                    args: vec![tp("u8", span)],
                    span,
                }),
                span,
            },
            Param {
                name: Ident("src".into()),
                ty: Some(TypeExpr::App {
                    base: Box::new(tp("RawPtr", span)),
                    args: vec![tp("u8", span)],
                    span,
                }),
                span,
            },
            Param {
                name: Ident("len".into()),
                ty: Some(tp("usize", span)),
                span,
            },
        ],
        ret_type: Some(TypeExpr::App {
            base: Box::new(tp("RawPtr", span)),
            args: vec![tp("u8", span)],
            span,
        }),
        link_name: Some("memcpy".into()),
        span,
    };

    let strstr_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("strstr".into()),
        params: vec![
            Param {
                name: Ident("haystack".into()),
                ty: Some(tp("str", span)),
                span,
            },
            Param {
                name: Ident("needle".into()),
                ty: Some(tp("str", span)),
                span,
            },
        ],
        ret_type: Some(TypeExpr::App {
            base: Box::new(tp("RawPtr", span)),
            args: vec![tp("u8", span)],
            span,
        }),
        link_name: Some("strstr".into()),
        span,
    };

    let isspace_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("isspace".into()),
        params: vec![Param {
            name: Ident("ch".into()),
            ty: Some(tp("i32", span)),
            span,
        }],
        ret_type: Some(tp("i32", span)),
        link_name: Some("isspace".into()),
        span,
    };

    let atof_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("atof".into()),
        params: vec![Param {
            name: Ident("s".into()),
            ty: Some(tp("str", span)),
            span,
        }],
        ret_type: Some(tp("f64", span)),
        link_name: Some("atof".into()),
        span,
    };

    let isdigit_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("isdigit".into()),
        params: vec![Param {
            name: Ident("ch".into()),
            ty: Some(tp("i32", span)),
            span,
        }],
        ret_type: Some(tp("i32", span)),
        link_name: Some("isdigit".into()),
        span,
    };

    let atoi_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("atoi".into()),
        params: vec![Param {
            name: Ident("s".into()),
            ty: Some(tp("str", span)),
            span,
        }],
        ret_type: Some(tp("i32", span)),
        link_name: Some("atoi".into()),
        span,
    };

    let byte_at_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("byte_at".into()),
        params: vec![
            Param {
                name: Ident("s".into()),
                ty: Some(tp("str", span)),
                span,
            },
            Param {
                name: Ident("index".into()),
                ty: Some(tp("usize", span)),
                span,
            },
        ],
        ret_type: Some(tp("u8", span)),
        link_name: Some("glyph_byte_at".into()),
        span,
    };

    let string_from_byte_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("string_from_byte".into()),
        params: vec![Param {
            name: Ident("byte".into()),
            ty: Some(tp("u8", span)),
            span,
        }],
        ret_type: Some(tp("String", span)),
        link_name: Some("glyph_string_from_byte".into()),
        span,
    };

    let string_from_char_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("string_from_char".into()),
        params: vec![Param {
            name: Ident("ch".into()),
            ty: Some(tp("char", span)),
            span,
        }],
        ret_type: Some(tp("String", span)),
        link_name: Some("glyph_string_from_char".into()),
        span,
    };

    let string_from_i32_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("string_from_i32".into()),
        params: vec![Param {
            name: Ident("value".into()),
            ty: Some(tp("i32", span)),
            span,
        }],
        ret_type: Some(tp("String", span)),
        link_name: Some("glyph_string_from_i32".into()),
        span,
    };

    let string_from_f64_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("string_from_f64".into()),
        params: vec![Param {
            name: Ident("value".into()),
            ty: Some(tp("f64", span)),
            span,
        }],
        ret_type: Some(tp("String", span)),
        link_name: Some("glyph_string_from_f64".into()),
        span,
    };

    let string_char_at_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("string_char_at".into()),
        params: vec![
            Param {
                name: Ident("s".into()),
                ty: Some(tp("str", span)),
                span,
            },
            Param {
                name: Ident("index".into()),
                ty: Some(tp("usize", span)),
                span,
            },
        ],
        ret_type: Some(tp("char", span)),
        link_name: Some("glyph_string_char_at".into()),
        span,
    };

    let string_index_of_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("string_index_of".into()),
        params: vec![
            Param {
                name: Ident("haystack".into()),
                ty: Some(tp("str", span)),
                span,
            },
            Param {
                name: Ident("needle".into()),
                ty: Some(tp("str", span)),
                span,
            },
        ],
        ret_type: Some(tp("i64", span)),
        link_name: Some("glyph_string_index_of".into()),
        span,
    };

    let std_string_module = Module {
        imports: vec![],
        items: vec![
            glyph_core::ast::Item::ExternFunction(strdup_extern),
            glyph_core::ast::Item::ExternFunction(strlen_extern),
            glyph_core::ast::Item::ExternFunction(memcmp_extern),
            glyph_core::ast::Item::ExternFunction(memcpy_extern),
            glyph_core::ast::Item::ExternFunction(strstr_extern),
            glyph_core::ast::Item::ExternFunction(isspace_extern),
            glyph_core::ast::Item::ExternFunction(atof_extern),
            glyph_core::ast::Item::ExternFunction(isdigit_extern),
            glyph_core::ast::Item::ExternFunction(atoi_extern),
            glyph_core::ast::Item::ExternFunction(byte_at_extern),
            glyph_core::ast::Item::ExternFunction(string_from_byte_extern),
            glyph_core::ast::Item::ExternFunction(string_from_char_extern),
            glyph_core::ast::Item::ExternFunction(string_from_i32_extern),
            glyph_core::ast::Item::ExternFunction(string_from_f64_extern),
            glyph_core::ast::Item::ExternFunction(string_char_at_extern),
            glyph_core::ast::Item::ExternFunction(string_index_of_extern),
        ],
    };
    modules.insert("std/string".into(), std_string_module);

    // std/sys
    let argv_extern = ExternFunctionDecl {
        abi: None,
        name: Ident("argv".into()),
        params: vec![],
        ret_type: Some(TypeExpr::App {
            base: Box::new(tp("Vec", span)),
            args: vec![tp("String", span)],
            span,
        }),
        link_name: None,
        span,
    };

    let std_sys_module = Module {
        imports: vec![],
        items: vec![glyph_core::ast::Item::ExternFunction(argv_extern)],
    };
    modules.insert("std/sys".into(), std_sys_module);

    let std_time_module = parse_std_source("std/time", include_str!("stdlib/time.glyph"));
    modules.insert("std/time".into(), std_time_module);
    let std_term_module = parse_std_source("std/term", include_str!("stdlib/term.glyph"));
    modules.insert("std/term".into(), std_term_module);
    let std_net_module = parse_std_source("std/net", include_str!("stdlib/net.glyph"));
    modules.insert("std/net".into(), std_net_module);
    let std_math_module = parse_std_source("std/math", include_str!("stdlib/math.glyph"));
    modules.insert("std/math".into(), std_math_module);
    let std_audio_module = parse_std_source("std/audio", include_str!("stdlib/audio.glyph"));
    modules.insert("std/audio".into(), std_audio_module);

    // std/process
    let run_extern = ExternFunctionDecl {
        abi: Some("C".into()),
        name: Ident("run".into()),
        params: vec![
            Param {
                name: Ident("cmd".into()),
                ty: Some(tp("str", span)),
                span,
            },
            Param {
                name: Ident("args".into()),
                ty: Some(TypeExpr::Ref {
                    mutability: Mutability::Immutable,
                    inner: Box::new(TypeExpr::App {
                        base: Box::new(tp("Vec", span)),
                        args: vec![tp("String", span)],
                        span,
                    }),
                    span,
                }),
                span,
            },
        ],
        ret_type: Some(tp("i32", span)),
        link_name: Some("glyph_process_run".into()),
        span,
    };

    let std_process_module = Module {
        imports: vec![],
        items: vec![glyph_core::ast::Item::ExternFunction(run_extern)],
    };
    modules.insert("std/process".into(), std_process_module);

    // std/hashing with Hash interface placeholder
    let hash_interface = InterfaceDef {
        name: Ident("Hash".into()),
        methods: vec![glyph_core::ast::InterfaceMethod {
            name: Ident("hash".into()),
            params: vec![Param {
                name: Ident("self".into()),
                ty: Some(TypeExpr::Ref {
                    mutability: Mutability::Immutable,
                    inner: Box::new(tp("Self", span)),
                    span,
                }),
                span,
            }],
            ret_type: Some(tp("u64", span)),
            span,
        }],
        span,
    };

    let std_hashing = Module {
        imports: vec![],
        items: vec![glyph_core::ast::Item::Interface(hash_interface)],
    };
    modules.insert("std/hashing".into(), std_hashing);

    // std/map
    // Map<K, V> API contract:
    // - Map::new() -> Map<K, V>
    // - Map::with_capacity(cap: usize) -> Map<K, V>
    // - add(key: K, val: V) -> Result<(), Err>
    // - update(key: K, val: V) -> Result<(), Err>
    // - del(key: K) -> Result<V, Err>
    // - get(key: K) -> Option<V>
    // - has(key: K) -> bool
    // - keys() -> Vec<K>
    // - vals() -> Vec<V>
    let map_bucket_struct = StructDef {
        name: Ident("MapBucket".into()),
        generic_params: vec![Ident("K".into()), Ident("V".into())],
        fields: vec![
            glyph_core::ast::FieldDef {
                name: Ident("key".into()),
                ty: tp("K", span),
                span,
            },
            glyph_core::ast::FieldDef {
                name: Ident("value".into()),
                ty: tp("V", span),
                span,
            },
            glyph_core::ast::FieldDef {
                name: Ident("next".into()),
                ty: TypeExpr::App {
                    base: Box::new(tp("RawPtr", span)),
                    args: vec![TypeExpr::App {
                        base: Box::new(tp("MapBucket", span)),
                        args: vec![tp("K", span), tp("V", span)],
                        span,
                    }],
                    span,
                },
                span,
            },
        ],
        interfaces: vec![],
        methods: Vec::new(),
        inline_impls: Vec::new(),
        span,
    };

    let map_struct = StructDef {
        name: Ident("Map".into()),
        generic_params: vec![Ident("K".into()), Ident("V".into())],
        fields: vec![
            glyph_core::ast::FieldDef {
                name: Ident("buckets".into()),
                ty: TypeExpr::App {
                    base: Box::new(tp("RawPtr", span)),
                    args: vec![TypeExpr::App {
                        base: Box::new(tp("RawPtr", span)),
                        args: vec![TypeExpr::App {
                            base: Box::new(tp("MapBucket", span)),
                            args: vec![tp("K", span), tp("V", span)],
                            span,
                        }],
                        span,
                    }],
                    span,
                },
                span,
            },
            glyph_core::ast::FieldDef {
                name: Ident("cap".into()),
                ty: tp("usize", span),
                span,
            },
            glyph_core::ast::FieldDef {
                name: Ident("len".into()),
                ty: tp("usize", span),
                span,
            },
        ],
        interfaces: vec![],
        methods: Vec::new(),
        inline_impls: Vec::new(),
        span,
    };

    let std_map_module = Module {
        imports: vec![Import {
            kind: ImportKind::Wildcard,
            path: ImportPath {
                segments: vec!["std/enums".into()],
                span,
            },
            span,
        }],
        items: vec![
            glyph_core::ast::Item::Struct(map_bucket_struct),
            glyph_core::ast::Item::Struct(map_struct),
        ],
    };
    modules.insert("std/map".into(), std_map_module);

    // std/json - JSON parsing and manipulation
    let json_value_enum = EnumDef {
        name: Ident("JsonValue".into()),
        generic_params: vec![],
        variants: vec![
            EnumVariantDef {
                name: Ident("Null".into()),
                payload: None,
                span,
            },
            EnumVariantDef {
                name: Ident("Bool".into()),
                payload: Some(tp("bool", span)),
                span,
            },
            EnumVariantDef {
                name: Ident("Number".into()),
                payload: Some(tp("f64", span)),
                span,
            },
            EnumVariantDef {
                name: Ident("String".into()),
                payload: Some(tp("String", span)),
                span,
            },
            EnumVariantDef {
                name: Ident("Array".into()),
                payload: Some(TypeExpr::App {
                    base: Box::new(tp("Vec", span)),
                    args: vec![tp("JsonValue", span)],
                    span,
                }),
                span,
            },
            EnumVariantDef {
                name: Ident("Object".into()),
                payload: Some(TypeExpr::App {
                    base: Box::new(tp("Map", span)),
                    args: vec![tp("String", span), tp("JsonValue", span)],
                    span,
                }),
                span,
            },
        ],
        span,
    };

    let parse_error_struct = StructDef {
        name: Ident("ParseError".into()),
        generic_params: vec![],
        fields: vec![
            glyph_core::ast::FieldDef {
                name: Ident("message".into()),
                ty: tp("String", span),
                span,
            },
            glyph_core::ast::FieldDef {
                name: Ident("position".into()),
                ty: tp("usize", span),
                span,
            },
        ],
        interfaces: vec![],
        methods: Vec::new(),
        inline_impls: Vec::new(),
        span,
    };

    // std/json ParseResult<T> = Ok(T) | Err(ParseError)
    // We keep this as a single-parameter result type because multi-parameter enum
    // monomorphization is still evolving in the compiler.
    let parse_result_enum = EnumDef {
        name: Ident("ParseResult".into()),
        generic_params: vec![Ident("T".into())],
        variants: vec![
            EnumVariantDef {
                name: Ident("Ok".into()),
                payload: Some(tp("T", span)),
                span,
            },
            EnumVariantDef {
                name: Ident("Err".into()),
                payload: Some(tp("ParseError", span)),
                span,
            },
        ],
        span,
    };

    // std/json provides the JsonValue/ParseError/ParseResult types. The
    // parser itself (parse, accessors, stringify) lives in std/json/parser,
    // registered below as embedded Glyph source - see that module for the
    // full, release-shipping implementation
    // (crates/glyph-frontend/src/stdlib/json/parser.glyph).
    let json_items = vec![
        glyph_core::ast::Item::Enum(json_value_enum),
        glyph_core::ast::Item::Struct(parse_error_struct),
        glyph_core::ast::Item::Enum(parse_result_enum),
    ];

    let std_json_module = Module {
        imports: vec![
            Import {
                kind: ImportKind::Wildcard,
                path: ImportPath {
                    segments: vec!["std/enums".into()],
                    span,
                },
                span,
            },
            Import {
                kind: ImportKind::Wildcard,
                path: ImportPath {
                    segments: vec!["std/vec".into()],
                    span,
                },
                span,
            },
            Import {
                kind: ImportKind::Wildcard,
                path: ImportPath {
                    segments: vec!["std/string".into()],
                    span,
                },
                span,
            },
            Import {
                kind: ImportKind::Wildcard,
                path: ImportPath {
                    segments: vec!["std/map".into()],
                    span,
                },
                span,
            },
        ],
        items: json_items,
    };
    modules.insert("std/json".into(), std_json_module);

    // std/json/parser (embedded Glyph source)
    // We embed the parser source at compile time so the compiler can ship std as one unit.
    const STD_JSON_PARSER_SRC: &str =
        include_str!(concat!(env!("OUT_DIR"), "/json_parser_source.txt"));
    let std_json_parser_module = parse_std_source("std/json/parser", STD_JSON_PARSER_SRC);
    modules.insert("std/json/parser".into(), std_json_parser_module);

    modules
}
