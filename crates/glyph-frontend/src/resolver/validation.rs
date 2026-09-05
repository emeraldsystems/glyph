use std::collections::HashSet;

use glyph_core::{
    ast::{Item, Module, TypeExpr},
    diag::Diagnostic,
    types::Type,
};

use super::{
    ResolvedSymbol, ResolverContext, resolve_type_expr_to_type, struct_generic_params,
    type_expr_to_string,
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
    validate_borrowed_callable_placements(module, ctx, diagnostics);
    validate_scoped_thread_placements(module, ctx, diagnostics);
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

fn type_contains_scoped_thread_value(ty: &Type) -> bool {
    match ty {
        Type::Named(name) => name == glyph_core::thread::SCOPED_THREAD_SCOPE_TYPE,
        Type::App { base, args } => {
            base == glyph_core::thread::SCOPED_THREAD_HANDLE_TYPE
                || args.iter().any(type_contains_scoped_thread_value)
        }
        Type::Array(inner, _)
        | Type::Own(inner)
        | Type::RawPtr(inner)
        | Type::Shared(inner)
        | Type::Ref(inner, _) => type_contains_scoped_thread_value(inner),
        Type::Tuple(elements) => elements.iter().any(type_contains_scoped_thread_value),
        Type::Function { params, ret } | Type::BorrowedFunction { params, ret, .. } => {
            params.iter().any(type_contains_scoped_thread_value)
                || type_contains_scoped_thread_value(ret)
        }
        _ => false,
    }
}

fn scoped_thread_identity_is_resolver_issued(ty: &TypeExpr, ctx: &ResolverContext) -> bool {
    match ty {
        TypeExpr::Path { segments, .. } => {
            let name = segments.join("::");
            let leaf = segments.last().map(String::as_str);
            match leaf {
                Some("Scope") => matches!(
                    ctx.resolve_symbol(&name),
                    Some(ResolvedSymbol::Struct(module, symbol))
                        if module == "std/thread" && symbol == "Scope"
                ),
                Some("ScopedJoinHandle") => matches!(
                    ctx.resolve_symbol(&name),
                    Some(ResolvedSymbol::Struct(module, symbol))
                        if module == "std/thread" && symbol == "ScopedJoinHandle"
                ),
                _ => true,
            }
        }
        TypeExpr::App { base, args, .. } => {
            scoped_thread_identity_is_resolver_issued(base, ctx)
                && args
                    .iter()
                    .all(|arg| scoped_thread_identity_is_resolver_issued(arg, ctx))
        }
        TypeExpr::Ref { inner, .. } => scoped_thread_identity_is_resolver_issued(inner, ctx),
        TypeExpr::Array { elem, .. } => scoped_thread_identity_is_resolver_issued(elem, ctx),
        TypeExpr::Tuple { elements, .. } => elements
            .iter()
            .all(|element| scoped_thread_identity_is_resolver_issued(element, ctx)),
    }
}

fn reject_scoped_thread_type(
    ty: &TypeExpr,
    allow_direct: bool,
    placement: &str,
    ctx: &ResolverContext,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(resolved) = resolve_type_expr_to_type(ty, ctx) else {
        return;
    };
    if type_contains_scoped_thread_value(&resolved)
        && !scoped_thread_identity_is_resolver_issued(ty, ctx)
    {
        diagnostics.push(Diagnostic::error(
            "Scope and ScopedJoinHandle are compiler-owned identities and must resolve from std/thread",
            Some(ty.span()),
        ));
        return;
    }
    let direct = matches!(
        &resolved,
        Type::Named(name)
            if name == glyph_core::thread::SCOPED_THREAD_SCOPE_TYPE
                || name == glyph_core::thread::SCOPED_THREAD_HANDLE_TYPE
    ) || glyph_core::thread::is_canonical_scoped_thread_handle(&resolved)
        || matches!(
            &resolved,
            Type::Ref(inner, _)
                if matches!(inner.as_ref(), Type::Named(name)
                    if name == glyph_core::thread::SCOPED_THREAD_SCOPE_TYPE)
        );
    if type_contains_scoped_thread_value(&resolved) && !(allow_direct && direct) {
        diagnostics.push(Diagnostic::error(
            format!(
                "a scoped-thread token cannot appear in {placement}; Scope and ScopedJoinHandle values must remain in direct lexical bindings"
            ),
            Some(ty.span()),
        ));
    }
}

fn validate_scoped_function_placements(
    function: &glyph_core::ast::Function,
    allow_scoped_return: bool,
    allow_scope_callback: bool,
    ctx: &ResolverContext,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for param in &function.params {
        if let Some(ty) = &param.ty {
            if !allow_scope_callback {
                reject_scoped_thread_type(ty, true, "a nested parameter type", ctx, diagnostics);
            }
        }
    }
    if let Some(ret) = &function.ret_type
        && !allow_scoped_return
    {
        reject_scoped_thread_type(ret, false, "a function return type", ctx, diagnostics);
    }
}

fn validate_scoped_thread_placements(
    module: &Module,
    ctx: &ResolverContext,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let compiling_thread_module = ctx.current_module.as_deref() == Some("std/thread");
    let canonical_scope = compiling_thread_module
        || matches!(
            ctx.resolve_symbol("scope"),
            Some(ResolvedSymbol::Function(module, symbol))
                if module == "std/thread" && symbol == "scope"
        );
    let canonical_scope_type = compiling_thread_module
        || matches!(
            ctx.resolve_symbol("Scope"),
            Some(ResolvedSymbol::Struct(module, symbol))
                if module == "std/thread" && symbol == "Scope"
        );
    for item in &module.items {
        match item {
            Item::Struct(definition) => {
                for field in &definition.fields {
                    reject_scoped_thread_type(&field.ty, false, "a struct field", ctx, diagnostics);
                }
                for method in &definition.methods {
                    let intrinsic_spawn = canonical_scope_type
                        && definition.name.0 == "Scope"
                        && method.name.0 == "spawn";
                    validate_scoped_function_placements(
                        method,
                        intrinsic_spawn,
                        false,
                        ctx,
                        diagnostics,
                    );
                }
                for implementation in &definition.inline_impls {
                    for method in &implementation.methods {
                        let intrinsic_spawn = canonical_scope_type
                            && definition.name.0 == "Scope"
                            && method.name.0 == "spawn";
                        validate_scoped_function_placements(
                            method,
                            intrinsic_spawn,
                            false,
                            ctx,
                            diagnostics,
                        );
                    }
                }
            }
            Item::Enum(definition) => {
                for variant in &definition.variants {
                    if let Some(payload) = &variant.payload {
                        reject_scoped_thread_type(
                            payload,
                            false,
                            "an enum payload",
                            ctx,
                            diagnostics,
                        );
                    }
                }
            }
            Item::Function(function) => {
                let intrinsic_scope = canonical_scope && function.name.0 == "scope";
                let intrinsic_spawn = canonical_scope && function.name.0 == "Scope::spawn";
                validate_scoped_function_placements(
                    function,
                    intrinsic_spawn,
                    intrinsic_scope,
                    ctx,
                    diagnostics,
                )
            }
            Item::Impl(implementation) => {
                for method in &implementation.methods {
                    validate_scoped_function_placements(method, false, false, ctx, diagnostics);
                }
            }
            Item::Interface(interface) => {
                for method in &interface.methods {
                    for param in &method.params {
                        if let Some(ty) = &param.ty {
                            reject_scoped_thread_type(
                                ty,
                                true,
                                "a nested interface parameter type",
                                ctx,
                                diagnostics,
                            );
                        }
                    }
                    if let Some(ret) = &method.ret_type {
                        reject_scoped_thread_type(
                            ret,
                            false,
                            "an interface return type",
                            ctx,
                            diagnostics,
                        );
                    }
                }
            }
            Item::ExternFunction(function) => {
                for param in &function.params {
                    if let Some(ty) = &param.ty {
                        reject_scoped_thread_type(
                            ty,
                            false,
                            "an extern parameter type",
                            ctx,
                            diagnostics,
                        );
                    }
                }
                if let Some(ret) = &function.ret_type {
                    reject_scoped_thread_type(
                        ret,
                        false,
                        "an extern return type",
                        ctx,
                        diagnostics,
                    );
                }
            }
            Item::Const(definition) => {
                reject_scoped_thread_type(&definition.ty, false, "a const type", ctx, diagnostics)
            }
        }
    }
}

fn type_contains_borrowed_callable(ty: &Type) -> bool {
    match ty {
        Type::BorrowedFunction { .. } => true,
        Type::Array(inner, _)
        | Type::Own(inner)
        | Type::RawPtr(inner)
        | Type::Shared(inner)
        | Type::Ref(inner, _) => type_contains_borrowed_callable(inner),
        Type::App { args, .. } | Type::Tuple(args) | Type::Function { params: args, .. } => {
            args.iter().any(type_contains_borrowed_callable)
                || matches!(ty, Type::Function { ret, .. } if type_contains_borrowed_callable(ret))
        }
        _ => false,
    }
}

fn reject_borrowed_callable_type(
    ty: &TypeExpr,
    allow_direct: bool,
    placement: &str,
    ctx: &ResolverContext,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(resolved) = resolve_type_expr_to_type(ty, ctx) else {
        return;
    };
    if type_contains_borrowed_callable(&resolved)
        && !(allow_direct && matches!(resolved, Type::BorrowedFunction { .. }))
    {
        diagnostics.push(Diagnostic::error(
            format!(
                "a borrowed callable is noescape and cannot appear in {placement}; use a direct Fn/FnMut parameter or local binding"
            ),
            Some(ty.span()),
        ));
    }
}

fn validate_function_borrowed_callable_placements(
    function: &glyph_core::ast::Function,
    ctx: &ResolverContext,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for param in &function.params {
        if let Some(ty) = &param.ty {
            reject_borrowed_callable_type(ty, true, "a nested parameter type", ctx, diagnostics);
        }
    }
    if let Some(ret) = &function.ret_type {
        reject_borrowed_callable_type(ret, false, "a function return type", ctx, diagnostics);
    }
    for statement in &function.body.stmts {
        if let glyph_core::ast::Stmt::Let { ty: Some(ty), .. } = statement {
            reject_borrowed_callable_type(ty, true, "a nested local type", ctx, diagnostics);
        }
    }
}

fn validate_borrowed_callable_placements(
    module: &Module,
    ctx: &ResolverContext,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for item in &module.items {
        match item {
            Item::Struct(definition) => {
                for field in &definition.fields {
                    reject_borrowed_callable_type(
                        &field.ty,
                        false,
                        "a struct field",
                        ctx,
                        diagnostics,
                    );
                }
                for method in &definition.methods {
                    validate_function_borrowed_callable_placements(method, ctx, diagnostics);
                }
                for implementation in &definition.inline_impls {
                    for method in &implementation.methods {
                        validate_function_borrowed_callable_placements(method, ctx, diagnostics);
                    }
                }
            }
            Item::Enum(definition) => {
                for variant in &definition.variants {
                    if let Some(payload) = &variant.payload {
                        reject_borrowed_callable_type(
                            payload,
                            false,
                            "an enum payload",
                            ctx,
                            diagnostics,
                        );
                    }
                }
            }
            Item::Function(function) => {
                validate_function_borrowed_callable_placements(function, ctx, diagnostics)
            }
            Item::Impl(implementation) => {
                for method in &implementation.methods {
                    validate_function_borrowed_callable_placements(method, ctx, diagnostics);
                }
            }
            Item::Interface(interface) => {
                for method in &interface.methods {
                    for param in &method.params {
                        if let Some(ty) = &param.ty {
                            reject_borrowed_callable_type(
                                ty,
                                true,
                                "a nested interface parameter type",
                                ctx,
                                diagnostics,
                            );
                        }
                    }
                    if let Some(ret) = &method.ret_type {
                        reject_borrowed_callable_type(
                            ret,
                            false,
                            "an interface return type",
                            ctx,
                            diagnostics,
                        );
                    }
                }
            }
            Item::ExternFunction(function) => {
                for param in &function.params {
                    if let Some(ty) = &param.ty {
                        reject_borrowed_callable_type(
                            ty,
                            false,
                            "an extern parameter type",
                            ctx,
                            diagnostics,
                        );
                    }
                }
                if let Some(ret) = &function.ret_type {
                    reject_borrowed_callable_type(
                        ret,
                        false,
                        "an extern return type",
                        ctx,
                        diagnostics,
                    );
                }
            }
            Item::Const(definition) => reject_borrowed_callable_type(
                &definition.ty,
                false,
                "a const type",
                ctx,
                diagnostics,
            ),
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
                if matches!(base_name.as_str(), "FnOnce" | "Fn" | "FnMut") && args.len() != 2 {
                    diagnostics.push(Diagnostic::error(
                        format!(
                            "{base_name} expects 2 type arguments but got {}",
                            args.len()
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
            | Type::I16
            | Type::I32
            | Type::I64
            | Type::U8
            | Type::U16
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
