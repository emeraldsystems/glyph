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

/// Ownership action performed when a local enters a closure environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CaptureTransfer {
    /// Duplicate a trivially copyable value; the source remains usable.
    Copy,
    /// Transfer the sole owner into the environment; the source is consumed.
    Move,
}

/// One deterministic field in a compiler-generated closure environment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MirCapture {
    pub name: String,
    pub local: LocalId,
    pub ty: Type,
    pub transfer: CaptureTransfer,
}

/// Access granted to a borrowed closure environment field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BorrowKind {
    Shared,
    Mutable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BorrowCaptureSource {
    /// Store the address of an ordinary local.
    Local,
    /// Forward the reference value already stored in a reference local.
    Reborrow,
}

/// One non-owning field in a compiler-generated borrowed closure environment.
///
/// `ty` is the referent type of the source local. The environment stores its
/// address and the lifted body receives `&ty` or `&mut ty` according to
/// `borrow`. The frontend must prove that the source outlives the environment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MirBorrowCapture {
    pub name: String,
    pub local: LocalId,
    pub ty: Type,
    pub borrow: BorrowKind,
    pub source: BorrowCaptureSource,
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
    AssignIndex {
        base: LocalId,
        index: MirValue,
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
    /// Nonblocking cleanup for an internal native-thread handle local.
    ///
    /// The handle local has the private MIR representation `RawPtr<I8>`.
    /// Codegen calls `glyph_thread_detach` only while it is non-null. The
    /// surface language must expose a canonical nominal `JoinHandle<()>`
    /// instead of allowing this raw representation to be named or forged.
    DropThreadHandle(LocalId),
    /// Mandatory lexical cleanup for a private scoped-thread owner.
    ///
    /// Codegen joins every registered child before this instruction returns.
    /// A persistent runtime failure is fail-stop: compiled code must never
    /// continue and drop stack storage still borrowed by a child.
    DropThreadScope(LocalId),
    /// Mandatory callback-body cleanup: drain children while borrowed frame
    /// locals are still alive, retrying once and aborting on persistent error.
    DrainThreadScope(LocalId),
    /// Discard explicit access to a scope-owned child without detaching it.
    /// The enclosing scope remains the owner and will join the child.
    DropScopedThreadHandle(LocalId),
    Nop,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Rvalue {
    ConstInt(i64),
    ConstFloat(f64),
    ConstBool(bool),
    Move(LocalId),
    /// Load the referent of a typed reference local.
    Deref {
        base: LocalId,
        ty: Type,
    },
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
    /// Construct an owned callable around a lifted closure body.
    ///
    /// The lifted function receives capture values first, in this vector's
    /// order, followed by the callable's declared parameters. Its return type
    /// must match `signature`. The backend owns the environment layout and
    /// generates the erased invoke/drop thunks.
    MakeClosure {
        function: String,
        signature: Type,
        captures: Vec<MirCapture>,
    },
    /// Construct a non-owning, repeatable callable around a lifted body.
    ///
    /// Unlike [`Rvalue::MakeClosure`], capture fields point at source locals,
    /// the environment does not own their values, and invocation never frees
    /// or consumes the environment. Escape and loan validity are frontend
    /// invariants.
    MakeBorrowedClosure {
        function: String,
        signature: Type,
        captures: Vec<MirBorrowCapture>,
    },
    /// Consume an owned callable and invoke it once.
    CallIndirect {
        callee: LocalId,
        signature: Type,
        args: Vec<MirValue>,
    },
    /// Invoke a borrowed `Fn` without consuming its carrier.
    CallIndirectShared {
        callee: LocalId,
        signature: Type,
        args: Vec<MirValue>,
    },
    /// Invoke a borrowed `FnMut` without consuming its carrier.
    ///
    /// The frontend must prove exclusive environment access for the duration
    /// of this call.
    CallIndirectMut {
        callee: LocalId,
        signature: Type,
        args: Vec<MirValue>,
    },
    /// Transfer an owned `FnOnce() -> ()` carrier to a native thread.
    ///
    /// `out_handle` is private compiler storage with type `RawPtr<I8>`. The
    /// result is the runtime's `i32` status: zero means that `out_handle` now
    /// owns a live joinable thread, while a negative value means the runtime
    /// consumed and dropped the callable without publishing a handle.
    ThreadSpawnUnit {
        task: LocalId,
        out_handle: LocalId,
    },
    /// Transfer an owned `FnOnce() -> T` carrier to a native thread.
    ///
    /// The runtime owns an uninitialized `T` slot after a successful spawn.
    /// `result_type` fixes the compiler-generated entry and drop thunks; it
    /// must exactly match the task's concrete return type.
    ThreadSpawnResult {
        task: LocalId,
        out_handle: LocalId,
        result_type: Type,
    },
    /// Join a private unit-thread handle, returning the runtime `i32` status.
    /// The runtime nulls handle storage only after a successful join.
    ThreadJoinUnit {
        handle: LocalId,
    },
    /// Join a typed thread and move its result into `out_result` exactly once.
    ///
    /// The output local is initialized only when the returned runtime status
    /// is zero. A failed join leaves both handle and runtime result retryable.
    ThreadJoinResult {
        handle: LocalId,
        out_result: LocalId,
        result_type: Type,
    },
    /// Detach a private unit-thread handle, returning the runtime `i32` status.
    /// The runtime nulls handle storage only after a successful detach.
    ThreadDetachUnit {
        handle: LocalId,
    },
    /// Wrap private raw handle storage in the canonical, source-visible
    /// `std::thread::JoinHandle<()>` identity. The raw source is cleared.
    ThreadHandleFromRaw {
        raw: LocalId,
    },
    /// Consume a canonical unit join handle into private raw storage. The
    /// public source is cleared so its drop is a no-op.
    ThreadHandleIntoRaw {
        handle: LocalId,
    },
    /// Wrap a negative runtime status in canonical `ThreadError` storage.
    ThreadErrorFromStatus {
        status: MirValue,
    },
    /// Allocate an empty lexical thread scope into private raw storage.
    ThreadScopeCreate {
        out_scope: LocalId,
    },
    /// Join all registered children in spawn order and consume the scope.
    ThreadScopeExit {
        scope: LocalId,
    },
    /// Produce a non-owning public `Scope` view without consuming raw storage.
    ThreadScopeFromRaw {
        raw: LocalId,
    },
    /// Join every current child without consuming the owner. Scope callback
    /// bodies emit this on every exit before their borrowed local frames drop.
    ThreadScopeDrain {
        scope: LocalId,
    },
    /// Spawn a scope-owned unit child from a borrowed Fn/FnMut carrier.
    /// Neither success nor failure consumes the carrier or its environment.
    ScopedThreadSpawnUnit {
        scope: LocalId,
        task: LocalId,
        out_handle: LocalId,
    },
    /// Spawn a scope-owned typed child from a borrowed Fn/FnMut carrier.
    ScopedThreadSpawnResult {
        scope: LocalId,
        task: LocalId,
        out_handle: LocalId,
        result_type: Type,
    },
    /// Explicitly join and unregister a scope-owned unit child.
    ScopedThreadJoinUnit {
        handle: LocalId,
    },
    /// Explicitly join a typed child and transfer its result exactly once.
    ScopedThreadJoinResult {
        handle: LocalId,
        out_result: LocalId,
        result_type: Type,
    },
    /// Wrap a private child token in canonical public storage.
    ScopedThreadHandleFromRaw {
        raw: LocalId,
        result_type: Type,
    },
    /// Consume a canonical public child token into private raw storage.
    ScopedThreadHandleIntoRaw {
        handle: LocalId,
        result_type: Type,
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
    /// Allocate one immutable payload behind an atomic strong count.
    /// `value` is transferred into the new `std::sync::Arc<T>` allocation.
    ArcNew {
        value: MirValue,
        elem_type: Type,
    },
    /// Explicitly clone an Arc owner with an atomic strong-count increment.
    ArcClone {
        base: LocalId,
        elem_type: Type,
    },
    /// Borrow the immutable payload. The frontend must constrain the returned
    /// reference to the source Arc owner's lifetime and reject escaping it.
    ArcBorrow {
        base: LocalId,
        elem_type: Type,
    },
    /// Allocate a stable, exclusively locked payload owner.
    MutexNew {
        value: MirValue,
        elem_type: Type,
    },
    /// Block until the mutex is acquired and return its nonescaping guard.
    MutexLock {
        base: LocalId,
        elem_type: Type,
    },
    /// Attempt acquisition without blocking. A null guard denotes contention;
    /// runtime failures other than contention are fatal.
    MutexTryLock {
        base: LocalId,
        elem_type: Type,
    },
    /// Test the nullable guard returned by `MutexTryLock`.
    MutexGuardIsAcquired {
        guard: LocalId,
        elem_type: Type,
    },
    /// Borrow the guarded payload mutably for the guard's lexical lifetime.
    MutexGuardBorrow {
        guard: LocalId,
        elem_type: Type,
    },
    /// Allocate one fixed-capacity ring and initialize its unique endpoints.
    /// The returned value is `Sender<T>`; `out_receiver` names writable
    /// `Receiver<T>` storage (a local or `&mut Receiver<T>` parameter).
    SpscChannelNew {
        capacity: MirValue,
        out_receiver: LocalId,
        elem_type: Type,
    },
    /// Attempt to transfer `value` into the ring without blocking. The value
    /// local is always consumed. Status is 0 on success, 1 when full, and 2
    /// when disconnected; on either failure ownership is moved to
    /// `out_unsent` (a local or mutable reference).
    SpscTrySend {
        sender: LocalId,
        value: LocalId,
        out_unsent: LocalId,
        elem_type: Type,
    },
    /// Attempt to receive without blocking. Status is 0 with `out_value`
    /// initialized, 1 while empty, and 2 when the producer is disconnected.
    SpscTryRecv {
        receiver: LocalId,
        out_value: LocalId,
        elem_type: Type,
    },
    /// Initialize atomic storage before it can be shared.
    AtomicNew {
        value: MirValue,
        scalar: AtomicScalar,
    },
    /// Atomically load from addressable storage. `atomic` must name either an
    /// `Atomic<T>` local or a `Ref<Atomic<T>>`; the latter preserves identity
    /// when storage is borrowed, captured, or selected from an aggregate.
    AtomicLoad {
        atomic: LocalId,
        scalar: AtomicScalar,
        ordering: AtomicOrdering,
    },
    /// Atomically store through the same addressable target contract as
    /// [`Rvalue::AtomicLoad`]. Atomic mutation is interior mutability, so an
    /// immutable reference to the wrapper is sufficient.
    AtomicStore {
        atomic: LocalId,
        value: MirValue,
        scalar: AtomicScalar,
        ordering: AtomicOrdering,
    },
    /// Atomic read-modify-write through an addressable atomic target.
    AtomicRmw {
        atomic: LocalId,
        value: MirValue,
        scalar: AtomicScalar,
        op: AtomicRmwOp,
        ordering: AtomicOrdering,
    },
    /// Compare-exchange through an addressable atomic target. Returns the
    /// value observed before the attempt; the exchange succeeded exactly when
    /// the result equals `expected`.
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
    use super::{
        BorrowCaptureSource, BorrowKind, CaptureTransfer, LocalId, MirBorrowCapture, MirCapture,
        MirInst, MirValue, Rvalue,
    };
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
                signature: signature.clone(),
                args: vec![MirValue::Int(3), MirValue::Bool(true)],
            },
            Rvalue::MakeClosure {
                function: "main::__closure_0".into(),
                signature: signature.clone(),
                captures: vec![MirCapture {
                    name: "message".into(),
                    local: LocalId(2),
                    ty: Type::String,
                    transfer: CaptureTransfer::Move,
                }],
            },
        ];

        for rvalue in rvalues {
            let encoded = serde_json::to_string(&rvalue).unwrap();
            let decoded: Rvalue = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, rvalue);
        }
    }

    #[test]
    fn borrowed_callable_mir_round_trips_through_json() {
        let shared_signature = Type::BorrowedFunction {
            kind: crate::types::BorrowedCallableKind::Fn,
            params: vec![Type::I32],
            ret: Box::new(Type::I32),
        };
        let mutable_signature = Type::BorrowedFunction {
            kind: crate::types::BorrowedCallableKind::FnMut,
            params: vec![],
            ret: Box::new(Type::Void),
        };
        let rvalues = [
            Rvalue::MakeBorrowedClosure {
                function: "main::__closure_0".into(),
                signature: shared_signature.clone(),
                captures: vec![MirBorrowCapture {
                    name: "offset".into(),
                    local: LocalId(2),
                    ty: Type::I32,
                    borrow: BorrowKind::Shared,
                    source: BorrowCaptureSource::Local,
                }],
            },
            Rvalue::CallIndirectShared {
                callee: LocalId(4),
                signature: shared_signature,
                args: vec![MirValue::Int(3)],
            },
            Rvalue::CallIndirectMut {
                callee: LocalId(5),
                signature: mutable_signature,
                args: vec![],
            },
        ];

        for rvalue in rvalues {
            let encoded = serde_json::to_string(&rvalue).unwrap();
            let decoded: Rvalue = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, rvalue);
        }
    }

    #[test]
    fn scoped_thread_mir_round_trips_through_json() {
        let rvalues = [
            Rvalue::ThreadScopeCreate {
                out_scope: LocalId(0),
            },
            Rvalue::ThreadScopeExit { scope: LocalId(0) },
            Rvalue::ThreadScopeFromRaw { raw: LocalId(0) },
            Rvalue::ThreadScopeDrain { scope: LocalId(1) },
            Rvalue::ScopedThreadSpawnUnit {
                scope: LocalId(0),
                task: LocalId(1),
                out_handle: LocalId(2),
            },
            Rvalue::ScopedThreadSpawnResult {
                scope: LocalId(0),
                task: LocalId(1),
                out_handle: LocalId(2),
                result_type: Type::String,
            },
            Rvalue::ScopedThreadJoinUnit { handle: LocalId(2) },
            Rvalue::ScopedThreadJoinResult {
                handle: LocalId(2),
                out_result: LocalId(3),
                result_type: Type::String,
            },
            Rvalue::ScopedThreadHandleFromRaw {
                raw: LocalId(2),
                result_type: Type::String,
            },
            Rvalue::ScopedThreadHandleIntoRaw {
                handle: LocalId(4),
                result_type: Type::String,
            },
        ];
        for rvalue in rvalues {
            let encoded = serde_json::to_string(&rvalue).unwrap();
            assert_eq!(serde_json::from_str::<Rvalue>(&encoded).unwrap(), rvalue);
        }

        for inst in [
            MirInst::DropThreadScope(LocalId(0)),
            MirInst::DrainThreadScope(LocalId(1)),
            MirInst::DropScopedThreadHandle(LocalId(2)),
        ] {
            let encoded = serde_json::to_string(&inst).unwrap();
            assert_eq!(serde_json::from_str::<MirInst>(&encoded).unwrap(), inst);
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

    #[test]
    fn arc_mir_round_trips_through_json() {
        let rvalues = [
            Rvalue::ArcNew {
                value: MirValue::Local(LocalId(0)),
                elem_type: Type::String,
            },
            Rvalue::ArcClone {
                base: LocalId(1),
                elem_type: Type::String,
            },
            Rvalue::ArcBorrow {
                base: LocalId(2),
                elem_type: Type::String,
            },
        ];

        for rvalue in rvalues {
            let encoded = serde_json::to_string(&rvalue).unwrap();
            let decoded: Rvalue = serde_json::from_str(&encoded).unwrap();
            assert_eq!(decoded, rvalue);
        }
    }

    #[test]
    fn mutex_mir_round_trips_through_json() {
        let rvalues = [
            Rvalue::MutexNew {
                value: MirValue::Int(1),
                elem_type: Type::I32,
            },
            Rvalue::MutexLock {
                base: LocalId(0),
                elem_type: Type::I32,
            },
            Rvalue::MutexTryLock {
                base: LocalId(0),
                elem_type: Type::I32,
            },
            Rvalue::MutexGuardIsAcquired {
                guard: LocalId(1),
                elem_type: Type::I32,
            },
            Rvalue::MutexGuardBorrow {
                guard: LocalId(1),
                elem_type: Type::I32,
            },
        ];
        for rvalue in rvalues {
            let encoded = serde_json::to_string(&rvalue).unwrap();
            assert_eq!(serde_json::from_str::<Rvalue>(&encoded).unwrap(), rvalue);
        }
    }

    #[test]
    fn spsc_mir_round_trips_through_json() {
        let rvalues = [
            Rvalue::SpscChannelNew {
                capacity: MirValue::Int(8),
                out_receiver: LocalId(1),
                elem_type: Type::String,
            },
            Rvalue::SpscTrySend {
                sender: LocalId(0),
                value: LocalId(2),
                out_unsent: LocalId(3),
                elem_type: Type::String,
            },
            Rvalue::SpscTryRecv {
                receiver: LocalId(1),
                out_value: LocalId(4),
                elem_type: Type::String,
            },
        ];
        for rvalue in rvalues {
            let encoded = serde_json::to_string(&rvalue).unwrap();
            assert_eq!(serde_json::from_str::<Rvalue>(&encoded).unwrap(), rvalue);
        }
    }
}
