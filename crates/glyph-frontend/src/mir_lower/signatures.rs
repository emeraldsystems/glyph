use std::collections::HashMap;

use glyph_core::ast::{Item, Module, TypeExpr};
use glyph_core::diag::Diagnostic;
use glyph_core::span::Span;
use glyph_core::types::Type;

use crate::resolver::ResolverContext;

use super::types::type_expr_to_string;

#[derive(Debug, Clone)]
pub(crate) struct FnSig {
    pub(crate) params: Vec<Option<Type>>,
    pub(crate) ret: Option<Type>,
    pub(crate) abi: Option<String>,
    /// The actual target name to call in MIR (unqualified, without aliases).
    pub(crate) target_name: String,
    pub(crate) enum_ctor: Option<EnumCtorInfo>,
}

#[derive(Debug, Clone)]
pub(crate) struct EnumCtorInfo {
    pub(crate) enum_name: String,
    pub(crate) variant_index: usize,
    pub(crate) has_generics: bool,
}

pub(crate) fn collect_function_signatures(
    module: &Module,
    resolver: &ResolverContext,
) -> (HashMap<String, FnSig>, Vec<Diagnostic>) {
    let mut signatures = HashMap::new();
    let mut diagnostics = Vec::new();

    fn resolve_fn_sig(
        resolver: &ResolverContext,
        diagnostics: &mut Vec<Diagnostic>,
        key_name: &str,
        target_name: &str,
        params: &[glyph_core::ast::Param],
        ret_type: &Option<TypeExpr>,
        abi: Option<String>,
        is_extern: bool,
        span: Span,
    ) -> FnSig {
        let mut param_types = Vec::new();
        for param in params {
            let ty = match &param.ty {
                Some(t) => match crate::resolver::resolve_type_expr_to_type(t, resolver) {
                    Some(resolved) => Some(resolved),
                    None => {
                        let ty_str = type_expr_to_string(t);
                        diagnostics.push(Diagnostic::error(
                            format!("unknown type '{}' for parameter '{}'", ty_str, param.name.0),
                            Some(param.span),
                        ));
                        None
                    }
                },
                None => {
                    if is_extern {
                        diagnostics.push(Diagnostic::error(
                            format!(
                                "extern function '{}' parameter '{}' must have a type",
                                key_name, param.name.0
                            ),
                            Some(param.span),
                        ));
                    }
                    None
                }
            };
            param_types.push(ty);
        }

        let ret_type = match ret_type {
            Some(t) => match crate::resolver::resolve_type_expr_to_type(t, resolver) {
                Some(resolved) => Some(resolved),
                None => {
                    let ty_str = type_expr_to_string(t);
                    diagnostics.push(Diagnostic::error(
                        format!(
                            "unknown return type '{}' for function '{}'",
                            ty_str, key_name
                        ),
                        Some(span),
                    ));
                    None
                }
            },
            None => None,
        };

        FnSig {
            params: param_types,
            ret: ret_type,
            abi,
            target_name: target_name.to_string(),
            enum_ctor: None,
        }
    }

    for item in &module.items {
        match item {
            Item::Function(func) => {
                let name = func.name.0.clone();
                if signatures.contains_key(&name) {
                    diagnostics.push(Diagnostic::error(
                        format!("function '{}' is defined multiple times", name),
                        Some(func.span),
                    ));
                    continue;
                }

                let sig = resolve_fn_sig(
                    resolver,
                    &mut diagnostics,
                    &name,
                    &name,
                    &func.params,
                    &func.ret_type,
                    None,
                    false,
                    func.span,
                );
                signatures.insert(name, sig);
            }
            Item::ExternFunction(func) => {
                let name = func.name.0.clone();
                if signatures.contains_key(&name) {
                    diagnostics.push(Diagnostic::error(
                        format!("function '{}' is defined multiple times", name),
                        Some(func.span),
                    ));
                    continue;
                }

                let sig = resolve_fn_sig(
                    resolver,
                    &mut diagnostics,
                    &name,
                    &name,
                    &func.params,
                    &func.ret_type,
                    func.abi.clone(),
                    true,
                    func.span,
                );
                signatures.insert(name, sig);
            }
            _ => {}
        }
    }

    // Also make local functions/externs addressable via fully-qualified module path.
    if let Some(module_id) = &resolver.current_module {
        let module_prefix = module_id.replace('/', "::");
        let local_names: Vec<String> = module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Function(f) => Some(f.name.0.clone()),
                Item::ExternFunction(f) => Some(f.name.0.clone()),
                _ => None,
            })
            .collect();

        for name in local_names {
            let qualified = format!("{}::{}", module_prefix, name);
            if signatures.contains_key(&qualified) {
                continue;
            }
            if let Some(sig) = signatures.get(&name).cloned() {
                signatures.insert(qualified, sig);
            }
        }
    }

    // Enum variant constructors act like functions returning the enum type.
    for (enum_name, enum_ty) in &resolver.enum_types {
        let has_generics = enum_has_generics(enum_name, module, resolver);
        for (idx, variant) in enum_ty.variants.iter().enumerate() {
            let ctor_name = format!("{}::{}", enum_name, variant.name);
            let mut params = Vec::new();
            if let Some(payload_ty) = &variant.payload {
                params.push(Some(payload_ty.clone()));
            }
            let ctor_sig = FnSig {
                params: params.clone(),
                ret: Some(Type::Enum(enum_name.clone())),
                abi: None,
                target_name: ctor_name.clone(),
                enum_ctor: Some(EnumCtorInfo {
                    enum_name: enum_name.clone(),
                    variant_index: idx,
                    has_generics,
                }),
            };
            if !signatures.contains_key(&ctor_name) {
                signatures.insert(ctor_name.clone(), ctor_sig.clone());
            }

            // Also make unqualified variant name available if not shadowed.
            if !signatures.contains_key(&variant.name) {
                let mut unqualified_sig = ctor_sig.clone();
                unqualified_sig.target_name = ctor_name;
                signatures.insert(variant.name.clone(), unqualified_sig);
            }
        }
    }

    // Add signatures for imported symbols so qualified std calls resolve correctly.
    if let (Some(import_scope), Some(all_modules)) = (&resolver.import_scope, &resolver.all_modules)
    {
        // Selective imports (with optional alias)
        for (local_name, (source_module, original_name)) in &import_scope.direct_symbols {
            if signatures.contains_key(local_name) {
                continue;
            }

            if let Some(module) = all_modules.modules.get(source_module) {
                for item in &module.items {
                    match item {
                        Item::Function(func) if func.name.0 == *original_name => {
                            let sig = resolve_fn_sig(
                                resolver,
                                &mut diagnostics,
                                local_name,
                                &func.name.0,
                                &func.params,
                                &func.ret_type,
                                None,
                                false,
                                func.span,
                            );
                            signatures.insert(local_name.clone(), sig);
                            break;
                        }
                        Item::ExternFunction(func) if func.name.0 == *original_name => {
                            let sig = resolve_fn_sig(
                                resolver,
                                &mut diagnostics,
                                local_name,
                                &func.name.0,
                                &func.params,
                                &func.ret_type,
                                func.abi.clone(),
                                true,
                                func.span,
                            );
                            signatures.insert(local_name.clone(), sig);
                            break;
                        }
                        _ => {}
                    }
                }

                if let Some(symbols) = all_modules.module_symbols.get(source_module) {
                    if symbols.structs.contains(original_name) {
                        let prefix = format!("{}::", original_name);
                        for item in &module.items {
                            if let Item::Function(func) = item {
                                if !func.name.0.starts_with(&prefix) {
                                    continue;
                                }
                                if signatures.contains_key(&func.name.0) {
                                    continue;
                                }
                                let sig = resolve_fn_sig(
                                    resolver,
                                    &mut diagnostics,
                                    &func.name.0,
                                    &func.name.0,
                                    &func.params,
                                    &func.ret_type,
                                    None,
                                    false,
                                    func.span,
                                );
                                signatures.insert(func.name.0.clone(), sig);
                            }
                        }
                    }
                }
            }
        }

        // Wildcard imports: make qualified names available (e.g., std::io::fopen).
        for wildcard in &import_scope.wildcard_modules {
            for (module_id, module) in all_modules
                .modules
                .iter()
                .filter(|(id, _)| *id == wildcard || id.starts_with(&format!("{}/", wildcard)))
            {
                let module_prefix = module_id.replace('/', "::");
                for item in &module.items {
                    match item {
                        Item::Function(func) => {
                            let key = format!("{}::{}", module_prefix, func.name.0);
                            if signatures.contains_key(&key) {
                                continue;
                            }
                            let sig = resolve_fn_sig(
                                resolver,
                                &mut diagnostics,
                                &key,
                                &func.name.0,
                                &func.params,
                                &func.ret_type,
                                None,
                                false,
                                func.span,
                            );
                            signatures.insert(key, sig);

                            if func.name.0.contains("::") && !signatures.contains_key(&func.name.0)
                            {
                                let sig = resolve_fn_sig(
                                    resolver,
                                    &mut diagnostics,
                                    &func.name.0,
                                    &func.name.0,
                                    &func.params,
                                    &func.ret_type,
                                    None,
                                    false,
                                    func.span,
                                );
                                signatures.insert(func.name.0.clone(), sig);
                            }
                        }
                        Item::ExternFunction(func) => {
                            let key = format!("{}::{}", module_prefix, func.name.0);
                            if signatures.contains_key(&key) {
                                continue;
                            }
                            let sig = resolve_fn_sig(
                                resolver,
                                &mut diagnostics,
                                &key,
                                &func.name.0,
                                &func.params,
                                &func.ret_type,
                                func.abi.clone(),
                                true,
                                func.span,
                            );
                            signatures.insert(key, sig);
                        }
                        _ => {}
                    }
                }
            }
        }

        // Built-in std helpers needed for print/println even without explicit imports.
        if all_modules.module_symbols.contains_key("std/io") {
            let mut insert_builtin =
                |key: &str, params: Vec<Option<Type>>, ret: Option<Type>, abi: Option<String>| {
                    if signatures.contains_key(key) {
                        return;
                    }
                    signatures.insert(
                        key.to_string(),
                        FnSig {
                            params,
                            ret,
                            abi,
                            target_name: key.split("::").last().unwrap_or(key).to_string(),
                            enum_ctor: None,
                        },
                    );
                };

            insert_builtin(
                "std::io::raw_write",
                vec![
                    Some(Type::I32),
                    Some(Type::RawPtr(Box::new(Type::U8))),
                    Some(Type::U32),
                ],
                Some(Type::I32),
                Some("C".into()),
            );

            // Scalar/string print segments lower to direct calls into the C
            // formatting runtime (glyph_fmt_write_*); those externs are
            // declared in lower_module, not here. Only struct formatting still
            // resolves through std::fmt::fmt_<type> signatures.
        }
    }

    (signatures, diagnostics)
}

pub(crate) fn enum_has_generics(
    enum_name: &str,
    module: &Module,
    resolver: &ResolverContext,
) -> bool {
    if let Some(all_modules) = &resolver.all_modules {
        for (module_id, module) in &all_modules.modules {
            let module_prefix = module_id.replace('/', "::");
            for item in &module.items {
                if let Item::Enum(def) = item {
                    let qualified = format!("{}::{}", module_prefix, def.name.0);
                    if def.name.0 == enum_name || qualified == enum_name {
                        return !def.generic_params.is_empty();
                    }
                }
            }
        }
    }

    for item in &module.items {
        if let Item::Enum(def) = item {
            if def.name.0 == enum_name {
                return !def.generic_params.is_empty();
            }
        }
    }

    false
}
