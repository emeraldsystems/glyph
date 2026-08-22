use serde::{Deserialize, Serialize};

use crate::atomic::AtomicScalar;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Mutability {
    Immutable,
    Mutable,
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
    Tuple(Vec<Type>),
}

impl Type {
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
            Type::Function { params, ret } => Some((params, ret)),
            _ => None,
        }
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
