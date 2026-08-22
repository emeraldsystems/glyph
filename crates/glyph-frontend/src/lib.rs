use glyph_core::{
    ast::{Block, Expr, Function, Ident, Literal, Module, Stmt},
    diag::Diagnostic,
    mir::MirModule,
    span::Span,
};

mod closure_analysis;
mod lexer;
mod method_symbols;
mod mir_lower;
mod module_resolver;
mod monomorphize;
mod parser;
mod resolver;
mod stdlib;

pub use closure_analysis::{
    CaptureAccess, CaptureOwnership, ClosureAnalysis, ClosureCapture, ClosureEscape,
    ClosureEscapeKind, ClosureInfo, analyze_function_closure_ownership, analyze_function_closures,
};
pub use lexer::{LexOutput, lex};
pub use mir_lower::lower_module;
pub use module_resolver::{
    DependencyGraph, ImportScope, ModuleSymbols, MultiModuleContext, build_dependency_graph,
    resolve_multi_module,
};
pub use parser::{ParseOutput, parse};
pub use resolver::{ResolverContext, resolve_types};
pub use stdlib::std_modules;

#[derive(Debug, Clone, Default)]
pub struct FrontendOptions {
    pub emit_mir: bool,
    /// Include the built-in std modules during lowering (import std will work).
    pub include_std: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FrontendOutput {
    pub module: Option<Module>,
    pub mir: MirModule,
    pub diagnostics: Vec<Diagnostic>,
}

fn tag_diagnostics(module_id: &str, diags: Vec<Diagnostic>) -> Vec<Diagnostic> {
    diags
        .into_iter()
        .map(|diag| {
            if diag.module_id.is_some() {
                diag
            } else {
                diag.with_module_id(module_id.to_string())
            }
        })
        .collect()
}

fn tag_new_diagnostics(module_id: &str, diagnostics: &mut Vec<Diagnostic>, start: usize) {
    for diag in diagnostics.iter_mut().skip(start) {
        if diag.module_id.is_none() {
            diag.module_id = Some(module_id.to_string());
        }
    }
}

fn check_name_collision(
    kind: &str,
    name: &str,
    module_id: &str,
    seen: &mut std::collections::HashMap<String, String>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if let Some(existing) = seen.get(name) {
        if existing != module_id {
            if is_std_module(existing) && is_std_module(module_id) {
                return;
            }
            diagnostics.push(Diagnostic::error(
                format!(
                    "{} '{}' defined in both '{}' and '{}'. Cross-case {} name collisions are not yet supported.",
                    kind, name, existing, module_id, kind
                ),
                None,
            ));
        }
    } else {
        seen.insert(name.to_string(), module_id.to_string());
    }
}

fn is_std_module(module_id: &str) -> bool {
    module_id == "std" || module_id.starts_with("std/")
}

fn compile_multi_module_graph(
    modules: std::collections::HashMap<String, Module>,
    entry_module: &str,
) -> Result<MirModule, Vec<Diagnostic>> {
    match module_resolver::resolve_multi_module(modules, entry_module, std::path::Path::new(".")) {
        Ok((multi_ctx, compile_order)) => {
            let mut merged = MirModule::default();
            let mut seen_externs = std::collections::HashSet::new();
            let mut seen_functions = std::collections::HashMap::new();
            let mut seen_structs = std::collections::HashMap::new();
            let mut seen_enums = std::collections::HashMap::new();
            let mut diagnostics = Vec::new();

            for module_id in compile_order {
                let module = multi_ctx.modules.get(&module_id).unwrap();
                if let Some(symbols) = multi_ctx.module_symbols.get(&module_id) {
                    for name in &symbols.structs {
                        check_name_collision(
                            "struct",
                            name,
                            &module_id,
                            &mut seen_structs,
                            &mut diagnostics,
                        );
                    }
                    for name in &symbols.enums {
                        check_name_collision(
                            "enum",
                            name,
                            &module_id,
                            &mut seen_enums,
                            &mut diagnostics,
                        );
                    }
                    for name in &symbols.functions {
                        check_name_collision(
                            "function",
                            name,
                            &module_id,
                            &mut seen_functions,
                            &mut diagnostics,
                        );
                    }
                }
                if !diagnostics.is_empty() {
                    return Err(diagnostics);
                }

                let (mut ctx, resolve_diags) = resolver::resolve_types(module);
                diagnostics.extend(tag_diagnostics(&module_id, resolve_diags));
                if !diagnostics.is_empty() {
                    return Err(diagnostics);
                }

                ctx.current_module = Some(module_id.clone());
                ctx.import_scope = multi_ctx.import_scopes.get(&module_id).cloned();
                ctx.all_modules = Some(multi_ctx.clone());
                resolver::populate_imported_types(&mut ctx);
                let diag_start = diagnostics.len();
                resolver::validate_named_types(module, &ctx, &mut diagnostics);
                tag_new_diagnostics(&module_id, &mut diagnostics, diag_start);
                let diag_start = diagnostics.len();
                resolver::validate_map_usage(module, &ctx, &mut diagnostics);
                tag_new_diagnostics(&module_id, &mut diagnostics, diag_start);
                let diag_start = diagnostics.len();
                resolver::validate_consts(module, &mut ctx, &mut diagnostics);
                tag_new_diagnostics(&module_id, &mut diagnostics, diag_start);
                if !diagnostics.is_empty() {
                    return Err(diagnostics);
                }

                let (lowered, lower_diags) = mir_lower::lower_module(module, &ctx);
                diagnostics.extend(tag_diagnostics(&module_id, lower_diags));
                if !diagnostics.is_empty() {
                    return Err(diagnostics);
                }

                merged.struct_types.extend(lowered.struct_types.into_iter());
                merged.enum_types.extend(lowered.enum_types.into_iter());
                merged.functions.extend(lowered.functions);

                for ex in lowered.extern_functions {
                    if seen_externs.insert(ex.name.clone()) {
                        merged.extern_functions.push(ex);
                    }
                }
            }

            let mut merged = merged;
            let mono_diags = monomorphize::monomorphize_mir(&mut merged, &multi_ctx.modules);
            if !mono_diags.is_empty() {
                return Err(mono_diags);
            }

            Ok(merged)
        }
        Err(diags) => Err(diags),
    }
}

pub fn compile_modules(
    modules: std::collections::HashMap<String, Module>,
    entry_module: &str,
    opts: FrontendOptions,
) -> FrontendOutput {
    let module = modules.get(entry_module).cloned();
    let mut diagnostics = Vec::new();
    let mut mir = MirModule::default();

    if module.is_none() {
        diagnostics.push(Diagnostic::error(
            format!("entry module '{}' not found", entry_module),
            None,
        ));
    } else if opts.include_std {
        match compile_multi_module_graph(modules, entry_module) {
            Ok(compiled) => mir = compiled,
            Err(diags) => diagnostics.extend(diags),
        }
    } else if let Some(ref m) = module {
        let (mut ctx, resolve_diags) = resolver::resolve_types(m);
        ctx.current_module = Some(entry_module.to_string());
        diagnostics.extend(tag_diagnostics(entry_module, resolve_diags));
        if diagnostics.is_empty() {
            let diag_start = diagnostics.len();
            resolver::validate_named_types(m, &ctx, &mut diagnostics);
            tag_new_diagnostics(entry_module, &mut diagnostics, diag_start);
            if diagnostics.is_empty() {
                let diag_start = diagnostics.len();
                resolver::validate_map_usage(m, &ctx, &mut diagnostics);
                tag_new_diagnostics(entry_module, &mut diagnostics, diag_start);
            }
            if diagnostics.is_empty() {
                let diag_start = diagnostics.len();
                resolver::validate_consts(m, &mut ctx, &mut diagnostics);
                tag_new_diagnostics(entry_module, &mut diagnostics, diag_start);
            }
            if diagnostics.is_empty() {
                let (mut lowered, lower_diags) = mir_lower::lower_module(m, &ctx);
                diagnostics.extend(tag_diagnostics(entry_module, lower_diags));
                if diagnostics.is_empty() {
                    let modules =
                        std::collections::HashMap::from([(entry_module.to_string(), m.clone())]);
                    diagnostics.extend(monomorphize::monomorphize_mir(&mut lowered, &modules));
                    if diagnostics.is_empty() {
                        mir = lowered;
                    }
                }
            }
        }
    }

    if diagnostics.is_empty() && !opts.emit_mir {
        mir = MirModule::default();
    }

    FrontendOutput {
        module,
        mir,
        diagnostics,
    }
}

pub fn compile_source(source: &str, opts: FrontendOptions) -> FrontendOutput {
    let LexOutput {
        tokens,
        diagnostics,
    } = lex(source);
    let mut diagnostics = diagnostics;

    let mut module = None;

    if diagnostics.is_empty() {
        let ParseOutput {
            module: parsed_module,
            diagnostics: parse_diags,
        } = parse(&tokens, source);
        diagnostics.extend(parse_diags);
        if diagnostics.is_empty() {
            module = Some(parsed_module);
        }
    }

    let mut mir = MirModule::default();

    if diagnostics.is_empty() {
        if let Some(ref m) = module {
            if opts.include_std {
                let modules = std::collections::HashMap::from([("main".to_string(), m.clone())]);
                return compile_modules(modules, "main", opts);
            } else {
                let (mut ctx, resolve_diags) = resolver::resolve_types(m);
                ctx.current_module = Some("main".to_string());
                diagnostics.extend(resolve_diags);
                if diagnostics.is_empty() {
                    resolver::validate_named_types(m, &ctx, &mut diagnostics);
                    if !diagnostics.is_empty() {
                        return FrontendOutput {
                            module,
                            mir,
                            diagnostics,
                        };
                    }
                    resolver::validate_map_usage(m, &ctx, &mut diagnostics);
                    if !diagnostics.is_empty() {
                        return FrontendOutput {
                            module,
                            mir,
                            diagnostics,
                        };
                    }
                    resolver::validate_consts(m, &mut ctx, &mut diagnostics);
                    if !diagnostics.is_empty() {
                        return FrontendOutput {
                            module,
                            mir,
                            diagnostics,
                        };
                    }
                    let (mut lowered, lower_diags) = mir_lower::lower_module(m, &ctx);
                    diagnostics.extend(lower_diags);
                    if diagnostics.is_empty() {
                        let modules =
                            std::collections::HashMap::from([("main".to_string(), m.clone())]);
                        diagnostics.extend(monomorphize::monomorphize_mir(&mut lowered, &modules));
                        if diagnostics.is_empty() {
                            mir = lowered;
                        }
                    }
                }
            }
        }
    }

    if diagnostics.is_empty() && !opts.emit_mir {
        mir = MirModule::default();
    }

    FrontendOutput {
        module,
        mir,
        diagnostics,
    }
}

pub fn demo_function() -> Function {
    let span = Span::new(0, 3);
    Function {
        name: Ident("demo".into()),
        params: vec![],
        ret_type: None,
        body: Block {
            span,
            stmts: vec![Stmt::Expr(Expr::Lit(Literal::Int(1), span), span)],
        },
        span,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glyph_core::diag::Severity;
    use std::collections::HashMap;

    fn parse_module(source: &str) -> Module {
        let lex_out = lex(source);
        assert!(
            lex_out.diagnostics.is_empty(),
            "lex diags: {:?}",
            lex_out.diagnostics
        );
        let parse_out = parse(&lex_out.tokens, source);
        assert!(
            parse_out.diagnostics.is_empty(),
            "parse diags: {:?}",
            parse_out.diagnostics
        );
        parse_out.module
    }

    #[test]
    fn compile_source_returns_empty_diagnostics_on_valid_input() {
        let output = compile_source("fn main() {}", FrontendOptions::default());
        assert!(output.diagnostics.is_empty());
    }

    #[test]
    fn compile_source_returns_errors_on_lex_failure() {
        let output = compile_source("\"unterminated", FrontendOptions::default());
        assert!(!output.diagnostics.is_empty());
        assert!(matches!(output.diagnostics[0].severity, Severity::Error));
    }

    #[test]
    fn emit_mir_option_lowers_function() {
        let output = compile_source(
            "fn main() { ret }",
            FrontendOptions {
                emit_mir: true,
                include_std: false,
            },
        );
        assert_eq!(output.mir.functions.len(), 1);
        assert_eq!(output.mir.functions[0].name, "main");
    }

    #[test]
    fn emit_mir_const_return_value() {
        let output = compile_source(
            "fn main() { ret 1 }",
            FrontendOptions {
                emit_mir: true,
                include_std: false,
            },
        );
        let insts = &output.mir.functions[0].blocks[0].insts;
        assert!(matches!(
            insts.last(),
            Some(glyph_core::mir::MirInst::Return(Some(
                glyph_core::mir::MirValue::Int(1)
            )))
        ));
    }

    #[test]
    fn if_without_else_can_be_unit_expression() {
        let output = compile_source(
            "fn main() { let x = if true { () } }",
            FrontendOptions {
                emit_mir: true,
                include_std: false,
            },
        );
        assert!(
            output.diagnostics.is_empty(),
            "diags: {:?}",
            output.diagnostics
        );
    }

    #[test]
    fn compiles_std_println_from_import() {
        let source = "from std import println\nfn main() { println(\"hi\") }";
        let output = compile_source(
            source,
            FrontendOptions {
                emit_mir: true,
                include_std: true,
            },
        );
        assert!(
            output.diagnostics.is_empty(),
            "diags: {:?}",
            output.diagnostics
        );
    }

    #[test]
    fn string_from_str_rejects_non_string_arg() {
        let source = "from std/enums import Option\nfn main() {\n  let x = Option::Some(1)\n  let y = String::from_str(x)\n}\n";
        let output = compile_source(
            source,
            FrontendOptions {
                emit_mir: true,
                include_std: true,
            },
        );
        assert!(
            output.diagnostics.iter().any(|d| d
                .message
                .contains("String::from_str expects a str or String")),
            "expected String::from_str diagnostic, got: {:?}",
            output.diagnostics
        );
    }

    #[test]
    fn compiles_local_imports_from_modules_map() {
        let main_src = "from utils import helper\nfn main() -> i32 { ret helper() }";
        let utils_src = "fn helper() -> i32 { ret 1 }";

        let main_lex = lex(main_src);
        assert!(main_lex.diagnostics.is_empty());
        let main_parsed = parse(&main_lex.tokens, main_src);
        assert!(main_parsed.diagnostics.is_empty());

        let utils_lex = lex(utils_src);
        assert!(utils_lex.diagnostics.is_empty());
        let utils_parsed = parse(&utils_lex.tokens, utils_src);
        assert!(utils_parsed.diagnostics.is_empty());

        let modules = std::collections::HashMap::from([
            ("main".to_string(), main_parsed.module),
            ("utils".to_string(), utils_parsed.module),
        ]);

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
            "diags: {:?}",
            output.diagnostics
        );
    }

    #[test]
    fn multi_module_rejects_function_name_collisions() {
        let modules = HashMap::from([
            (
                "main".to_string(),
                parse_module("import a\nimport b\nfn main() -> i32 { ret 0 }"),
            ),
            (
                "a".to_string(),
                parse_module("fn helper() -> i32 { ret 1 }"),
            ),
            (
                "b".to_string(),
                parse_module("fn helper() -> i32 { ret 2 }"),
            ),
        ]);

        let output = compile_modules(
            modules,
            "main",
            FrontendOptions {
                emit_mir: true,
                include_std: true,
            },
        );

        let message = output
            .diagnostics
            .iter()
            .find(|d| d.message.contains("function 'helper' defined in both"))
            .map(|d| d.message.as_str());
        let Some(message) = message else {
            panic!("diags: {:?}", output.diagnostics);
        };
        assert!(message.contains("'a'") && message.contains("'b'"));
    }
}
