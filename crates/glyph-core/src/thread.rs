//! Compiler contract for the first unit-returning native-thread builtin.
//!
//! The surface API is `Result<JoinHandle<()>, ThreadError>`, but MIR keeps the
//! opaque runtime pointer and status separate. This avoids coupling runtime
//! retry semantics to the user-visible `Result` layout. Only lowering may
//! create the private raw handle local.

use crate::thread_safety::{ThreadSafetyError, ThreadSafetyRegistry, ThreadSafetyType};
use crate::types::Type;

/// The only callable signature accepted by the GLYPH-42 spawn primitive.
pub fn unit_task_type() -> Type {
    Type::Function {
        params: Vec::new(),
        ret: Box::new(Type::Void),
    }
}

/// Private low-level representation of `JoinHandle<()>` in MIR.
///
/// This type must never be surfaced through source type resolution.
pub fn private_unit_handle_type() -> Type {
    Type::RawPtr(Box::new(Type::I8))
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
}
