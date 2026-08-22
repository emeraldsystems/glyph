use std::collections::HashSet;

use glyph_core::{
    ast::{Item, Module, TypeExpr},
    diag::Diagnostic,
    types::Type,
};

use super::{
    ResolverContext, resolve_type_expr_to_type, struct_generic_params, type_expr_to_string,
};

const HASH_INTERFACE: &str = "Hash";

/// Validate that all Named types referenced in struct fields / enum payloads
/// exist in the resolver context.
///
/// Note: this must run *after* `populate_imported_types()` for multi-module
/// compilation, otherwise references to imported types will be flagged as
/// undefined.
pub fn validate_named_types(
    module: &Module,
    ctx: &ResolverContext,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let struct_names: std::collections::HashSet<_> = ctx.struct_types.keys().cloned().collect();
    let enum_names: std::collections::HashSet<_> = ctx.enum_types.keys().cloned().collect();

    for item in &module.items {
        if let Item::Struct(s) = item {
            let struct_name = &s.name.0;
            let generic_params: std::collections::HashSet<String> =
                s.generic_params.iter().map(|p| p.0.clone()).collect();

            if let Some(struct_type) = ctx.struct_types.get(struct_name) {
                for (field_name, field_type) in &struct_type.fields {
                    if let Type::Named(type_name) = field_type {
                        let is_generic = generic_params.contains(type_name);
                        let is_known =
                            struct_names.contains(type_name) || enum_names.contains(type_name);
                        if !is_generic && !is_known {
                            diagnostics.push(Diagnostic::error(
                                format!(
                                    "undefined type '{}' used in field '{}' of struct '{}'",
                                    type_name, field_name, struct_name
                                ),
                                Some(s.span),
                            ));
                        }
                    }
                }
            }
        }
    }

    for item in &module.items {
        if let Item::Enum(e) = item {
            if let Some(enum_type) = ctx.enum_types.get(&e.name.0) {
                let generic_params: std::collections::HashSet<String> =
                    e.generic_params.iter().map(|p| p.0.clone()).collect();
                for variant in &enum_type.variants {
                    if let Some(Type::Named(type_name)) = &variant.payload {
                        let is_generic = generic_params.contains(type_name);
                        let is_known =
                            struct_names.contains(type_name) || enum_names.contains(type_name);
                        if !is_generic && !is_known {
                            diagnostics.push(Diagnostic::error(
                                format!(
                                    "undefined type '{}' used in variant '{}' of enum '{}'",
                                    type_name, variant.name, e.name.0
                                ),
                                Some(e.span),
                            ));
                        }
                    }
                }
            }
        }
    }

    for item in &module.items {
        if let Item::Const(def) = item {
            let ty = resolve_type_expr_to_type(&def.ty, ctx)
                .unwrap_or_else(|| Type::Named(type_expr_to_string(&def.ty)));
            if let Type::Named(type_name) = ty {
                let is_known = struct_names.contains(&type_name) || enum_names.contains(&type_name);
                if !is_known {
                    diagnostics.push(Diagnostic::error(
                        format!(
                            "undefined type '{}' used in const '{}'",
                            type_name, def.name.0
                        ),
                        Some(def.span),
                    ));
                }
            }
        }
    }
}

pub fn validate_map_usage(
    module: &Module,
    ctx: &ResolverContext,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for item in &module.items {
        match item {
            Item::Struct(def) => {
                let generics: HashSet<String> =
                    def.generic_params.iter().map(|p| p.0.clone()).collect();
                for field in &def.fields {
                    validate_map_type_expr(&field.ty, ctx, &generics, module, diagnostics);
                }
                for method in &def.methods {
                    validate_map_function(method, ctx, &generics, module, diagnostics);
                }
                for inline in &def.inline_impls {
                    for method in &inline.methods {
                        validate_map_function(method, ctx, &generics, module, diagnostics);
                    }
                }
            }
            Item::Enum(def) => {
                let generics: HashSet<String> =
                    def.generic_params.iter().map(|p| p.0.clone()).collect();
                for variant in &def.variants {
                    if let Some(payload) = &variant.payload {
                        validate_map_type_expr(payload, ctx, &generics, module, diagnostics);
                    }
                }
            }
            Item::Interface(def) => {
                let generics = HashSet::new();
                for method in &def.methods {
                    for param in &method.params {
                        if let Some(ty) = &param.ty {
                            validate_map_type_expr(ty, ctx, &generics, module, diagnostics);
                        }
                    }
                    if let Some(ret) = &method.ret_type {
                        validate_map_type_expr(ret, ctx, &generics, module, diagnostics);
                    }
                }
            }
            Item::Impl(block) => {
                let generics = struct_generic_params(block.target.0.as_str(), module, ctx);
                for method in &block.methods {
                    validate_map_function(method, ctx, &generics, module, diagnostics);
                }
            }
            Item::Function(func) => {
                let generics = HashSet::new();
                validate_map_function(func, ctx, &generics, module, diagnostics);
            }
            Item::ExternFunction(func) => {
                let generics = HashSet::new();
                for param in &func.params {
                    if let Some(ty) = &param.ty {
                        validate_map_type_expr(ty, ctx, &generics, module, diagnostics);
                    }
                }
                if let Some(ret) = &func.ret_type {
                    validate_map_type_expr(ret, ctx, &generics, module, diagnostics);
                }
            }
            Item::Const(def) => {
                let generics = HashSet::new();
                validate_map_type_expr(&def.ty, ctx, &generics, module, diagnostics);
            }
        }
    }
}

fn validate_map_function(
    func: &glyph_core::ast::Function,
    ctx: &ResolverContext,
    generics: &HashSet<String>,
    module: &Module,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for param in &func.params {
        if let Some(ty) = &param.ty {
            validate_map_type_expr(ty, ctx, generics, module, diagnostics);
        }
    }
    if let Some(ret) = &func.ret_type {
        validate_map_type_expr(ret, ctx, generics, module, diagnostics);
    }
    validate_map_block(&func.body, ctx, generics, module, diagnostics);
}

fn validate_map_block(
    block: &glyph_core::ast::Block,
    ctx: &ResolverContext,
    generics: &HashSet<String>,
    module: &Module,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for stmt in &block.stmts {
        if let glyph_core::ast::Stmt::Let { ty: Some(ty), .. } = stmt {
            validate_map_type_expr(ty, ctx, generics, module, diagnostics);
        }
    }
}

fn validate_map_type_expr(
    ty: &TypeExpr,
    ctx: &ResolverContext,
    generics: &HashSet<String>,
    module: &Module,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match ty {
        TypeExpr::App { base, args, span } => {
            if let TypeExpr::Path { segments, .. } = base.as_ref() {
                let base_name = segments.join("::");
                if base_name == "Map" {
                    if args.len() != 2 {
                        diagnostics.push(Diagnostic::error(
                            format!("Map expects 2 type arguments but got {}", args.len()),
                            Some(*span),
                        ));
                    } else if !map_key_is_hashable(&args[0], ctx, generics, module) {
                        diagnostics.push(Diagnostic::error(
                            format!(
                                "Map key type '{}' must implement Hash",
                                type_expr_to_string(&args[0])
                            ),
                            Some(*span),
                        ));
                    }
                }
                if base_name == "FnOnce" && args.len() != 2 {
                    diagnostics.push(Diagnostic::error(
                        format!("FnOnce expects 2 type arguments but got {}", args.len()),
                        Some(*span),
                    ));
                }
                if matches!(base_name.as_str(), "Fn" | "FnMut") {
                    diagnostics.push(Diagnostic::error(
                        format!(
                            "{base_name} closures are not supported in v0; use owned FnOnce<Args, Return>"
                        ),
                        Some(*span),
                    ));
                }
            }
            for arg in args {
                validate_map_type_expr(arg, ctx, generics, module, diagnostics);
            }
        }
        TypeExpr::Ref { inner, .. } => {
            validate_map_type_expr(inner, ctx, generics, module, diagnostics);
        }
        TypeExpr::Array { elem, .. } => {
            validate_map_type_expr(elem, ctx, generics, module, diagnostics);
        }
        TypeExpr::Tuple { elements, .. } => {
            for elem in elements {
                validate_map_type_expr(elem, ctx, generics, module, diagnostics);
            }
        }
        TypeExpr::Path { .. } => {}
    }
}

fn map_key_is_hashable(
    key: &TypeExpr,
    ctx: &ResolverContext,
    generics: &HashSet<String>,
    module: &Module,
) -> bool {
    if let TypeExpr::Path { segments, .. } = key {
        if segments.len() == 1 && generics.contains(&segments[0]) {
            return true;
        }
    }

    let key_type = resolve_type_expr_to_type(key, ctx)
        .unwrap_or_else(|| Type::Named(type_expr_to_string(key)));
    if is_hashable_scalar(&key_type) {
        return true;
    }

    match key_type {
        Type::Named(name) => struct_supports_hash(name.as_str(), module, ctx),
        Type::Param(_) => true,
        _ => false,
    }
}

fn is_hashable_scalar(ty: &Type) -> bool {
    matches!(
        ty,
        Type::I8
            | Type::I32
            | Type::I64
            | Type::U8
            | Type::U32
            | Type::U64
            | Type::Usize
            | Type::Bool
            | Type::Char
            | Type::RawPtr(_)
            | Type::Ref(_, _)
            | Type::String
            | Type::Str
    )
}

fn struct_supports_hash(struct_name: &str, module: &Module, ctx: &ResolverContext) -> bool {
    if let Some(impls) = ctx.interface_impls.get(struct_name) {
        if impls.contains_key(HASH_INTERFACE) {
            return true;
        }
    }

    if module_struct_has_hash(module, struct_name) {
        return true;
    }

    if let Some(all_modules) = ctx.all_modules.as_ref() {
        if let Some(crate::resolver::ResolvedSymbol::Struct(module_id, resolved_name)) =
            ctx.resolve_symbol(struct_name)
        {
            if let Some(target) = all_modules.modules.get(&module_id) {
                return module_struct_has_hash(target, &resolved_name);
            }
        }
    }

    false
}

fn module_struct_has_hash(module: &Module, struct_name: &str) -> bool {
    for item in &module.items {
        match item {
            Item::Struct(def) if def.name.0 == struct_name => {
                if def.interfaces.iter().any(|iface| iface.0 == HASH_INTERFACE) {
                    return true;
                }
                if def
                    .inline_impls
                    .iter()
                    .any(|inline| inline.interface.0 == HASH_INTERFACE)
                {
                    return true;
                }
            }
            Item::Impl(block)
                if block.target.0 == struct_name && block.interface.0 == HASH_INTERFACE =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}
