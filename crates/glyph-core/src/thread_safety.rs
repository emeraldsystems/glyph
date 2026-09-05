//! Structural `Send` and `Sync` predicates for thread-escaping values.
//!
//! These predicates are compiler policy, not user-implementable interfaces.
//! In particular, policy is selected by resolver-issued identities rather than
//! source-spellable names: declaring a user type called `Arc` or `Mutex` must
//! not acquire the standard library type's special rules.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::types::{EnumType, StructType, Type};

static NEXT_IDENTITY_SPACE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThreadSafetyRequirement {
    Send,
    Sync,
}

impl fmt::Display for ThreadSafetyRequirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Send => f.write_str("Send"),
            Self::Sync => f.write_str("Sync"),
        }
    }
}

/// One compiler rule in the chain that led to a thread-safety failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadSafetyConstraint {
    pub path: String,
    pub requirement: ThreadSafetyRequirement,
    pub rule: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadSafetyError {
    pub requirement: ThreadSafetyRequirement,
    /// Source-oriented path such as `task.state.shared` or `result::Err[0]`.
    pub path: String,
    pub reason: String,
    /// Outermost-to-innermost compiler constraints that selected the failing
    /// requirement. This makes `Arc<T>: Send` failures explain the additional
    /// `T: Sync` obligation instead of reporting only a leaf type.
    pub constraint_trace: Vec<ThreadSafetyConstraint>,
}

impl fmt::Display for ThreadSafetyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}` is not {}: {}",
            self.path, self.requirement, self.reason
        )?;
        for constraint in &self.constraint_trace {
            write!(
                f,
                "\n  required by {} at `{}` ({})",
                constraint.rule, constraint.path, constraint.requirement
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for ThreadSafetyError {}

/// Opaque identity for a compiler-resolved generic constructor.
///
/// Only a `ThreadSafetyRegistry` can issue one. The frontend should retain the
/// identity produced while resolving the canonical stdlib declaration and
/// attach it to the corresponding [`ThreadSafetyType::Application`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CanonicalConstructorId {
    identity_space: u64,
    slot: u32,
}

/// Opaque identity for a compiler-resolved nominal type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CanonicalNominalId {
    identity_space: u64,
    slot: u32,
}

/// Thread-safety rule for one canonical generic constructor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalApplicationPolicy {
    Vec,
    Map,
    Option,
    Result,
    Arc,
    Mutex,
    JoinHandle,
    Sender,
    Receiver,
    /// A compiler/runtime type that must never cross a thread boundary.
    Deny {
        arity: usize,
        reason: String,
    },
    /// An opaque compiler/runtime type whose implementation has been audited.
    Audited {
        arity: usize,
        send: bool,
        sync: bool,
        reason: String,
    },
}

/// Thread-safety rule for one canonical nominal type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NominalThreadSafetyPolicy {
    /// Evaluate the already-monomorphized fields in `struct_types`.
    StructuralStruct { definition: String },
    /// Evaluate the already-monomorphized variants in `enum_types`.
    StructuralEnum { definition: String },
    /// Evaluate a resolver-produced, fully substituted generic struct layout.
    /// These fields retain canonical identities for nested compiler-known
    /// applications such as `Arc<T>` and `Vec<T>`.
    MonomorphicStruct {
        fields: Vec<(String, ThreadSafetyType)>,
    },
    /// Evaluate a resolver-produced, fully substituted generic enum layout.
    MonomorphicEnum {
        variants: Vec<(String, Option<ThreadSafetyType>)>,
    },
    /// Runtime handles are denied unless their implementation has an explicit
    /// audited policy.
    Deny { reason: String },
    Audited {
        send: bool,
        sync: bool,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConstructorEntry {
    display_name: String,
    policy: CanonicalApplicationPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NominalEntry {
    display_name: String,
    policy: NominalThreadSafetyPolicy,
}

/// Capture-level provenance retained for an owned callable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallableCaptureProvenance {
    pub name: String,
    pub ty: ThreadSafetyType,
}

impl CallableCaptureProvenance {
    pub fn new(name: impl Into<String>, ty: ThreadSafetyType) -> Self {
        Self {
            name: name.into(),
            ty,
        }
    }
}

/// Provenance needed to decide whether an erased `FnOnce` value is `Send`.
///
/// The ordinary `Type::Function` signature intentionally does not contain an
/// environment layout. Closure analysis creates `OwnedClosure`; function-item
/// coercion creates `FunctionItem`. `Unknown` is fail-closed for values loaded
/// without compiler provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallableSendProvenance {
    FunctionItem {
        symbol: String,
    },
    OwnedClosure {
        origin: String,
        captures: Vec<CallableCaptureProvenance>,
    },
    Unknown {
        reason: String,
    },
}

/// A type plus the compiler provenance needed for special thread-safety rules.
///
/// Plain structural types remain represented by [`Type`]. Generic intrinsics,
/// runtime handles, and callables must use the resolved variants so source
/// spelling alone cannot select compiler privilege.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadSafetyType {
    Plain(Type),
    Own(Box<ThreadSafetyType>),
    Array {
        element: Box<ThreadSafetyType>,
        len: usize,
    },
    Tuple(Vec<ThreadSafetyType>),
    Application {
        constructor: CanonicalConstructorId,
        args: Vec<ThreadSafetyType>,
    },
    Nominal(CanonicalNominalId),
    Callable {
        signature: Type,
        provenance: CallableSendProvenance,
    },
}

impl ThreadSafetyType {
    pub fn plain(ty: Type) -> Self {
        Self::Plain(ty)
    }

    pub fn application(constructor: CanonicalConstructorId, args: Vec<ThreadSafetyType>) -> Self {
        Self::Application { constructor, args }
    }

    pub fn own(inner: ThreadSafetyType) -> Self {
        Self::Own(Box::new(inner))
    }

    pub fn array(element: ThreadSafetyType, len: usize) -> Self {
        Self::Array {
            element: Box::new(element),
            len,
        }
    }

    pub fn tuple(elements: Vec<ThreadSafetyType>) -> Self {
        Self::Tuple(elements)
    }

    pub fn nominal(identity: CanonicalNominalId) -> Self {
        Self::Nominal(identity)
    }

    pub fn callable(signature: Type, provenance: CallableSendProvenance) -> Self {
        Self::Callable {
            signature,
            provenance,
        }
    }
}

impl From<Type> for ThreadSafetyType {
    fn from(value: Type) -> Self {
        Self::Plain(value)
    }
}

/// Compatibility/input adapter for thread-safety checks.
///
/// Passing a bare [`Type`] remains supported for ordinary structural types,
/// but fails closed for applications and callables that need provenance.
pub trait ThreadSafetyCheckInput {
    fn to_thread_safety_type(&self) -> ThreadSafetyType;
}

impl ThreadSafetyCheckInput for Type {
    fn to_thread_safety_type(&self) -> ThreadSafetyType {
        ThreadSafetyType::Plain(self.clone())
    }
}

impl ThreadSafetyCheckInput for ThreadSafetyType {
    fn to_thread_safety_type(&self) -> ThreadSafetyType {
        self.clone()
    }
}

/// Side table used by source and MIR lowering to keep callable environment
/// provenance attached while the erased three-pointer callable value moves.
///
/// `K` may be a resolver value id, MIR local id, aggregate slot id, or a
/// dedicated returned-value id. Moves remove the source entry, preventing a
/// stale source local from later being accepted as thread-safe.
#[derive(Debug, Clone)]
pub struct CallableProvenanceTable<K> {
    values: HashMap<K, CallableSendProvenance>,
}

impl<K> Default for CallableProvenanceTable<K> {
    fn default() -> Self {
        Self {
            values: HashMap::new(),
        }
    }
}

impl<K: Eq + Hash> CallableProvenanceTable<K> {
    pub fn insert(&mut self, place: K, provenance: CallableSendProvenance) {
        self.values.insert(place, provenance);
    }

    pub fn get(&self, place: &K) -> Option<&CallableSendProvenance> {
        self.values.get(place)
    }

    pub fn move_to(&mut self, source: &K, destination: K) -> bool {
        let Some(provenance) = self.values.remove(source) else {
            return false;
        };
        self.values.insert(destination, provenance);
        true
    }

    pub fn store(&mut self, source: &K, storage: K) -> bool {
        self.move_to(source, storage)
    }

    pub fn take_returned(&mut self, source: &K) -> Option<CallableSendProvenance> {
        self.values.remove(source)
    }

    pub fn restore_returned(&mut self, destination: K, provenance: CallableSendProvenance) {
        self.values.insert(destination, provenance);
    }
}

/// Read-only type definitions plus compiler-issued thread-safety policies.
pub struct ThreadSafetyRegistry<'a> {
    structs: &'a HashMap<String, StructType>,
    enums: &'a HashMap<String, EnumType>,
    identity_space: u64,
    constructors: Vec<ConstructorEntry>,
    nominals: Vec<NominalEntry>,
}

impl<'a> ThreadSafetyRegistry<'a> {
    pub fn new(
        structs: &'a HashMap<String, StructType>,
        enums: &'a HashMap<String, EnumType>,
    ) -> Self {
        Self {
            structs,
            enums,
            identity_space: NEXT_IDENTITY_SPACE.fetch_add(1, Ordering::Relaxed),
            constructors: Vec::new(),
            nominals: Vec::new(),
        }
    }

    /// Register a constructor only after resolver provenance confirms the
    /// canonical declaration. The returned identity, not `display_name`, is
    /// what selects the policy during checking.
    pub fn register_constructor(
        &mut self,
        display_name: impl Into<String>,
        policy: CanonicalApplicationPolicy,
    ) -> CanonicalConstructorId {
        let slot = self.constructors.len() as u32;
        self.constructors.push(ConstructorEntry {
            display_name: display_name.into(),
            policy,
        });
        CanonicalConstructorId {
            identity_space: self.identity_space,
            slot,
        }
    }

    /// Register a nominal identity after name resolution and, for structural
    /// generic aggregates, after all type parameters have been substituted.
    pub fn register_nominal(
        &mut self,
        display_name: impl Into<String>,
        policy: NominalThreadSafetyPolicy,
    ) -> CanonicalNominalId {
        let slot = self.nominals.len() as u32;
        self.nominals.push(NominalEntry {
            display_name: display_name.into(),
            policy,
        });
        CanonicalNominalId {
            identity_space: self.identity_space,
            slot,
        }
    }

    pub fn check_send<T: ThreadSafetyCheckInput + ?Sized>(
        &self,
        root: &str,
        ty: &T,
    ) -> Result<(), ThreadSafetyError> {
        let ty = ty.to_thread_safety_type();
        self.check(
            &ty,
            ThreadSafetyRequirement::Send,
            root,
            &mut HashSet::new(),
            &mut Vec::new(),
        )
    }

    pub fn check_sync<T: ThreadSafetyCheckInput + ?Sized>(
        &self,
        root: &str,
        ty: &T,
    ) -> Result<(), ThreadSafetyError> {
        let ty = ty.to_thread_safety_type();
        self.check(
            &ty,
            ThreadSafetyRequirement::Sync,
            root,
            &mut HashSet::new(),
            &mut Vec::new(),
        )
    }

    /// Compatibility hook for closure analysis while it still owns the
    /// concrete capture list. New lowering should attach
    /// [`CallableSendProvenance`] to the value and call `check_send` instead.
    pub fn check_callable_send<'b, I>(&self, captures: I) -> Result<(), ThreadSafetyError>
    where
        I: IntoIterator<Item = (&'b str, &'b Type)>,
    {
        for (name, ty) in captures {
            self.check_send(&format!("capture `{name}`"), ty)?;
        }
        Ok(())
    }

    /// Source/MIR integration gate for `spawn`.
    ///
    /// The emission callback is invoked only after the task environment and
    /// result type pass `Send`. GLYPH-42 should route its runtime MIR emission
    /// through this hook so a rejected source expression cannot leave a
    /// `glyph_thread_spawn` call in partially built MIR.
    pub fn validate_spawn_then<T, F>(
        &self,
        task: &ThreadSafetyType,
        result: &ThreadSafetyType,
        emit_runtime_mir: F,
    ) -> Result<T, ThreadSafetyError>
    where
        F: FnOnce() -> T,
    {
        self.check_send("spawn task", task)?;
        self.check_send("spawn result", result)?;
        Ok(emit_runtime_mir())
    }

    fn check(
        &self,
        ty: &ThreadSafetyType,
        requirement: ThreadSafetyRequirement,
        path: &str,
        visiting: &mut HashSet<(String, ThreadSafetyRequirement)>,
        trace: &mut Vec<ThreadSafetyConstraint>,
    ) -> Result<(), ThreadSafetyError> {
        match ty {
            ThreadSafetyType::Plain(ty) => self.check_plain(ty, requirement, path, visiting, trace),
            ThreadSafetyType::Own(inner) => self.check(inner, requirement, path, visiting, trace),
            ThreadSafetyType::Array { element, .. } => {
                self.check(element, requirement, &format!("{path}[]"), visiting, trace)
            }
            ThreadSafetyType::Tuple(elements) => {
                for (index, element) in elements.iter().enumerate() {
                    self.check(
                        element,
                        requirement,
                        &format!("{path}[{index}]"),
                        visiting,
                        trace,
                    )?;
                }
                Ok(())
            }
            ThreadSafetyType::Application { constructor, args } => {
                self.check_application(*constructor, args, requirement, path, visiting, trace)
            }
            ThreadSafetyType::Nominal(identity) => {
                self.check_nominal(*identity, requirement, path, visiting, trace)
            }
            ThreadSafetyType::Callable {
                signature,
                provenance,
            } => self.check_callable(signature, provenance, requirement, path, visiting, trace),
        }
    }

    fn check_plain(
        &self,
        ty: &Type,
        requirement: ThreadSafetyRequirement,
        path: &str,
        visiting: &mut HashSet<(String, ThreadSafetyRequirement)>,
        trace: &mut Vec<ThreadSafetyConstraint>,
    ) -> Result<(), ThreadSafetyError> {
        match ty {
            Type::I8
            | Type::I16
            | Type::I32
            | Type::I64
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::U64
            | Type::Usize
            | Type::F32
            | Type::F64
            | Type::Bool
            | Type::Char
            | Type::String
            | Type::Void
            | Type::Atomic(_) => Ok(()),

            Type::Str => self.reject(
                requirement,
                path,
                "borrowed `str` may not escape to a thread",
                trace,
            ),
            Type::Ref(..) => self.reject(
                requirement,
                path,
                "borrowed references may not escape to a thread",
                trace,
            ),
            Type::RawPtr(..) => self.reject(
                requirement,
                path,
                "raw pointers have no compiler-verifiable ownership protocol",
                trace,
            ),
            Type::Shared(..) => self.reject(
                requirement,
                path,
                "`Shared<T>` is not synchronized and cannot cross threads",
                trace,
            ),

            Type::Own(inner) => self.check_plain(inner, requirement, path, visiting, trace),
            Type::Array(inner, _) => self.check_plain(
                inner,
                requirement,
                &format!("{path}[]"),
                visiting,
                trace,
            ),
            Type::Tuple(elements) => {
                for (index, element) in elements.iter().enumerate() {
                    self.check_plain(
                        element,
                        requirement,
                        &format!("{path}[{index}]"),
                        visiting,
                        trace,
                    )?;
                }
                Ok(())
            }
            Type::Named(name) => {
                self.check_struct_definition(name, requirement, path, visiting, trace)
            }
            Type::Enum(name) => {
                self.check_enum_definition(name, requirement, path, visiting, trace)
            }
            Type::Param(name) => self.reject(
                requirement,
                path,
                &format!(
                    "generic parameter `{name}` was not resolved before thread-safety checking"
                ),
                trace,
            ),
            Type::Function { .. } => self.reject(
                requirement,
                path,
                "callable environment provenance was lost before thread-safety checking",
                trace,
            ),
            Type::BorrowedFunction { .. } => self.reject(
                requirement,
                path,
                "borrowed callable environments cannot escape to an unscoped thread",
                trace,
            ),
            Type::App { base, .. } => self.reject(
                requirement,
                path,
                &format!(
                    "generic type `{base}` has no canonical resolver identity; source spelling cannot select compiler thread-safety policy"
                ),
                trace,
            ),
        }
    }

    fn check_struct_definition(
        &self,
        name: &str,
        requirement: ThreadSafetyRequirement,
        path: &str,
        visiting: &mut HashSet<(String, ThreadSafetyRequirement)>,
        trace: &mut Vec<ThreadSafetyConstraint>,
    ) -> Result<(), ThreadSafetyError> {
        let Some(definition) = self.structs.get(name) else {
            return self.reject(
                requirement,
                path,
                &format!("type `{name}` has no structural thread-safety definition"),
                trace,
            );
        };
        let key = (format!("struct:{name}"), requirement);
        if !visiting.insert(key.clone()) {
            return Ok(());
        }
        let result = (|| {
            for (field, ty) in &definition.fields {
                self.check_plain(ty, requirement, &format!("{path}.{field}"), visiting, trace)?;
            }
            Ok(())
        })();
        visiting.remove(&key);
        result
    }

    fn check_enum_definition(
        &self,
        name: &str,
        requirement: ThreadSafetyRequirement,
        path: &str,
        visiting: &mut HashSet<(String, ThreadSafetyRequirement)>,
        trace: &mut Vec<ThreadSafetyConstraint>,
    ) -> Result<(), ThreadSafetyError> {
        let Some(definition) = self.enums.get(name) else {
            return self.reject(
                requirement,
                path,
                &format!("enum `{name}` has no structural thread-safety definition"),
                trace,
            );
        };
        let key = (format!("enum:{name}"), requirement);
        if !visiting.insert(key.clone()) {
            return Ok(());
        }
        let result = (|| {
            for variant in &definition.variants {
                if let Some(payload) = &variant.payload {
                    self.check_plain(
                        payload,
                        requirement,
                        &format!("{path}::{}", variant.name),
                        visiting,
                        trace,
                    )?;
                }
            }
            Ok(())
        })();
        visiting.remove(&key);
        result
    }

    fn check_nominal(
        &self,
        identity: CanonicalNominalId,
        requirement: ThreadSafetyRequirement,
        path: &str,
        visiting: &mut HashSet<(String, ThreadSafetyRequirement)>,
        trace: &mut Vec<ThreadSafetyConstraint>,
    ) -> Result<(), ThreadSafetyError> {
        let entry = match self.nominal_entry(identity) {
            Some(entry) => entry,
            None => {
                return self.reject(
                    requirement,
                    path,
                    "nominal type identity did not originate from this compilation registry",
                    trace,
                );
            }
        };
        match &entry.policy {
            NominalThreadSafetyPolicy::StructuralStruct { definition } => self.with_constraint(
                path,
                requirement,
                format!("structural fields of `{}`", entry.display_name),
                trace,
                |trace| {
                    self.check_struct_definition(definition, requirement, path, visiting, trace)
                },
            ),
            NominalThreadSafetyPolicy::StructuralEnum { definition } => self.with_constraint(
                path,
                requirement,
                format!("structural variants of `{}`", entry.display_name),
                trace,
                |trace| self.check_enum_definition(definition, requirement, path, visiting, trace),
            ),
            NominalThreadSafetyPolicy::MonomorphicStruct { fields } => self.with_constraint(
                path,
                requirement,
                format!("monomorphic structural fields of `{}`", entry.display_name),
                trace,
                |trace| {
                    for (field, ty) in fields {
                        self.check(ty, requirement, &format!("{path}.{field}"), visiting, trace)?;
                    }
                    Ok(())
                },
            ),
            NominalThreadSafetyPolicy::MonomorphicEnum { variants } => self.with_constraint(
                path,
                requirement,
                format!(
                    "monomorphic structural variants of `{}`",
                    entry.display_name
                ),
                trace,
                |trace| {
                    for (variant, payload) in variants {
                        if let Some(payload) = payload {
                            self.check(
                                payload,
                                requirement,
                                &format!("{path}::{variant}"),
                                visiting,
                                trace,
                            )?;
                        }
                    }
                    Ok(())
                },
            ),
            NominalThreadSafetyPolicy::Deny { reason } => {
                self.reject(requirement, path, reason, trace)
            }
            NominalThreadSafetyPolicy::Audited { send, sync, reason } => {
                let allowed = match requirement {
                    ThreadSafetyRequirement::Send => *send,
                    ThreadSafetyRequirement::Sync => *sync,
                };
                if allowed {
                    Ok(())
                } else {
                    self.reject(requirement, path, reason, trace)
                }
            }
        }
    }

    fn check_callable(
        &self,
        signature: &Type,
        provenance: &CallableSendProvenance,
        requirement: ThreadSafetyRequirement,
        path: &str,
        visiting: &mut HashSet<(String, ThreadSafetyRequirement)>,
        trace: &mut Vec<ThreadSafetyConstraint>,
    ) -> Result<(), ThreadSafetyError> {
        if !matches!(signature, Type::Function { .. }) {
            return self.reject(
                requirement,
                path,
                "callable provenance was attached to a non-callable type",
                trace,
            );
        }
        if requirement == ThreadSafetyRequirement::Sync {
            return self.reject(
                requirement,
                path,
                "owned `FnOnce` values have one exclusive owner and are not `Sync`",
                trace,
            );
        }
        match provenance {
            CallableSendProvenance::FunctionItem { .. } => Ok(()),
            CallableSendProvenance::Unknown { reason } => self.reject(
                requirement,
                path,
                &format!("callable environment provenance is unknown: {reason}"),
                trace,
            ),
            CallableSendProvenance::OwnedClosure { origin, captures } => {
                for capture in captures {
                    let capture_path = format!("{path}.capture `{}`", capture.name);
                    self.with_constraint(
                        &capture_path,
                        ThreadSafetyRequirement::Send,
                        format!("owned closure `{origin}` requires every capture to be Send"),
                        trace,
                        |trace| {
                            self.check(
                                &capture.ty,
                                ThreadSafetyRequirement::Send,
                                &capture_path,
                                visiting,
                                trace,
                            )
                        },
                    )?;
                }
                Ok(())
            }
        }
    }

    fn check_application(
        &self,
        identity: CanonicalConstructorId,
        args: &[ThreadSafetyType],
        requirement: ThreadSafetyRequirement,
        path: &str,
        visiting: &mut HashSet<(String, ThreadSafetyRequirement)>,
        trace: &mut Vec<ThreadSafetyConstraint>,
    ) -> Result<(), ThreadSafetyError> {
        use ThreadSafetyRequirement::{Send, Sync};

        let entry =
            match self.constructor_entry(identity) {
                Some(entry) => entry,
                None => return self.reject(
                    requirement,
                    path,
                    "generic constructor identity did not originate from this compilation registry",
                    trace,
                ),
            };
        let expected_arity = match &entry.policy {
            CanonicalApplicationPolicy::Map | CanonicalApplicationPolicy::Result => 2,
            CanonicalApplicationPolicy::Deny { arity, .. }
            | CanonicalApplicationPolicy::Audited { arity, .. } => *arity,
            _ => 1,
        };
        if args.len() != expected_arity {
            return self.reject(
                requirement,
                path,
                &format!(
                    "canonical `{}` expected {expected_arity} type arguments, found {}",
                    entry.display_name,
                    args.len()
                ),
                trace,
            );
        }

        let check_arg = |this: &Self,
                         index: usize,
                         arg_requirement: ThreadSafetyRequirement,
                         suffix: &str,
                         rule: String,
                         visiting: &mut HashSet<(String, ThreadSafetyRequirement)>,
                         trace: &mut Vec<ThreadSafetyConstraint>| {
            let arg_path = format!("{path}{suffix}");
            this.with_constraint(&arg_path, arg_requirement, rule, trace, |trace| {
                this.check(&args[index], arg_requirement, &arg_path, visiting, trace)
            })
        };

        match &entry.policy {
            CanonicalApplicationPolicy::Vec | CanonicalApplicationPolicy::Option => check_arg(
                self,
                0,
                requirement,
                ".value",
                format!("`{}<T>` requires `T: {requirement}`", entry.display_name),
                visiting,
                trace,
            ),
            CanonicalApplicationPolicy::Map => {
                check_arg(
                    self,
                    0,
                    requirement,
                    ".key",
                    format!("`{}<K, V>` requires `K: {requirement}`", entry.display_name),
                    visiting,
                    trace,
                )?;
                check_arg(
                    self,
                    1,
                    requirement,
                    ".value",
                    format!("`{}<K, V>` requires `V: {requirement}`", entry.display_name),
                    visiting,
                    trace,
                )
            }
            CanonicalApplicationPolicy::Result => {
                check_arg(
                    self,
                    0,
                    requirement,
                    "::Ok",
                    format!("`{}<T, E>` requires `T: {requirement}`", entry.display_name),
                    visiting,
                    trace,
                )?;
                check_arg(
                    self,
                    1,
                    requirement,
                    "::Err",
                    format!("`{}<T, E>` requires `E: {requirement}`", entry.display_name),
                    visiting,
                    trace,
                )
            }
            CanonicalApplicationPolicy::Arc => {
                check_arg(
                    self,
                    0,
                    Send,
                    ".value",
                    format!("`{}<T>` requires `T: Send + Sync`", entry.display_name),
                    visiting,
                    trace,
                )?;
                check_arg(
                    self,
                    0,
                    Sync,
                    ".value",
                    format!("`{}<T>` requires `T: Send + Sync`", entry.display_name),
                    visiting,
                    trace,
                )
            }
            CanonicalApplicationPolicy::Mutex => check_arg(
                self,
                0,
                Send,
                ".value",
                format!("`{}<T>` requires `T: Send`", entry.display_name),
                visiting,
                trace,
            ),
            CanonicalApplicationPolicy::JoinHandle => {
                if requirement == Sync {
                    return self.reject(
                        requirement,
                        path,
                        "join handles have one exclusive owner",
                        trace,
                    );
                }
                check_arg(
                    self,
                    0,
                    Send,
                    ".result",
                    format!("`{}<T>` requires `T: Send`", entry.display_name),
                    visiting,
                    trace,
                )
            }
            CanonicalApplicationPolicy::Sender | CanonicalApplicationPolicy::Receiver => {
                if requirement == Sync {
                    return self.reject(
                        requirement,
                        path,
                        "SPSC endpoints require one exclusive owner",
                        trace,
                    );
                }
                check_arg(
                    self,
                    0,
                    Send,
                    ".item",
                    format!("`{}<T>` requires `T: Send`", entry.display_name),
                    visiting,
                    trace,
                )
            }
            CanonicalApplicationPolicy::Deny { reason, .. } => {
                self.reject(requirement, path, reason, trace)
            }
            CanonicalApplicationPolicy::Audited {
                send, sync, reason, ..
            } => {
                let allowed = match requirement {
                    Send => *send,
                    Sync => *sync,
                };
                if allowed {
                    Ok(())
                } else {
                    self.reject(requirement, path, reason, trace)
                }
            }
        }
    }

    fn constructor_entry(&self, id: CanonicalConstructorId) -> Option<&ConstructorEntry> {
        (id.identity_space == self.identity_space)
            .then(|| self.constructors.get(id.slot as usize))
            .flatten()
    }

    fn nominal_entry(&self, id: CanonicalNominalId) -> Option<&NominalEntry> {
        (id.identity_space == self.identity_space)
            .then(|| self.nominals.get(id.slot as usize))
            .flatten()
    }

    fn with_constraint<T, F>(
        &self,
        path: &str,
        requirement: ThreadSafetyRequirement,
        rule: String,
        trace: &mut Vec<ThreadSafetyConstraint>,
        check: F,
    ) -> Result<T, ThreadSafetyError>
    where
        F: FnOnce(&mut Vec<ThreadSafetyConstraint>) -> Result<T, ThreadSafetyError>,
    {
        trace.push(ThreadSafetyConstraint {
            path: path.to_string(),
            requirement,
            rule,
        });
        let result = check(trace);
        trace.pop();
        result
    }

    fn reject<T>(
        &self,
        requirement: ThreadSafetyRequirement,
        path: &str,
        reason: &str,
        trace: &[ThreadSafetyConstraint],
    ) -> Result<T, ThreadSafetyError> {
        Err(ThreadSafetyError {
            requirement,
            path: path.to_string(),
            reason: reason.to_string(),
            constraint_trace: trace.to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atomic::AtomicScalar;
    use crate::types::EnumVariant;

    fn function_type() -> Type {
        Type::Function {
            params: vec![],
            ret: Box::new(Type::Void),
        }
    }

    fn registry<'a>(
        structs: &'a HashMap<String, StructType>,
        enums: &'a HashMap<String, EnumType>,
    ) -> ThreadSafetyRegistry<'a> {
        ThreadSafetyRegistry::new(structs, enums)
    }

    #[test]
    fn scalars_strings_and_atomics_are_send_and_sync() {
        let structs = HashMap::new();
        let enums = HashMap::new();
        let registry = registry(&structs, &enums);
        for ty in [Type::I32, Type::String, Type::Atomic(AtomicScalar::Usize)] {
            let ty = ThreadSafetyType::plain(ty);
            assert!(registry.check_send("value", &ty).is_ok());
            assert!(registry.check_sync("value", &ty).is_ok());
        }
    }

    #[test]
    fn fake_arc_and_mutex_names_do_not_acquire_canonical_policy() {
        let structs = HashMap::new();
        let enums = HashMap::new();
        let mut registry = registry(&structs, &enums);
        let arc = registry.register_constructor("std::sync::Arc", CanonicalApplicationPolicy::Arc);
        let mutex =
            registry.register_constructor("std::sync::Mutex", CanonicalApplicationPolicy::Mutex);

        for base in ["Arc", "Mutex"] {
            let fake = ThreadSafetyType::plain(Type::App {
                base: base.into(),
                args: vec![Type::I32],
            });
            let error = registry.check_send("fake", &fake).unwrap_err();
            assert!(error.reason.contains("no canonical resolver identity"));
        }

        let canonical_arc =
            ThreadSafetyType::application(arc, vec![ThreadSafetyType::plain(Type::I32)]);
        let canonical_mutex =
            ThreadSafetyType::application(mutex, vec![ThreadSafetyType::plain(Type::String)]);
        assert!(registry.check_send("arc", &canonical_arc).is_ok());
        assert!(registry.check_sync("mutex", &canonical_mutex).is_ok());

        let mut other_registry = ThreadSafetyRegistry::new(&structs, &enums);
        let foreign_arc =
            other_registry.register_constructor("std::sync::Arc", CanonicalApplicationPolicy::Arc);
        let forged_across_compilations =
            ThreadSafetyType::application(foreign_arc, vec![ThreadSafetyType::plain(Type::I32)]);
        let error = registry
            .check_send("foreign", &forged_across_compilations)
            .unwrap_err();
        assert!(error.reason.contains("did not originate"));
    }

    #[test]
    fn monomorphic_generic_aggregates_check_substituted_fields() {
        let structs = HashMap::new();
        let enums = HashMap::new();
        let mut registry = registry(&structs, &enums);
        let vec = registry.register_constructor("std::vec::Vec", CanonicalApplicationPolicy::Vec);
        let good = registry.register_nominal(
            "Wrapper<Vec<i32>>",
            NominalThreadSafetyPolicy::MonomorphicStruct {
                fields: vec![(
                    "value".into(),
                    ThreadSafetyType::application(vec, vec![ThreadSafetyType::plain(Type::I32)]),
                )],
            },
        );
        let bad = registry.register_nominal(
            "Wrapper<Vec<Shared<i32>>>",
            NominalThreadSafetyPolicy::MonomorphicStruct {
                fields: vec![(
                    "value".into(),
                    ThreadSafetyType::application(
                        vec,
                        vec![ThreadSafetyType::plain(Type::Shared(Box::new(Type::I32)))],
                    ),
                )],
            },
        );

        assert!(
            registry
                .check_send("wrapper", &ThreadSafetyType::nominal(good))
                .is_ok()
        );
        let error = registry
            .check_send("wrapper", &ThreadSafetyType::nominal(bad))
            .unwrap_err();
        assert_eq!(error.path, "wrapper.value.value");
        assert!(
            error.constraint_trace[0]
                .rule
                .contains("Wrapper<Vec<Shared<i32>>>")
        );
        assert!(error.constraint_trace[1].rule.contains("Vec<T>"));
    }

    #[test]
    fn nominal_runtime_handles_are_denied_unless_explicitly_audited() {
        let structs = HashMap::new();
        let enums = HashMap::new();
        let mut registry = registry(&structs, &enums);
        let unsafe_handle = registry.register_nominal(
            "AudioDeviceHandle",
            NominalThreadSafetyPolicy::Deny {
                reason: "audio device handles are bound to their creating thread".into(),
            },
        );
        let audited = registry.register_nominal(
            "ThreadCompletionHandle",
            NominalThreadSafetyPolicy::Audited {
                send: true,
                sync: false,
                reason: "completion handles have one exclusive owner".into(),
            },
        );

        let error = registry
            .check_send("device", &ThreadSafetyType::nominal(unsafe_handle))
            .unwrap_err();
        assert!(error.reason.contains("creating thread"));
        assert!(
            registry
                .check_send("handle", &ThreadSafetyType::nominal(audited))
                .is_ok()
        );
        assert!(
            registry
                .check_sync("handle", &ThreadSafetyType::nominal(audited))
                .is_err()
        );
    }

    #[test]
    fn arc_and_mutex_failures_include_constraint_traces() {
        let structs = HashMap::new();
        let enums = HashMap::new();
        let mut registry = registry(&structs, &enums);
        let arc = registry.register_constructor("std::sync::Arc", CanonicalApplicationPolicy::Arc);
        let mutex =
            registry.register_constructor("std::sync::Mutex", CanonicalApplicationPolicy::Mutex);
        let handle = registry.register_constructor(
            "std::thread::JoinHandle",
            CanonicalApplicationPolicy::JoinHandle,
        );

        let arc_handle = ThreadSafetyType::application(
            arc,
            vec![ThreadSafetyType::application(
                handle,
                vec![ThreadSafetyType::plain(Type::I32)],
            )],
        );
        let arc_error = registry.check_send("arc", &arc_handle).unwrap_err();
        assert_eq!(arc_error.requirement, ThreadSafetyRequirement::Sync);
        assert_eq!(arc_error.path, "arc.value");
        assert_eq!(arc_error.constraint_trace.len(), 1);
        assert!(arc_error.constraint_trace[0].rule.contains("Send + Sync"));

        let mutex_raw = ThreadSafetyType::application(
            mutex,
            vec![ThreadSafetyType::plain(Type::RawPtr(Box::new(Type::I32)))],
        );
        let mutex_error = registry.check_sync("mutex", &mutex_raw).unwrap_err();
        assert_eq!(mutex_error.requirement, ThreadSafetyRequirement::Send);
        assert_eq!(mutex_error.path, "mutex.value");
        assert!(mutex_error.constraint_trace[0].rule.contains("T: Send"));
    }

    #[test]
    fn callable_send_uses_capture_provenance_and_fails_closed_without_it() {
        let structs = HashMap::new();
        let enums = HashMap::new();
        let registry = registry(&structs, &enums);
        let safe = ThreadSafetyType::callable(
            function_type(),
            CallableSendProvenance::OwnedClosure {
                origin: "render task".into(),
                captures: vec![CallableCaptureProvenance::new(
                    "samples",
                    ThreadSafetyType::plain(Type::String),
                )],
            },
        );
        assert!(registry.check_send("task", &safe).is_ok());

        let unsafe_callable = ThreadSafetyType::callable(
            function_type(),
            CallableSendProvenance::OwnedClosure {
                origin: "render task".into(),
                captures: vec![CallableCaptureProvenance::new(
                    "view",
                    ThreadSafetyType::plain(Type::Str),
                )],
            },
        );
        let error = registry.check_send("task", &unsafe_callable).unwrap_err();
        assert_eq!(error.path, "task.capture `view`");
        assert!(
            error.constraint_trace[0]
                .rule
                .contains("every capture to be Send")
        );

        let unknown = ThreadSafetyType::callable(
            function_type(),
            CallableSendProvenance::Unknown {
                reason: "loaded from an untracked slot".into(),
            },
        );
        assert!(registry.check_send("task", &unknown).is_err());
    }

    #[test]
    fn callable_provenance_survives_moves_storage_and_returns() {
        let provenance = CallableSendProvenance::FunctionItem {
            symbol: "render".into(),
        };
        let mut table = CallableProvenanceTable::<u32>::default();
        table.insert(0, provenance.clone());
        assert!(table.move_to(&0, 1));
        assert!(table.get(&0).is_none());
        assert!(table.store(&1, 2));
        let returned = table.take_returned(&2).unwrap();
        assert_eq!(returned, provenance);
        table.restore_returned(3, returned);
        assert_eq!(table.get(&3), Some(&provenance));
    }

    #[test]
    fn rejected_spawn_never_emits_runtime_mir() {
        let structs = HashMap::new();
        let enums = HashMap::new();
        let registry = registry(&structs, &enums);
        let task = ThreadSafetyType::callable(
            function_type(),
            CallableSendProvenance::OwnedClosure {
                origin: "task".into(),
                captures: vec![CallableCaptureProvenance::new(
                    "raw",
                    ThreadSafetyType::plain(Type::RawPtr(Box::new(Type::I32))),
                )],
            },
        );
        let mut emitted = 0;
        let result =
            registry
                .validate_spawn_then(&task, &ThreadSafetyType::plain(Type::Void), || emitted += 1);
        assert!(result.is_err());
        assert_eq!(emitted, 0);

        let function_item = ThreadSafetyType::callable(
            function_type(),
            CallableSendProvenance::FunctionItem {
                symbol: "task".into(),
            },
        );
        let result = registry.validate_spawn_then(
            &function_item,
            &ThreadSafetyType::plain(Type::Shared(Box::new(Type::I32))),
            || emitted += 1,
        );
        assert!(result.is_err());
        assert_eq!(emitted, 0);
    }

    #[test]
    fn nested_struct_and_enum_report_exact_paths() {
        let structs = HashMap::from([
            (
                "Inner".into(),
                StructType {
                    name: "Inner".into(),
                    fields: vec![("raw".into(), Type::RawPtr(Box::new(Type::I32)))],
                },
            ),
            (
                "Outer".into(),
                StructType {
                    name: "Outer".into(),
                    fields: vec![("inner".into(), Type::Named("Inner".into()))],
                },
            ),
        ]);
        let enums = HashMap::from([(
            "Message".into(),
            EnumType {
                name: "Message".into(),
                variants: vec![EnumVariant {
                    name: "Data".into(),
                    payload: Some(Type::Tuple(vec![
                        Type::I32,
                        Type::Shared(Box::new(Type::I32)),
                    ])),
                }],
            },
        )]);
        let registry = registry(&structs, &enums);
        assert_eq!(
            registry
                .check_send(
                    "capture `state`",
                    &ThreadSafetyType::plain(Type::Named("Outer".into())),
                )
                .unwrap_err()
                .path,
            "capture `state`.inner.raw"
        );
        assert_eq!(
            registry
                .check_send(
                    "message",
                    &ThreadSafetyType::plain(Type::Enum("Message".into())),
                )
                .unwrap_err()
                .path,
            "message::Data[1]"
        );
    }

    #[test]
    fn recursive_owned_types_terminate_without_hiding_bad_edges() {
        let structs = HashMap::from([
            (
                "Node".into(),
                StructType {
                    name: "Node".into(),
                    fields: vec![(
                        "next".into(),
                        Type::Own(Box::new(Type::Named("Node".into()))),
                    )],
                },
            ),
            (
                "BadNode".into(),
                StructType {
                    name: "BadNode".into(),
                    fields: vec![
                        (
                            "next".into(),
                            Type::Own(Box::new(Type::Named("BadNode".into()))),
                        ),
                        ("shared".into(), Type::Shared(Box::new(Type::I32))),
                    ],
                },
            ),
        ]);
        let enums = HashMap::new();
        let registry = registry(&structs, &enums);
        assert!(
            registry
                .check_send("node", &ThreadSafetyType::plain(Type::Named("Node".into())),)
                .is_ok()
        );
        assert_eq!(
            registry
                .check_send(
                    "node",
                    &ThreadSafetyType::plain(Type::Named("BadNode".into())),
                )
                .unwrap_err()
                .path,
            "node.shared"
        );
    }
}
