use std::collections::{HashMap, HashSet, VecDeque};

use glyph_core::{
    ast::{Item, Module, TypeExpr},
    diag::Diagnostic,
    mir::{LocalId, MirInst, MirModule, Rvalue},
    types::{EnumType, EnumVariant, Mutability, StructType, Type},
};

#[derive(Debug, Clone)]
enum Template {
    Struct {
        params: Vec<String>,
        fields: Vec<(String, Type)>,
    },
    Enum {
        params: Vec<String>,
        variants: Vec<EnumVariant>,
    },
}

fn collect_templates(modules: &HashMap<String, Module>) -> HashMap<String, Template> {
    let mut templates = HashMap::new();

    for (module_id, module) in modules {
        let module_prefix = module_id.replace('/', "::");

        for item in &module.items {
            match item {
                Item::Struct(def) if !def.generic_params.is_empty() => {
                    let params: Vec<String> =
                        def.generic_params.iter().map(|p| p.0.clone()).collect();
                    let param_set: HashSet<String> = params.iter().cloned().collect();
                    let fields = def
                        .fields
                        .iter()
                        .map(|f| (f.name.0.clone(), type_expr_to_type(&f.ty, &param_set)))
                        .collect();

                    let templ = Template::Struct {
                        params: params.clone(),
                        fields,
                    };
                    templates
                        .entry(def.name.0.clone())
                        .or_insert_with(|| templ.clone());
                    templates
                        .entry(format!("{}::{}", module_prefix, def.name.0))
                        .or_insert(templ);
                }
                Item::Enum(def) if !def.generic_params.is_empty() => {
                    let params: Vec<String> =
                        def.generic_params.iter().map(|p| p.0.clone()).collect();
                    let param_set: HashSet<String> = params.iter().cloned().collect();
                    let variants: Vec<EnumVariant> = def
                        .variants
                        .iter()
                        .map(|v| EnumVariant {
                            name: v.name.0.clone(),
                            payload: v.payload.as_ref().map(|p| type_expr_to_type(p, &param_set)),
                        })
                        .collect();

                    let templ = Template::Enum {
                        params: params.clone(),
                        variants,
                    };
                    templates
                        .entry(def.name.0.clone())
                        .or_insert_with(|| templ.clone());
                    templates
                        .entry(format!("{}::{}", module_prefix, def.name.0))
                        .or_insert(templ);
                }
                _ => {}
            }
        }
    }

    templates
}

fn type_expr_to_type(expr: &TypeExpr, param_set: &HashSet<String>) -> Type {
    match expr {
        TypeExpr::Path { segments, .. } => {
            let name = segments.join("::");
            if segments.len() == 1 && param_set.contains(&name) {
                return Type::Param(name);
            }
            if let Some(builtin) = Type::from_name(&name) {
                return builtin;
            }
            if name == "str" {
                return Type::Str;
            }
            Type::Named(name)
        }
        TypeExpr::Ref {
            mutability, inner, ..
        } => Type::Ref(Box::new(type_expr_to_type(inner, param_set)), *mutability),
        TypeExpr::Array { elem, size, .. } => {
            Type::Array(Box::new(type_expr_to_type(elem, param_set)), *size)
        }
        TypeExpr::App { base, args, .. } => {
            let base_name = match base.as_ref() {
                TypeExpr::Path { segments, .. } => segments.join("::"),
                _ => "<unknown>".to_string(),
            };

            let mut arg_tys: Vec<Type> = args
                .iter()
                .map(|a| type_expr_to_type(a, param_set))
                .collect();

            match base_name.as_str() {
                "Own" if arg_tys.len() == 1 => Type::Own(Box::new(arg_tys.remove(0))),
                "RawPtr" if arg_tys.len() == 1 => Type::RawPtr(Box::new(arg_tys.remove(0))),
                "Shared" if arg_tys.len() == 1 => Type::Shared(Box::new(arg_tys.remove(0))),
                "FnOnce" if arg_tys.len() == 2 => {
                    let ret = arg_tys.pop().unwrap();
                    let args = arg_tys.pop().unwrap();
                    Type::Function {
                        params: match args {
                            Type::Tuple(params) => params,
                            param => vec![param],
                        },
                        ret: Box::new(ret),
                    }
                }
                "Fn" | "FnMut" if arg_tys.len() == 2 => {
                    let ret = arg_tys.pop().unwrap();
                    let args = arg_tys.pop().unwrap();
                    Type::BorrowedFunction {
                        kind: if base_name == "Fn" {
                            glyph_core::types::BorrowedCallableKind::Fn
                        } else {
                            glyph_core::types::BorrowedCallableKind::FnMut
                        },
                        params: match args {
                            Type::Tuple(params) => params,
                            param => vec![param],
                        },
                        ret: Box::new(ret),
                    }
                }
                _ => Type::App {
                    base: base_name,
                    args: arg_tys,
                },
            }
        }
        TypeExpr::Tuple { elements, .. } => {
            let elem_types: Vec<Type> = elements
                .iter()
                .map(|e| type_expr_to_type(e, param_set))
                .collect();
            Type::Tuple(elem_types)
        }
    }
}

fn type_key(ty: &Type) -> String {
    match ty {
        Type::I8 => "i8".into(),
        Type::I32 => "i32".into(),
        Type::I64 => "i64".into(),
        Type::U8 => "u8".into(),
        Type::U32 => "u32".into(),
        Type::U64 => "u64".into(),
        Type::Usize => "usize".into(),
        Type::F32 => "f32".into(),
        Type::F64 => "f64".into(),
        Type::Bool => "bool".into(),
        Type::Char => "char".into(),
        Type::Str => "str".into(),
        Type::String => "String".into(),
        Type::Void => "void".into(),
        Type::Named(n) => sanitize(n),
        Type::Enum(n) => format!("enum_{}", sanitize(n)),
        Type::Param(p) => format!("P_{}", sanitize(p)),
        Type::Ref(inner, mutability) => {
            let m = if matches!(mutability, Mutability::Mutable) {
                "mut"
            } else {
                "ref"
            };
            format!("{}_{}", m, type_key(inner))
        }
        Type::Array(inner, size) => format!("arr{}_{}", size, type_key(inner)),
        Type::Own(inner) => format!("own_{}", type_key(inner)),
        Type::RawPtr(inner) => format!("rawptr_{}", type_key(inner)),
        Type::Shared(inner) => format!("shared_{}", type_key(inner)),
        Type::Atomic(scalar) => format!("atomic_{}", scalar.type_name()),
        Type::Function { params, ret } => {
            let params: Vec<String> = params.iter().map(type_key).collect();
            format!(
                "fn_{}_to_{}",
                if params.is_empty() {
                    "unit".to_string()
                } else {
                    params.join("__")
                },
                type_key(ret)
            )
        }
        Type::BorrowedFunction { kind, params, ret } => {
            let capability = match kind {
                glyph_core::types::BorrowedCallableKind::Fn => "fn_ref",
                glyph_core::types::BorrowedCallableKind::FnMut => "fn_mut",
            };
            let params: Vec<String> = params.iter().map(type_key).collect();
            format!(
                "{}_{}_to_{}",
                capability,
                if params.is_empty() {
                    "unit".to_string()
                } else {
                    params.join("__")
                },
                type_key(ret)
            )
        }
        Type::App { base, args } => {
            let args: Vec<String> = args.iter().map(type_key).collect();
            format!("app_{}_{}", sanitize(base), args.join("__"))
        }
        Type::Tuple(elem_types) => {
            if elem_types.is_empty() {
                "unit".into()
            } else {
                let type_names: Vec<String> = elem_types.iter().map(type_key).collect();
                format!("__Tuple{}_{}", elem_types.len(), type_names.join("_"))
            }
        }
    }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect()
}

fn substitute(ty: &Type, subst: &HashMap<String, Type>) -> Type {
    match ty {
        Type::Param(p) => subst.get(p).cloned().unwrap_or(Type::Param(p.clone())),
        Type::Ref(inner, m) => Type::Ref(Box::new(substitute(inner, subst)), *m),
        Type::Array(inner, size) => Type::Array(Box::new(substitute(inner, subst)), *size),
        Type::Own(inner) => Type::Own(Box::new(substitute(inner, subst))),
        Type::RawPtr(inner) => Type::RawPtr(Box::new(substitute(inner, subst))),
        Type::Shared(inner) => Type::Shared(Box::new(substitute(inner, subst))),
        Type::Function { params, ret } => Type::Function {
            params: params
                .iter()
                .map(|param| substitute(param, subst))
                .collect(),
            ret: Box::new(substitute(ret, subst)),
        },
        Type::BorrowedFunction { kind, params, ret } => Type::BorrowedFunction {
            kind: *kind,
            params: params
                .iter()
                .map(|param| substitute(param, subst))
                .collect(),
            ret: Box::new(substitute(ret, subst)),
        },
        Type::Tuple(elements) => Type::Tuple(
            elements
                .iter()
                .map(|element| substitute(element, subst))
                .collect(),
        ),
        Type::App { base, args } => Type::App {
            base: base.clone(),
            args: args.iter().map(|a| substitute(a, subst)).collect(),
        },
        other => other.clone(),
    }
}

fn normalize_enum_named_types(ty: &Type, enum_names: &HashSet<String>) -> Type {
    match ty {
        Type::Named(n) if enum_names.contains(n) => Type::Enum(n.clone()),
        Type::Ref(inner, m) => {
            Type::Ref(Box::new(normalize_enum_named_types(inner, enum_names)), *m)
        }
        Type::Array(inner, size) => Type::Array(
            Box::new(normalize_enum_named_types(inner, enum_names)),
            *size,
        ),
        Type::Own(inner) => Type::Own(Box::new(normalize_enum_named_types(inner, enum_names))),
        Type::RawPtr(inner) => {
            Type::RawPtr(Box::new(normalize_enum_named_types(inner, enum_names)))
        }
        Type::Shared(inner) => {
            Type::Shared(Box::new(normalize_enum_named_types(inner, enum_names)))
        }
        Type::Function { params, ret } => Type::Function {
            params: params
                .iter()
                .map(|param| normalize_enum_named_types(param, enum_names))
                .collect(),
            ret: Box::new(normalize_enum_named_types(ret, enum_names)),
        },
        Type::BorrowedFunction { kind, params, ret } => Type::BorrowedFunction {
            kind: *kind,
            params: params
                .iter()
                .map(|param| normalize_enum_named_types(param, enum_names))
                .collect(),
            ret: Box::new(normalize_enum_named_types(ret, enum_names)),
        },
        Type::App { base, args } => Type::App {
            base: base.clone(),
            args: args
                .iter()
                .map(|a| normalize_enum_named_types(a, enum_names))
                .collect(),
        },
        Type::Tuple(elems) => Type::Tuple(
            elems
                .iter()
                .map(|e| normalize_enum_named_types(e, enum_names))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn rewrite_type(
    ty: &Type,
    templates: &HashMap<String, Template>,
    instantiations: &mut HashMap<(String, Vec<Type>), String>,
    worklist: &mut VecDeque<Type>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Type {
    if let Some(result) = glyph_core::thread::private_scoped_thread_handle_result(ty) {
        return glyph_core::thread::private_scoped_thread_handle_type(rewrite_type(
            result,
            templates,
            instantiations,
            worklist,
            diagnostics,
        ));
    }
    if let Some(result) = glyph_core::thread::canonical_scoped_thread_handle_result(ty) {
        return glyph_core::thread::canonical_scoped_thread_handle_type(rewrite_type(
            result,
            templates,
            instantiations,
            worklist,
            diagnostics,
        ));
    }
    if let Some(inner) = ty.arc_inner_type() {
        return Type::arc(rewrite_type(
            inner,
            templates,
            instantiations,
            worklist,
            diagnostics,
        ));
    }
    if let Some(inner) = ty.mutex_inner_type() {
        return Type::mutex(rewrite_type(
            inner,
            templates,
            instantiations,
            worklist,
            diagnostics,
        ));
    }
    if let Some(inner) = ty.mutex_guard_inner_type() {
        return Type::mutex_guard(rewrite_type(
            inner,
            templates,
            instantiations,
            worklist,
            diagnostics,
        ));
    }
    if let Some(inner) = ty.spsc_sender_inner_type() {
        return Type::spsc_sender(rewrite_type(
            inner,
            templates,
            instantiations,
            worklist,
            diagnostics,
        ));
    }
    if let Some(inner) = ty.spsc_receiver_inner_type() {
        return Type::spsc_receiver(rewrite_type(
            inner,
            templates,
            instantiations,
            worklist,
            diagnostics,
        ));
    }
    match ty {
        Type::App { base, args } => {
            let args_rewritten: Vec<Type> = args
                .iter()
                .map(|a| rewrite_type(a, templates, instantiations, worklist, diagnostics))
                .collect();

            let canonical = Type::App {
                base: base.clone(),
                args: args_rewritten.clone(),
            };
            if glyph_core::thread::is_canonical_thread_handle(&canonical) {
                return canonical;
            }

            let Some(template) = templates.get(base) else {
                diagnostics.push(Diagnostic::error(
                    format!("unknown generic template '{}'", base),
                    None,
                ));
                return Type::Named(format!(
                    "{}<{}>",
                    base,
                    args_rewritten
                        .iter()
                        .map(type_key)
                        .collect::<Vec<_>>()
                        .join(",")
                ));
            };

            let key = (base.clone(), args_rewritten.clone());
            if let Some(existing) = instantiations.get(&key) {
                return match template {
                    Template::Enum { .. } => Type::Enum(existing.clone()),
                    Template::Struct { .. } => Type::Named(existing.clone()),
                };
            }

            let inst_name = format!(
                "{}${}",
                sanitize(base),
                args_rewritten
                    .iter()
                    .map(type_key)
                    .collect::<Vec<_>>()
                    .join("__")
            );
            instantiations.insert(key, inst_name.clone());
            worklist.push_back(Type::App {
                base: base.clone(),
                args: args_rewritten,
            });

            match template {
                Template::Enum { .. } => Type::Enum(inst_name),
                Template::Struct { .. } => Type::Named(inst_name),
            }
        }
        Type::Ref(inner, m) => Type::Ref(
            Box::new(rewrite_type(
                inner,
                templates,
                instantiations,
                worklist,
                diagnostics,
            )),
            *m,
        ),
        Type::Array(inner, size) => Type::Array(
            Box::new(rewrite_type(
                inner,
                templates,
                instantiations,
                worklist,
                diagnostics,
            )),
            *size,
        ),
        Type::Own(inner) => Type::Own(Box::new(rewrite_type(
            inner,
            templates,
            instantiations,
            worklist,
            diagnostics,
        ))),
        Type::RawPtr(inner) => Type::RawPtr(Box::new(rewrite_type(
            inner,
            templates,
            instantiations,
            worklist,
            diagnostics,
        ))),
        Type::Shared(inner) => Type::Shared(Box::new(rewrite_type(
            inner,
            templates,
            instantiations,
            worklist,
            diagnostics,
        ))),
        Type::Function { params, ret } => Type::Function {
            params: params
                .iter()
                .map(|param| rewrite_type(param, templates, instantiations, worklist, diagnostics))
                .collect(),
            ret: Box::new(rewrite_type(
                ret,
                templates,
                instantiations,
                worklist,
                diagnostics,
            )),
        },
        Type::BorrowedFunction { kind, params, ret } => Type::BorrowedFunction {
            kind: *kind,
            params: params
                .iter()
                .map(|param| rewrite_type(param, templates, instantiations, worklist, diagnostics))
                .collect(),
            ret: Box::new(rewrite_type(
                ret,
                templates,
                instantiations,
                worklist,
                diagnostics,
            )),
        },
        Type::Param(p) => {
            diagnostics.push(Diagnostic::error(
                format!("unresolved generic parameter '{}'", p),
                None,
            ));
            Type::Param(p.clone())
        }
        Type::Tuple(elem_types) => {
            let rewritten_elems: Vec<Type> = elem_types
                .iter()
                .map(|t| rewrite_type(t, templates, instantiations, worklist, diagnostics))
                .collect();
            Type::Tuple(rewritten_elems)
        }
        other => other.clone(),
    }
}

fn rewrite_type_with_instantiations(
    ty: &Type,
    templates: &HashMap<String, Template>,
    instantiations: &HashMap<(String, Vec<Type>), String>,
) -> Type {
    if let Some(result) = glyph_core::thread::private_scoped_thread_handle_result(ty) {
        return glyph_core::thread::private_scoped_thread_handle_type(
            rewrite_type_with_instantiations(result, templates, instantiations),
        );
    }
    if let Some(result) = glyph_core::thread::canonical_scoped_thread_handle_result(ty) {
        return glyph_core::thread::canonical_scoped_thread_handle_type(
            rewrite_type_with_instantiations(result, templates, instantiations),
        );
    }
    if let Some(inner) = ty.arc_inner_type() {
        return Type::arc(rewrite_type_with_instantiations(
            inner,
            templates,
            instantiations,
        ));
    }
    if let Some(inner) = ty.mutex_inner_type() {
        return Type::mutex(rewrite_type_with_instantiations(
            inner,
            templates,
            instantiations,
        ));
    }
    if let Some(inner) = ty.mutex_guard_inner_type() {
        return Type::mutex_guard(rewrite_type_with_instantiations(
            inner,
            templates,
            instantiations,
        ));
    }
    match ty {
        Type::App { base, args } => {
            let args_rewritten: Vec<Type> = args
                .iter()
                .map(|a| rewrite_type_with_instantiations(a, templates, instantiations))
                .collect();
            let key = (base.clone(), args_rewritten.clone());
            if let Some(inst_name) = instantiations.get(&key) {
                if let Some(template) = templates.get(base) {
                    return match template {
                        Template::Enum { .. } => Type::Enum(inst_name.clone()),
                        Template::Struct { .. } => Type::Named(inst_name.clone()),
                    };
                }
            }
            Type::App {
                base: base.clone(),
                args: args_rewritten,
            }
        }
        Type::Ref(inner, m) => Type::Ref(
            Box::new(rewrite_type_with_instantiations(
                inner,
                templates,
                instantiations,
            )),
            *m,
        ),
        Type::Array(inner, size) => Type::Array(
            Box::new(rewrite_type_with_instantiations(
                inner,
                templates,
                instantiations,
            )),
            *size,
        ),
        Type::Own(inner) => Type::Own(Box::new(rewrite_type_with_instantiations(
            inner,
            templates,
            instantiations,
        ))),
        Type::RawPtr(inner) => Type::RawPtr(Box::new(rewrite_type_with_instantiations(
            inner,
            templates,
            instantiations,
        ))),
        Type::Shared(inner) => Type::Shared(Box::new(rewrite_type_with_instantiations(
            inner,
            templates,
            instantiations,
        ))),
        Type::Function { params, ret } => Type::Function {
            params: params
                .iter()
                .map(|param| rewrite_type_with_instantiations(param, templates, instantiations))
                .collect(),
            ret: Box::new(rewrite_type_with_instantiations(
                ret,
                templates,
                instantiations,
            )),
        },
        Type::BorrowedFunction { kind, params, ret } => Type::BorrowedFunction {
            kind: *kind,
            params: params
                .iter()
                .map(|param| rewrite_type_with_instantiations(param, templates, instantiations))
                .collect(),
            ret: Box::new(rewrite_type_with_instantiations(
                ret,
                templates,
                instantiations,
            )),
        },
        Type::Tuple(elements) => Type::Tuple(
            elements
                .iter()
                .map(|element| rewrite_type_with_instantiations(element, templates, instantiations))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn rewrite_rvalue(
    rvalue: &mut Rvalue,
    assigned_ty: Option<&Type>,
    templates: &HashMap<String, Template>,
    instantiations: &mut HashMap<(String, Vec<Type>), String>,
    worklist: &mut VecDeque<Type>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match rvalue {
        Rvalue::ArrayLit { elem_type, .. } => {
            *elem_type = rewrite_type(elem_type, templates, instantiations, worklist, diagnostics);
        }
        Rvalue::VecNew { elem_type }
        | Rvalue::VecWithCapacity { elem_type, .. }
        | Rvalue::VecPush { elem_type, .. }
        | Rvalue::VecPop { elem_type, .. }
        | Rvalue::VecIndex { elem_type, .. }
        | Rvalue::VecIndexRef { elem_type, .. } => {
            *elem_type = rewrite_type(elem_type, templates, instantiations, worklist, diagnostics);
        }
        Rvalue::MapNew {
            key_type,
            value_type,
        }
        | Rvalue::MapWithCapacity {
            key_type,
            value_type,
            ..
        }
        | Rvalue::MapAdd {
            key_type,
            value_type,
            ..
        }
        | Rvalue::MapUpdate {
            key_type,
            value_type,
            ..
        }
        | Rvalue::MapDel {
            key_type,
            value_type,
            ..
        }
        | Rvalue::MapGet {
            key_type,
            value_type,
            ..
        }
        | Rvalue::MapKeys {
            key_type,
            value_type,
            ..
        }
        | Rvalue::MapVals {
            key_type,
            value_type,
            ..
        } => {
            *key_type = rewrite_type(key_type, templates, instantiations, worklist, diagnostics);
            *value_type =
                rewrite_type(value_type, templates, instantiations, worklist, diagnostics);
        }
        Rvalue::MapHas { key_type, .. } => {
            *key_type = rewrite_type(key_type, templates, instantiations, worklist, diagnostics);
        }
        Rvalue::OwnNew { elem_type, .. }
        | Rvalue::OwnIntoRaw { elem_type, .. }
        | Rvalue::OwnFromRaw { elem_type, .. }
        | Rvalue::RawPtrNull { elem_type }
        | Rvalue::SharedNew { elem_type, .. }
        | Rvalue::SharedClone { elem_type, .. }
        | Rvalue::ArcNew { elem_type, .. }
        | Rvalue::ArcClone { elem_type, .. }
        | Rvalue::ArcBorrow { elem_type, .. }
        | Rvalue::MutexNew { elem_type, .. }
        | Rvalue::MutexLock { elem_type, .. }
        | Rvalue::MutexTryLock { elem_type, .. }
        | Rvalue::MutexGuardIsAcquired { elem_type, .. }
        | Rvalue::MutexGuardBorrow { elem_type, .. }
        | Rvalue::SpscChannelNew { elem_type, .. }
        | Rvalue::SpscTrySend { elem_type, .. }
        | Rvalue::SpscTryRecv { elem_type, .. } => {
            *elem_type = rewrite_type(elem_type, templates, instantiations, worklist, diagnostics);
        }
        Rvalue::Deref { ty, .. } => {
            *ty = rewrite_type(ty, templates, instantiations, worklist, diagnostics);
        }
        Rvalue::EnumPayload { payload_type, .. } => {
            *payload_type = rewrite_type(
                payload_type,
                templates,
                instantiations,
                worklist,
                diagnostics,
            );
        }
        Rvalue::EnumConstruct { enum_name, .. } => {
            if let Some(ty) = assigned_ty {
                if let Type::Enum(name) | Type::Named(name) = ty {
                    *enum_name = name.clone();
                }
            }
        }
        Rvalue::StructLit { struct_name, .. } => {
            if let Some(Type::Named(name)) = assigned_ty {
                *struct_name = name.clone();
            }
        }
        Rvalue::FunctionRef { signature, .. }
        | Rvalue::CallIndirect { signature, .. }
        | Rvalue::CallIndirectShared { signature, .. }
        | Rvalue::CallIndirectMut { signature, .. } => {
            *signature = rewrite_type(signature, templates, instantiations, worklist, diagnostics);
        }
        Rvalue::MakeClosure {
            signature,
            captures,
            ..
        } => {
            *signature = rewrite_type(signature, templates, instantiations, worklist, diagnostics);
            for capture in captures {
                capture.ty = rewrite_type(
                    &capture.ty,
                    templates,
                    instantiations,
                    worklist,
                    diagnostics,
                );
            }
        }
        Rvalue::MakeBorrowedClosure {
            signature,
            captures,
            ..
        } => {
            *signature = rewrite_type(signature, templates, instantiations, worklist, diagnostics);
            for capture in captures {
                capture.ty = rewrite_type(
                    &capture.ty,
                    templates,
                    instantiations,
                    worklist,
                    diagnostics,
                );
            }
        }
        Rvalue::ThreadErrorFromStatus { .. }
        | Rvalue::ThreadHandleFromRaw { .. }
        | Rvalue::ThreadHandleIntoRaw { .. } => {}
        _ => {}
    }
}

fn field_assignment_type(
    base: LocalId,
    field_index: u32,
    locals: &[glyph_core::mir::Local],
    struct_types: &HashMap<String, StructType>,
    templates: &HashMap<String, Template>,
    instantiations: &HashMap<(String, Vec<Type>), String>,
) -> Option<Type> {
    let mut ty = locals.get(base.0 as usize).and_then(|l| l.ty.clone())?;

    loop {
        match ty {
            Type::Ref(inner, _) | Type::Own(inner) | Type::RawPtr(inner) | Type::Shared(inner) => {
                ty = *inner;
            }
            Type::Tuple(elem_types) => {
                return elem_types.get(field_index as usize).cloned();
            }
            Type::Named(name) => {
                if let Some(struct_type) = struct_types.get(&name) {
                    return struct_type
                        .fields
                        .get(field_index as usize)
                        .map(|(_, ty)| ty.clone());
                }

                let (base_name, args) = instantiations.iter().find_map(|(key, inst_name)| {
                    if inst_name == &name {
                        Some((key.0.clone(), key.1.clone()))
                    } else {
                        None
                    }
                })?;
                let Template::Struct { params, fields } = templates.get(&base_name)? else {
                    return None;
                };
                let (_, field_ty) = fields.get(field_index as usize)?;
                let subst: HashMap<String, Type> =
                    params.iter().cloned().zip(args.iter().cloned()).collect();
                let substituted = substitute(field_ty, &subst);
                return Some(rewrite_type_with_instantiations(
                    &substituted,
                    templates,
                    instantiations,
                ));
            }
            _ => return None,
        }
    }
}

fn instantiate_all(
    mir: &mut MirModule,
    templates: &HashMap<String, Template>,
    instantiations: &HashMap<(String, Vec<Type>), String>,
) {
    // Reify each Type::App key into a concrete StructType/EnumType.
    for ((base, args), inst_name) in instantiations {
        let Some(template) = templates.get(base) else {
            continue;
        };

        match template {
            Template::Struct { params, fields } => {
                let subst: HashMap<String, Type> =
                    params.iter().cloned().zip(args.iter().cloned()).collect();
                let inst_fields = fields
                    .iter()
                    .map(|(n, t)| {
                        let substituted = substitute(t, &subst);
                        let rewritten = rewrite_type_with_instantiations(
                            &substituted,
                            templates,
                            instantiations,
                        );
                        (n.clone(), rewritten)
                    })
                    .collect();
                mir.struct_types.insert(
                    inst_name.clone(),
                    StructType {
                        name: inst_name.clone(),
                        fields: inst_fields,
                    },
                );
            }
            Template::Enum { params, variants } => {
                let subst: HashMap<String, Type> =
                    params.iter().cloned().zip(args.iter().cloned()).collect();

                let inst_variants = variants
                    .iter()
                    .map(|v| EnumVariant {
                        name: v.name.clone(),
                        payload: v.payload.as_ref().map(|p| substitute(p, &subst)).map(|p| {
                            rewrite_type_with_instantiations(&p, templates, instantiations)
                        }),
                    })
                    .collect();

                mir.enum_types.insert(
                    inst_name.clone(),
                    EnumType {
                        name: inst_name.clone(),
                        variants: inst_variants,
                    },
                );
            }
        }
    }
}

pub fn monomorphize_mir(mir: &mut MirModule, modules: &HashMap<String, Module>) -> Vec<Diagnostic> {
    let templates = collect_templates(modules);

    // Normalize enum references: some lowering paths may record an enum type as `Named`,
    // which can lead to duplicate monomorphizations (e.g. ParseResult$JsonValue vs
    // ParseResult$enum_JsonValue) and ABI mismatches.
    let enum_names: HashSet<String> = mir.enum_types.keys().cloned().collect();

    let mut diagnostics = Vec::new();
    let mut instantiations: HashMap<(String, Vec<Type>), String> = HashMap::new();
    let mut worklist = VecDeque::new();

    // Rewrite all existing types and seed instantiation worklist.
    for (name, st) in mir.struct_types.iter_mut() {
        if templates.contains_key(name) {
            continue;
        }
        for (_n, ty) in &mut st.fields {
            *ty = normalize_enum_named_types(ty, &enum_names);
            *ty = rewrite_type(
                ty,
                &templates,
                &mut instantiations,
                &mut worklist,
                &mut diagnostics,
            );
        }
    }

    for (name, et) in mir.enum_types.iter_mut() {
        if templates.contains_key(name) {
            continue;
        }
        for v in &mut et.variants {
            if let Some(payload) = &mut v.payload {
                *payload = normalize_enum_named_types(payload, &enum_names);
                *payload = rewrite_type(
                    payload,
                    &templates,
                    &mut instantiations,
                    &mut worklist,
                    &mut diagnostics,
                );
            }
        }
    }

    let struct_types_snapshot = mir.struct_types.clone();

    for func in &mut mir.functions {
        if let Some(ret) = &mut func.ret_type {
            *ret = normalize_enum_named_types(ret, &enum_names);
            *ret = rewrite_type(
                ret,
                &templates,
                &mut instantiations,
                &mut worklist,
                &mut diagnostics,
            );
        }
        for local in &mut func.locals {
            if let Some(ty) = &mut local.ty {
                *ty = normalize_enum_named_types(ty, &enum_names);
                *ty = rewrite_type(
                    ty,
                    &templates,
                    &mut instantiations,
                    &mut worklist,
                    &mut diagnostics,
                );
            }
        }

        let locals_snapshot = func.locals.clone();

        for block in &mut func.blocks {
            for inst in &mut block.insts {
                match inst {
                    MirInst::Assign { local, value } => {
                        let assigned_ty = func
                            .locals
                            .get(local.0 as usize)
                            .and_then(|l| l.ty.as_ref());
                        rewrite_rvalue(
                            value,
                            assigned_ty,
                            &templates,
                            &mut instantiations,
                            &mut worklist,
                            &mut diagnostics,
                        );
                    }
                    MirInst::AssignField {
                        base,
                        field_index,
                        value,
                        ..
                    } => {
                        let assigned_ty = field_assignment_type(
                            *base,
                            *field_index,
                            &locals_snapshot,
                            &struct_types_snapshot,
                            &templates,
                            &instantiations,
                        );
                        rewrite_rvalue(
                            value,
                            assigned_ty.as_ref(),
                            &templates,
                            &mut instantiations,
                            &mut worklist,
                            &mut diagnostics,
                        );
                    }
                    _ => {}
                }
            }
        }
    }

    for ex in &mut mir.extern_functions {
        if let Some(ret) = &mut ex.ret_type {
            *ret = normalize_enum_named_types(ret, &enum_names);
            *ret = rewrite_type(
                ret,
                &templates,
                &mut instantiations,
                &mut worklist,
                &mut diagnostics,
            );
        }
        for p in &mut ex.params {
            *p = normalize_enum_named_types(p, &enum_names);
            *p = rewrite_type(
                p,
                &templates,
                &mut instantiations,
                &mut worklist,
                &mut diagnostics,
            );
        }
    }

    // Generate struct types for all tuples
    let mut tuple_types_to_generate = std::collections::HashSet::new();
    for func in &mir.functions {
        // Check return type
        if let Some(Type::Tuple(elem_types)) = &func.ret_type {
            if !elem_types.is_empty() {
                tuple_types_to_generate.insert(elem_types.clone());
            }
        }
        // Check local types
        for local in &func.locals {
            if let Some(Type::Tuple(elem_types)) = &local.ty {
                if !elem_types.is_empty() {
                    tuple_types_to_generate.insert(elem_types.clone());
                }
            }
        }
    }

    // Helper function to generate tuple struct names consistently
    let tuple_struct_name_mono = |elem_types: &[Type]| -> String {
        if elem_types.is_empty() {
            "unit".to_string()
        } else {
            let type_names: Vec<String> = elem_types.iter().map(type_key).collect();
            format!("__Tuple{}_{}", elem_types.len(), type_names.join("_"))
        }
    };

    for elem_types in tuple_types_to_generate {
        let struct_name = tuple_struct_name_mono(&elem_types);
        let fields: Vec<(String, Type)> = elem_types
            .into_iter()
            .enumerate()
            .map(|(idx, ty)| (idx.to_string(), ty))
            .collect();

        mir.struct_types.insert(
            struct_name.clone(),
            StructType {
                name: struct_name,
                fields,
            },
        );
    }

    // Instantiate until queue drains. (Queue items were pushed as Type::App.)
    while let Some(Type::App { base, args }) = worklist.pop_front() {
        let Some(template) = templates.get(&base) else {
            continue;
        };

        let inst_name = instantiations
            .get(&(base.clone(), args.clone()))
            .cloned()
            .unwrap_or_else(|| {
                format!(
                    "{}${}",
                    sanitize(&base),
                    args.iter().map(type_key).collect::<Vec<_>>().join("__")
                )
            });

        // Materialize the instantiation.
        match template {
            Template::Struct { params, fields } => {
                let subst: HashMap<String, Type> =
                    params.iter().cloned().zip(args.iter().cloned()).collect();
                let mut inst_fields = Vec::new();
                for (n, t) in fields {
                    let substituted = substitute(t, &subst);
                    let rewritten = rewrite_type(
                        &substituted,
                        &templates,
                        &mut instantiations,
                        &mut worklist,
                        &mut diagnostics,
                    );
                    inst_fields.push((n.clone(), rewritten));
                }

                mir.struct_types.insert(
                    inst_name.clone(),
                    StructType {
                        name: inst_name.clone(),
                        fields: inst_fields,
                    },
                );
            }
            Template::Enum { params, variants } => {
                let subst: HashMap<String, Type> =
                    params.iter().cloned().zip(args.iter().cloned()).collect();

                let mut inst_variants = Vec::new();
                for v in variants {
                    let payload = v.payload.as_ref().map(|p| substitute(p, &subst));
                    let payload = payload.map(|p| {
                        rewrite_type(
                            &p,
                            &templates,
                            &mut instantiations,
                            &mut worklist,
                            &mut diagnostics,
                        )
                    });
                    inst_variants.push(EnumVariant {
                        name: v.name.clone(),
                        payload,
                    });
                }

                mir.enum_types.insert(
                    inst_name.clone(),
                    EnumType {
                        name: inst_name.clone(),
                        variants: inst_variants,
                    },
                );
            }
        }
    }

    // Also ensure any pre-collected instantiations exist.
    instantiate_all(mir, &templates, &instantiations);

    // Drop generic templates from the concrete MIR; keep only instantiated forms.
    for key in templates.keys() {
        mir.struct_types.remove(key);
        mir.enum_types.remove(key);
    }

    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_generic_types_inside_callable_signatures() {
        let signature = Type::Function {
            params: vec![Type::Param("T".into())],
            ret: Box::new(Type::Tuple(vec![Type::Param("T".into()), Type::Bool])),
        };
        let subst = HashMap::from([("T".to_string(), Type::I64)]);

        assert_eq!(
            substitute(&signature, &subst),
            Type::Function {
                params: vec![Type::I64],
                ret: Box::new(Type::Tuple(vec![Type::I64, Type::Bool])),
            }
        );
    }

    #[test]
    fn monomorphizes_simple_struct_template() {
        let span = glyph_core::span::Span::new(0, 0);
        let modules = HashMap::from([(
            "main".to_string(),
            Module {
                imports: vec![],
                items: vec![Item::Struct(glyph_core::ast::StructDef {
                    name: glyph_core::ast::Ident("Box".into()),
                    generic_params: vec![glyph_core::ast::Ident("T".into())],
                    fields: vec![glyph_core::ast::FieldDef {
                        name: glyph_core::ast::Ident("value".into()),
                        ty: TypeExpr::Path {
                            segments: vec!["T".into()],
                            span,
                        },
                        span,
                    }],
                    interfaces: vec![],
                    methods: vec![],
                    inline_impls: vec![],
                    span,
                })],
            },
        )]);

        let mut mir = MirModule::default();
        mir.functions.push(glyph_core::mir::MirFunction {
            name: "main".into(),
            ret_type: None,
            params: vec![],
            locals: vec![glyph_core::mir::Local {
                name: Some("x".into()),
                ty: Some(Type::App {
                    base: "Box".into(),
                    args: vec![Type::I32],
                }),
                mutable: false,
                skip_drop: false,
            }],
            blocks: vec![glyph_core::mir::MirBlock { insts: vec![] }],
        });

        let diags = monomorphize_mir(&mut mir, &modules);
        assert!(diags.is_empty(), "diags: {:?}", diags);

        let local_ty = mir.functions[0].locals[0].ty.clone().unwrap();
        match local_ty {
            Type::Named(name) => {
                assert!(name.starts_with("Box$"));
                assert!(mir.struct_types.contains_key(&name));
                let st = mir.struct_types.get(&name).unwrap();
                assert_eq!(st.fields[0].1, Type::I32);
            }
            _ => panic!("expected monomorphized named type"),
        }
    }

    #[test]
    fn monomorphizes_option_enum() {
        let span = glyph_core::span::Span::new(0, 0);
        let modules = HashMap::from([(
            "main".to_string(),
            Module {
                imports: vec![],
                items: vec![Item::Enum(glyph_core::ast::EnumDef {
                    name: glyph_core::ast::Ident("Option".into()),
                    generic_params: vec![glyph_core::ast::Ident("T".into())],
                    variants: vec![
                        glyph_core::ast::EnumVariantDef {
                            name: glyph_core::ast::Ident("None".into()),
                            payload: None,
                            span,
                        },
                        glyph_core::ast::EnumVariantDef {
                            name: glyph_core::ast::Ident("Some".into()),
                            payload: Some(TypeExpr::Path {
                                segments: vec!["T".into()],
                                span,
                            }),
                            span,
                        },
                    ],
                    span,
                })],
            },
        )]);

        let mut mir = MirModule::default();
        mir.functions.push(glyph_core::mir::MirFunction {
            name: "main".into(),
            ret_type: Some(Type::App {
                base: "Option".into(),
                args: vec![Type::I32],
            }),
            params: vec![],
            locals: vec![glyph_core::mir::Local {
                name: Some("v".into()),
                ty: Some(Type::App {
                    base: "Option".into(),
                    args: vec![Type::I32],
                }),
                mutable: false,
                skip_drop: false,
            }],
            blocks: vec![glyph_core::mir::MirBlock { insts: vec![] }],
        });

        let diags = monomorphize_mir(&mut mir, &modules);
        assert!(diags.is_empty(), "diags: {:?}", diags);

        // Ensure the enum instantiation exists and payload is i32.
        let inst_name = match mir.functions[0].locals[0].ty.as_ref().unwrap() {
            Type::Enum(name) => name.clone(),
            Type::Named(name) => name.clone(),
            other => panic!("unexpected local type: {:?}", other),
        };
        let en = mir.enum_types.get(&inst_name).expect("instantiated enum");
        assert_eq!(en.variants.len(), 2);
        assert_eq!(en.variants[1].name, "Some");
        assert_eq!(en.variants[1].payload, Some(Type::I32));
    }

    #[test]
    fn monomorphizes_two_param_enum() {
        let span = glyph_core::span::Span::new(0, 0);
        let modules = HashMap::from([(
            "main".to_string(),
            Module {
                imports: vec![],
                items: vec![Item::Enum(glyph_core::ast::EnumDef {
                    name: glyph_core::ast::Ident("Error".into()),
                    generic_params: vec![
                        glyph_core::ast::Ident("T".into()),
                        glyph_core::ast::Ident("E".into()),
                    ],
                    variants: vec![
                        glyph_core::ast::EnumVariantDef {
                            name: glyph_core::ast::Ident("Message".into()),
                            payload: Some(TypeExpr::Path {
                                segments: vec!["T".into()],
                                span,
                            }),
                            span,
                        },
                        glyph_core::ast::EnumVariantDef {
                            name: glyph_core::ast::Ident("Code".into()),
                            payload: Some(TypeExpr::Path {
                                segments: vec!["E".into()],
                                span,
                            }),
                            span,
                        },
                    ],
                    span,
                })],
            },
        )]);

        let mut mir = MirModule::default();
        mir.functions.push(glyph_core::mir::MirFunction {
            name: "main".into(),
            ret_type: Some(Type::App {
                base: "Error".into(),
                args: vec![Type::I32, Type::String],
            }),
            params: vec![],
            locals: vec![glyph_core::mir::Local {
                name: Some("e".into()),
                ty: Some(Type::App {
                    base: "Error".into(),
                    args: vec![Type::I32, Type::String],
                }),
                mutable: false,
                skip_drop: false,
            }],
            blocks: vec![glyph_core::mir::MirBlock { insts: vec![] }],
        });

        let diags = monomorphize_mir(&mut mir, &modules);
        assert!(diags.is_empty(), "diags: {:?}", diags);

        let inst_name = match mir.functions[0].locals[0].ty.as_ref().unwrap() {
            Type::Enum(name) => name.clone(),
            Type::Named(name) => name.clone(),
            other => panic!("unexpected local type: {:?}", other),
        };
        let en = mir.enum_types.get(&inst_name).expect("instantiated enum");
        assert_eq!(en.variants.len(), 2);
        assert_eq!(en.variants[0].payload, Some(Type::I32));
        assert_eq!(en.variants[1].payload, Some(Type::String));
    }

    #[test]
    fn monomorphize_reports_unresolved_param() {
        let mut mir = MirModule::default();
        mir.functions.push(glyph_core::mir::MirFunction {
            name: "main".into(),
            ret_type: None,
            params: vec![],
            locals: vec![glyph_core::mir::Local {
                name: Some("x".into()),
                ty: Some(Type::Param("T".into())),
                mutable: false,
                skip_drop: false,
            }],
            blocks: vec![glyph_core::mir::MirBlock { insts: vec![] }],
        });

        let diags = monomorphize_mir(&mut mir, &HashMap::new());
        assert!(
            diags
                .iter()
                .any(|d| d.message.contains("unresolved generic parameter")),
            "expected unresolved param diagnostic, got: {:?}",
            diags
        );
    }
}
