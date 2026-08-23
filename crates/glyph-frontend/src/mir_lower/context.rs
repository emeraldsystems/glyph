use std::collections::{HashMap, HashSet};

use glyph_core::ast::{Item, Module};
use glyph_core::diag::Diagnostic;
use glyph_core::mir::{BlockId, BorrowKind, Local, LocalId, MirBlock, MirInst, MirValue, Rvalue};
use glyph_core::span::Span;
use glyph_core::thread_safety::{
    CallableCaptureProvenance, CallableProvenanceTable, CallableSendProvenance,
    CanonicalApplicationPolicy, CanonicalConstructorId, CanonicalNominalId,
    NominalThreadSafetyPolicy, ThreadSafetyRegistry, ThreadSafetyType,
};
use glyph_core::types::Type;

use crate::closure_analysis::{ClosureEscapeKind, ClosureInfo, analyze_function_closure_ownership};
use crate::resolver::{ResolvedSymbol, ResolverContext};

use super::signatures::FnSig;

#[derive(Debug, Clone)]
pub(crate) struct LoopContext {
    pub(crate) continue_target: BlockId,
    pub(crate) exit: BlockId, // for break
    // Scope stack depth when entering the loop body.
    // Locals in deeper scopes must be dropped on break/continue.
    pub(crate) scope_depth: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LocalState {
    Uninitialized,
    Initialized,
    Moved,
}

/// A source-level immutable reference produced by `Arc::borrow()` remains
/// tied to the exact owner local that created it. Glyph does not yet have a
/// general lifetime solver, so v1 keeps these loans lexical and refuses to
/// store them in longer-lived aggregates or containers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ArcLoan {
    owner: LocalId,
    creation_scope: usize,
    origin: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MutexLoan {
    owner: LocalId,
    creation_scope: usize,
    origin: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MutexGuardBorrow {
    guard: LocalId,
    creation_scope: usize,
    origin: Span,
}

/// A conservative, whole-local loan held by a lexical MIR local.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LexicalLoan {
    owner: LocalId,
    kind: BorrowKind,
    creation_scope: usize,
    origin: Span,
}

pub(crate) struct LowerCtx<'a> {
    pub(crate) resolver: &'a ResolverContext,
    pub(crate) module: &'a Module,
    pub(crate) fn_sigs: &'a HashMap<String, FnSig>,
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) locals: Vec<Local>,
    pub(crate) bindings: HashMap<String, LocalId>,
    pub(crate) next_local: u32,
    pub(crate) function_name: String,
    pub(crate) fn_ret_type: Option<Type>,
    pub(crate) blocks: Vec<MirBlock>,
    pub(crate) current: BlockId,
    pub(crate) loop_stack: Vec<LoopContext>,
    pub(crate) scope_stack: Vec<Vec<LocalId>>,
    /// Previous lexical bindings to restore when the corresponding local
    /// scope exits. MIR locals remain function-wide, but source names must
    /// still obey block shadowing.
    binding_undo: Vec<Vec<(String, Option<LocalId>)>>,
    pub(crate) local_states: Vec<LocalState>,
    local_scope_depths: Vec<usize>,
    implicit_deref_locals: HashSet<LocalId>,
    lexical_loans: HashMap<LocalId, Vec<LexicalLoan>>,
    scoped_loan_reservations: HashMap<LocalId, Vec<LexicalLoan>>,
    active_scoped_fnmut_tasks: HashMap<LocalId, HashSet<LocalId>>,
    known_borrowed_callables: HashSet<LocalId>,
    arc_loans: HashMap<LocalId, ArcLoan>,
    mutex_loans: HashMap<LocalId, MutexLoan>,
    mutex_guard_borrows: HashMap<LocalId, MutexGuardBorrow>,
    mutex_try_option_locals: HashSet<LocalId>,
    pub(crate) string_counter: u32,
    /// Source declaration identities are stable across closure analysis and
    /// MIR lowering even when the same spelling is shadowed.
    pub(crate) source_binding_locals: HashMap<(String, u32, u32), LocalId>,
    pub(crate) closure_infos: Vec<ClosureInfo>,
    pub(crate) lifted_functions: Vec<glyph_core::mir::MirFunction>,
    pub(crate) closure_name_root: String,
    pub(crate) callable_provenance: CallableProvenanceTable<LocalId>,
    pub(crate) callable_return_provenance: HashMap<String, CallableSendProvenance>,
    pub(crate) thread_safety_registry: ThreadSafetyRegistry<'a>,
    runtime_nominals: HashMap<String, CanonicalNominalId>,
    structural_nominals: HashMap<String, CanonicalNominalId>,
    join_handle_constructor: CanonicalConstructorId,
    arc_constructor: CanonicalConstructorId,
    mutex_constructor: CanonicalConstructorId,
    mutex_guard_constructor: CanonicalConstructorId,
    spsc_sender_constructor: CanonicalConstructorId,
    spsc_receiver_constructor: CanonicalConstructorId,
    /// Private raw thread slots need nonblocking detach cleanup even though
    /// ordinary RawPtr values have no drop glue.
    thread_handle_locals: std::collections::HashSet<LocalId>,
    /// Canonical Scope parameters owned by a compiler-generated scope callback.
    /// They are drained before every lexical cleanup boundary.
    scoped_callback_scopes: Vec<LocalId>,
    /// Private and public scoped child tokens need scope-aware drop behavior.
    scoped_thread_handle_locals: HashSet<LocalId>,
}

impl<'a> LowerCtx<'a> {
    pub(crate) fn new(
        resolver: &'a ResolverContext,
        module: &'a Module,
        fn_sigs: &'a HashMap<String, FnSig>,
        function_name: String,
    ) -> Self {
        let mut blocks = Vec::new();
        blocks.push(MirBlock::default());
        let mut thread_safety_registry =
            ThreadSafetyRegistry::new(&resolver.struct_types, &resolver.enum_types);
        let join_handle_constructor = thread_safety_registry.register_constructor(
            "std::thread::JoinHandle",
            CanonicalApplicationPolicy::JoinHandle,
        );
        let arc_constructor = thread_safety_registry.register_constructor(
            glyph_core::types::ARC_TYPE_CONSTRUCTOR,
            CanonicalApplicationPolicy::Arc,
        );
        let mutex_constructor = thread_safety_registry.register_constructor(
            glyph_core::types::MUTEX_TYPE_CONSTRUCTOR,
            CanonicalApplicationPolicy::Mutex,
        );
        let mutex_guard_constructor = thread_safety_registry.register_constructor(
            glyph_core::types::MUTEX_GUARD_TYPE_CONSTRUCTOR,
            CanonicalApplicationPolicy::Deny {
                arity: 1,
                reason: "mutex guards are lexical, thread-affine lock tokens".into(),
            },
        );
        let spsc_sender_constructor = thread_safety_registry.register_constructor(
            glyph_core::types::SPSC_SENDER_TYPE_CONSTRUCTOR,
            CanonicalApplicationPolicy::Sender,
        );
        let spsc_receiver_constructor = thread_safety_registry.register_constructor(
            glyph_core::types::SPSC_RECEIVER_TYPE_CONSTRUCTOR,
            CanonicalApplicationPolicy::Receiver,
        );
        let mut runtime_nominals = HashMap::new();
        for (name, send, sync, reason) in [
            (
                "std::io::Stdout",
                true,
                true,
                "stdout formatting is reentrant",
            ),
            (
                "std::io::File",
                true,
                false,
                "file streams require exclusive ownership",
            ),
            (
                "std::net::TcpStream",
                true,
                false,
                "socket state requires exclusive ownership",
            ),
            (
                "std::net::TcpListener",
                true,
                false,
                "listener state requires exclusive ownership",
            ),
            (
                "std::net::UdpSocket",
                true,
                false,
                "socket state requires exclusive ownership",
            ),
            (
                "std::term::Terminal",
                false,
                false,
                "terminal sessions are thread-affine",
            ),
            (
                "std::term::UiSessionGuard",
                false,
                false,
                "terminal guards are thread-affine",
            ),
            (
                "std::audio::WavWriter",
                true,
                false,
                "WAV writers require exclusive ownership",
            ),
            (
                "std::audio::AudioOut",
                false,
                false,
                "live audio devices are engine-thread-affine",
            ),
        ] {
            let id = thread_safety_registry.register_nominal(
                name,
                NominalThreadSafetyPolicy::Audited {
                    send,
                    sync,
                    reason: reason.into(),
                },
            );
            runtime_nominals.insert(name.into(), id);
        }

        let mut context = Self {
            resolver,
            module,
            fn_sigs,
            diagnostics: Vec::new(),
            locals: Vec::new(),
            bindings: HashMap::new(),
            next_local: 0,
            function_name: function_name.clone(),
            fn_ret_type: None,
            blocks,
            current: BlockId(0),
            loop_stack: Vec::new(),
            scope_stack: vec![Vec::new()],
            binding_undo: vec![Vec::new()],
            local_states: Vec::new(),
            local_scope_depths: Vec::new(),
            implicit_deref_locals: HashSet::new(),
            lexical_loans: HashMap::new(),
            scoped_loan_reservations: HashMap::new(),
            active_scoped_fnmut_tasks: HashMap::new(),
            known_borrowed_callables: HashSet::new(),
            arc_loans: HashMap::new(),
            mutex_loans: HashMap::new(),
            mutex_guard_borrows: HashMap::new(),
            mutex_try_option_locals: HashSet::new(),
            string_counter: 0,
            source_binding_locals: HashMap::new(),
            closure_infos: Vec::new(),
            lifted_functions: Vec::new(),
            closure_name_root: function_name,
            callable_provenance: CallableProvenanceTable::default(),
            callable_return_provenance: HashMap::new(),
            thread_safety_registry,
            runtime_nominals,
            structural_nominals: HashMap::new(),
            join_handle_constructor,
            arc_constructor,
            mutex_constructor,
            mutex_guard_constructor,
            spsc_sender_constructor,
            spsc_receiver_constructor,
            thread_handle_locals: std::collections::HashSet::new(),
            scoped_callback_scopes: Vec::new(),
            scoped_thread_handle_locals: HashSet::new(),
        };

        // Preserve a certificate across ordinary calls returning one known
        // closure. Multiple alternative returned closures and unknown capture
        // layouts remain fail-closed rather than guessing.
        for item in &module.items {
            let Item::Function(function) = item else {
                continue;
            };
            let analysis = analyze_function_closure_ownership(function, resolver);
            let returned = analysis
                .closures
                .iter()
                .filter(|closure| {
                    closure
                        .escapes
                        .iter()
                        .any(|escape| escape.kind == ClosureEscapeKind::Return)
                })
                .collect::<Vec<_>>();
            if returned.len() != 1 {
                continue;
            }
            let closure = returned[0];
            let provenance = if closure
                .captures
                .iter()
                .all(|capture| capture.resolved_type.is_some())
            {
                CallableSendProvenance::OwnedClosure {
                    origin: format!(
                        "{}::__glyph_closure_{}",
                        function.name.0, closure.closure_id
                    ),
                    captures: closure
                        .captures
                        .iter()
                        .map(|capture| {
                            CallableCaptureProvenance::new(
                                capture.name.clone(),
                                context.thread_safety_type(
                                    capture.resolved_type.as_ref().expect("checked above"),
                                ),
                            )
                        })
                        .collect(),
                }
            } else {
                CallableSendProvenance::Unknown {
                    reason: format!(
                        "returned closure from '{}' has an unresolved capture layout",
                        function.name.0
                    ),
                }
            };
            context
                .callable_return_provenance
                .insert(function.name.0.clone(), provenance);
        }
        context
    }

    pub(crate) fn thread_safety_type(&mut self, ty: &Type) -> ThreadSafetyType {
        self.thread_safety_type_inner(ty, &mut HashSet::new())
    }

    fn thread_safety_type_inner(
        &mut self,
        ty: &Type,
        visiting: &mut HashSet<String>,
    ) -> ThreadSafetyType {
        match ty {
            Type::Own(inner) => {
                ThreadSafetyType::own(self.thread_safety_type_inner(inner, visiting))
            }
            Type::Array(inner, len) => {
                ThreadSafetyType::array(self.thread_safety_type_inner(inner, visiting), *len)
            }
            Type::Tuple(elements) => ThreadSafetyType::tuple(
                elements
                    .iter()
                    .map(|element| self.thread_safety_type_inner(element, visiting))
                    .collect(),
            ),
            Type::Named(name) => {
                if let Some(identity) = self.runtime_nominal_identity(name) {
                    return ThreadSafetyType::nominal(identity);
                }
                if let Some(identity) = self.structural_nominals.get(name).copied() {
                    return ThreadSafetyType::nominal(identity);
                }
                if !visiting.insert(name.clone()) {
                    return ThreadSafetyType::plain(ty.clone());
                }
                let fields = self
                    .resolver
                    .get_struct(name)
                    .map(|definition| definition.fields.clone());
                let converted = fields.map(|fields| {
                    fields
                        .into_iter()
                        .map(|(field, field_ty)| {
                            (field, self.thread_safety_type_inner(&field_ty, visiting))
                        })
                        .collect()
                });
                visiting.remove(name);
                if let Some(fields) = converted {
                    let identity = self.thread_safety_registry.register_nominal(
                        name.clone(),
                        NominalThreadSafetyPolicy::MonomorphicStruct { fields },
                    );
                    self.structural_nominals.insert(name.clone(), identity);
                    ThreadSafetyType::nominal(identity)
                } else {
                    ThreadSafetyType::plain(ty.clone())
                }
            }
            Type::Enum(name) => {
                if let Some(identity) = self.structural_nominals.get(name).copied() {
                    return ThreadSafetyType::nominal(identity);
                }
                if !visiting.insert(name.clone()) {
                    return ThreadSafetyType::plain(ty.clone());
                }
                let variants = self.resolver.get_enum(name).map(|definition| {
                    definition
                        .variants
                        .iter()
                        .map(|variant| (variant.name.clone(), variant.payload.clone()))
                        .collect::<Vec<_>>()
                });
                let converted = variants.map(|variants| {
                    variants
                        .into_iter()
                        .map(|(variant, payload)| {
                            (
                                variant,
                                payload.map(|payload| {
                                    self.thread_safety_type_inner(&payload, visiting)
                                }),
                            )
                        })
                        .collect()
                });
                visiting.remove(name);
                if let Some(variants) = converted {
                    let identity = self.thread_safety_registry.register_nominal(
                        name.clone(),
                        NominalThreadSafetyPolicy::MonomorphicEnum { variants },
                    );
                    self.structural_nominals.insert(name.clone(), identity);
                    ThreadSafetyType::nominal(identity)
                } else {
                    ThreadSafetyType::plain(ty.clone())
                }
            }
            Type::App { base, args }
                if args.len() == 1 && self.resolves_to_struct(base, "std/sync", "Arc") =>
            {
                ThreadSafetyType::application(
                    self.arc_constructor,
                    vec![self.thread_safety_type_inner(&args[0], visiting)],
                )
            }
            Type::App { base, args }
                if args.len() == 1 && self.resolves_to_struct(base, "std/sync", "Mutex") =>
            {
                ThreadSafetyType::application(
                    self.mutex_constructor,
                    vec![self.thread_safety_type_inner(&args[0], visiting)],
                )
            }
            Type::App { base, args }
                if args.len() == 1 && self.resolves_to_struct(base, "std/sync", "MutexGuard") =>
            {
                ThreadSafetyType::application(
                    self.mutex_guard_constructor,
                    vec![self.thread_safety_type_inner(&args[0], visiting)],
                )
            }
            Type::App { base, args }
                if args.len() == 1 && self.resolves_to_struct(base, "std/thread", "JoinHandle") =>
            {
                ThreadSafetyType::application(
                    self.join_handle_constructor,
                    vec![self.thread_safety_type_inner(&args[0], visiting)],
                )
            }
            Type::App { base, args }
                if args.len() == 1 && self.resolves_to_struct(base, "std/sync/spsc", "Sender") =>
            {
                ThreadSafetyType::application(
                    self.spsc_sender_constructor,
                    vec![self.thread_safety_type_inner(&args[0], visiting)],
                )
            }
            Type::App { base, args }
                if args.len() == 1
                    && self.resolves_to_struct(base, "std/sync/spsc", "Receiver") =>
            {
                ThreadSafetyType::application(
                    self.spsc_receiver_constructor,
                    vec![self.thread_safety_type_inner(&args[0], visiting)],
                )
            }
            ty if ty.is_arc() => ThreadSafetyType::application(
                self.arc_constructor,
                vec![self.thread_safety_type_inner(
                    ty.arc_inner_type().expect("is_arc validated one argument"),
                    visiting,
                )],
            ),
            ty if ty.is_mutex() => ThreadSafetyType::application(
                self.mutex_constructor,
                vec![
                    self.thread_safety_type_inner(
                        ty.mutex_inner_type()
                            .expect("is_mutex validated one argument"),
                        visiting,
                    ),
                ],
            ),
            ty if ty.is_mutex_guard() => ThreadSafetyType::application(
                self.mutex_guard_constructor,
                vec![
                    self.thread_safety_type_inner(
                        ty.mutex_guard_inner_type()
                            .expect("is_mutex_guard validated one argument"),
                        visiting,
                    ),
                ],
            ),
            ty if ty.is_spsc_sender() => ThreadSafetyType::application(
                self.spsc_sender_constructor,
                vec![
                    self.thread_safety_type_inner(
                        ty.spsc_sender_inner_type()
                            .expect("SPSC sender validated one argument"),
                        visiting,
                    ),
                ],
            ),
            ty if ty.is_spsc_receiver() => ThreadSafetyType::application(
                self.spsc_receiver_constructor,
                vec![
                    self.thread_safety_type_inner(
                        ty.spsc_receiver_inner_type()
                            .expect("SPSC receiver validated one argument"),
                        visiting,
                    ),
                ],
            ),
            ty if glyph_core::thread::is_canonical_thread_handle(ty) => {
                ThreadSafetyType::application(
                    self.join_handle_constructor,
                    vec![
                        self.thread_safety_type_inner(
                            glyph_core::thread::canonical_thread_handle_result(ty)
                                .expect("canonical handle has one result argument"),
                            visiting,
                        ),
                    ],
                )
            }
            _ => ThreadSafetyType::plain(ty.clone()),
        }
    }

    fn runtime_nominal_identity(&self, name: &str) -> Option<CanonicalNominalId> {
        self.runtime_nominals.get(name).copied().or_else(|| {
            let ResolvedSymbol::Struct(module, symbol) = self.resolver.resolve_symbol(name)? else {
                return None;
            };
            let canonical = format!("{}::{symbol}", module.replace('/', "::"));
            self.runtime_nominals.get(&canonical).copied()
        })
    }

    fn resolves_to_struct(&self, name: &str, module: &str, symbol: &str) -> bool {
        if name == format!("{}::{symbol}", module.replace('/', "::")) {
            return true;
        }
        matches!(
            self.resolver.resolve_symbol(name),
            Some(ResolvedSymbol::Struct(resolved_module, resolved_symbol))
                if resolved_module == module && resolved_symbol == symbol
        )
    }

    pub(crate) fn fresh_thread_handle_local(&mut self) -> LocalId {
        let local = self.fresh_local(None);
        self.locals[local.0 as usize].ty = Some(glyph_core::thread::private_unit_handle_type());
        self.thread_handle_locals.insert(local);
        local
    }

    pub(crate) fn fresh_scoped_thread_handle_local(&mut self, result: Type) -> LocalId {
        let local = self.fresh_local(None);
        self.locals[local.0 as usize].ty = Some(
            glyph_core::thread::private_scoped_thread_handle_type(result),
        );
        self.scoped_thread_handle_locals.insert(local);
        local
    }

    pub(crate) fn mark_scoped_thread_handle(&mut self, local: LocalId) {
        self.scoped_thread_handle_locals.insert(local);
    }

    pub(crate) fn register_scoped_callback_scope(&mut self, local: LocalId) {
        if !self.scoped_callback_scopes.contains(&local) {
            self.scoped_callback_scopes.push(local);
        }
    }

    fn drain_scoped_callback_scopes(&mut self) {
        let scopes = self.scoped_callback_scopes.clone();
        for scope in scopes {
            self.scoped_loan_reservations.remove(&scope);
            self.active_scoped_fnmut_tasks.remove(&scope);
            let inst = MirInst::DrainThreadScope(scope);
            let insert_before_terminator =
                self.current_block_mut().insts.last().is_some_and(|inst| {
                    matches!(
                        inst,
                        MirInst::Return(_) | MirInst::Goto(_) | MirInst::If { .. }
                    )
                });
            if insert_before_terminator {
                let index = self.current_block_mut().insts.len() - 1;
                self.current_block_mut().insts.insert(index, inst);
            } else {
                self.current_block_mut().insts.push(inst);
            }
        }
    }

    pub(crate) fn bind_name(&mut self, name: &str, local: LocalId) {
        let previous = self.bindings.insert(name.to_string(), local);
        self.binding_undo
            .last_mut()
            .expect("MIR lowering always has a lexical scope")
            .push((name.to_string(), previous));
    }

    pub(crate) fn register_source_binding(
        &mut self,
        name: &str,
        declaration_span: Span,
        local: LocalId,
    ) {
        self.source_binding_locals.insert(
            (
                name.to_string(),
                declaration_span.start,
                declaration_span.end,
            ),
            local,
        );
    }

    pub(crate) fn source_binding_local(
        &self,
        name: &str,
        declaration_span: Span,
    ) -> Option<LocalId> {
        self.source_binding_locals
            .get(&(
                name.to_string(),
                declaration_span.start,
                declaration_span.end,
            ))
            .copied()
    }

    pub(crate) fn error(&mut self, message: impl Into<String>, span: Option<Span>) {
        self.diagnostics.push(Diagnostic::error(message, span));
    }

    pub(crate) fn current_block_mut(&mut self) -> &mut MirBlock {
        &mut self.blocks[self.current.0 as usize]
    }

    pub(crate) fn push_inst(&mut self, inst: MirInst) {
        match &inst {
            MirInst::Assign { local, value } => {
                self.reject_arc_loan_storage(value);
                self.reject_lexical_loan_storage(value);
                self.reject_mutex_guard_storage(*local, value);
                self.arc_loans.remove(local);
                self.release_lexical_loans_for_holder(*local);
                match value {
                    Rvalue::Ref { base, mutability } => {
                        let kind = if *mutability == glyph_core::types::Mutability::Mutable {
                            BorrowKind::Mutable
                        } else {
                            BorrowKind::Shared
                        };
                        self.register_lexical_loan(*local, *base, kind, Span::new(0, 0));
                    }
                    Rvalue::MakeBorrowedClosure { captures, .. } => {
                        self.known_borrowed_callables.insert(*local);
                        for capture in captures {
                            self.register_lexical_loan(
                                *local,
                                capture.local,
                                capture.borrow,
                                Span::new(0, 0),
                            );
                        }
                    }
                    Rvalue::FunctionRef {
                        name, signature, ..
                    } => {
                        if matches!(signature, Type::BorrowedFunction { .. }) {
                            self.known_borrowed_callables.insert(*local);
                        }
                        self.callable_provenance.insert(
                            *local,
                            CallableSendProvenance::FunctionItem {
                                symbol: name.clone(),
                            },
                        )
                    }
                    Rvalue::MakeClosure {
                        function, captures, ..
                    } => {
                        let captures = captures
                            .iter()
                            .map(|capture| {
                                CallableCaptureProvenance::new(
                                    capture.name.clone(),
                                    self.thread_safety_type(&capture.ty),
                                )
                            })
                            .collect();
                        self.callable_provenance.insert(
                            *local,
                            CallableSendProvenance::OwnedClosure {
                                origin: function.clone(),
                                captures,
                            },
                        );
                    }
                    Rvalue::Move(source) => {
                        if matches!(
                            self.local_ty(*source),
                            Some(Type::BorrowedFunction {
                                kind: glyph_core::types::BorrowedCallableKind::FnMut,
                                ..
                            })
                        ) {
                            self.error(
                                "an FnMut callable cannot be assigned or aliased; pass or call it directly",
                                None,
                            );
                        }
                        self.callable_provenance.move_to(source, *local);
                        if let Some(loan) = self.arc_loans.get(source).copied() {
                            let destination_scope = self
                                .local_scope_depths
                                .get(local.0 as usize)
                                .copied()
                                .unwrap_or(loan.creation_scope);
                            if destination_scope < loan.creation_scope {
                                self.error(
                                    "an Arc::borrow() reference cannot escape the lexical scope where it was created",
                                    Some(loan.origin),
                                );
                            }
                            // Keep propagating after a diagnostic so the
                            // owner's later drop is also rejected and the
                            // invalid MIR cannot accidentally look safe.
                            self.arc_loans.insert(*local, loan);
                        }
                        self.propagate_lexical_loans(*source, *local);
                    }
                    _ => {}
                }
                self.handle_reassign(*local);
                match value {
                    Rvalue::Move(source) => self.transfer_mutex_provenance(*source, *local),
                    Rvalue::EnumPayload {
                        base, payload_type, ..
                    } if payload_type.is_mutex_guard() => {
                        self.transfer_mutex_provenance(*base, *local);
                    }
                    _ => {}
                }
                if let Some(state) = self.local_states.get_mut(local.0 as usize) {
                    *state = LocalState::Initialized;
                }
                if let Rvalue::Move(src) = value {
                    // If source was already Moved (e.g., VecIndex snapshot
                    // marked non-owning), propagate to dest so the shallow
                    // copy doesn't get independently dropped.
                    //
                    // Only propagate for types where consume_local doesn't
                    // track moves (Named/App types). For String/Own/Shared,
                    // the Moved state came from consume_local (normal
                    // ownership transfer), not a non-owning marker.
                    let src_was_moved = matches!(
                        self.local_states.get(src.0 as usize),
                        Some(LocalState::Moved)
                    );
                    if let Some(state) = self.local_states.get_mut(src.0 as usize) {
                        *state = LocalState::Moved;
                    }
                    if src_was_moved && !self.local_uses_ownership_tracking(*src) {
                        if let Some(state) = self.local_states.get_mut(local.0 as usize) {
                            *state = LocalState::Moved;
                        }
                    }
                    // Propagate skip_drop: if the source is a non-owning
                    // alias (e.g. the injected argv local that is a shallow
                    // copy of the global), any move destination must also
                    // skip its drop to avoid freeing the global's memory.
                    if self
                        .locals
                        .get(src.0 as usize)
                        .map_or(false, |l| l.skip_drop)
                    {
                        if let Some(dest) = self.locals.get_mut(local.0 as usize) {
                            dest.skip_drop = true;
                        }
                    }
                }
                // Track ownership transfers for types with drop glue.
                // Operations that consume values must mark the source as
                // Moved to prevent double-free at scope exit.
                self.track_rvalue_ownership(value, *local);
            }
            MirInst::AssignField { value, .. } => {
                self.reject_mutex_guard_field_storage(value);
                self.reject_lexical_loan_field_storage(value);
                if let Rvalue::Move(source) = value
                    && let Some(loan) = self.arc_loans.get(source).copied()
                {
                    self.error(
                        "an Arc::borrow() reference cannot be stored in a struct field; keep it in a direct lexical binding",
                        Some(loan.origin),
                    );
                }
                if let Rvalue::Move(src) = value {
                    if let Some(state) = self.local_states.get_mut(src.0 as usize) {
                        *state = LocalState::Moved;
                    }
                }
            }
            MirInst::AssignIndex { value, .. } => {
                // The value moves into the container: mark the source Moved so
                // scope exit doesn't drop it a second time (the container's
                // own drop now owns it).
                self.reject_mutex_guard_field_storage(value);
                self.reject_lexical_loan_field_storage(value);
                if let Rvalue::Move(src) = value {
                    if let Some(state) = self.local_states.get_mut(src.0 as usize) {
                        *state = LocalState::Moved;
                    }
                }
            }
            _ => {}
        }
        self.current_block_mut().insts.push(inst);
    }

    pub(crate) fn terminated(&self) -> bool {
        self.blocks[self.current.0 as usize]
            .insts
            .last()
            .map(|inst| {
                matches!(
                    inst,
                    MirInst::Return(_) | MirInst::Goto(_) | MirInst::If { .. }
                )
            })
            .unwrap_or(false)
    }

    pub(crate) fn fresh_local(&mut self, name: Option<&'a str>) -> LocalId {
        let id = LocalId(self.next_local);
        self.next_local += 1;
        self.locals.push(Local {
            name: name.map(|s| s.to_string()),
            ty: None,
            mutable: false,
            skip_drop: false,
        });
        self.local_states.push(LocalState::Uninitialized);
        self.local_scope_depths
            .push(self.scope_stack.len().saturating_sub(1));
        if let Some(scope) = self.scope_stack.last_mut() {
            scope.push(id);
        }
        id
    }

    pub(crate) fn fresh_string_global(&mut self) -> String {
        let fn_part = self.function_name.replace("::", "_");
        let name = format!(".str.{}.{}", fn_part, self.string_counter);
        self.string_counter += 1;
        name
    }

    pub(crate) fn new_block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(MirBlock::default());
        id
    }

    pub(crate) fn switch_to(&mut self, id: BlockId) {
        self.current = id;
    }

    pub(crate) fn enter_loop(&mut self, continue_target: BlockId, exit: BlockId) {
        self.loop_stack.push(LoopContext {
            continue_target,
            exit,
            scope_depth: self.scope_stack.len(),
        });
    }

    pub(crate) fn exit_loop(&mut self) {
        self.loop_stack.pop();
    }

    pub(crate) fn current_loop(&self) -> Option<&LoopContext> {
        self.loop_stack.last()
    }

    pub(crate) fn drop_scopes_after_depth(&mut self, depth: usize) {
        if self.scope_stack.len() <= depth {
            return;
        }
        self.drain_scoped_callback_scopes();
        let scopes = self.scope_stack.clone();
        for scope in scopes.iter().skip(depth) {
            self.release_arc_loans_for_locals(scope);
            self.release_lexical_loans_for_locals(scope);
        }
        for scope in scopes.iter().skip(depth).rev() {
            for &local in scope.iter().rev() {
                self.drop_local_if_needed(local);
                self.release_mutex_provenance_for_local(local);
            }
        }
    }

    pub(crate) fn handle_reassign(&mut self, local: LocalId) {
        if !self.validate_arc_owner_invalidation(local, "reassign", None) {
            return;
        }
        self.validate_lexical_owner_invalidation(local, "reassign", None);
        if self
            .locals
            .get(local.0 as usize)
            .map_or(false, |l| l.skip_drop)
        {
            return;
        }
        let dominated = self
            .local_ty(local)
            .map(|ty| Self::type_has_drop_glue(ty))
            .unwrap_or(false);
        if !dominated {
            return;
        }
        if let Some(LocalState::Initialized) = self.local_states.get(local.0 as usize) {
            if !self.validate_mutex_owner_invalidation(local, "reassign", None)
                || !self.validate_mutex_guard_invalidation(local, "reassign", None)
            {
                return;
            }
            self.emit_drop(local);
        }
    }

    pub(crate) fn enter_scope(&mut self) {
        self.scope_stack.push(Vec::new());
        self.binding_undo.push(Vec::new());
    }

    pub(crate) fn exit_scope(&mut self) {
        if self.scope_stack.len() <= 1 {
            return;
        }
        self.drain_scoped_callback_scopes();
        if let Some(locals) = self.scope_stack.pop() {
            self.release_arc_loans_for_locals(&locals);
            self.release_lexical_loans_for_locals(&locals);
            for local in locals.into_iter().rev() {
                self.drop_local_if_needed(local);
                self.release_mutex_provenance_for_local(local);
            }
        }
        if let Some(bindings) = self.binding_undo.pop() {
            for (name, previous) in bindings.into_iter().rev() {
                if let Some(local) = previous {
                    self.bindings.insert(name, local);
                } else {
                    self.bindings.remove(&name);
                }
            }
        }
    }

    pub(crate) fn drop_local_if_needed(&mut self, local: LocalId) {
        if self
            .locals
            .get(local.0 as usize)
            .map_or(false, |l| l.skip_drop)
        {
            return;
        }
        if self.thread_handle_locals.contains(&local)
            && matches!(
                self.local_states.get(local.0 as usize),
                Some(LocalState::Initialized)
            )
        {
            self.current_block_mut()
                .insts
                .push(MirInst::DropThreadHandle(local));
            if let Some(state) = self.local_states.get_mut(local.0 as usize) {
                *state = LocalState::Moved;
            }
            return;
        }
        if self.scoped_thread_handle_locals.contains(&local)
            && matches!(
                self.local_states.get(local.0 as usize),
                Some(LocalState::Initialized)
            )
        {
            self.current_block_mut()
                .insts
                .push(MirInst::DropScopedThreadHandle(local));
            if let Some(state) = self.local_states.get_mut(local.0 as usize) {
                *state = LocalState::Moved;
            }
            return;
        }
        let dominated = self
            .local_ty(local)
            .map(|ty| Self::type_has_drop_glue(ty))
            .unwrap_or(false);
        if !dominated {
            return;
        }
        let idx = local.0 as usize;
        if matches!(self.local_states.get(idx), Some(LocalState::Initialized)) {
            self.emit_drop(local);
        }
    }

    pub(crate) fn drop_all_active_locals(&mut self) {
        self.drain_scoped_callback_scopes();
        let scopes: Vec<Vec<LocalId>> = self.scope_stack.clone();
        for scope in &scopes {
            self.release_arc_loans_for_locals(scope);
            self.release_lexical_loans_for_locals(scope);
        }
        for scope in scopes.iter().rev() {
            for &local in scope.iter().rev() {
                self.drop_local_if_needed(local);
                self.release_mutex_provenance_for_local(local);
            }
        }
    }

    pub(crate) fn emit_drop(&mut self, local: LocalId) {
        if !self.validate_arc_owner_invalidation(local, "drop", None)
            || !self.validate_lexical_owner_invalidation(local, "drop", None)
            || !self.validate_mutex_owner_invalidation(local, "drop", None)
            || !self.validate_mutex_guard_invalidation(local, "drop", None)
        {
            return;
        }
        let insert_before_terminator = self
            .current_block_mut()
            .insts
            .last()
            .map(|inst| {
                matches!(
                    inst,
                    MirInst::Return(_) | MirInst::Goto(_) | MirInst::If { .. }
                )
            })
            .unwrap_or(false);
        if insert_before_terminator {
            let idx = self.current_block_mut().insts.len() - 1;
            self.current_block_mut()
                .insts
                .insert(idx, MirInst::Drop(local));
        } else {
            self.current_block_mut().insts.push(MirInst::Drop(local));
        }
        if let Some(state) = self.local_states.get_mut(local.0 as usize) {
            *state = LocalState::Moved;
        }
        self.release_mutex_provenance_for_local(local);
    }

    pub(crate) fn local_ty(&self, local: LocalId) -> Option<&Type> {
        self.locals
            .get(local.0 as usize)
            .and_then(|l| l.ty.as_ref())
    }

    pub(crate) fn mark_implicit_deref(&mut self, local: LocalId) {
        self.implicit_deref_locals.insert(local);
    }

    pub(crate) fn implicit_deref_type(&self, local: LocalId) -> Option<&Type> {
        if !self.implicit_deref_locals.contains(&local) {
            return None;
        }
        match self.local_ty(local) {
            Some(Type::Ref(inner, _)) => Some(inner),
            _ => None,
        }
    }

    fn lexical_loan_root(&self, local: LocalId) -> LocalId {
        self.lexical_loans
            .get(&local)
            .and_then(|loans| loans.first())
            .map_or(local, |loan| loan.owner)
    }

    fn active_lexical_loan(&self, owner: LocalId) -> Option<LexicalLoan> {
        self.lexical_loans
            .values()
            .flatten()
            .chain(self.scoped_loan_reservations.values().flatten())
            .find(|loan| loan.owner == owner)
            .copied()
    }

    fn active_exclusive_loan(&self, owner: LocalId) -> Option<LexicalLoan> {
        self.lexical_loans
            .values()
            .flatten()
            .chain(self.scoped_loan_reservations.values().flatten())
            .find(|loan| loan.owner == owner && loan.kind == BorrowKind::Mutable)
            .copied()
    }

    pub(crate) fn validate_scoped_task_captures(&mut self, task: LocalId, span: Span) -> bool {
        let loans = self.lexical_loans.get(&task).cloned().unwrap_or_default();
        if loans.is_empty() && !self.known_borrowed_callables.contains(&task) {
            self.error(
                "scoped task capture provenance is unavailable; pass a closure literal or function item directly",
                Some(span),
            );
            return false;
        }

        let mut valid = true;
        for loan in loans {
            let owner_ty = self.local_ty(loan.owner).cloned().unwrap_or(Type::Void);
            let referent = match owner_ty {
                Type::Ref(inner, _) => *inner,
                other => other,
            };
            if referent == glyph_core::thread::canonical_thread_scope_type()
                || glyph_core::thread::is_canonical_scoped_thread_handle(&referent)
            {
                self.error(
                    "a scoped task cannot capture Scope or ScopedJoinHandle; nested spawn from a worker is not supported",
                    Some(span),
                );
                valid = false;
                continue;
            }
            let checked = self.thread_safety_type(&referent);
            let result = match loan.kind {
                BorrowKind::Shared => self
                    .thread_safety_registry
                    .check_sync("shared scoped capture", &checked),
                BorrowKind::Mutable => self
                    .thread_safety_registry
                    .check_send("mutable scoped capture", &checked),
            };
            if let Err(error) = result {
                self.error(error.to_string(), Some(span));
                valid = false;
            }
        }
        valid
    }

    pub(crate) fn reserve_scoped_task_loans(
        &mut self,
        scope: LocalId,
        task: LocalId,
        span: Span,
    ) -> bool {
        if matches!(
            self.local_ty(task),
            Some(Type::BorrowedFunction {
                kind: glyph_core::types::BorrowedCallableKind::FnMut,
                ..
            })
        ) && !self
            .active_scoped_fnmut_tasks
            .entry(scope)
            .or_default()
            .insert(task)
        {
            self.error(
                "an FnMut task cannot have more than one live scoped spawn; the conservative reservation lasts until the next lexical scope drain",
                Some(span),
            );
            return false;
        }
        if let Some(loans) = self.lexical_loans.get(&task).cloned() {
            self.scoped_loan_reservations
                .entry(scope)
                .or_default()
                .extend(loans);
        }
        true
    }

    pub(crate) fn validate_new_lexical_borrow(
        &mut self,
        base: LocalId,
        kind: BorrowKind,
        span: Span,
    ) -> bool {
        let owner = self.lexical_loan_root(base);
        let conflict = match kind {
            BorrowKind::Shared => self.active_exclusive_loan(owner),
            BorrowKind::Mutable => self.active_lexical_loan(owner),
        };
        let Some(conflict) = conflict else {
            return true;
        };
        let owner_name = self.local_name(owner).unwrap_or("<temporary>");
        self.error(
            match (kind, conflict.kind) {
                (BorrowKind::Shared, BorrowKind::Mutable) => format!(
                    "cannot immutably borrow `{owner_name}` while an exclusive loan is active"
                ),
                (BorrowKind::Mutable, BorrowKind::Shared) => {
                    format!("cannot mutably borrow `{owner_name}` while a shared loan is active")
                }
                (BorrowKind::Mutable, BorrowKind::Mutable) => format!(
                    "cannot mutably borrow `{owner_name}` while an exclusive loan is active"
                ),
                (BorrowKind::Shared, BorrowKind::Shared) => unreachable!(),
            },
            Some(span),
        );
        false
    }

    fn register_lexical_loan(
        &mut self,
        holder: LocalId,
        base: LocalId,
        kind: BorrowKind,
        origin: Span,
    ) {
        let owner = self.lexical_loan_root(base);
        if !self.validate_new_lexical_borrow(base, kind, origin) {
            return;
        }
        let creation_scope = self
            .local_scope_depths
            .get(holder.0 as usize)
            .copied()
            .unwrap_or(0);
        let owner_scope = self
            .local_scope_depths
            .get(owner.0 as usize)
            .copied()
            .unwrap_or(0);
        if owner_scope > creation_scope {
            self.error(
                "a borrowed reference or callable cannot escape to an outer lexical scope",
                Some(origin),
            );
        }
        self.lexical_loans
            .entry(holder)
            .or_default()
            .push(LexicalLoan {
                owner,
                kind,
                creation_scope,
                origin,
            });
    }

    fn propagate_lexical_loans(&mut self, source: LocalId, destination: LocalId) {
        let Some(loans) = self.lexical_loans.get(&source).cloned() else {
            return;
        };
        let destination_scope = self
            .local_scope_depths
            .get(destination.0 as usize)
            .copied()
            .unwrap_or(0);
        for mut loan in loans {
            if loan.kind == BorrowKind::Mutable {
                self.error(
                    "an FnMut or mutable reference cannot be aliased; pass it directly or create a shorter reborrow",
                    Some(loan.origin),
                );
                continue;
            }
            if destination_scope < loan.creation_scope {
                self.error(
                    "a borrowed reference or callable cannot escape the lexical scope where its loan was created",
                    Some(loan.origin),
                );
            }
            loan.creation_scope = destination_scope;
            self.lexical_loans
                .entry(destination)
                .or_default()
                .push(loan);
        }
    }

    fn release_lexical_loans_for_holder(&mut self, holder: LocalId) {
        self.lexical_loans.remove(&holder);
    }

    pub(crate) fn release_temporary_call_loan(&mut self, holder: LocalId) {
        self.release_lexical_loans_for_holder(holder);
        self.arc_loans.remove(&holder);
    }

    fn release_lexical_loans_for_locals(&mut self, locals: &[LocalId]) {
        for local in locals {
            self.release_lexical_loans_for_holder(*local);
        }
    }

    fn validate_lexical_owner_read(&mut self, owner: LocalId, span: Option<Span>) -> bool {
        let Some(loan) = self.active_exclusive_loan(owner) else {
            return true;
        };
        let owner_name = self.local_name(owner).unwrap_or("<temporary>");
        self.error(
            format!("cannot use `{owner_name}` while an exclusive loan is active"),
            span.or(Some(loan.origin)),
        );
        false
    }

    fn validate_lexical_owner_invalidation(
        &mut self,
        owner: LocalId,
        action: &str,
        span: Option<Span>,
    ) -> bool {
        let Some(loan) = self.active_lexical_loan(owner) else {
            return true;
        };
        let owner_name = self.local_name(owner).unwrap_or("<temporary>");
        let loan_name = if loan.kind == BorrowKind::Mutable {
            "exclusive"
        } else {
            "shared"
        };
        self.error(
            format!("cannot {action} `{owner_name}` while a {loan_name} loan is active"),
            span.or(Some(loan.origin)),
        );
        false
    }

    fn value_has_lexical_loan(&self, value: &MirValue) -> bool {
        matches!(value, MirValue::Local(local) if self.lexical_loans.contains_key(local)
            || matches!(self.local_ty(*local), Some(Type::BorrowedFunction { .. })))
    }

    fn reject_lexical_loan_value_storage(&mut self, value: &MirValue, destination: &str) {
        if self.value_has_lexical_loan(value) {
            self.error(
                format!(
                    "a borrowed reference or callable cannot be stored in {destination}; keep it in a direct lexical binding"
                ),
                None,
            );
        }
    }

    fn reject_lexical_loan_storage(&mut self, value: &Rvalue) {
        match value {
            Rvalue::StructLit { field_values, .. } => {
                for (_, value) in field_values {
                    self.reject_lexical_loan_value_storage(value, "an aggregate");
                }
            }
            Rvalue::ArrayLit { elements, .. } => {
                for value in elements {
                    self.reject_lexical_loan_value_storage(value, "an array");
                }
            }
            Rvalue::EnumConstruct {
                payload: Some(value),
                ..
            } => self.reject_lexical_loan_value_storage(value, "an enum payload"),
            Rvalue::VecPush { value, .. } => self.reject_lexical_loan_value_storage(value, "a Vec"),
            Rvalue::MapAdd { key, value, .. } | Rvalue::MapUpdate { key, value, .. } => {
                self.reject_lexical_loan_value_storage(key, "a Map");
                self.reject_lexical_loan_value_storage(value, "a Map");
            }
            Rvalue::OwnNew { value, .. } => {
                self.reject_lexical_loan_value_storage(value, "an Own allocation")
            }
            Rvalue::SharedNew { value, .. } => {
                self.reject_lexical_loan_value_storage(value, "a Shared allocation")
            }
            Rvalue::ArcNew { value, .. } => {
                self.reject_lexical_loan_value_storage(value, "an Arc allocation")
            }
            Rvalue::MutexNew { value, .. } => {
                self.reject_lexical_loan_value_storage(value, "a Mutex allocation")
            }
            Rvalue::MakeClosure { captures, .. } => {
                for capture in captures {
                    if self.lexical_loans.contains_key(&capture.local)
                        || matches!(
                            self.local_ty(capture.local),
                            Some(Type::BorrowedFunction { .. })
                        )
                    {
                        self.error(
                            "a borrowed reference or callable cannot be captured by an owned closure",
                            None,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    fn reject_lexical_loan_field_storage(&mut self, value: &Rvalue) {
        if let Rvalue::Move(source) = value {
            self.reject_lexical_loan_value_storage(&MirValue::Local(*source), "a struct field");
        }
    }

    pub(crate) fn reject_lexical_loan_return(&mut self, value: Option<&MirValue>, span: Span) {
        if value.is_some_and(|value| self.value_has_lexical_loan(value)) {
            self.error(
                "a borrowed reference or callable cannot escape through return",
                Some(span),
            );
        }
    }

    pub(crate) fn local_needs_drop(&self, local: LocalId) -> bool {
        self.local_ty(local)
            .map(Self::type_requires_owned_local_tracking)
            .unwrap_or(false)
    }

    pub(crate) fn local_uses_ownership_tracking(&self, local: LocalId) -> bool {
        self.local_needs_drop(local) || self.local_is_guard(local)
    }

    pub(crate) fn emit_use_of_moved_local(&mut self, local: LocalId, span: Option<Span>) {
        let message = if self.local_is_guard(local) {
            match self.local_name(local) {
                Some(name) => format!(
                    "use of moved guard `{}`; guard values are single-owner and cleanup runs exactly once",
                    name
                ),
                None => "use of moved guard value; guard values are single-owner and cleanup runs exactly once".to_string(),
            }
        } else {
            match (self.local_name(local), self.local_type_label(local)) {
                (Some(name), Some(ty)) => {
                    format!("use of moved value `{}` of type `{}`", name, ty)
                }
                (Some(name), None) => format!("use of moved value `{}`", name),
                (None, Some(ty)) => format!("use of moved value of type `{}`", ty),
                (None, None) => "use of moved value".to_string(),
            }
        };
        self.error(message, span);
        self.diagnostics.push(Diagnostic::note(
            "Glyph uses single-owner move semantics and conservatively merges ownership across reachable control-flow paths",
            span,
        ));
        self.diagnostics.push(Diagnostic::help(
            "avoid branch-local moves before unconditional post-branch use, or reinitialize the value on every reachable branch",
            span,
        ));
    }

    pub(crate) fn emit_use_of_uninitialized_local(&mut self, local: LocalId, span: Option<Span>) {
        let message = if self.local_is_guard(local) {
            match self.local_name(local) {
                Some(name) => format!(
                    "use of uninitialized guard `{}`; initialize the guard before using it",
                    name
                ),
                None => "use of uninitialized guard value; initialize the guard before using it"
                    .to_string(),
            }
        } else {
            match (self.local_name(local), self.local_type_label(local)) {
                (Some(name), Some(ty)) => {
                    format!("use of uninitialized value `{}` of type `{}`", name, ty)
                }
                (Some(name), None) => format!("use of uninitialized value `{}`", name),
                (None, Some(ty)) => format!("use of uninitialized value of type `{}`", ty),
                (None, None) => "use of uninitialized value".to_string(),
            }
        };
        self.error(message, span);
        self.diagnostics.push(Diagnostic::note(
            "this value is not definitely initialized on all reachable paths",
            span,
        ));
        self.diagnostics.push(Diagnostic::help(
            "initialize the value on every reachable branch before use",
            span,
        ));
    }

    fn type_has_drop_glue(ty: &Type) -> bool {
        match ty {
            Type::Own(_)
            | Type::Shared(_)
            | Type::Atomic(_)
            | Type::String
            | Type::Enum(_)
            | Type::Function { .. } => true,
            Type::App { base, .. } => {
                matches!(
                    base.rsplit("::").next().unwrap_or(base),
                    "Vec" | "Map" | "Result" | "Option" | "TrySendResult" | "TryRecvResult"
                ) || ty.is_arc()
                    || ty.is_mutex()
                    || ty.is_mutex_guard()
                    || ty.is_spsc_sender()
                    || ty.is_spsc_receiver()
                    || glyph_core::thread::is_canonical_thread_handle(ty)
                    || glyph_core::thread::is_canonical_scoped_thread_handle(ty)
            }
            Type::Named(_) => true,
            _ => false,
        }
    }

    // Keep local move tracking narrower than structural backend drop glue.
    // Named aggregate field access can borrow from its base; treating every
    // named aggregate as consumed would incorrectly move the whole value.
    fn type_requires_owned_local_tracking(ty: &Type) -> bool {
        match ty {
            Type::Own(_)
            | Type::Shared(_)
            | Type::Atomic(_)
            | Type::String
            | Type::Enum(_)
            | Type::Function { .. } => true,
            Type::App { base, .. } => {
                matches!(
                    base.rsplit("::").next().unwrap_or(base),
                    "Result" | "Option" | "TrySendResult" | "TryRecvResult"
                ) || ty.is_arc()
                    || ty.is_mutex()
                    || ty.is_mutex_guard()
                    || ty.is_spsc_sender()
                    || ty.is_spsc_receiver()
                    || glyph_core::thread::is_canonical_thread_handle(ty)
                    || glyph_core::thread::is_canonical_scoped_thread_handle(ty)
            }
            _ => false,
        }
    }

    fn type_is_guard(ty: &Type) -> bool {
        if ty.is_mutex_guard() {
            return true;
        }
        match ty {
            Type::Named(name) => {
                let leaf = name.rsplit("::").next().unwrap_or(name);
                leaf.ends_with("Guard")
            }
            _ => false,
        }
    }

    pub(crate) fn type_label(ty: &Type) -> String {
        match ty {
            Type::I8 => "i8".to_string(),
            Type::I32 => "i32".to_string(),
            Type::I64 => "i64".to_string(),
            Type::U8 => "u8".to_string(),
            Type::U32 => "u32".to_string(),
            Type::U64 => "u64".to_string(),
            Type::Usize => "usize".to_string(),
            Type::F32 => "f32".to_string(),
            Type::F64 => "f64".to_string(),
            Type::Bool => "bool".to_string(),
            Type::Char => "char".to_string(),
            Type::Str => "str".to_string(),
            Type::String => "String".to_string(),
            Type::Void => "()".to_string(),
            Type::Named(name) | Type::Enum(name) | Type::Param(name) => name.clone(),
            Type::App { base, args } => {
                if args.is_empty() {
                    return base.clone();
                }
                let rendered_args: Vec<String> = args.iter().map(Self::type_label).collect();
                format!("{}<{}>", base, rendered_args.join(", "))
            }
            Type::Ref(inner, glyph_core::types::Mutability::Immutable) => {
                format!("&{}", Self::type_label(inner))
            }
            Type::Ref(inner, glyph_core::types::Mutability::Mutable) => {
                format!("&mut {}", Self::type_label(inner))
            }
            Type::Array(elem, size) => format!("[{}; {}]", Self::type_label(elem), size),
            Type::Own(inner) => format!("Own<{}>", Self::type_label(inner)),
            Type::RawPtr(inner) => format!("RawPtr<{}>", Self::type_label(inner)),
            Type::Shared(inner) => format!("Shared<{}>", Self::type_label(inner)),
            Type::Atomic(scalar) => scalar.type_name().to_string(),
            Type::Function { params, ret } => {
                let args = match params.as_slice() {
                    [] => "()".to_string(),
                    [Type::Tuple(_)] => format!("({},)", Self::type_label(&params[0])),
                    [param] => Self::type_label(param),
                    params => format!(
                        "({})",
                        params
                            .iter()
                            .map(Self::type_label)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                };
                format!("FnOnce<{}, {}>", args, Self::type_label(ret))
            }
            Type::BorrowedFunction { kind, params, ret } => {
                let args = match params.as_slice() {
                    [] => "()".to_string(),
                    [Type::Tuple(_)] => format!("({},)", Self::type_label(&params[0])),
                    [param] => Self::type_label(param),
                    params => format!(
                        "({})",
                        params
                            .iter()
                            .map(Self::type_label)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                };
                let capability = match kind {
                    glyph_core::types::BorrowedCallableKind::Fn => "Fn",
                    glyph_core::types::BorrowedCallableKind::FnMut => "FnMut",
                };
                format!("{capability}<{}, {}>", args, Self::type_label(ret))
            }
            Type::Tuple(elements) => {
                if elements.is_empty() {
                    "()".to_string()
                } else {
                    let rendered: Vec<String> = elements.iter().map(Self::type_label).collect();
                    format!("({})", rendered.join(", "))
                }
            }
        }
    }

    fn local_name(&self, local: LocalId) -> Option<&str> {
        self.locals
            .get(local.0 as usize)
            .and_then(|local| local.name.as_deref())
    }

    fn local_type_label(&self, local: LocalId) -> Option<String> {
        self.local_ty(local).map(Self::type_label)
    }

    pub(crate) fn local_is_guard(&self, local: LocalId) -> bool {
        self.local_ty(local)
            .map(Self::type_is_guard)
            .unwrap_or(false)
    }

    pub(crate) fn check_local_available(&mut self, local: LocalId, span: Option<Span>) -> bool {
        if !self.validate_lexical_owner_read(local, span) {
            return false;
        }
        let skip_drop = self
            .locals
            .get(local.0 as usize)
            .map_or(false, |l| l.skip_drop);
        let tracked = self
            .local_ty(local)
            .map(Self::type_has_drop_glue)
            .unwrap_or(false)
            || self.local_is_guard(local);

        if !tracked || skip_drop {
            return true;
        }

        match self.local_states.get(local.0 as usize) {
            Some(LocalState::Moved) => {
                self.emit_use_of_moved_local(local, span);
                false
            }
            Some(LocalState::Uninitialized) => {
                self.emit_use_of_uninitialized_local(local, span);
                false
            }
            _ => true,
        }
    }

    /// Mark a MirValue's source local as Moved if it has drop glue.
    fn mark_moved_if_droppable(&mut self, val: &MirValue) {
        if let MirValue::Local(src) = val {
            if Self::type_has_drop_glue(self.local_ty(*src).unwrap_or(&Type::Void)) {
                if let Some(state) = self.local_states.get_mut(src.0 as usize) {
                    *state = LocalState::Moved;
                }
            }
        }
    }

    fn mark_skip_drop(&mut self, local: LocalId) {
        if let Some(dest) = self.locals.get_mut(local.0 as usize) {
            dest.skip_drop = true;
        }
    }

    /// Track ownership transfers for rvalues that consume their arguments.
    fn track_rvalue_ownership(&mut self, value: &Rvalue, dest: LocalId) {
        match value {
            // VecPush modifies the vec alloca in place and returns a
            // snapshot. The snapshot must not be independently dropped,
            // and the pushed value is now owned by the Vec.
            Rvalue::VecPush { value: val, .. } => {
                if let Some(state) = self.local_states.get_mut(dest.0 as usize) {
                    *state = LocalState::Moved;
                }
                self.mark_moved_if_droppable(val);
            }
            // StructLit takes ownership of field values.
            Rvalue::StructLit { field_values, .. } => {
                for (_name, val) in field_values {
                    self.mark_moved_if_droppable(val);
                }
            }
            // Enum construction takes ownership of its payload, matching
            // struct literals and collection insertion.
            Rvalue::EnumConstruct {
                payload: Some(val), ..
            } => {
                self.mark_moved_if_droppable(val);
            }
            // Function call ownership depends on the callee parameter type,
            // so call lowering handles by-value consumption and by-reference
            // non-consumption explicitly.
            // An indirect call consumes its FnOnce carrier. Argument ownership
            // is still handled by call lowering using the signature.
            Rvalue::CallIndirect { callee, .. } => {
                if let Some(state) = self.local_states.get_mut(callee.0 as usize) {
                    *state = LocalState::Moved;
                }
            }
            Rvalue::ThreadHandleIntoRaw { handle } => {
                if let Some(state) = self.local_states.get_mut(handle.0 as usize) {
                    *state = LocalState::Moved;
                }
            }
            // Arc allocation transfers its payload into the stable heap
            // allocation. Cloning and immutable borrowing leave the source
            // Arc owner live.
            Rvalue::ArcNew { value, .. } | Rvalue::MutexNew { value, .. } => {
                self.mark_moved_if_droppable(value);
            }
            Rvalue::SpscTrySend { value, .. } => {
                if let Some(state) = self.local_states.get_mut(value.0 as usize) {
                    *state = LocalState::Moved;
                }
            }
            // Closure construction immediately transfers every non-Copy
            // capture into its owned environment.
            Rvalue::MakeClosure { captures, .. } => {
                for capture in captures {
                    if capture.transfer == glyph_core::mir::CaptureTransfer::Move {
                        if let Some(state) = self.local_states.get_mut(capture.local.0 as usize) {
                            *state = LocalState::Moved;
                        }
                    }
                }
            }
            // Map mutations take ownership of keys/values.
            Rvalue::MapAdd {
                key, value: val, ..
            }
            | Rvalue::MapUpdate {
                key, value: val, ..
            } => {
                self.mark_moved_if_droppable(key);
                self.mark_moved_if_droppable(val);
            }
            // VecIndex returns a shallow copy of the element. For types
            // with drop glue, the copy aliases the Vec's element data.
            // Mark it non-owning so the copy is not independently dropped.
            Rvalue::VecIndex { elem_type, .. } if Self::type_has_drop_glue(elem_type) => {
                self.mark_skip_drop(dest);
            }
            // MapGet returns a shallow snapshot of the stored value. For
            // droppable payloads the map remains the owner, so the returned
            // Option must not run drop glue on its payload copy.
            Rvalue::MapGet { value_type, .. } if Self::type_has_drop_glue(value_type) => {
                self.mark_skip_drop(dest);
            }
            // FieldAccess returns a shallow copy of the field value.
            // For droppable types, the copy aliases the struct's field.
            Rvalue::FieldAccess { .. } => {
                let dest_ty = self.local_ty(dest);
                if dest_ty
                    .map(|ty| Self::type_has_drop_glue(ty))
                    .unwrap_or(false)
                {
                    self.mark_skip_drop(dest);
                }
            }
            // EnumPayload is also a shallow snapshot. Preserve alias semantics
            // by suppressing drop glue on the extracted payload local.
            Rvalue::EnumPayload { base, .. }
                if self
                    .locals
                    .get(base.0 as usize)
                    .map(|l| l.skip_drop)
                    .unwrap_or(false) =>
            {
                self.mark_skip_drop(dest);
            }
            _ => {}
        }
    }

    pub(crate) fn consume_local(&mut self, local: LocalId, span: Option<Span>) -> bool {
        if !self.check_local_available(local, span) {
            return false;
        }

        if !self.validate_arc_owner_invalidation(local, "move", span)
            || !self.validate_lexical_owner_invalidation(local, "move", span)
        {
            return false;
        }
        if !self.validate_mutex_owner_invalidation(local, "move", span)
            || !self.validate_mutex_guard_invalidation(local, "move", span)
        {
            return false;
        }

        if !self.local_uses_ownership_tracking(local) {
            return true;
        }
        let idx = local.0 as usize;
        match self.local_states.get(idx) {
            Some(LocalState::Moved) => {
                self.emit_use_of_moved_local(local, span);
                false
            }
            Some(LocalState::Initialized) => {
                if let Some(state) = self.local_states.get_mut(idx) {
                    *state = LocalState::Moved;
                }
                true
            }
            _ => {
                self.emit_use_of_uninitialized_local(local, span);
                if let Some(state) = self.local_states.get_mut(idx) {
                    *state = LocalState::Moved;
                }
                false
            }
        }
    }

    pub(crate) fn register_arc_borrow(&mut self, reference: LocalId, owner: LocalId, origin: Span) {
        self.register_lexical_loan(reference, owner, BorrowKind::Shared, origin);
        self.arc_loans.insert(
            reference,
            ArcLoan {
                owner,
                creation_scope: self
                    .local_scope_depths
                    .get(reference.0 as usize)
                    .copied()
                    .unwrap_or(0),
                origin,
            },
        );
    }

    pub(crate) fn reject_arc_loan_scope_escape(&mut self, local: LocalId) {
        let Some(loan) = self.arc_loans.get(&local).copied() else {
            return;
        };
        let current_scope = self.scope_stack.len().saturating_sub(1);
        if loan.creation_scope == current_scope {
            self.error(
                "an Arc::borrow() reference cannot escape the lexical scope where it was created",
                Some(loan.origin),
            );
        }
    }

    fn validate_arc_owner_invalidation(
        &mut self,
        owner: LocalId,
        action: &str,
        span: Option<Span>,
    ) -> bool {
        if !self.local_ty(owner).is_some_and(Type::is_arc) {
            return true;
        }
        let Some(loan) = self
            .arc_loans
            .values()
            .find(|loan| loan.owner == owner)
            .copied()
        else {
            return true;
        };
        let owner = self.local_name(owner).unwrap_or("<temporary>");
        self.error(
            format!(
                "cannot {action} Arc owner `{owner}` while an Arc::borrow() reference is active; the loan lasts until its binding's lexical scope ends"
            ),
            span.or(Some(loan.origin)),
        );
        false
    }

    fn release_arc_loans_for_locals(&mut self, locals: &[LocalId]) {
        for local in locals {
            self.arc_loans.remove(local);
        }
    }

    fn arc_loan_for_value(&self, value: &MirValue) -> Option<ArcLoan> {
        let MirValue::Local(local) = value else {
            return None;
        };
        self.arc_loans.get(local).copied()
    }

    fn reject_arc_loan_value_storage(&mut self, value: &MirValue, destination: &str) {
        let Some(loan) = self.arc_loan_for_value(value) else {
            return;
        };
        self.error(
            format!(
                "an Arc::borrow() reference cannot be stored in {destination}; keep it in a direct lexical binding"
            ),
            Some(loan.origin),
        );
    }

    fn reject_arc_loan_storage(&mut self, value: &Rvalue) {
        match value {
            Rvalue::StructLit { field_values, .. } => {
                for (_, value) in field_values {
                    self.reject_arc_loan_value_storage(value, "an aggregate");
                }
            }
            Rvalue::ArrayLit { elements, .. } => {
                for value in elements {
                    self.reject_arc_loan_value_storage(value, "an array");
                }
            }
            Rvalue::EnumConstruct {
                payload: Some(value),
                ..
            } => self.reject_arc_loan_value_storage(value, "an enum payload"),
            Rvalue::VecPush { value, .. } => {
                self.reject_arc_loan_value_storage(value, "a Vec");
            }
            Rvalue::MapAdd { key, value, .. } | Rvalue::MapUpdate { key, value, .. } => {
                self.reject_arc_loan_value_storage(key, "a Map");
                self.reject_arc_loan_value_storage(value, "a Map");
            }
            Rvalue::OwnNew { value, .. } => {
                self.reject_arc_loan_value_storage(value, "an Own allocation");
            }
            Rvalue::SharedNew { value, .. } => {
                self.reject_arc_loan_value_storage(value, "a Shared allocation");
            }
            Rvalue::MakeClosure { captures, .. } => {
                for capture in captures {
                    if let Some(loan) = self.arc_loans.get(&capture.local).copied() {
                        self.error(
                            "an Arc::borrow() reference cannot be captured by a closure; borrow inside the closure from an owned Arc clone instead",
                            Some(loan.origin),
                        );
                    }
                    if capture.transfer == glyph_core::mir::CaptureTransfer::Move {
                        self.validate_arc_owner_invalidation(
                            capture.local,
                            "move into a closure",
                            None,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    pub(crate) fn mark_mutex_try_option(&mut self, local: LocalId) {
        self.mutex_try_option_locals.insert(local);
    }

    pub(crate) fn register_mutex_guard(&mut self, holder: LocalId, owner: LocalId, origin: Span) {
        let creation_scope = self
            .local_scope_depths
            .get(holder.0 as usize)
            .copied()
            .unwrap_or(0);
        let owner_scope = self
            .local_scope_depths
            .get(owner.0 as usize)
            .copied()
            .unwrap_or(0);
        if owner_scope > creation_scope {
            self.error(
                "a MutexGuard cannot outlive the Mutex binding it locks",
                Some(origin),
            );
        }
        self.mutex_loans.insert(
            holder,
            MutexLoan {
                owner,
                creation_scope,
                origin,
            },
        );
    }

    pub(crate) fn register_mutex_guard_borrow(
        &mut self,
        reference: LocalId,
        guard: LocalId,
        origin: Span,
    ) {
        self.mutex_guard_borrows.insert(
            reference,
            MutexGuardBorrow {
                guard,
                creation_scope: self
                    .local_scope_depths
                    .get(reference.0 as usize)
                    .copied()
                    .unwrap_or(0),
                origin,
            },
        );
    }

    fn transfer_mutex_provenance(&mut self, source: LocalId, destination: LocalId) {
        if let Some(mut loan) = self.mutex_loans.remove(&source) {
            let destination_scope = self
                .local_scope_depths
                .get(destination.0 as usize)
                .copied()
                .unwrap_or(loan.creation_scope);
            if destination_scope < loan.creation_scope {
                self.error(
                    "a MutexGuard cannot escape the lexical scope where it was acquired",
                    Some(loan.origin),
                );
            }
            loan.creation_scope = destination_scope;
            self.mutex_loans.insert(destination, loan);
        }
        if let Some(mut borrow) = self.mutex_guard_borrows.remove(&source) {
            let destination_scope = self
                .local_scope_depths
                .get(destination.0 as usize)
                .copied()
                .unwrap_or(borrow.creation_scope);
            if destination_scope < borrow.creation_scope {
                self.error(
                    "a reference borrowed from MutexGuard cannot escape its lexical scope",
                    Some(borrow.origin),
                );
            }
            borrow.creation_scope = destination_scope;
            self.mutex_guard_borrows.insert(destination, borrow);
        }
    }

    fn release_mutex_provenance_for_local(&mut self, local: LocalId) {
        self.mutex_loans.remove(&local);
        self.mutex_guard_borrows.remove(&local);
        self.mutex_try_option_locals.remove(&local);
        self.release_lexical_loans_for_holder(local);
    }

    fn validate_mutex_owner_invalidation(
        &mut self,
        owner: LocalId,
        action: &str,
        span: Option<Span>,
    ) -> bool {
        let Some((holder, loan)) = self
            .mutex_loans
            .iter()
            .find(|(_, loan)| loan.owner == owner)
            .map(|(holder, loan)| (*holder, *loan))
        else {
            return true;
        };
        let owner_name = self.local_name(owner).unwrap_or("<temporary>");
        let guard_name = self.local_name(holder).unwrap_or("<temporary>");
        self.error(
            format!(
                "cannot {action} Mutex owner `{owner_name}` while guard `{guard_name}` is live"
            ),
            span.or(Some(loan.origin)),
        );
        self.diagnostics.push(Diagnostic::note(
            "the exclusive Mutex loan begins at this lock operation and lasts until the guard's lexical scope ends",
            Some(loan.origin),
        ));
        self.diagnostics.push(Diagnostic::help(
            "put the guard in a nested block so it unlocks before moving or dropping the Mutex",
            span.or(Some(loan.origin)),
        ));
        false
    }

    fn validate_mutex_guard_invalidation(
        &mut self,
        guard: LocalId,
        action: &str,
        span: Option<Span>,
    ) -> bool {
        let Some(borrow) = self
            .mutex_guard_borrows
            .values()
            .find(|borrow| borrow.guard == guard)
            .copied()
        else {
            return true;
        };
        let guard_name = self.local_name(guard).unwrap_or("<temporary>");
        self.error(
            format!(
                "cannot {action} MutexGuard `{guard_name}` while a reference borrowed from it is live"
            ),
            span.or(Some(borrow.origin)),
        );
        false
    }

    pub(crate) fn type_contains_mutex_guard(ty: &Type) -> bool {
        if ty.is_mutex_guard() {
            return true;
        }
        match ty {
            Type::App { args, .. } | Type::Tuple(args) | Type::Function { params: args, .. } => {
                args.iter().any(Self::type_contains_mutex_guard)
                    || matches!(ty, Type::Function { ret, .. } if Self::type_contains_mutex_guard(ret))
            }
            Type::Array(inner, _)
            | Type::Own(inner)
            | Type::RawPtr(inner)
            | Type::Shared(inner)
            | Type::Ref(inner, _) => Self::type_contains_mutex_guard(inner),
            _ => false,
        }
    }

    fn value_contains_mutex_guard(&self, value: &MirValue) -> bool {
        let MirValue::Local(local) = value else {
            return false;
        };
        self.mutex_loans.contains_key(local)
            || self.mutex_guard_borrows.contains_key(local)
            || self
                .local_ty(*local)
                .is_some_and(Self::type_contains_mutex_guard)
    }

    pub(crate) fn reject_mutex_guard_return(&mut self, value: Option<&MirValue>, span: Span) {
        if value.is_some_and(|value| self.value_contains_mutex_guard(value)) {
            self.error(
                "MutexGuard and references borrowed from it cannot escape through return",
                Some(span),
            );
        }
    }

    fn reject_mutex_guard_field_storage(&mut self, value: &Rvalue) {
        if let Rvalue::Move(source) = value
            && self.value_contains_mutex_guard(&MirValue::Local(*source))
        {
            self.error(
                "MutexGuard cannot be stored in a struct field; keep it in a direct lexical binding",
                None,
            );
        }
    }

    fn reject_mutex_guard_storage(&mut self, destination: LocalId, value: &Rvalue) {
        let reject_value = |this: &mut Self, value: &MirValue, destination_name: &str| {
            if this.value_contains_mutex_guard(value) {
                this.error(
                    format!(
                        "MutexGuard cannot be stored in {destination_name}; keep it in a direct lexical binding"
                    ),
                    None,
                );
            }
        };
        match value {
            Rvalue::StructLit { field_values, .. } => {
                for (_, value) in field_values {
                    reject_value(self, value, "an aggregate");
                }
            }
            Rvalue::ArrayLit { elements, .. } => {
                for value in elements {
                    reject_value(self, value, "an array");
                }
            }
            Rvalue::EnumConstruct {
                payload: Some(value),
                ..
            } if !self.mutex_try_option_locals.contains(&destination) => {
                reject_value(self, value, "an enum payload");
            }
            Rvalue::VecPush { value, .. } => reject_value(self, value, "a Vec"),
            Rvalue::MapAdd { key, value, .. } | Rvalue::MapUpdate { key, value, .. } => {
                reject_value(self, key, "a Map");
                reject_value(self, value, "a Map");
            }
            Rvalue::OwnNew { value, .. } => reject_value(self, value, "an Own allocation"),
            Rvalue::SharedNew { value, .. } => reject_value(self, value, "a Shared allocation"),
            Rvalue::ArcNew { value, .. } => reject_value(self, value, "an Arc allocation"),
            Rvalue::MutexNew { value, .. } => reject_value(self, value, "a Mutex allocation"),
            Rvalue::MakeClosure { captures, .. } => {
                for capture in captures {
                    if self.mutex_loans.contains_key(&capture.local)
                        || self
                            .local_ty(capture.local)
                            .is_some_and(Self::type_contains_mutex_guard)
                    {
                        self.error("MutexGuard cannot be captured by a closure", None);
                    }
                }
            }
            _ => {}
        }
    }
}
