//! Compiler contract for native-thread builtins.
//!
//! The surface API specializes `spawn<T>` and `JoinHandle<T>` without requiring
//! general generic functions. MIR keeps the opaque runtime pointer, status,
//! and typed output slot separate. This avoids coupling retry semantics to the
//! user-visible `Result` layout. Only lowering may create raw handle locals.

use crate::thread_safety::{ThreadSafetyError, ThreadSafetyRegistry, ThreadSafetyType};
use crate::types::{BorrowedCallableKind, Type};

pub const SCOPED_THREAD_SCOPE_TYPE: &str = "std::thread::Scope";
pub const SCOPED_THREAD_HANDLE_TYPE: &str = "std::thread::ScopedJoinHandle";
pub const PRIVATE_THREAD_SCOPE_TYPE: &str = "$glyph::thread::ScopeRaw";
pub const PRIVATE_SCOPED_THREAD_HANDLE_TYPE: &str = "$glyph::thread::ScopedJoinHandleRaw";

/// The only callable signature accepted by the GLYPH-42 spawn primitive.
pub fn unit_task_type() -> Type {
    task_type(Type::Tuple(Vec::new()))
}

/// Concrete compiler-intrinsic task signature for `spawn<T>`.
pub fn task_type(result: Type) -> Type {
    Type::Function {
        params: Vec::new(),
        ret: Box::new(result),
    }
}

/// Concrete borrowed callable signature accepted by a scoped spawn.
pub fn scoped_task_type(kind: BorrowedCallableKind, result: Type) -> Type {
    Type::BorrowedFunction {
        kind,
        params: Vec::new(),
        ret: Box::new(result),
    }
}

/// Private low-level representation of `JoinHandle<()>` in MIR.
///
/// This type must never be surfaced through source type resolution.
pub fn private_unit_handle_type() -> Type {
    private_thread_handle_type()
}

/// Private low-level representation shared by every `JoinHandle<T>`.
pub fn private_thread_handle_type() -> Type {
    Type::RawPtr(Box::new(Type::I8))
}

/// Private opaque pointer used while lowering a lexical scope.
pub fn private_thread_scope_type() -> Type {
    Type::Named(PRIVATE_THREAD_SCOPE_TYPE.into())
}

/// Private opaque pointer used while lowering a scope-owned child token.
pub fn private_scoped_thread_handle_type(result: Type) -> Type {
    Type::App {
        base: PRIVATE_SCOPED_THREAD_HANDLE_TYPE.into(),
        args: vec![result],
    }
}

pub fn private_scoped_thread_handle_result(ty: &Type) -> Option<&Type> {
    match ty {
        Type::App { base, args }
            if base == PRIVATE_SCOPED_THREAD_HANDLE_TYPE && args.len() == 1 =>
        {
            args.first()
        }
        _ => None,
    }
}

/// Resolver-issued public identity for the unit join handle.
///
/// The generic-looking type is deliberately not backed by a source-visible
/// struct. Codegen maps this one canonical application to opaque pointer
/// storage; a user declaration named `JoinHandle` remains an ordinary type.
pub fn canonical_unit_handle_type() -> Type {
    canonical_thread_handle_type(Type::Void)
}

/// Resolver-issued public identity for a concrete typed join handle.
pub fn canonical_thread_handle_type(result: Type) -> Type {
    Type::App {
        base: "std::thread::JoinHandle".into(),
        args: vec![result],
    }
}

/// Resolver-issued public identity for native thread errors.
pub fn canonical_thread_error_type() -> Type {
    Type::Named("std::thread::ThreadError".into())
}

/// Resolver-issued identity for the lexical scope token.
pub fn canonical_thread_scope_type() -> Type {
    Type::Named(SCOPED_THREAD_SCOPE_TYPE.into())
}

/// Resolver-issued identity for a scope-owned typed child token.
pub fn canonical_scoped_thread_handle_type(result: Type) -> Type {
    Type::App {
        base: SCOPED_THREAD_HANDLE_TYPE.into(),
        args: vec![result],
    }
}

/// Public result returned by the unit-only spawn primitive.
pub fn unit_spawn_result_type() -> Type {
    spawn_result_type(Type::Void)
}

/// Public result returned by compiler-specialized `spawn<T>`.
pub fn spawn_result_type(result: Type) -> Type {
    Type::App {
        base: "Result".into(),
        args: vec![
            canonical_thread_handle_type(result),
            canonical_thread_error_type(),
        ],
    }
}

/// Public result returned by unit join and detach operations.
pub fn unit_thread_status_result_type() -> Type {
    join_result_type(Type::Void)
}

/// Public result returned by compiler-specialized `JoinHandle<T>::join`.
pub fn join_result_type(result: Type) -> Type {
    Type::App {
        base: "Result".into(),
        args: vec![result, canonical_thread_error_type()],
    }
}

pub fn is_canonical_unit_handle(ty: &Type) -> bool {
    ty == &canonical_unit_handle_type()
}

/// Return the concrete result parameter only for the compiler-owned handle.
pub fn canonical_thread_handle_result(ty: &Type) -> Option<&Type> {
    match ty {
        Type::App { base, args } if base == "std::thread::JoinHandle" && args.len() == 1 => {
            args.first()
        }
        _ => None,
    }
}

pub fn is_canonical_thread_handle(ty: &Type) -> bool {
    canonical_thread_handle_result(ty).is_some()
}

pub fn is_canonical_thread_error(ty: &Type) -> bool {
    ty == &canonical_thread_error_type()
}

pub fn canonical_scoped_thread_handle_result(ty: &Type) -> Option<&Type> {
    match ty {
        Type::App { base, args } if base == SCOPED_THREAD_HANDLE_TYPE && args.len() == 1 => {
            args.first()
        }
        _ => None,
    }
}

pub fn is_canonical_scoped_thread_handle(ty: &Type) -> bool {
    canonical_scoped_thread_handle_result(ty).is_some()
}

/// Validate the complete thread transfer before emitting spawn MIR.
///
/// In particular, an erased callable without capture provenance is rejected
/// by `validate_spawn_then`; the emitter is never run on a safety error.
pub fn validate_unit_spawn_then<T, F>(
    registry: &ThreadSafetyRegistry<'_>,
    task: &ThreadSafetyType,
    emit_runtime_mir: F,
) -> Result<T, ThreadSafetyError>
where
    F: FnOnce() -> T,
{
    registry.validate_spawn_then(task, &ThreadSafetyType::plain(Type::Void), emit_runtime_mir)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::thread_safety::{CallableSendProvenance, ThreadSafetyType};

    #[test]
    fn unit_spawn_gate_fails_closed_before_emission() {
        let structs = HashMap::new();
        let enums = HashMap::new();
        let registry = ThreadSafetyRegistry::new(&structs, &enums);
        let task = ThreadSafetyType::callable(
            unit_task_type(),
            CallableSendProvenance::Unknown {
                reason: "callable loaded without its provenance certificate".into(),
            },
        );
        let mut emitted = false;

        let error = validate_unit_spawn_then(&registry, &task, || emitted = true).unwrap_err();

        assert!(!emitted);
        assert!(error.reason.contains("provenance is unknown"));
    }

    #[test]
    fn unit_spawn_gate_accepts_a_function_item() {
        let structs = HashMap::new();
        let enums = HashMap::new();
        let registry = ThreadSafetyRegistry::new(&structs, &enums);
        let task = ThreadSafetyType::callable(
            unit_task_type(),
            CallableSendProvenance::FunctionItem {
                symbol: "worker".into(),
            },
        );

        assert_eq!(validate_unit_spawn_then(&registry, &task, || 42), Ok(42));
    }

    #[test]
    fn low_level_handle_representation_stays_explicitly_private_and_opaque() {
        assert_eq!(private_unit_handle_type(), Type::RawPtr(Box::new(Type::I8)));
    }

    #[test]
    fn typed_handle_identity_retains_its_concrete_result() {
        let result = Type::Tuple(vec![Type::I32, Type::String]);
        let handle = canonical_thread_handle_type(result.clone());

        assert!(is_canonical_thread_handle(&handle));
        assert_eq!(canonical_thread_handle_result(&handle), Some(&result));
        assert!(!is_canonical_thread_handle(&Type::App {
            base: "user::JoinHandle".into(),
            args: vec![result],
        }));
    }

    #[test]
    fn scoped_identities_are_canonical_and_keep_the_result_type() {
        let result = Type::Tuple(vec![Type::I32, Type::String]);
        let handle = canonical_scoped_thread_handle_type(result.clone());

        assert_eq!(
            canonical_thread_scope_type(),
            Type::Named(SCOPED_THREAD_SCOPE_TYPE.into())
        );
        assert!(is_canonical_scoped_thread_handle(&handle));
        assert_eq!(
            canonical_scoped_thread_handle_result(&handle),
            Some(&result)
        );
        assert!(!is_canonical_scoped_thread_handle(&Type::App {
            base: "user::ScopedJoinHandle".into(),
            args: vec![result.clone()],
        }));
        assert_eq!(
            private_thread_scope_type(),
            Type::Named(PRIVATE_THREAD_SCOPE_TYPE.into())
        );
        assert_ne!(
            private_thread_scope_type(),
            private_scoped_thread_handle_type(Type::Void)
        );
        assert_eq!(
            private_scoped_thread_handle_result(&private_scoped_thread_handle_type(result.clone())),
            Some(&result)
        );
    }
}
