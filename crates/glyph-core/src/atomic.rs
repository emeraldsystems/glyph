use serde::{Deserialize, Serialize};

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
    use super::{AtomicOrdering, validate_compare_exchange_orderings};

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
