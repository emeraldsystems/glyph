//! Centralized enum-variant resolution (GLYPH-4).
//!
//! Before this module existed, enum construction, `match` lowering, and the
//! `?` operator each independently searched an `EnumType`'s variant list by
//! name (`enum_def.variants.iter().position(...)`), with slightly different
//! error handling and no single place to reason about variant-order
//! guarantees. This module centralizes that lookup so every consumer agrees
//! on how a variant's declared index and payload type are derived from its
//! name.
//!
//! Variant order itself is NOT free to change here: `glyph-backend`'s
//! `codegen_option_none`/`codegen_option_some`/`codegen_result_ok`/
//! `codegen_result_err` (see `crates/glyph-backend/src/codegen/aggregate.rs`)
//! hardcode `None`/`Ok` at index 0 and `Some`/`Err` at index 1, matching the
//! declaration order in `crates/glyph-frontend/src/stdlib.rs`. Several
//! compiler-synthesized constructions in `mir_lower/builtins/*.rs` (Vec/Map
//! accessors returning `Option`, spsc `TrySendResult`/`TryRecvResult`, mutex
//! lock results, thread join results) also hardcode literal variant indices
//! for these same enums, and deliberately do NOT run through
//! `ResolverContext::get_enum` at all: they lower to a bare `App { base:
//! "Option", .. }`/`"Result"` MIR type before that enum is guaranteed to be
//! registered in the *current module's* resolver context (it is only
//! guaranteed to be resolvable later, during `monomorphize_mir`, which scans
//! the full injected module set rather than the current module's reachable
//! imports). Routing those sites through `resolve_enum_variant` would regress
//! programs that use `Vec`/`Map`/`thread`/`sync` helpers without an explicit
//! `from std/enums import Option` — this was verified empirically before
//! writing this module. Those sites are intentionally left untouched.
//!
//! What *is* centralized here: any lowering step that already knows (or can
//! look up) an `EnumType` and a variant name and needs the corresponding
//! declared index and payload type. That covers `match` arm lowering (both
//! the tag comparison and payload extraction) and the `?` operator's Ok/Err
//! (Some/None) resolution.

use glyph_core::types::{EnumType, EnumVariant, Type};

use crate::resolver::ResolverContext;

/// A successfully resolved enum variant: its declared index (used for the
/// runtime tag comparison / `EnumPayload` extraction) and its declared
/// payload type, if any.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedVariant {
    pub(crate) variant_index: u32,
    pub(crate) payload: Option<Type>,
}

/// Why a `(enum_name, variant_name)` pair failed to resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VariantLookupError {
    /// No enum named `enum_name` is visible in the current resolver context.
    UnknownEnum,
    /// The enum exists, but has no variant named `variant_name`.
    UnknownVariant,
}

/// Find `variant_name` among the variants already known to belong to
/// `enum_def`. This is the single place that walks an enum's variant list by
/// name; every other lookup in `mir_lower` should go through this function
/// (or [`resolve_enum_variant`], which also resolves the enum itself).
pub(crate) fn find_variant(enum_def: &EnumType, variant_name: &str) -> Option<ResolvedVariant> {
    enum_def
        .variants
        .iter()
        .enumerate()
        .find(|(_, v): &(usize, &EnumVariant)| v.name == variant_name)
        .map(|(idx, v)| ResolvedVariant {
            variant_index: idx as u32,
            payload: v.payload.clone(),
        })
}

/// Resolve `enum_name::variant_name` from scratch via the resolver context.
/// Use this when the caller only has the two names (e.g. verifying a
/// pattern's qualifier against the enum it claims to belong to) and hasn't
/// already fetched the `EnumType`.
pub(crate) fn resolve_enum_variant(
    resolver: &ResolverContext,
    enum_name: &str,
    variant_name: &str,
) -> Result<ResolvedVariant, VariantLookupError> {
    let enum_def = resolver
        .get_enum(enum_name)
        .ok_or(VariantLookupError::UnknownEnum)?;
    find_variant(enum_def, variant_name).ok_or(VariantLookupError::UnknownVariant)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enum_type(name: &str, variants: &[(&str, Option<Type>)]) -> EnumType {
        EnumType {
            name: name.to_string(),
            variants: variants
                .iter()
                .map(|(n, p)| EnumVariant {
                    name: n.to_string(),
                    payload: p.clone(),
                })
                .collect(),
        }
    }

    #[test]
    fn find_variant_returns_declared_index_and_payload() {
        let opt = enum_type("Option", &[("None", None), ("Some", Some(Type::I32))]);
        let none = find_variant(&opt, "None").expect("None should resolve");
        assert_eq!(none.variant_index, 0);
        assert_eq!(none.payload, None);

        let some = find_variant(&opt, "Some").expect("Some should resolve");
        assert_eq!(some.variant_index, 1);
        assert_eq!(some.payload, Some(Type::I32));
    }

    #[test]
    fn find_variant_respects_non_alphabetical_custom_order() {
        // A custom enum declared with Some/None in the OPPOSITE order from
        // stdlib Option must resolve against its own declaration order, not
        // stdlib's.
        let custom = enum_type("MyOpt", &[("Some", Some(Type::I32)), ("None", None)]);
        let some = find_variant(&custom, "Some").expect("Some should resolve");
        assert_eq!(some.variant_index, 0);
        let none = find_variant(&custom, "None").expect("None should resolve");
        assert_eq!(none.variant_index, 1);
    }

    #[test]
    fn find_variant_unknown_variant_is_none() {
        let opt = enum_type("Option", &[("None", None), ("Some", Some(Type::I32))]);
        assert!(find_variant(&opt, "Nope").is_none());
    }

    #[test]
    fn resolve_enum_variant_unknown_enum() {
        let resolver = ResolverContext::default();
        let err = resolve_enum_variant(&resolver, "DoesNotExist", "X").unwrap_err();
        assert_eq!(err, VariantLookupError::UnknownEnum);
    }

    #[test]
    fn resolve_enum_variant_unknown_variant() {
        let mut resolver = ResolverContext::default();
        resolver
            .enum_types
            .insert("Foo".to_string(), enum_type("Foo", &[("Bar", None)]));
        let err = resolve_enum_variant(&resolver, "Foo", "Baz").unwrap_err();
        assert_eq!(err, VariantLookupError::UnknownVariant);
    }

    #[test]
    fn resolve_enum_variant_success() {
        let mut resolver = ResolverContext::default();
        resolver.enum_types.insert(
            "Foo".to_string(),
            enum_type("Foo", &[("Bar", None), ("Baz", Some(Type::Bool))]),
        );
        let resolved = resolve_enum_variant(&resolver, "Foo", "Baz").expect("should resolve");
        assert_eq!(resolved.variant_index, 1);
        assert_eq!(resolved.payload, Some(Type::Bool));
    }
}
