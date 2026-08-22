use glyph_core::ast::{Item, Module};
use glyph_core::diag::Diagnostic;
use glyph_core::mir::{MirExternFunction, MirModule};
use glyph_core::types::Type;

use crate::resolver::ResolverContext;

mod builtins;
mod call;
mod context;
mod expr;
mod flow;
mod signatures;
mod types;
mod value;

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use types::type_expr_to_string;

pub fn lower_module(module: &Module, resolver: &ResolverContext) -> (MirModule, Vec<Diagnostic>) {
    let (fn_sigs, mut diagnostics) = signatures::collect_function_signatures(module, resolver);

    let mut mir = MirModule {
        struct_types: resolver.struct_types.clone(),
        enum_types: resolver.enum_types.clone(),
        functions: Vec::new(),
        extern_functions: Vec::new(),
    };

    for item in &module.items {
        match item {
            Item::Function(func) => {
                let (lowered, lifted, diags) =
                    flow::lower_function(func, module, resolver, &fn_sigs);
                diagnostics.extend(diags);
                mir.functions.push(lowered);
                mir.functions.extend(lifted);
            }
            Item::Struct(_) | Item::Interface(_) | Item::Impl(_) | Item::Const(_) => {
                // Handled during resolution or desugaring stages.
            }
            Item::Enum(_) => {
                // Enums are type-level; constructors are lowered via call sites.
            }
            Item::ExternFunction(func) => {
                let Some(sig) = fn_sigs.get(&func.name.0) else {
                    continue;
                };

                // Extern functions require fully known param types
                let mut params = Vec::new();
                for (idx, ty) in sig.params.iter().enumerate() {
                    if let Some(t) = ty.clone() {
                        params.push(t);
                    } else {
                        diagnostics.push(Diagnostic::error(
                            format!(
                                "extern function '{}' parameter {} has unknown type",
                                func.name.0,
                                idx + 1
                            ),
                            Some(func.span),
                        ));
                    }
                }

                let extern_fn = MirExternFunction {
                    name: func.name.0.clone(),
                    ret_type: sig.ret.clone(),
                    params,
                    abi: sig.abi.clone(),
                    link_name: func.link_name.clone(),
                };
                mir.extern_functions.push(extern_fn);
            }
        }
    }

    // print/println lower scalar and string segments to direct calls into the
    // C formatting runtime (runtime/glyph_fmt.c); declare those externs so
    // codegen and linking can resolve them whenever std/io is available.
    if fn_sigs.contains_key("std::io::raw_write") {
        let fmt_externs: [(&str, Type); 9] = [
            ("glyph_fmt_write_i32", Type::I32),
            ("glyph_fmt_write_u32", Type::U32),
            ("glyph_fmt_write_i64", Type::I64),
            ("glyph_fmt_write_u64", Type::U64),
            ("glyph_fmt_write_bool", Type::U32),
            ("glyph_fmt_write_char", Type::U32),
            ("glyph_fmt_write_str", Type::Str),
            ("glyph_fmt_write_f32", Type::F32),
            ("glyph_fmt_write_f64", Type::F64),
        ];
        for (name, val_ty) in fmt_externs {
            if mir.extern_functions.iter().any(|f| f.name == name) {
                continue;
            }
            mir.extern_functions.push(MirExternFunction {
                name: name.to_string(),
                ret_type: Some(Type::I32),
                params: vec![Type::I32, val_ty],
                abi: Some("C".into()),
                link_name: None,
            });
        }
    }

    (mir, diagnostics)
}
