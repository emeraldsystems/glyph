use super::ast::BinaryOp;
use super::atomic::{AtomicOrdering, AtomicRmwOp, AtomicScalar};
use super::types::{EnumType, Mutability, StructType, Type};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MirFunction {
    pub name: String,
    pub ret_type: Option<Type>,
    pub params: Vec<LocalId>,
    pub locals: Vec<Local>,
    pub blocks: Vec<MirBlock>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Local {
    pub name: Option<String>,
    pub ty: Option<Type>,
    pub mutable: bool,
    #[serde(default)]
    pub skip_drop: bool,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MirModule {
    pub struct_types: HashMap<String, StructType>,
    pub enum_types: HashMap<String, EnumType>,
    pub functions: Vec<MirFunction>,
    pub extern_functions: Vec<MirExternFunction>,
}

impl std::fmt::Debug for MirModule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use std::collections::BTreeMap;

        let mut ds = f.debug_struct("MirModule");
        let struct_types: BTreeMap<_, _> = self.struct_types.iter().collect();
        ds.field("struct_types", &struct_types);
        if !self.enum_types.is_empty() {
            let enum_types: BTreeMap<_, _> = self.enum_types.iter().collect();
            ds.field("enum_types", &enum_types);
        }
        ds.field("functions", &self.functions);
        if !self.extern_functions.is_empty() {
            ds.field("extern_functions", &self.extern_functions);
        }
        ds.finish()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MirExternFunction {
    pub name: String,
    pub ret_type: Option<Type>,
    pub params: Vec<Type>,
    pub abi: Option<String>,
    pub link_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MirBlock {
    pub insts: Vec<MirInst>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlockId(pub u32);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MirInst {
    Assign {
        local: LocalId,
        value: Rvalue,
    },
    AssignField {
        base: LocalId,
        field_name: String,
        field_index: u32,
        value: Rvalue,
    },
    Return(Option<MirValue>),
    Goto(BlockId),
    If {
        cond: MirValue,
        then_bb: BlockId,
        else_bb: BlockId,
    },
    Drop(LocalId),
    Nop,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Rvalue {
    ConstInt(i64),
    ConstFloat(f64),
    ConstBool(bool),
    Move(LocalId),
    StringLit {
        content: String,
        global_name: String,
    },
    Binary {
        op: BinaryOp,
        lhs: MirValue,
        rhs: MirValue,
    },
    Cast {
        value: MirValue,
        from: Type,
        to: Type,
    },
    Call {
        name: String,
        args: Vec<MirValue>,
    },
    /// Construct a non-capturing callable value for a named function.
    ///
    /// The explicit signature keeps serialized MIR self-describing and lets
    /// the backend validate/reconstruct the erased indirect-call ABI.
    FunctionRef {
        name: String,
        signature: Type,
    },
    /// Consume an owned callable and invoke it once.
    CallIndirect {
        callee: LocalId,
        signature: Type,
        args: Vec<MirValue>,
    },
    StructLit {
        struct_name: String,
        field_values: Vec<(String, MirValue)>,
    },
    FieldAccess {
        base: LocalId,
        field_name: String,
        field_index: u32,
    },
    FieldRef {
        base: LocalId,
        field_name: String,
        field_index: u32,
        mutability: Mutability,
    },
    Ref {
        base: LocalId,
        mutability: Mutability,
    },
    ArrayLit {
        elem_type: Type,
        elements: Vec<MirValue>,
    },
    ArrayIndex {
        base: LocalId,
        index: MirValue,
        bounds_check: bool,
    },
    ArrayLen {
        base: LocalId,
    },
    VecNew {
        elem_type: Type,
    },
    VecWithCapacity {
        elem_type: Type,
        capacity: MirValue,
    },
    VecPush {
        vec: LocalId,
        elem_type: Type,
        value: MirValue,
    },
    VecPop {
        vec: LocalId,
        elem_type: Type,
    },
    VecLen {
        vec: LocalId,
    },
    VecIndex {
        vec: LocalId,
        elem_type: Type,
        index: MirValue,
        bounds_check: bool,
    },
    VecIndexRef {
        vec: LocalId,
        elem_type: Type,
        index: MirValue,
        bounds_check: bool,
        mutability: Mutability,
    },
    MapNew {
        key_type: Type,
        value_type: Type,
    },
    MapWithCapacity {
        key_type: Type,
        value_type: Type,
        capacity: MirValue,
    },
    MapAdd {
        map: LocalId,
        key_type: Type,
        key: MirValue,
        value_type: Type,
        value: MirValue,
    },
    MapUpdate {
        map: LocalId,
        key_type: Type,
        key: MirValue,
        value_type: Type,
        value: MirValue,
    },
    MapDel {
        map: LocalId,
        key_type: Type,
        value_type: Type,
        key: MirValue,
    },
    MapGet {
        map: LocalId,
        key_type: Type,
        value_type: Type,
        key: MirValue,
    },
    MapHas {
        map: LocalId,
        key_type: Type,
        key: MirValue,
    },
    MapKeys {
        map: LocalId,
        key_type: Type,
        value_type: Type,
    },
    MapVals {
        map: LocalId,
        key_type: Type,
        value_type: Type,
    },
    FileOpen {
        path: MirValue,
        create: bool,
    },
    FileReadToString {
        file: LocalId,
    },
    FileWriteString {
        file: LocalId,
        contents: MirValue,
    },
    FileClose {
        file: LocalId,
    },
    StringLen {
        base: LocalId,
    },
    StringConcat {
        base: LocalId,
        value: MirValue,
    },
    StringSlice {
        base: LocalId,
        start: MirValue,
        len: MirValue,
    },
    StringTrim {
        base: LocalId,
    },
    StringSplit {
        base: LocalId,
        sep: MirValue,
    },
    StringStartsWith {
        base: LocalId,
        needle: MirValue,
    },
    StringEndsWith {
        base: LocalId,
        needle: MirValue,
    },
    StringClone {
        base: LocalId,
    },
    OwnNew {
        value: MirValue,
        elem_type: Type,
    },
    OwnIntoRaw {
        base: LocalId,
        elem_type: Type,
    },
    OwnFromRaw {
        ptr: MirValue,
        elem_type: Type,
    },
    RawPtrNull {
        elem_type: Type,
    },
    SharedNew {
        value: MirValue,
        elem_type: Type,
    },
    SharedClone {
        base: LocalId,
        elem_type: Type,
    },
    /// Initialize atomic storage before it can be shared.
    AtomicNew {
        value: MirValue,
        scalar: AtomicScalar,
    },
    AtomicLoad {
        atomic: LocalId,
        scalar: AtomicScalar,
        ordering: AtomicOrdering,
    },
    AtomicStore {
        atomic: LocalId,
        value: MirValue,
        scalar: AtomicScalar,
        ordering: AtomicOrdering,
    },
    AtomicRmw {
        atomic: LocalId,
        value: MirValue,
        scalar: AtomicScalar,
        op: AtomicRmwOp,
        ordering: AtomicOrdering,
    },
    /// Returns the value observed before the compare-exchange attempt. The
    /// exchange succeeded exactly when the result equals `expected`.
    AtomicCompareExchange {
        atomic: LocalId,
        expected: MirValue,
        desired: MirValue,
        scalar: AtomicScalar,
        success: AtomicOrdering,
        failure: AtomicOrdering,
    },
    AtomicFence {
        ordering: AtomicOrdering,
    },
    AtomicIsLockFree {
        scalar: AtomicScalar,
    },
    EnumConstruct {
        enum_name: String,
        variant_index: u32,
        payload: Option<MirValue>,
    },
    EnumTag {
        base: LocalId,
    },
    EnumPayload {
        base: LocalId,
        variant_index: u32,
        payload_type: Type,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LocalId(pub u32);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MirValue {
    Unit,
    Int(i64),
    Float(f64),
    Bool(bool),
    Local(LocalId),
}

#[cfg(test)]
mod tests {
    use super::{LocalId, MirValue, Rvalue};
    use crate::atomic::{AtomicOrdering, AtomicRmwOp, AtomicScalar};
    use crate::types::Type;

    #[test]
    fn callable_mir_round_trips_through_json() {
        let signature = Type::Function {
            params: vec![Type::I32, Type::Bool],
            ret: Box::new(Type::String),
        };
        let rvalues = [
            Rvalue::FunctionRef {
                name: "format_value".into(),
                signature: signature.clone(),
            },
            Rvalue::CallIndirect {
                callee: LocalId(4),
                signature,
                args: vec![MirValue::Int(3), MirValue::Bool(true)],
            },
        ];

        for rvalue in rvalues {
            let encoded = serde_json::to_string(&rvalue).unwrap();
            let decoded: Rvalue = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, rvalue);
        }
    }

    #[test]
    fn atomic_mir_round_trips_through_json() {
        let rvalues = [
            Rvalue::AtomicLoad {
                atomic: LocalId(0),
                scalar: AtomicScalar::Usize,
                ordering: AtomicOrdering::Acquire,
            },
            Rvalue::AtomicRmw {
                atomic: LocalId(0),
                value: MirValue::Int(1),
                scalar: AtomicScalar::Usize,
                op: AtomicRmwOp::Add,
                ordering: AtomicOrdering::Release,
            },
            Rvalue::AtomicCompareExchange {
                atomic: LocalId(0),
                expected: MirValue::Int(1),
                desired: MirValue::Int(2),
                scalar: AtomicScalar::Usize,
                success: AtomicOrdering::AcqRel,
                failure: AtomicOrdering::Acquire,
            },
        ];

        for rvalue in rvalues {
            let encoded = serde_json::to_string(&rvalue).unwrap();
            let decoded: Rvalue = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, rvalue);
        }
    }
}
