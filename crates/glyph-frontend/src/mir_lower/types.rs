use glyph_core::ast::TypeExpr;
use glyph_core::types::{Mutability, Type};

use crate::resolver::ResolverContext;

pub fn type_expr_to_string(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Path { segments, .. } => segments.join("::"),
        TypeExpr::App { base, args, .. } => {
            let mut s = type_expr_to_string(base);
            let rendered_args: Vec<String> = args.iter().map(type_expr_to_string).collect();
            s.push('<');
            s.push_str(&rendered_args.join(", "));
            s.push('>');
            s
        }
        TypeExpr::Ref {
            mutability, inner, ..
        } => {
            let mut s = String::from("&");
            if matches!(mutability, Mutability::Mutable) {
                s.push_str("mut ");
            }
            s.push_str(&type_expr_to_string(inner));
            s
        }
        TypeExpr::Array { elem, size, .. } => format!("[{}; {}]", type_expr_to_string(elem), size),
        TypeExpr::Tuple { elements, .. } => {
            let elem_strs: Vec<String> = elements.iter().map(type_expr_to_string).collect();
            if elem_strs.len() == 1 {
                format!("({},)", elem_strs[0])
            } else {
                format!("({})", elem_strs.join(", "))
            }
        }
    }
}

pub(crate) fn tuple_struct_name(elem_types: &[Type]) -> String {
    if elem_types.is_empty() {
        return "unit".to_string();
    }

    let type_names: Vec<String> = elem_types.iter().map(type_key_simple).collect();
    format!("__Tuple{}_{}", elem_types.len(), type_names.join("_"))
}

pub(crate) fn type_key_simple(ty: &Type) -> String {
    match ty {
        Type::I8 => "i8".into(),
        Type::I16 => "i16".into(),
        Type::I32 => "i32".into(),
        Type::I64 => "i64".into(),
        Type::U8 => "u8".into(),
        Type::U16 => "u16".into(),
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
        Type::Named(n) => n.replace("::", "_"),
        Type::Enum(n) => format!("enum_{}", n.replace("::", "_")),
        Type::Param(p) => format!("P_{}", p),
        Type::Ref(inner, _) => format!("ref_{}", type_key_simple(inner)),
        Type::Array(inner, size) => format!("arr{}_{}", size, type_key_simple(inner)),
        Type::Own(inner) => format!("own_{}", type_key_simple(inner)),
        Type::RawPtr(inner) => format!("rawptr_{}", type_key_simple(inner)),
        Type::Shared(inner) => format!("shared_{}", type_key_simple(inner)),
        Type::Atomic(scalar) => format!("atomic_{}", scalar.type_name()),
        Type::Function { params, ret } => {
            let params: Vec<String> = params.iter().map(type_key_simple).collect();
            format!(
                "fn_{}_to_{}",
                if params.is_empty() {
                    "unit".to_string()
                } else {
                    params.join("__")
                },
                type_key_simple(ret)
            )
        }
        Type::BorrowedFunction { kind, params, ret } => {
            let capability = match kind {
                glyph_core::types::BorrowedCallableKind::Fn => "fn_ref",
                glyph_core::types::BorrowedCallableKind::FnMut => "fn_mut",
            };
            let params: Vec<String> = params.iter().map(type_key_simple).collect();
            format!(
                "{}_{}_to_{}",
                capability,
                if params.is_empty() {
                    "unit".to_string()
                } else {
                    params.join("__")
                },
                type_key_simple(ret)
            )
        }
        Type::App { base, args } => {
            let args: Vec<String> = args.iter().map(type_key_simple).collect();
            format!("app_{}_{}", base.replace("::", "_"), args.join("__"))
        }
        Type::Tuple(elem_types) => {
            if elem_types.is_empty() {
                "unit".into()
            } else {
                let type_names: Vec<String> = elem_types.iter().map(type_key_simple).collect();
                format!("__Tuple{}_{}", elem_types.len(), type_names.join("_"))
            }
        }
    }
}

pub(crate) fn vec_elem_type_from_type(ty: &Type) -> Option<Type> {
    match ty {
        Type::App { base, args }
            if (base == "Vec" || base.ends_with("::Vec")) && args.len() == 1 =>
        {
            args.get(0).cloned()
        }
        Type::Ref(inner, _) | Type::Own(inner) | Type::RawPtr(inner) | Type::Shared(inner) => {
            vec_elem_type_from_type(inner)
        }
        _ => None,
    }
}

pub(crate) fn resolve_type_name(name: &str, resolver: &ResolverContext) -> Option<Type> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed == "&str" {
        return Some(Type::Str);
    }

    if let Some(primitive) = Type::from_name(trimmed) {
        return Some(primitive);
    }

    if let Some(tuple) = parse_tuple_type(trimmed, resolver) {
        return Some(tuple);
    }

    if let Some(array) = parse_array_type(trimmed, resolver) {
        return Some(array);
    }

    if let Some(own_ty) = parse_own_type(trimmed, resolver) {
        return Some(own_ty);
    }

    if let Some(raw_ptr_ty) = parse_raw_ptr_type(trimmed, resolver) {
        return Some(raw_ptr_ty);
    }

    if let Some(shared_ty) = parse_shared_type(trimmed, resolver) {
        return Some(shared_ty);
    }

    if let Some(app_ty) = parse_type_application(trimmed, resolver) {
        return Some(app_ty);
    }

    if resolver.get_enum(trimmed).is_some() {
        return Some(Type::Enum(trimmed.to_string()));
    }

    if resolver.get_struct(trimmed).is_some() {
        return Some(Type::Named(trimmed.to_string()));
    }

    if let Some(reference) = parse_reference_type(trimmed, resolver) {
        return Some(reference);
    }

    // Try to resolve through imports or qualified names
    if let Some(resolved) = resolver.resolve_symbol(trimmed) {
        match resolved {
            crate::resolver::ResolvedSymbol::Struct(module_id, struct_name) => {
                if resolver.current_module.as_ref() == Some(&module_id) {
                    return Some(Type::Named(struct_name));
                } else {
                    return Some(Type::Named(format!("{}::{}", module_id, struct_name)));
                }
            }
            crate::resolver::ResolvedSymbol::Enum(_, enum_name) => {
                return Some(Type::Enum(enum_name));
            }
            _ => {}
        }
    }

    resolver
        .struct_types
        .contains_key(trimmed)
        .then(|| Type::Named(trimmed.to_string()))
}

fn parse_tuple_type(name: &str, resolver: &ResolverContext) -> Option<Type> {
    let trimmed = name.trim();
    if !trimmed.starts_with('(') || !trimmed.ends_with(')') {
        return None;
    }
    let inner = trimmed[1..trimmed.len() - 1].trim();
    if inner.is_empty() {
        return Some(Type::Tuple(Vec::new()));
    }

    let mut parts = Vec::new();
    let mut start = 0;
    let mut depth = 0usize;
    let mut saw_comma = false;
    for (index, ch) in inner.char_indices() {
        match ch {
            '<' | '[' | '(' => depth += 1,
            '>' | ']' | ')' => depth = depth.checked_sub(1)?,
            ',' if depth == 0 => {
                saw_comma = true;
                let part = inner[start..index].trim();
                if !part.is_empty() {
                    parts.push(resolve_type_name(part, resolver)?);
                } else if index != inner.len() - 1 {
                    return None;
                }
                start = index + 1;
            }
            _ => {}
        }
    }
    if depth != 0 {
        return None;
    }

    if !saw_comma {
        return resolve_type_name(inner, resolver);
    }
    let last = inner[start..].trim();
    if !last.is_empty() {
        parts.push(resolve_type_name(last, resolver)?);
    }
    if parts.is_empty() {
        return None;
    }
    Some(Type::Tuple(parts))
}

pub(crate) fn parse_array_type(name: &str, resolver: &ResolverContext) -> Option<Type> {
    // Parse "[T; N]" format
    if !name.starts_with('[') || !name.ends_with(']') {
        return None;
    }

    // Remove brackets
    let inner = &name[1..name.len() - 1];

    // Find the semicolon at the correct bracket nesting level
    // We need to find the last semicolon that's at depth 0
    let mut depth = 0;
    let mut semicolon_pos = None;
    for (i, ch) in inner.chars().enumerate() {
        match ch {
            '[' => depth += 1,
            ']' => depth -= 1,
            ';' if depth == 0 => semicolon_pos = Some(i),
            _ => {}
        }
    }

    let semicolon_pos = semicolon_pos?;
    let elem_type_str = inner[..semicolon_pos].trim();
    let size_str = inner[semicolon_pos + 1..].trim();

    // Parse size
    let size: usize = size_str.parse().ok()?;
    if size == 0 {
        return None; // Array size must be > 0
    }

    // Recursively resolve element type
    let elem_type = resolve_type_name(elem_type_str, resolver)?;

    Some(Type::Array(Box::new(elem_type), size))
}

fn parse_own_type(name: &str, resolver: &ResolverContext) -> Option<Type> {
    parse_single_arg_type(name, "Own", resolver).map(|inner| Type::Own(Box::new(inner)))
}

fn parse_raw_ptr_type(name: &str, resolver: &ResolverContext) -> Option<Type> {
    parse_single_arg_type(name, "RawPtr", resolver).map(|inner| Type::RawPtr(Box::new(inner)))
}

fn parse_shared_type(name: &str, resolver: &ResolverContext) -> Option<Type> {
    parse_single_arg_type(name, "Shared", resolver).map(|inner| Type::Shared(Box::new(inner)))
}

fn parse_type_application(name: &str, resolver: &ResolverContext) -> Option<Type> {
    let trimmed = name.trim();
    let lt = trimmed.find('<')?;
    if !trimmed.ends_with('>') {
        return None;
    }

    // Avoid treating "&mut" prefixes as type applications.
    if trimmed.starts_with('&') {
        return None;
    }

    let base_str = trimmed[..lt].trim();
    if base_str.is_empty() {
        return None;
    }

    // Extract the interior between matching <...> at depth 0.
    let mut depth = 0;
    let mut end_idx = None;
    for (idx, ch) in trimmed[lt + 1..].char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => {
                if depth == 0 {
                    end_idx = Some(lt + 1 + idx);
                    break;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    let end = end_idx?;
    if end + 1 != trimmed.len() {
        return None;
    }

    let inner = &trimmed[lt + 1..end];

    // Split args at commas at depth 0.
    let mut args = Vec::new();
    let mut start = 0;
    let mut depth = 0;
    for (i, ch) in inner.char_indices() {
        match ch {
            '<' | '[' | '(' => depth += 1,
            '>' | ']' | ')' => {
                if depth > 0 {
                    depth -= 1;
                }
            }
            ',' if depth == 0 => {
                let part = inner[start..i].trim();
                if part.is_empty() {
                    return None;
                }
                args.push(resolve_type_name(part, resolver)?);
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = inner[start..].trim();
    if last.is_empty() {
        return None;
    }
    args.push(resolve_type_name(last, resolver)?);

    // Recognize built-in pointer wrappers.
    match base_str {
        "Own" if args.len() == 1 => return Some(Type::Own(Box::new(args.remove(0)))),
        "RawPtr" if args.len() == 1 => return Some(Type::RawPtr(Box::new(args.remove(0)))),
        "Shared" if args.len() == 1 => return Some(Type::Shared(Box::new(args.remove(0)))),
        "FnOnce" if args.len() == 2 => {
            let ret = args.pop().unwrap();
            let args = args.pop().unwrap();
            let params = match args {
                Type::Tuple(params) => params,
                param => vec![param],
            };
            return Some(Type::Function {
                params,
                ret: Box::new(ret),
            });
        }
        "Fn" | "FnMut" if args.len() == 2 => {
            let ret = args.pop().unwrap();
            let args = args.pop().unwrap();
            let params = match args {
                Type::Tuple(params) => params,
                param => vec![param],
            };
            return Some(Type::BorrowedFunction {
                kind: if base_str == "Fn" {
                    glyph_core::types::BorrowedCallableKind::Fn
                } else {
                    glyph_core::types::BorrowedCallableKind::FnMut
                },
                params,
                ret: Box::new(ret),
            });
        }
        "FnOnce" | "Fn" | "FnMut" => return None,
        _ => {}
    }

    // Preserve compiler-issued identities for audited generic runtime types.
    // This must precede the ordinary `get_struct` shortcut below, which keeps
    // user-facing imported names unqualified for structural generics.
    if let Some(crate::resolver::ResolvedSymbol::Struct(module, symbol)) =
        resolver.resolve_symbol(base_str)
    {
        match (module.as_str(), symbol.as_str(), args.as_slice()) {
            ("std/thread", "JoinHandle", [result]) => {
                return Some(glyph_core::thread::canonical_thread_handle_type(
                    result.clone(),
                ));
            }
            ("std/thread", "ScopedJoinHandle", [result]) => {
                return Some(glyph_core::thread::canonical_scoped_thread_handle_type(
                    result.clone(),
                ));
            }
            ("std/sync", "Arc", [inner]) => return Some(Type::arc(inner.clone())),
            ("std/sync", "Mutex", [inner]) => return Some(Type::mutex(inner.clone())),
            ("std/sync", "MutexGuard", [inner]) => {
                return Some(Type::mutex_guard(inner.clone()));
            }
            ("std/sync/spsc", "Sender", [inner]) => {
                return Some(Type::spsc_sender(inner.clone()));
            }
            ("std/sync/spsc", "Receiver", [inner]) => {
                return Some(Type::spsc_receiver(inner.clone()));
            }
            _ => {}
        }
    }

    // Normalize base using existing symbol resolution.
    let mut base_name = base_str.to_string();
    if resolver.get_enum(base_str).is_some() {
        base_name = base_str.to_string();
    } else if resolver.get_struct(base_str).is_some() {
        base_name = base_str.to_string();
    } else if let Some(resolved) = resolver.resolve_symbol(base_str) {
        match resolved {
            crate::resolver::ResolvedSymbol::Struct(module_id, struct_name) => {
                if resolver.current_module.as_ref() == Some(&module_id) {
                    base_name = struct_name;
                } else {
                    base_name = format!("{}::{}", module_id, struct_name);
                }
            }
            crate::resolver::ResolvedSymbol::Enum(_module_id, enum_name) => {
                base_name = enum_name;
            }
            _ => {}
        }
    }

    Some(Type::App {
        base: base_name,
        args,
    })
}

fn parse_single_arg_type(name: &str, keyword: &str, resolver: &ResolverContext) -> Option<Type> {
    let trimmed = name.trim();
    let rest = trimmed.strip_prefix(keyword)?;
    let rest = rest.trim_start();
    if !rest.starts_with('<') {
        return None;
    }

    // Drop leading '<'
    let inner = &rest[1..];
    let mut depth = 0;
    let mut end = None;
    for (idx, ch) in inner.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => {
                if depth == 0 {
                    end = Some(idx);
                    break;
                } else {
                    depth -= 1;
                }
            }
            _ => {}
        }
    }

    let end_idx = end?;
    let inner_str = inner[..end_idx].trim();
    let tail = inner[end_idx + 1..].trim();
    if !tail.is_empty() {
        return None;
    }

    resolve_type_name(inner_str, resolver)
}

fn parse_reference_type(name: &str, resolver: &ResolverContext) -> Option<Type> {
    if !name.starts_with('&') {
        return None;
    }

    let mut rest = name;
    let mut mutabilities = Vec::new();

    while let Some(after_amp) = rest.strip_prefix('&') {
        rest = after_amp.trim_start();
        if rest.is_empty() {
            return None;
        }

        let (mutability, after_mut) = take_mutability_prefix(rest);
        mutabilities.push(mutability);
        rest = after_mut.trim_start();

        if rest.is_empty() {
            return None;
        }

        if !rest.starts_with('&') {
            break;
        }
    }

    let base_name = rest.trim();
    if base_name.is_empty() {
        return None;
    }

    if base_name == "str" {
        if mutabilities.len() == 1 && matches!(mutabilities[0], Mutability::Immutable) {
            return Some(Type::Str);
        }
        return None;
    }

    let mut ty = resolve_type_name(base_name, resolver)?;
    for mutability in mutabilities.into_iter().rev() {
        ty = Type::Ref(Box::new(ty), mutability);
    }
    Some(ty)
}

fn take_mutability_prefix(input: &str) -> (Mutability, &str) {
    if let Some(after) = input.strip_prefix("mut") {
        if is_keyword_boundary(after.chars().next()) {
            return (Mutability::Mutable, after.trim_start());
        }
    }
    (Mutability::Immutable, input)
}

fn is_keyword_boundary(next: Option<char>) -> bool {
    match next {
        None => true,
        Some(c) => !c.is_alphanumeric() && c != '_',
    }
}

pub(crate) fn struct_name_from_type(ty: &Type) -> Option<String> {
    match ty {
        Type::Named(name) => Some(name.clone()),
        Type::Ref(inner, _) => struct_name_from_type(inner),
        Type::Own(inner) => struct_name_from_type(inner),
        Type::RawPtr(inner) => struct_name_from_type(inner),
        Type::Shared(inner) => struct_name_from_type(inner),
        Type::Tuple(elem_types) => Some(tuple_struct_name(elem_types)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_type_name;
    use crate::resolver::ResolverContext;
    use glyph_core::types::Type;

    #[test]
    fn parses_callable_arity_and_preserves_one_tuple_parameter() {
        let resolver = ResolverContext::default();

        assert_eq!(
            resolve_type_name("FnOnce<((i32, bool),), String>", &resolver),
            Some(Type::Function {
                params: vec![Type::Tuple(vec![Type::I32, Type::Bool])],
                ret: Box::new(Type::String),
            })
        );
        assert_eq!(resolve_type_name("FnOnce<i32>", &resolver), None);
        assert_eq!(resolve_type_name("FnOnce<i32, i32, i32>", &resolver), None);
    }
}
