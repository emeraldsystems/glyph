use serde::{Deserialize, Serialize};

/// The scalar payload supported by Glyph's safe atomic wrappers.
///
/// Pointer and floating-point payloads are intentionally absent: a pointer
/// atomic cannot establish pointee lifetime, and LLVM has no portable
/// floating-point atomic load/store/RMW surface matching the v1 contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AtomicScalar {
    Bool,
    I32,
    U32,
    I64,
    U64,
    Usize,
}

impl AtomicScalar {
    pub const fn type_name(self) -> &'static str {
        match self {
            Self::Bool => "AtomicBool",
            Self::I32 => "AtomicI32",
            Self::U32 => "AtomicU32",
            Self::I64 => "AtomicI64",
            Self::U64 => "AtomicU64",
            Self::Usize => "AtomicUsize",
        }
    }

    pub const fn fixed_width_bits(self) -> Option<u32> {
        match self {
            Self::Bool => Some(8),
            Self::I32 | Self::U32 => Some(32),
            // Glyph's current backend defines `usize` as i64 on every target
            // (see docs/todo/VEC_TODO.md). Keep AtomicUsize representation-
            // compatible and reject targets without native 64-bit atomics
            // instead of silently truncating at the API boundary.
            Self::I64 | Self::U64 | Self::Usize => Some(64),
        }
    }

    pub const fn is_integer(self) -> bool {
        !matches!(self, Self::Bool)
    }
}

/// Target facts used to reject atomic wrappers that cannot be represented as
/// naturally aligned, lock-free scalar operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtomicTargetCapabilities {
    pub pointer_width_bits: u32,
    pub max_atomic_width_bits: u32,
}

impl AtomicTargetCapabilities {
    pub fn validate(self, scalar: AtomicScalar) -> Result<u32, String> {
        let width = scalar.fixed_width_bits().unwrap_or(self.pointer_width_bits);
        if !matches!(width, 8 | 32 | 64) {
            return Err(format!(
                "{} requires an unsupported {}-bit atomic width",
                scalar.type_name(),
                width
            ));
        }
        if width > self.max_atomic_width_bits {
            return Err(format!(
                "{} requires lock-free {}-bit atomics, but this target guarantees only {}-bit atomics",
                scalar.type_name(),
                width,
                self.max_atomic_width_bits
            ));
        }
        Ok(width)
    }
}

/// Maximum atomic width Glyph guarantees will lower without a libcall or
/// hidden lock for the named LLVM architecture baseline.
pub fn guaranteed_native_atomic_width(architecture: &str) -> Option<u32> {
    match architecture {
        "x86_64" | "aarch64" | "arm64" => Some(64),
        "i386" | "i486" | "i586" | "i686" | "arm" | "armv7" | "thumbv7" => Some(32),
        _ => None,
    }
}

/// Memory ordering used by compiler-generated atomic operations.
///
/// Glyph's public v1 atomic API emits [`AtomicOrdering::SeqCst`]. The weaker
/// variants exist for compiler-owned implementations such as `Arc<T>` and the
/// bounded SPSC channel, where the ordering proof lives with the lowering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AtomicOrdering {
    Relaxed,
    Acquire,
    Release,
    AcqRel,
    SeqCst,
}

impl AtomicOrdering {
    pub fn valid_for_load(self) -> bool {
        !matches!(self, Self::Release | Self::AcqRel)
    }

    pub fn valid_for_store(self) -> bool {
        !matches!(self, Self::Acquire | Self::AcqRel)
    }

    pub fn valid_for_compare_exchange_failure(self) -> bool {
        !matches!(self, Self::Release | Self::AcqRel)
    }

    pub fn is_at_least_as_strong_as(self, other: Self) -> bool {
        use AtomicOrdering::*;

        match (self, other) {
            (SeqCst, _) | (_, Relaxed) => true,
            (Acquire, Acquire) | (Release, Release) | (AcqRel, Acquire | Release | AcqRel) => true,
            _ => self == other,
        }
    }
}

/// Atomic read-modify-write operations shared by public atomics, `Arc<T>`,
/// and channel lowering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AtomicRmwOp {
    Swap,
    Add,
    Sub,
    And,
    Or,
    Xor,
}

/// Validate the ordering pair for compare-exchange.
pub fn validate_compare_exchange_orderings(
    success: AtomicOrdering,
    failure: AtomicOrdering,
) -> Result<(), &'static str> {
    if !failure.valid_for_compare_exchange_failure() {
        return Err("compare-exchange failure ordering cannot be Release or AcqRel");
    }
    if !success.is_at_least_as_strong_as(failure) {
        return Err("compare-exchange failure ordering cannot be stronger than success ordering");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        AtomicOrdering, AtomicScalar, AtomicTargetCapabilities, guaranteed_native_atomic_width,
        validate_compare_exchange_orderings,
    };

    #[test]
    fn atomic_scalar_surface_excludes_pointers_and_floats() {
        let names = [
            AtomicScalar::Bool,
            AtomicScalar::I32,
            AtomicScalar::U32,
            AtomicScalar::I64,
            AtomicScalar::U64,
            AtomicScalar::Usize,
        ]
        .map(AtomicScalar::type_name);

        assert_eq!(
            names,
            [
                "AtomicBool",
                "AtomicI32",
                "AtomicU32",
                "AtomicI64",
                "AtomicU64",
                "AtomicUsize"
            ]
        );
    }

    #[test]
    fn target_capabilities_reject_unsupported_atomic_widths() {
        let target32 = AtomicTargetCapabilities {
            pointer_width_bits: 32,
            max_atomic_width_bits: 32,
        };
        assert!(target32.validate(AtomicScalar::Usize).is_err());
        assert!(
            target32
                .validate(AtomicScalar::U64)
                .unwrap_err()
                .contains("guarantees only 32-bit atomics")
        );

        let target64 = AtomicTargetCapabilities {
            pointer_width_bits: 64,
            max_atomic_width_bits: 64,
        };
        assert_eq!(target64.validate(AtomicScalar::Bool), Ok(8));
        assert_eq!(target64.validate(AtomicScalar::I64), Ok(64));
        assert_eq!(target64.validate(AtomicScalar::Usize), Ok(64));

        assert_eq!(guaranteed_native_atomic_width("arm64"), Some(64));
        assert_eq!(guaranteed_native_atomic_width("i686"), Some(32));
        assert_eq!(guaranteed_native_atomic_width("wasm32"), None);
        assert_eq!(guaranteed_native_atomic_width("riscv64"), None);
    }

    #[test]
    fn rejects_invalid_load_and_store_orderings() {
        assert!(!AtomicOrdering::Release.valid_for_load());
        assert!(!AtomicOrdering::AcqRel.valid_for_load());
        assert!(!AtomicOrdering::Acquire.valid_for_store());
        assert!(!AtomicOrdering::AcqRel.valid_for_store());
        assert!(AtomicOrdering::SeqCst.valid_for_load());
        assert!(AtomicOrdering::SeqCst.valid_for_store());
    }

    #[test]
    fn validates_compare_exchange_ordering_pairs() {
        assert!(
            validate_compare_exchange_orderings(AtomicOrdering::SeqCst, AtomicOrdering::Acquire,)
                .is_ok()
        );
        assert!(
            validate_compare_exchange_orderings(AtomicOrdering::AcqRel, AtomicOrdering::Acquire,)
                .is_ok()
        );
        assert!(
            validate_compare_exchange_orderings(AtomicOrdering::Acquire, AtomicOrdering::SeqCst,)
                .is_err()
        );
        assert!(
            validate_compare_exchange_orderings(AtomicOrdering::SeqCst, AtomicOrdering::Release,)
                .is_err()
        );
    }

    #[test]
    fn compare_exchange_ordering_matrix_matches_the_memory_model() {
        use AtomicOrdering::{AcqRel, Acquire, Relaxed, Release, SeqCst};

        let valid = [
            (Relaxed, Relaxed),
            (Acquire, Relaxed),
            (Acquire, Acquire),
            (Release, Relaxed),
            (AcqRel, Relaxed),
            (AcqRel, Acquire),
            (SeqCst, Relaxed),
            (SeqCst, Acquire),
            (SeqCst, SeqCst),
        ];
        let invalid = [
            (Relaxed, Acquire),
            (Relaxed, SeqCst),
            (Acquire, SeqCst),
            (Release, Acquire),
            (Release, SeqCst),
            (AcqRel, SeqCst),
            (SeqCst, Release),
            (SeqCst, AcqRel),
        ];

        for (success, failure) in valid {
            assert!(
                validate_compare_exchange_orderings(success, failure).is_ok(),
                "expected {success:?}/{failure:?} to be valid"
            );
        }
        for (success, failure) in invalid {
            assert!(
                validate_compare_exchange_orderings(success, failure).is_err(),
                "expected {success:?}/{failure:?} to be invalid"
            );
        }
    }
}
