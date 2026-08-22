use std::collections::{HashMap, HashSet};

use glyph_core::{
    ast::{EnumDef, Item, StructDef},
    types::Type,
};

use crate::method_symbols::inherent_method_symbol;
use crate::module_resolver::MultiModuleContext;

use super::{
    MethodInfo, ResolverContext, collect_type_names, detect_self_kind, enum_type_from_def,
    find_enum_def, find_struct_def, struct_type_from_def,
};

/// Populate the resolver context with struct/enum types made available through imports.
/// This enables cross-module type usage during lowering without requiring explicit
/// duplication in the source module.
pub fn populate_imported_types(ctx: &mut ResolverContext) {
    let (Some(scope), Some(all_modules)) = (ctx.import_scope.clone(), ctx.all_modules.clone())
    else {
        return;
    };

    let mut visited: HashSet<(String, String)> = HashSet::new();

    for (local_name, (source_module, original_name)) in &scope.direct_symbols {
        if source_module == "std/thread" && original_name == "spawn" {
            for (dependency_module, dependency_name) in [
                ("std/thread", "JoinHandle"),
                ("std/thread", "ThreadError"),
                ("std/enums", "Result"),
            ] {
                import_named_type(
                    ctx,
                    &all_modules,
                    dependency_module,
                    dependency_name,
                    None,
                    &mut visited,
                );
            }
        }
        if source_module == "std/sync" && original_name == "Mutex" {
            for (dependency_module, dependency_name) in
                [("std/sync", "MutexGuard"), ("std/enums", "Option")]
            {
                import_named_type(
                    ctx,
                    &all_modules,
                    dependency_module,
                    dependency_name,
                    None,
                    &mut visited,
                );
            }
        }
        if ctx.struct_types.contains_key(local_name) || ctx.enum_types.contains_key(local_name) {
            continue;
        }
        import_named_type(
            ctx,
            &all_modules,
            source_module,
            original_name,
            Some(local_name.as_str()),
            &mut visited,
        );
    }

    for wildcard in &scope.wildcard_modules {
        for (module_id, module) in all_modules
            .modules
            .iter()
            .filter(|(id, _)| *id == wildcard || id.starts_with(&format!("{}/", wildcard)))
        {
            for item in &module.items {
                match item {
                    Item::Struct(s) => {
                        if ctx.struct_types.contains_key(&s.name.0) {
                            continue;
                        }
                        import_struct_with_deps(
                            ctx,
                            &all_modules,
                            module_id,
                            s,
                            None,
                            &mut visited,
                        );
                    }
                    Item::Enum(e) => {
                        if ctx.enum_types.contains_key(&e.name.0) {
                            continue;
                        }
                        import_enum_with_deps(ctx, &all_modules, module_id, e, None, &mut visited);
                    }
                    _ => {}
                }
            }
        }
    }
}

fn import_named_type(
    ctx: &mut ResolverContext,
    all_modules: &MultiModuleContext,
    source_module: &str,
    original_name: &str,
    local_name: Option<&str>,
    visited: &mut HashSet<(String, String)>,
) {
    let Some(module) = all_modules.modules.get(source_module) else {
        return;
    };

    if let Some(def) = find_struct_def(module, original_name) {
        import_struct_with_deps(ctx, all_modules, source_module, def, local_name, visited);
        return;
    }

    if let Some(def) = find_enum_def(module, original_name) {
        import_enum_with_deps(ctx, all_modules, source_module, def, local_name, visited);
    }
}

fn import_struct_with_deps(
    ctx: &mut ResolverContext,
    all_modules: &MultiModuleContext,
    source_module: &str,
    def: &StructDef,
    local_name: Option<&str>,
    visited: &mut HashSet<(String, String)>,
) {
    let key = (source_module.to_string(), def.name.0.clone());
    let first_visit = visited.insert(key);

    if first_visit {
        let Some(module_symbols) = all_modules.module_symbols.get(source_module) else {
            return;
        };
        let generics: HashSet<String> = def.generic_params.iter().map(|p| p.0.clone()).collect();
        let mut referenced = HashSet::new();
        for field in &def.fields {
            collect_type_names(&field.ty, &mut referenced);
        }

        if let Some(module) = all_modules.modules.get(source_module) {
            for name in referenced {
                if generics.contains(&name) {
                    continue;
                }
                if Type::from_name(&name).is_some() {
                    continue;
                }
                if name.contains("::") {
                    continue;
                }

                if module_symbols.structs.contains(&name) {
                    if let Some(dep) = find_struct_def(module, &name) {
                        import_struct_with_deps(
                            ctx,
                            all_modules,
                            source_module,
                            dep,
                            None,
                            visited,
                        );
                    }
                } else if module_symbols.enums.contains(&name) {
                    if let Some(dep) = find_enum_def(module, &name) {
                        import_enum_with_deps(ctx, all_modules, source_module, dep, None, visited);
                    }
                }
            }
        }

        if !ctx.struct_types.contains_key(&def.name.0) {
            ctx.struct_types.insert(
                def.name.0.clone(),
                glyph_core::types::StructType {
                    name: def.name.0.clone(),
                    fields: Vec::new(),
                },
            );
            let st = struct_type_from_def(def, ctx);
            ctx.struct_types.insert(def.name.0.clone(), st);
        }

        if !def.methods.is_empty() {
            let entry = ctx
                .inherent_methods
                .entry(def.name.0.clone())
                .or_insert_with(HashMap::new);
            for method in &def.methods {
                if entry.contains_key(&method.name.0) {
                    continue;
                }
                let mangled_name = inherent_method_symbol(&def.name.0, &method.name.0);
                let self_kind = if let Some(first_param) = method.params.first() {
                    detect_self_kind(&first_param.ty)
                } else {
                    super::SelfKind::ByValue
                };
                entry.insert(
                    method.name.0.clone(),
                    MethodInfo {
                        function_name: mangled_name,
                        self_kind,
                    },
                );
            }
        }
    }

    if let Some(local) = local_name {
        if local != def.name.0 && !ctx.struct_types.contains_key(local) {
            let mut st = ctx
                .struct_types
                .get(&def.name.0)
                .cloned()
                .unwrap_or_else(|| struct_type_from_def(def, ctx));
            st.name = local.to_string();
            ctx.struct_types.insert(local.to_string(), st);
        }
    }
}

fn import_enum_with_deps(
    ctx: &mut ResolverContext,
    all_modules: &MultiModuleContext,
    source_module: &str,
    def: &EnumDef,
    local_name: Option<&str>,
    visited: &mut HashSet<(String, String)>,
) {
    let key = (source_module.to_string(), def.name.0.clone());
    let first_visit = visited.insert(key);

    if first_visit {
        let Some(module_symbols) = all_modules.module_symbols.get(source_module) else {
            return;
        };
        let generics: HashSet<String> = def.generic_params.iter().map(|p| p.0.clone()).collect();
        let mut referenced = HashSet::new();
        for variant in &def.variants {
            if let Some(payload) = &variant.payload {
                collect_type_names(payload, &mut referenced);
            }
        }

        if let Some(module) = all_modules.modules.get(source_module) {
            for name in referenced {
                if generics.contains(&name) {
                    continue;
                }
                if Type::from_name(&name).is_some() {
                    continue;
                }
                if name.contains("::") {
                    continue;
                }

                if module_symbols.structs.contains(&name) {
                    if let Some(dep) = find_struct_def(module, &name) {
                        import_struct_with_deps(
                            ctx,
                            all_modules,
                            source_module,
                            dep,
                            None,
                            visited,
                        );
                    }
                } else if module_symbols.enums.contains(&name) {
                    if let Some(dep) = find_enum_def(module, &name) {
                        import_enum_with_deps(ctx, all_modules, source_module, dep, None, visited);
                    }
                }
            }
        }

        if !ctx.enum_types.contains_key(&def.name.0) {
            ctx.enum_types.insert(
                def.name.0.clone(),
                glyph_core::types::EnumType {
                    name: def.name.0.clone(),
                    variants: Vec::new(),
                },
            );
            let et = enum_type_from_def(def, ctx);
            ctx.enum_types.insert(def.name.0.clone(), et);
        }
    }

    if let Some(local) = local_name {
        if local != def.name.0 && !ctx.enum_types.contains_key(local) {
            let mut et = ctx
                .enum_types
                .get(&def.name.0)
                .cloned()
                .unwrap_or_else(|| enum_type_from_def(def, ctx));
            et.name = local.to_string();
            ctx.enum_types.insert(local.to_string(), et);
        }
    }
}
