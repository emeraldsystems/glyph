use serde::{Deserialize, Serialize};

use crate::atomic::AtomicScalar;

/// Canonical compiler identity for the thread-safe reference-counted owner.
/// Aliases and qualified source paths must resolve to this constructor before
/// MIR is emitted; arbitrary user structs named `Arc` are not accepted by the
/// backend through nominal/source-name matching.
pub const ARC_TYPE_CONSTRUCTOR: &str = "std::sync::Arc";
/// Canonical compiler identities for exclusive shared mutation and its
/// lexical, nonescaping lock token.
pub const MUTEX_TYPE_CONSTRUCTOR: &str = "std::sync::Mutex";
pub const MUTEX_GUARD_TYPE_CONSTRUCTOR: &str = "std::sync::MutexGuard";
/// Canonical compiler identities for the two unique endpoints of a bounded
/// single-producer/single-consumer ring. These are deliberately distinct
/// nominal applications even though both lower to a pointer to the same
/// compiler-owned allocation.
pub const SPSC_SENDER_TYPE_CONSTRUCTOR: &str = "std::sync::spsc::Sender";
pub const SPSC_RECEIVER_TYPE_CONSTRUCTOR: &str = "std::sync::spsc::Receiver";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Mutability {
    Immutable,
    Mutable,
}

/// A non-owning callable capability whose environment is valid only for a
/// compiler-proven region.
///
/// `Fn` permits repeatable shared access to the environment. `FnMut` permits
/// repeatable exclusive access. Owned, consuming callables remain represented
/// by [`Type::Function`] so their serialized MIR and ABI stay unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BorrowedCallableKind {
    Fn,
    FnMut,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Type {
    I8,
    I32,
    I64,
    U8,
    U32,
    U64,
    Usize,
    F32,
    F64,
    Bool,
    Char,
    Str,
    String,
    Void,
    Named(String),
    Enum(String),
    Param(String),
    App {
        base: String,
        args: Vec<Type>,
    },
    Ref(Box<Type>, Mutability),
    Array(Box<Type>, usize),
    Own(Box<Type>),
    RawPtr(Box<Type>),
    Shared(Box<Type>),
    /// A non-Copy scalar atomic. Its backing storage may only be accessed by
    /// atomic MIR operations.
    Atomic(AtomicScalar),
    /// An owned, once-callable value.
    ///
    /// The backend represents this as an erased `{ env, invoke, drop }`
    /// carrier. `invoke` receives the hidden environment pointer before the
    /// declared parameters (after an ABI-mandated sret pointer, when present).
    Function {
        params: Vec<Type>,
        ret: Box<Type>,
    },
    /// A borrowed, repeatable callable view.
    ///
    /// This uses the same physical `{ env, invoke, drop }` carrier as
    /// [`Type::Function`], but does not own or consume `env`. Lifetime and
    /// exclusivity checks are a frontend responsibility; the backend preserves
    /// the non-consuming invocation contract.
    BorrowedFunction {
        kind: BorrowedCallableKind,
        params: Vec<Type>,
        ret: Box<Type>,
    },
    Tuple(Vec<Type>),
}

impl Type {
    pub fn arc(inner: Type) -> Self {
        Type::App {
            base: ARC_TYPE_CONSTRUCTOR.to_string(),
            args: vec![inner],
        }
    }

    pub fn arc_inner_type(&self) -> Option<&Type> {
        match self {
            Type::App { base, args } if base == ARC_TYPE_CONSTRUCTOR && args.len() == 1 => {
                args.first()
            }
            _ => None,
        }
    }

    pub fn is_arc(&self) -> bool {
        self.arc_inner_type().is_some()
    }

    pub fn mutex(inner: Type) -> Self {
        Type::App {
            base: MUTEX_TYPE_CONSTRUCTOR.to_string(),
            args: vec![inner],
        }
    }

    pub fn mutex_guard(inner: Type) -> Self {
        Type::App {
            base: MUTEX_GUARD_TYPE_CONSTRUCTOR.to_string(),
            args: vec![inner],
        }
    }

    pub fn mutex_inner_type(&self) -> Option<&Type> {
        match self {
            Type::App { base, args } if base == MUTEX_TYPE_CONSTRUCTOR && args.len() == 1 => {
                args.first()
            }
            _ => None,
        }
    }

    pub fn mutex_guard_inner_type(&self) -> Option<&Type> {
        match self {
            Type::App { base, args } if base == MUTEX_GUARD_TYPE_CONSTRUCTOR && args.len() == 1 => {
                args.first()
            }
            _ => None,
        }
    }

    pub fn is_mutex(&self) -> bool {
        self.mutex_inner_type().is_some()
    }

    pub fn is_mutex_guard(&self) -> bool {
        self.mutex_guard_inner_type().is_some()
    }

    pub fn spsc_sender(inner: Type) -> Self {
        Type::App {
            base: SPSC_SENDER_TYPE_CONSTRUCTOR.to_string(),
            args: vec![inner],
        }
    }

    pub fn spsc_receiver(inner: Type) -> Self {
        Type::App {
            base: SPSC_RECEIVER_TYPE_CONSTRUCTOR.to_string(),
            args: vec![inner],
        }
    }

    pub fn spsc_sender_inner_type(&self) -> Option<&Type> {
        match self {
            Type::App { base, args } if base == SPSC_SENDER_TYPE_CONSTRUCTOR && args.len() == 1 => {
                args.first()
            }
            _ => None,
        }
    }

    pub fn spsc_receiver_inner_type(&self) -> Option<&Type> {
        match self {
            Type::App { base, args }
                if base == SPSC_RECEIVER_TYPE_CONSTRUCTOR && args.len() == 1 =>
            {
                args.first()
            }
            _ => None,
        }
    }

    pub fn is_spsc_sender(&self) -> bool {
        self.spsc_sender_inner_type().is_some()
    }

    pub fn is_spsc_receiver(&self) -> bool {
        self.spsc_receiver_inner_type().is_some()
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "i8" => Some(Type::I8),
            "i32" | "i" => Some(Type::I32),
            "i64" => Some(Type::I64),
            "u8" => Some(Type::U8),
            "u32" | "u" => Some(Type::U32),
            "u64" => Some(Type::U64),
            "usize" => Some(Type::Usize),
            "char" | "c" => Some(Type::Char),
            "f32" => Some(Type::F32),
            "f64" | "f" => Some(Type::F64),
            "bool" | "b" => Some(Type::Bool),
            "str" => Some(Type::Str),
            "String" => Some(Type::String),
            "AtomicBool" => Some(Type::Atomic(AtomicScalar::Bool)),
            "AtomicI32" => Some(Type::Atomic(AtomicScalar::I32)),
            "AtomicU32" => Some(Type::Atomic(AtomicScalar::U32)),
            "AtomicI64" => Some(Type::Atomic(AtomicScalar::I64)),
            "AtomicU64" => Some(Type::Atomic(AtomicScalar::U64)),
            "AtomicUsize" => Some(Type::Atomic(AtomicScalar::Usize)),
            _ => None,
        }
    }

    pub fn is_int(&self) -> bool {
        matches!(
            self,
            Type::I8
                | Type::I32
                | Type::I64
                | Type::U8
                | Type::U32
                | Type::U64
                | Type::Usize
                | Type::Char
        )
    }

    pub fn is_float(&self) -> bool {
        matches!(self, Type::F32 | Type::F64)
    }

    pub fn is_numeric(&self) -> bool {
        self.is_int() || self.is_float()
    }

    pub fn is_ref(&self) -> bool {
        matches!(self, Type::Ref(..))
    }

    pub fn is_param(&self) -> bool {
        matches!(self, Type::Param(_))
    }

    pub fn inner_type(&self) -> Option<&Type> {
        match self {
            Type::Ref(inner, _) => Some(inner),
            _ => None,
        }
    }

    pub fn is_mut_ref(&self) -> bool {
        matches!(self, Type::Ref(_, Mutability::Mutable))
    }

    pub fn is_array(&self) -> bool {
        matches!(self, Type::Array(..))
    }

    pub fn array_element_type(&self) -> Option<&Type> {
        match self {
            Type::Array(elem, _) => Some(elem),
            _ => None,
        }
    }

    pub fn array_size(&self) -> Option<usize> {
        match self {
            Type::Array(_, size) => Some(*size),
            _ => None,
        }
    }

    pub fn is_own(&self) -> bool {
        matches!(self, Type::Own(_))
    }

    pub fn own_inner_type(&self) -> Option<&Type> {
        match self {
            Type::Own(inner) => Some(inner),
            _ => None,
        }
    }

    pub fn is_raw_ptr(&self) -> bool {
        matches!(self, Type::RawPtr(_))
    }

    pub fn raw_ptr_inner_type(&self) -> Option<&Type> {
        match self {
            Type::RawPtr(inner) => Some(inner),
            _ => None,
        }
    }

    pub fn is_shared(&self) -> bool {
        matches!(self, Type::Shared(_))
    }

    pub fn shared_inner_type(&self) -> Option<&Type> {
        match self {
            Type::Shared(inner) => Some(inner),
            _ => None,
        }
    }

    pub fn atomic_scalar(&self) -> Option<AtomicScalar> {
        match self {
            Type::Atomic(scalar) => Some(*scalar),
            _ => None,
        }
    }

    pub fn function_signature(&self) -> Option<(&[Type], &Type)> {
        match self {
            Type::Function { params, ret } | Type::BorrowedFunction { params, ret, .. } => {
                Some((params, ret))
            }
            _ => None,
        }
    }

    pub fn borrowed_callable_kind(&self) -> Option<BorrowedCallableKind> {
        match self {
            Type::BorrowedFunction { kind, .. } => Some(*kind),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arc_uses_one_canonical_type_application() {
        let arc = Type::arc(Type::String);
        assert!(arc.is_arc());
        assert_eq!(arc.arc_inner_type(), Some(&Type::String));
        assert!(
            !Type::App {
                base: "Arc".into(),
                args: vec![Type::String],
            }
            .is_arc()
        );
        assert!(
            !Type::App {
                base: "user::Arc".into(),
                args: vec![Type::String],
            }
            .is_arc()
        );
        assert!(
            !Type::App {
                base: ARC_TYPE_CONSTRUCTOR.into(),
                args: vec![Type::I32, Type::I32],
            }
            .is_arc()
        );
    }

    #[test]
    fn mutex_and_guard_require_canonical_single_argument_applications() {
        let mutex = Type::mutex(Type::I32);
        let guard = Type::mutex_guard(Type::I32);
        assert_eq!(mutex.mutex_inner_type(), Some(&Type::I32));
        assert_eq!(guard.mutex_guard_inner_type(), Some(&Type::I32));
        assert!(
            !Type::App {
                base: "Mutex".into(),
                args: vec![Type::I32]
            }
            .is_mutex()
        );
        assert!(
            !Type::App {
                base: MUTEX_GUARD_TYPE_CONSTRUCTOR.into(),
                args: vec![Type::I32, Type::I32],
            }
            .is_mutex_guard()
        );
    }

    #[test]
    fn spsc_endpoints_are_distinct_canonical_single_argument_applications() {
        let sender = Type::spsc_sender(Type::String);
        let receiver = Type::spsc_receiver(Type::String);
        assert_eq!(sender.spsc_sender_inner_type(), Some(&Type::String));
        assert_eq!(receiver.spsc_receiver_inner_type(), Some(&Type::String));
        assert!(!sender.is_spsc_receiver());
        assert!(!receiver.is_spsc_sender());
        assert!(
            !Type::App {
                base: "Sender".into(),
                args: vec![Type::String],
            }
            .is_spsc_sender()
        );
    }

    #[test]
    fn borrowed_callable_kinds_preserve_their_signature_and_serde_identity() {
        for kind in [BorrowedCallableKind::Fn, BorrowedCallableKind::FnMut] {
            let callable = Type::BorrowedFunction {
                kind,
                params: vec![Type::I32, Type::Bool],
                ret: Box::new(Type::String),
            };

            assert_eq!(
                callable.function_signature(),
                Some((&[Type::I32, Type::Bool][..], &Type::String))
            );
            assert_eq!(callable.borrowed_callable_kind(), Some(kind));

            let encoded = serde_json::to_string(&callable).unwrap();
            let decoded: Type = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, callable);
        }

        assert_eq!(
            Type::Function {
                params: vec![],
                ret: Box::new(Type::Void),
            }
            .borrowed_callable_kind(),
            None
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StructType {
    pub name: String,
    pub fields: Vec<(String, Type)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EnumVariant {
    pub name: String,
    pub payload: Option<Type>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EnumType {
    pub name: String,
    pub variants: Vec<EnumVariant>,
}
