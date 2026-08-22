//! Lexical capture and ownership analysis for closure conversion.
//!
//! The pass assigns a stable source binding identity to every parameter and
//! local, resolves free variables through nested lexical scopes, and records
//! the ownership facts needed by MIR closure conversion. It deliberately does
//! not decide cross-thread `Send`/`Sync` policy.

use std::collections::{HashMap, HashSet};

use glyph_core::{
    ast::{
        BinaryOp, Block, CaptureMode, Expr, Function, InterpSegment, Literal, MatchPattern, Param,
        Stmt, TypeExpr,
    },
    diag::Diagnostic,
    span::Span,
    types::Type,
};

use crate::resolver::{ResolverContext, resolve_type_expr_to_type};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureOwnership {
    /// The environment stores an independent bitwise value and the source
    /// remains available.
    Copy,
    /// The environment becomes the sole owner and the source is unavailable
    /// immediately after closure construction.
    Move,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClosureEscapeKind {
    Return,
    Storage,
    ContainerInsertion,
    Transfer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClosureEscape {
    pub kind: ClosureEscapeKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosureCapture {
    /// Stable source identity. GLYPH-37 maps this id to the MIR `LocalId`
    /// allocated for the declaration carrying the same span and name.
    pub binding_id: u32,
    pub name: String,
    pub declaration_span: Span,
    pub first_use_span: Span,
    pub declared_type: Option<TypeExpr>,
    /// Resolved annotation or conservative local expression inference.
    pub resolved_type: Option<Type>,
    pub ownership: CaptureOwnership,
    /// Preserves the user's `move ... -> ...` intent even when a Copy capture
    /// does not invalidate its source.
    pub explicit_move: bool,
    /// True for direct or structurally nested borrowed data. This is exposed
    /// for escape diagnostics and later thread-safety analysis.
    pub transitively_borrowed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosureInfo {
    pub closure_id: u32,
    pub span: Span,
    pub capture_mode: CaptureMode,
    /// MVP closure values are consuming callables.
    pub is_fn_once: bool,
    pub captures: Vec<ClosureCapture>,
    pub escapes: Vec<ClosureEscape>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClosureAnalysis {
    pub closures: Vec<ClosureInfo>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone)]
struct Binding {
    id: u32,
    name: String,
    declaration_span: Span,
    declared_type: Option<TypeExpr>,
    resolved_type: Option<Type>,
    declaration_order: u32,
    closure_values: Vec<u32>,
}

#[derive(Debug, Clone)]
struct CapturedBinding {
    binding: Binding,
    first_use_span: Span,
}

struct WalkState {
    scopes: Vec<Vec<Binding>>,
    outer_visible: HashMap<String, Binding>,
    captures: HashMap<u32, CapturedBinding>,
    recursive_owner: Option<(String, Option<u32>)>,
}

impl WalkState {
    fn top_level() -> Self {
        Self {
            scopes: vec![Vec::new()],
            outer_visible: HashMap::new(),
            captures: HashMap::new(),
            recursive_owner: None,
        }
    }

    fn for_closure(
        outer_visible: HashMap<String, Binding>,
        recursive_owner: Option<(String, Option<u32>)>,
    ) -> Self {
        Self {
            scopes: vec![Vec::new()],
            outer_visible,
            captures: HashMap::new(),
            recursive_owner,
        }
    }

    fn push_scope(&mut self) {
        self.scopes.push(Vec::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn declare(&mut self, binding: Binding) {
        self.scopes
            .last_mut()
            .expect("closure analysis always has a scope")
            .push(binding);
    }

    fn local_binding(&self, name: &str) -> Option<&Binding> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.iter().rev())
            .find(|binding| binding.name == name)
    }

    fn local_binding_mut(&mut self, name: &str) -> Option<&mut Binding> {
        self.scopes
            .iter_mut()
            .rev()
            .flat_map(|scope| scope.iter_mut().rev())
            .find(|binding| binding.name == name)
    }

    fn visible_binding(&self, name: &str) -> Option<&Binding> {
        self.local_binding(name)
            .or_else(|| self.outer_visible.get(name))
    }

    fn contains_local_id(&self, id: u32) -> bool {
        self.scopes.iter().flatten().any(|binding| binding.id == id)
    }

    fn visible_bindings(&self) -> HashMap<String, Binding> {
        let mut visible = self.outer_visible.clone();
        for scope in &self.scopes {
            for binding in scope {
                visible.insert(binding.name.clone(), binding.clone());
            }
        }
        visible
    }

    fn capture(&mut self, binding: Binding, span: Span) {
        self.captures.entry(binding.id).or_insert(CapturedBinding {
            binding,
            first_use_span: span,
        });
    }

    fn absorb_nested_capture(&mut self, capture: &CapturedBinding) {
        if self.contains_local_id(capture.binding.id) {
            return;
        }
        if self
            .outer_visible
            .values()
            .any(|binding| binding.id == capture.binding.id)
        {
            self.capture(capture.binding.clone(), capture.first_use_span);
        }
    }

    fn ordered_captures(&self) -> Vec<CapturedBinding> {
        let mut captures: Vec<_> = self.captures.values().cloned().collect();
        captures.sort_by_key(|capture| capture.binding.declaration_order);
        captures
    }
}

struct Analyzer<'a> {
    resolver: &'a ResolverContext,
    next_binding_id: u32,
    next_declaration_order: u32,
    closures: Vec<ClosureInfo>,
    closure_spans: Vec<(Span, u32)>,
    moved_bindings: HashMap<u32, Span>,
    closure_calls: HashMap<u32, Span>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Analyzer<'a> {
    fn new(resolver: &'a ResolverContext) -> Self {
        Self {
            resolver,
            next_binding_id: 0,
            next_declaration_order: 0,
            closures: Vec::new(),
            closure_spans: Vec::new(),
            moved_bindings: HashMap::new(),
            closure_calls: HashMap::new(),
            diagnostics: Vec::new(),
        }
    }

    fn binding(
        &mut self,
        name: &str,
        span: Span,
        declared_type: Option<&TypeExpr>,
        inferred_type: Option<Type>,
        closure_values: Vec<u32>,
    ) -> Binding {
        let resolved_type = declared_type
            .and_then(|ty| resolve_type_expr_to_type(ty, self.resolver))
            .or(inferred_type);
        let binding = Binding {
            id: self.next_binding_id,
            name: name.to_string(),
            declaration_span: span,
            declared_type: declared_type.cloned(),
            resolved_type,
            declaration_order: self.next_declaration_order,
            closure_values,
        };
        self.next_binding_id += 1;
        self.next_declaration_order += 1;
        binding
    }

    fn declare_param(&mut self, state: &mut WalkState, param: &Param) {
        let binding = self.binding(
            &param.name.0,
            param.span,
            param.ty.as_ref(),
            None,
            Vec::new(),
        );
        state.declare(binding);
    }

    fn check_available(&mut self, binding: &Binding, span: Span) {
        if self.moved_bindings.contains_key(&binding.id) {
            let ty = binding
                .resolved_type
                .as_ref()
                .map(type_label)
                .map(|ty| format!(" of type `{ty}`"))
                .unwrap_or_default();
            self.diagnostics.push(Diagnostic::error(
                format!("use of moved value `{}`{ty}", binding.name),
                Some(span),
            ));
        }
    }

    fn reference_name(&mut self, state: &mut WalkState, name: &str, span: Span) {
        if let Some(binding) = state.local_binding(name).cloned() {
            self.check_available(&binding, span);
            return;
        }
        if let Some(binding) = state.outer_visible.get(name).cloned() {
            self.check_available(&binding, span);
            state.capture(binding, span);
            return;
        }
        if state
            .recursive_owner
            .as_ref()
            .is_some_and(|(owner, _)| owner == name)
        {
            self.diagnostics.push(Diagnostic::error(
                format!(
                    "recursive closure cycle through `{name}` is not supported; initialize a non-recursive callable instead"
                ),
                Some(span),
            ));
        }
        // Function items, constants, enum constructors, and ordinary
        // unresolved names are not lexical captures. The resolver/lowerer
        // reports unknown symbols separately.
    }

    fn visit_block(
        &mut self,
        block: &Block,
        state: &mut WalkState,
        scoped: bool,
        tail_returns: bool,
    ) {
        if scoped {
            state.push_scope();
        }
        let last = block.stmts.len().saturating_sub(1);
        for (index, stmt) in block.stmts.iter().enumerate() {
            self.visit_stmt(stmt, state, tail_returns && index == last);
        }
        if scoped {
            state.pop_scope();
        }
    }

    fn visit_stmt(&mut self, stmt: &Stmt, state: &mut WalkState, implicit_return: bool) {
        match stmt {
            Stmt::Expr(expr, span) => {
                self.visit_expr(expr, state, None);
                if implicit_return {
                    self.mark_escape(expr, state, ClosureEscapeKind::Return, *span);
                }
            }
            Stmt::Ret(expr, span) => {
                if let Some(expr) = expr {
                    self.visit_expr(expr, state, None);
                    self.mark_escape(expr, state, ClosureEscapeKind::Return, *span);
                }
            }
            Stmt::Let {
                name,
                ty,
                value,
                span,
                ..
            } => {
                // The initializer is evaluated before the binding enters
                // scope. Passing the upcoming name lets us reject `let f =
                // () -> f()` rather than treating it as a function item.
                let owner = Some((name.0.clone(), None));
                if let Some(value) = value {
                    self.visit_expr(value, state, owner);
                }
                let closure_values = value
                    .as_ref()
                    .map(|value| self.closure_values(value, state))
                    .unwrap_or_default();
                let inferred_type = value
                    .as_ref()
                    .and_then(|value| self.infer_expr_type(value, state));
                let binding =
                    self.binding(&name.0, *span, ty.as_ref(), inferred_type, closure_values);
                state.declare(binding);
            }
            Stmt::Assign {
                target,
                value,
                span,
            } => {
                self.visit_expr(target, state, None);
                let owner = match target {
                    Expr::Ident(name, _) => state
                        .visible_binding(&name.0)
                        .map(|binding| (name.0.clone(), Some(binding.id))),
                    _ => None,
                };
                self.visit_expr(value, state, owner);
                let closure_values = self.closure_values(value, state);
                match target {
                    Expr::Ident(name, _) => {
                        let inferred = self.infer_expr_type(value, state);
                        if let Some(binding) = state.local_binding_mut(&name.0) {
                            binding.closure_values = closure_values;
                            if binding.resolved_type.is_none() {
                                binding.resolved_type = inferred;
                            }
                        }
                    }
                    Expr::FieldAccess { .. } | Expr::Index { .. } => {
                        self.mark_escape(value, state, ClosureEscapeKind::Storage, *span);
                    }
                    _ => {}
                }
            }
            Stmt::Break(_) | Stmt::Continue(_) => {}
        }
    }

    fn visit_expr(
        &mut self,
        expr: &Expr,
        state: &mut WalkState,
        recursive_owner: Option<(String, Option<u32>)>,
    ) {
        match expr {
            Expr::Lit(_, _) => {}
            Expr::InterpString { segments, .. } => {
                for segment in segments {
                    if let InterpSegment::Expr(expr) = segment {
                        self.visit_expr(expr, state, None);
                    }
                }
            }
            Expr::Ident(name, span) => self.reference_name(state, &name.0, *span),
            Expr::Unary { expr, .. }
            | Expr::Try { expr, .. }
            | Expr::Cast { expr, .. }
            | Expr::Ref { expr, .. } => self.visit_expr(expr, state, None),
            Expr::Binary { lhs, rhs, .. } => {
                self.visit_expr(lhs, state, None);
                self.visit_expr(rhs, state, None);
            }
            Expr::Call {
                callee, args, span, ..
            } => {
                self.visit_expr(callee, state, None);
                self.record_calls(callee, state, *span);
                for arg in args {
                    self.visit_expr(arg, state, None);
                    self.mark_escape(arg, state, ClosureEscapeKind::Transfer, *span);
                }
            }
            Expr::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.visit_expr(cond, state, None);
                self.visit_block(then_block, state, true, false);
                if let Some(block) = else_block {
                    self.visit_block(block, state, true, false);
                }
            }
            Expr::Block(block) => self.visit_block(block, state, true, false),
            Expr::StructLit { fields, span, .. } => {
                for (_, value) in fields {
                    self.visit_expr(value, state, None);
                    self.mark_escape(value, state, ClosureEscapeKind::Storage, *span);
                }
            }
            Expr::FieldAccess { base, .. } => self.visit_expr(base, state, None),
            Expr::While { cond, body, .. } => {
                self.visit_expr(cond, state, None);
                self.visit_block(body, state, true, false);
            }
            Expr::For {
                var,
                start,
                end,
                body,
                span,
            } => {
                self.visit_expr(start, state, None);
                self.visit_expr(end, state, None);
                state.push_scope();
                let binding = self.binding(&var.0, *span, None, Some(Type::I32), Vec::new());
                state.declare(binding);
                self.visit_block(body, state, false, false);
                state.pop_scope();
            }
            Expr::ForIn {
                var,
                iter,
                body,
                span,
            } => {
                self.visit_expr(iter, state, None);
                let element_type = self.iter_element_type(iter, state);
                state.push_scope();
                let binding = self.binding(&var.0, *span, None, element_type, Vec::new());
                state.declare(binding);
                self.visit_block(body, state, false, false);
                state.pop_scope();
            }
            Expr::ArrayLit { elements, span } | Expr::Tuple { elements, span } => {
                for element in elements {
                    self.visit_expr(element, state, None);
                    self.mark_escape(element, state, ClosureEscapeKind::Storage, *span);
                }
            }
            Expr::Index { base, index, .. } => {
                self.visit_expr(base, state, None);
                self.visit_expr(index, state, None);
            }
            Expr::MethodCall {
                receiver,
                method,
                args,
                span,
            } => {
                self.visit_expr(receiver, state, None);
                let container_insert = matches!(
                    method.0.as_str(),
                    "push" | "insert" | "add" | "update" | "set"
                );
                for arg in args {
                    self.visit_expr(arg, state, None);
                    self.mark_escape(
                        arg,
                        state,
                        if container_insert {
                            ClosureEscapeKind::ContainerInsertion
                        } else {
                            ClosureEscapeKind::Transfer
                        },
                        *span,
                    );
                }
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.visit_expr(scrutinee, state, None);
                for arm in arms {
                    state.push_scope();
                    if let MatchPattern::Variant {
                        binding: Some(binding),
                        ..
                    } = &arm.pattern
                    {
                        let local = self.binding(&binding.0, arm.span, None, None, Vec::new());
                        state.declare(local);
                    }
                    self.visit_expr(&arm.expr, state, None);
                    state.pop_scope();
                }
            }
            Expr::Closure {
                capture,
                params,
                body,
                span,
            } => {
                let nested = self.analyze_closure(
                    *capture,
                    params,
                    body,
                    *span,
                    state.visible_bindings(),
                    recursive_owner,
                );
                for capture in &nested {
                    state.absorb_nested_capture(capture);
                }
            }
        }
    }

    fn analyze_closure(
        &mut self,
        capture_mode: CaptureMode,
        params: &[Param],
        body: &Expr,
        span: Span,
        outer_visible: HashMap<String, Binding>,
        recursive_owner: Option<(String, Option<u32>)>,
    ) -> Vec<CapturedBinding> {
        let closure_id = self.closures.len() as u32;
        let info_index = self.closures.len();
        self.closure_spans.push((span, closure_id));
        self.closures.push(ClosureInfo {
            closure_id,
            span,
            capture_mode,
            is_fn_once: true,
            captures: Vec::new(),
            escapes: Vec::new(),
        });

        let mut state = WalkState::for_closure(outer_visible, recursive_owner.clone());
        for param in params {
            self.declare_param(&mut state, param);
        }
        match body {
            Expr::Block(block) => self.visit_block(block, &mut state, true, true),
            _ => {
                self.visit_expr(body, &mut state, None);
                self.mark_escape(body, &state, ClosureEscapeKind::Return, span);
            }
        }

        let captures = state.ordered_captures();
        if let Some((owner_name, Some(owner_id))) = recursive_owner {
            if let Some(capture) = captures
                .iter()
                .find(|capture| capture.binding.id == owner_id)
            {
                self.diagnostics.push(Diagnostic::error(
                    format!(
                        "recursive closure cycle through `{owner_name}` is not supported in v1"
                    ),
                    Some(capture.first_use_span),
                ));
            }
        }

        let public_captures: Vec<_> = captures
            .iter()
            .map(|capture| {
                let borrowed =
                    capture.binding.resolved_type.as_ref().is_some_and(|ty| {
                        self.type_is_transitively_borrowed(ty, &mut HashSet::new())
                    });
                let ownership = if capture
                    .binding
                    .resolved_type
                    .as_ref()
                    .is_some_and(type_is_copy)
                {
                    CaptureOwnership::Copy
                } else {
                    // Unknown types are conservatively treated as owned. This
                    // prevents a later inferred aggregate from being aliased.
                    CaptureOwnership::Move
                };
                ClosureCapture {
                    binding_id: capture.binding.id,
                    name: capture.binding.name.clone(),
                    declaration_span: capture.binding.declaration_span,
                    first_use_span: capture.first_use_span,
                    declared_type: capture.binding.declared_type.clone(),
                    resolved_type: capture.binding.resolved_type.clone(),
                    ownership,
                    explicit_move: capture_mode == CaptureMode::Move,
                    transitively_borrowed: borrowed,
                }
            })
            .collect();

        for capture in &public_captures {
            if capture.ownership == CaptureOwnership::Move {
                self.moved_bindings
                    .entry(capture.binding_id)
                    .or_insert(span);
            }
        }
        self.closures[info_index].captures = public_captures;
        captures
    }

    fn closure_id_for_span(&self, span: Span) -> Option<u32> {
        self.closure_spans
            .iter()
            .find_map(|(candidate, id)| (*candidate == span).then_some(*id))
    }

    fn closure_values(&self, expr: &Expr, state: &WalkState) -> Vec<u32> {
        let mut values = match expr {
            Expr::Closure { span, .. } => self
                .closure_id_for_span(*span)
                .into_iter()
                .collect::<Vec<_>>(),
            Expr::Ident(name, _) => state
                .visible_binding(&name.0)
                .map(|binding| binding.closure_values.clone())
                .unwrap_or_default(),
            Expr::ArrayLit { elements, .. } | Expr::Tuple { elements, .. } => elements
                .iter()
                .flat_map(|element| self.closure_values(element, state))
                .collect(),
            Expr::StructLit { fields, .. } => fields
                .iter()
                .flat_map(|(_, value)| self.closure_values(value, state))
                .collect(),
            Expr::Cast { expr, .. } | Expr::Try { expr, .. } => self.closure_values(expr, state),
            Expr::If {
                then_block,
                else_block,
                ..
            } => {
                let mut values = self.block_closure_values(then_block, state);
                if let Some(else_block) = else_block {
                    values.extend(self.block_closure_values(else_block, state));
                }
                values
            }
            Expr::Block(block) => self.block_closure_values(block, state),
            Expr::Match { arms, .. } => arms
                .iter()
                .flat_map(|arm| self.closure_values(&arm.expr, state))
                .collect(),
            _ => Vec::new(),
        };
        values.sort_unstable();
        values.dedup();
        values
    }

    fn block_closure_values(&self, block: &Block, state: &WalkState) -> Vec<u32> {
        match block.stmts.last() {
            Some(Stmt::Expr(expr, _)) | Some(Stmt::Ret(Some(expr), _)) => {
                self.closure_values(expr, state)
            }
            _ => Vec::new(),
        }
    }

    fn mark_escape(&mut self, expr: &Expr, state: &WalkState, kind: ClosureEscapeKind, span: Span) {
        for closure_id in self.closure_values(expr, state) {
            let info = &mut self.closures[closure_id as usize];
            let escape = ClosureEscape { kind, span };
            if !info.escapes.contains(&escape) {
                info.escapes.push(escape);
            }
        }
    }

    fn record_calls(&mut self, callee: &Expr, state: &WalkState, span: Span) {
        for closure_id in self.closure_values(callee, state) {
            if self.closure_calls.insert(closure_id, span).is_some() {
                let name = match callee {
                    Expr::Ident(name, _) => format!(" `{}`", name.0),
                    _ => String::new(),
                };
                self.diagnostics.push(Diagnostic::error(
                    format!("use of moved FnOnce closure{name}; closure values can be called once"),
                    Some(span),
                ));
            }
        }
    }

    fn infer_expr_type(&self, expr: &Expr, state: &WalkState) -> Option<Type> {
        match expr {
            Expr::Lit(literal, _) => Some(match literal {
                Literal::Int(_) => Type::I32,
                Literal::Float(_) => Type::F64,
                Literal::Bool(_) => Type::Bool,
                Literal::Str(_) => Type::Str,
                Literal::Char(_) => Type::Char,
            }),
            Expr::InterpString { .. } => Some(Type::String),
            Expr::Ident(name, _) => state
                .visible_binding(&name.0)
                .and_then(|binding| binding.resolved_type.clone()),
            Expr::Unary { expr, .. } | Expr::Try { expr, .. } => self.infer_expr_type(expr, state),
            Expr::Ref {
                expr, mutability, ..
            } => self
                .infer_expr_type(expr, state)
                .map(|ty| Type::Ref(Box::new(ty), *mutability)),
            Expr::Cast { target, .. } => resolve_type_expr_to_type(target, self.resolver),
            Expr::Binary { op, lhs, .. } => match op {
                BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::Lt
                | BinaryOp::Le
                | BinaryOp::Gt
                | BinaryOp::Ge
                | BinaryOp::And
                | BinaryOp::Or => Some(Type::Bool),
                _ => self.infer_expr_type(lhs, state),
            },
            Expr::ArrayLit { elements, .. } => {
                let first = elements.first()?;
                Some(Type::Array(
                    Box::new(self.infer_expr_type(first, state)?),
                    elements.len(),
                ))
            }
            Expr::Tuple { elements, .. } => elements
                .iter()
                .map(|element| self.infer_expr_type(element, state))
                .collect::<Option<Vec<_>>>()
                .map(Type::Tuple),
            Expr::StructLit { name, .. } => Some(Type::Named(name.0.clone())),
            Expr::Index { base, .. } => match self.infer_expr_type(base, state)? {
                Type::Array(element, _) => Some(*element),
                Type::App { base, mut args } if base == "Vec" && args.len() == 1 => args.pop(),
                _ => None,
            },
            Expr::FieldAccess { base, field, .. } => {
                let Type::Named(name) = self.infer_expr_type(base, state)? else {
                    return None;
                };
                self.resolver
                    .struct_types
                    .get(&name)?
                    .fields
                    .iter()
                    .find_map(|(candidate, ty)| (candidate == &field.0).then(|| ty.clone()))
            }
            Expr::Call { callee, args, .. } => {
                if let Some(Type::Function { ret, .. }) = self.infer_expr_type(callee, state) {
                    return Some(*ret);
                }
                if let Expr::Ident(name, _) = callee.as_ref() {
                    let argument = args
                        .first()
                        .and_then(|arg| self.infer_expr_type(arg, state));
                    match name.0.as_str() {
                        "Arc::new"
                            if matches!(
                                self.resolver.resolve_symbol("Arc"),
                                Some(crate::resolver::ResolvedSymbol::Struct(module, symbol))
                                    if module == "std/sync" && symbol == "Arc"
                            ) =>
                        {
                            return argument.map(Type::arc);
                        }
                        "Mutex::new"
                            if matches!(
                                self.resolver.resolve_symbol("Mutex"),
                                Some(crate::resolver::ResolvedSymbol::Struct(module, symbol))
                                    if module == "std/sync" && symbol == "Mutex"
                            ) =>
                        {
                            return argument.map(Type::mutex);
                        }
                        "Own::new" => return argument.map(|ty| Type::Own(Box::new(ty))),
                        "Shared::new" => {
                            return argument.map(|ty| Type::Shared(Box::new(ty)));
                        }
                        atomic if atomic.ends_with("::new") => {
                            let atomic = atomic.trim_end_matches("::new");
                            if let Some(ty) = Type::from_name(atomic)
                                && matches!(ty, Type::Atomic(_))
                            {
                                return Some(ty);
                            }
                        }
                        _ => {}
                    }
                }
                if let Expr::FieldAccess { base, field, .. } = callee.as_ref() {
                    if matches!(base.as_ref(), Expr::Ident(name, _) if name.0 == "String")
                        && matches!(field.0.as_str(), "from_str" | "with_capacity")
                    {
                        return Some(Type::String);
                    }
                    if let Expr::Ident(name, _) = base.as_ref() {
                        let argument = args
                            .first()
                            .and_then(|arg| self.infer_expr_type(arg, state));
                        match (name.0.as_str(), field.0.as_str()) {
                            ("Arc", "new")
                                if matches!(
                                    self.resolver.resolve_symbol("Arc"),
                                    Some(crate::resolver::ResolvedSymbol::Struct(module, symbol))
                                        if module == "std/sync" && symbol == "Arc"
                                ) =>
                            {
                                return argument.map(Type::arc);
                            }
                            ("Mutex", "new")
                                if matches!(
                                    self.resolver.resolve_symbol("Mutex"),
                                    Some(crate::resolver::ResolvedSymbol::Struct(module, symbol))
                                        if module == "std/sync" && symbol == "Mutex"
                                ) =>
                            {
                                return argument.map(Type::mutex);
                            }
                            ("Own", "new") => return argument.map(|ty| Type::Own(Box::new(ty))),
                            ("Shared", "new") => {
                                return argument.map(|ty| Type::Shared(Box::new(ty)));
                            }
                            (atomic, "new") => {
                                if let Some(ty) = Type::from_name(atomic)
                                    && matches!(ty, Type::Atomic(_))
                                {
                                    return Some(ty);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                None
            }
            Expr::Closure { params, body, .. } => {
                let params = params
                    .iter()
                    .map(|param| {
                        param
                            .ty
                            .as_ref()
                            .and_then(|ty| resolve_type_expr_to_type(ty, self.resolver))
                    })
                    .collect::<Option<Vec<_>>>()?;
                let ret = self.infer_expr_type(body, state)?;
                Some(Type::Function {
                    params,
                    ret: Box::new(ret),
                })
            }
            Expr::MethodCall {
                receiver, method, ..
            } => {
                let receiver_ty = self.infer_expr_type(receiver, state)?;
                if let Some(inner) = receiver_ty.arc_inner_type().cloned() {
                    return match method.0.as_str() {
                        "clone" => Some(receiver_ty),
                        "borrow" => Some(Type::Ref(
                            Box::new(inner),
                            glyph_core::types::Mutability::Immutable,
                        )),
                        _ => None,
                    };
                }
                let mutex_inner = receiver_ty.mutex_inner_type().cloned().or_else(|| {
                    if let Type::Ref(inner, _) = &receiver_ty {
                        inner.mutex_inner_type().cloned()
                    } else {
                        None
                    }
                });
                if let Some(inner) = mutex_inner {
                    return match method.0.as_str() {
                        "lock" => Some(Type::mutex_guard(inner)),
                        "try_lock" => Some(Type::App {
                            base: "Option".into(),
                            args: vec![Type::mutex_guard(inner)],
                        }),
                        _ => None,
                    };
                }
                if let Some(inner) = receiver_ty.mutex_guard_inner_type().cloned() {
                    return match method.0.as_str() {
                        "borrow" | "borrow_mut" => Some(Type::Ref(
                            Box::new(inner),
                            glyph_core::types::Mutability::Mutable,
                        )),
                        _ => None,
                    };
                }
                None
            }
            Expr::If { .. }
            | Expr::Block(_)
            | Expr::While { .. }
            | Expr::For { .. }
            | Expr::ForIn { .. }
            | Expr::Match { .. } => None,
        }
    }

    fn iter_element_type(&self, iter: &Expr, state: &WalkState) -> Option<Type> {
        match self.infer_expr_type(iter, state)? {
            Type::Array(element, _) => Some(*element),
            Type::App { base, mut args } if base == "Vec" && args.len() == 1 => args.pop(),
            _ => None,
        }
    }

    fn type_is_transitively_borrowed(&self, ty: &Type, visiting: &mut HashSet<String>) -> bool {
        match ty {
            Type::Str | Type::Ref(..) => true,
            Type::Array(inner, _)
            | Type::Own(inner)
            | Type::RawPtr(inner)
            | Type::Shared(inner) => self.type_is_transitively_borrowed(inner, visiting),
            Type::App { args, .. } | Type::Tuple(args) => args
                .iter()
                .any(|arg| self.type_is_transitively_borrowed(arg, visiting)),
            Type::Function { params, ret } => {
                params
                    .iter()
                    .any(|param| self.type_is_transitively_borrowed(param, visiting))
                    || self.type_is_transitively_borrowed(ret, visiting)
            }
            Type::Named(name) => {
                if !visiting.insert(name.clone()) {
                    return false;
                }
                let borrowed =
                    self.resolver
                        .struct_types
                        .get(name)
                        .is_some_and(|structure| {
                            structure.fields.iter().any(|(_, field)| {
                                self.type_is_transitively_borrowed(field, visiting)
                            })
                        });
                visiting.remove(name);
                borrowed
            }
            _ => false,
        }
    }

    fn finish(mut self) -> ClosureAnalysis {
        for closure in &self.closures {
            let Some(escape) = closure.escapes.first() else {
                continue;
            };
            for capture in closure
                .captures
                .iter()
                .filter(|capture| capture.transitively_borrowed)
            {
                self.diagnostics.push(Diagnostic::error(
                    format!(
                        "borrowed capture `{}` cannot escape through {}; capture owned data instead",
                        capture.name,
                        escape_label(escape.kind)
                    ),
                    Some(capture.first_use_span),
                ));
            }
        }
        ClosureAnalysis {
            closures: self.closures,
            diagnostics: self.diagnostics,
        }
    }
}

fn type_is_copy(ty: &Type) -> bool {
    match ty {
        Type::I8
        | Type::I32
        | Type::I64
        | Type::U8
        | Type::U32
        | Type::U64
        | Type::Usize
        | Type::F32
        | Type::F64
        | Type::Bool
        | Type::Char
        | Type::Str
        | Type::Ref(..)
        | Type::RawPtr(_) => true,
        Type::Array(element, _) => type_is_copy(element),
        Type::Tuple(elements) => elements.iter().all(type_is_copy),
        _ => false,
    }
}

fn type_label(ty: &Type) -> String {
    match ty {
        Type::I8 => "i8".into(),
        Type::I32 => "i32".into(),
        Type::I64 => "i64".into(),
        Type::U8 => "u8".into(),
        Type::U32 => "u32".into(),
        Type::U64 => "u64".into(),
        Type::Usize => "usize".into(),
        Type::F32 => "f32".into(),
        Type::F64 => "f64".into(),
        Type::Bool => "bool".into(),
        Type::Char => "char".into(),
        Type::Str => "str".into(),
        Type::String => "String".into(),
        Type::Void => "()".into(),
        Type::Named(name) | Type::Enum(name) | Type::Param(name) => name.clone(),
        Type::App { base, args } => format!(
            "{}<{}>",
            base,
            args.iter().map(type_label).collect::<Vec<_>>().join(", ")
        ),
        Type::Ref(inner, _) => format!("&{}", type_label(inner)),
        Type::Array(inner, size) => format!("[{}; {}]", type_label(inner), size),
        Type::Own(inner) => format!("Own<{}>", type_label(inner)),
        Type::RawPtr(inner) => format!("RawPtr<{}>", type_label(inner)),
        Type::Shared(inner) => format!("Shared<{}>", type_label(inner)),
        Type::Atomic(scalar) => scalar.type_name().into(),
        Type::Function { params, ret } => format!(
            "FnOnce<({}), {}>",
            params.iter().map(type_label).collect::<Vec<_>>().join(", "),
            type_label(ret)
        ),
        Type::Tuple(elements) => format!(
            "({})",
            elements
                .iter()
                .map(type_label)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn escape_label(kind: ClosureEscapeKind) -> &'static str {
    match kind {
        ClosureEscapeKind::Return => "return",
        ClosureEscapeKind::Storage => "storage",
        ClosureEscapeKind::ContainerInsertion => "container insertion",
        ClosureEscapeKind::Transfer => "a transfer site",
    }
}

/// Run closure analysis with resolved type metadata.
pub fn analyze_function_closure_ownership(
    function: &Function,
    resolver: &ResolverContext,
) -> ClosureAnalysis {
    let mut analyzer = Analyzer::new(resolver);
    let mut state = WalkState::top_level();
    for param in &function.params {
        analyzer.declare_param(&mut state, param);
    }
    analyzer.visit_block(&function.body, &mut state, false, true);
    analyzer.finish()
}

/// Compatibility entry point for lexical-only callers. Prefer
/// `analyze_function_closure_ownership` when a resolver is available.
pub fn analyze_function_closures(function: &Function) -> Vec<ClosureInfo> {
    analyze_function_closure_ownership(function, &ResolverContext::default()).closures
}

#[cfg(test)]
mod tests {
    use glyph_core::ast::{Item, TypeExpr};

    use super::{CaptureOwnership, ClosureEscapeKind, analyze_function_closure_ownership};
    use crate::{lex, parse, resolve_types};

    fn analyze(source: &str) -> super::ClosureAnalysis {
        let lexed = lex(source);
        assert!(lexed.diagnostics.is_empty(), "{:?}", lexed.diagnostics);
        let parsed = parse(&lexed.tokens, source);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let (resolver, diagnostics) = resolve_types(&parsed.module);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let function = parsed
            .module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Function(function) => Some(function),
                _ => None,
            })
            .expect("expected function");
        analyze_function_closure_ownership(function, &resolver)
    }

    #[test]
    fn captures_are_deduplicated_in_declaration_order_with_inferred_types() {
        let analysis = analyze(
            "fn main() { let first = 1; let second: i32 = 2; let f = x -> second + first + second + x }",
        );
        let captures = &analysis.closures[0].captures;
        assert_eq!(
            captures
                .iter()
                .map(|capture| capture.name.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(
            captures[0].resolved_type,
            Some(glyph_core::types::Type::I32)
        );
        assert!(matches!(
            captures[1].declared_type,
            Some(TypeExpr::Path { .. })
        ));
        assert!(
            captures
                .iter()
                .all(|capture| capture.ownership == CaptureOwnership::Copy)
        );
    }

    #[test]
    fn parameters_and_inner_bindings_shadow_outer_locals() {
        let analysis = analyze(
            "fn main() { let value = 1; let f = value -> { let inner = value; let value = 3; inner + value } }",
        );
        assert!(analysis.closures[0].captures.is_empty());
    }

    #[test]
    fn initializer_can_capture_the_binding_shadowed_by_its_let() {
        let analysis =
            analyze("fn main() { let value = 1; let f = () -> { let value = value + 1; value } }");
        assert_eq!(analysis.closures[0].captures[0].name, "value");
    }

    #[test]
    fn nested_closure_captures_propagate_to_the_parent_environment() {
        let analysis = analyze(
            "fn main() { let outside = 1; let outer = p -> { let inner = q -> outside + p + q; inner } }",
        );
        assert_eq!(analysis.closures.len(), 2);
        assert_eq!(
            analysis.closures[0]
                .captures
                .iter()
                .map(|capture| capture.name.as_str())
                .collect::<Vec<_>>(),
            ["outside"]
        );
        assert_eq!(
            analysis.closures[1]
                .captures
                .iter()
                .map(|capture| capture.name.as_str())
                .collect::<Vec<_>>(),
            ["outside", "p"]
        );
    }

    #[test]
    fn owned_capture_moves_source_but_copy_capture_does_not() {
        let analysis = analyze(
            r#"
fn main() {
  let count = 1
  let owned: String
  let f = move () -> count
  let g = () -> owned
  let still_available = count
  let moved = owned
}
"#,
        );
        assert_eq!(
            analysis.closures[0].captures[0].ownership,
            CaptureOwnership::Copy
        );
        assert!(analysis.closures[0].captures[0].explicit_move);
        assert_eq!(
            analysis.closures[1].captures[0].ownership,
            CaptureOwnership::Move
        );
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("use of moved value `owned`"))
        );
        assert!(
            !analysis
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("use of moved value `count`"))
        );
    }

    #[test]
    fn borrowed_capture_is_rejected_only_when_closure_escapes() {
        let local = analyze(
            "fn main() { let borrowed: str = \"hi\"; let f = () -> borrowed; let value = f() }",
        );
        assert!(local.diagnostics.is_empty(), "{:?}", local.diagnostics);

        let escaped =
            analyze("fn main() { let borrowed: str = \"hi\"; let f = () -> borrowed; ret f }");
        assert!(
            escaped.closures[0]
                .escapes
                .iter()
                .any(|escape| escape.kind == ClosureEscapeKind::Return)
        );
        assert!(escaped.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("borrowed capture `borrowed` cannot escape through return")
        }));
    }

    #[test]
    fn escape_analysis_tracks_storage_container_and_transfer_sites() {
        let cases = [
            (
                "fn main() { let borrowed: str = \"hi\"; let f = () -> borrowed; let stored = (f,) }",
                ClosureEscapeKind::Storage,
                "storage",
            ),
            (
                "fn main() { let borrowed: str = \"hi\"; let f = () -> borrowed; let callbacks; callbacks.push(f) }",
                ClosureEscapeKind::ContainerInsertion,
                "container insertion",
            ),
            (
                "fn main() { let borrowed: str = \"hi\"; let f = () -> borrowed; consume(f) }",
                ClosureEscapeKind::Transfer,
                "transfer site",
            ),
        ];
        for (source, kind, message) in cases {
            let analysis = analyze(source);
            assert!(
                analysis.closures[0]
                    .escapes
                    .iter()
                    .any(|escape| escape.kind == kind)
            );
            assert!(
                analysis
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.message.contains(message)),
                "{:?}",
                analysis.diagnostics
            );
        }
    }

    #[test]
    fn return_escape_propagates_through_if_and_match_values() {
        for source in [
            "fn main() { let borrowed: str = \"hi\"; let f = () -> borrowed; ret if true { f } else { f } }",
            "fn main() { let borrowed: str = \"hi\"; let f = () -> borrowed; ret match choice { Some(_) => f, None => f } }",
        ] {
            let analysis = analyze(source);
            assert!(analysis.diagnostics.iter().any(|diagnostic| {
                diagnostic
                    .message
                    .contains("borrowed capture `borrowed` cannot escape through return")
            }));
        }
    }

    #[test]
    fn borrowed_provenance_is_structural_and_names_the_exact_capture() {
        let analysis = analyze(
            r#"
struct View { text: str }
fn main() {
  let view: View
  let f = () -> view
  ret f
}
"#,
        );
        assert!(analysis.closures[0].captures[0].transitively_borrowed);
        assert!(analysis.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("borrowed capture `view` cannot escape")
        }));
    }

    #[test]
    fn method_self_and_field_access_resolve_to_the_self_binding() {
        let source = r#"
struct Accumulator {
  total: i32
  fn callback(self: &Accumulator) -> FnOnce<(), i32> {
    ret () -> self.total
  }
}
"#;
        let lexed = lex(source);
        let parsed = parse(&lexed.tokens, source);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let (resolver, diagnostics) = resolve_types(&parsed.module);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let Item::Struct(structure) = &parsed.module.items[0] else {
            panic!("expected struct")
        };
        let analysis = analyze_function_closure_ownership(&structure.methods[0], &resolver);
        let capture = &analysis.closures[0].captures[0];
        assert_eq!(capture.name, "self");
        assert_eq!(capture.ownership, CaptureOwnership::Copy);
        assert!(capture.transitively_borrowed);
        assert!(analysis.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("borrowed capture `self` cannot escape through return")
        }));
    }

    #[test]
    fn direct_and_assigned_recursive_closures_are_rejected() {
        for source in [
            "fn main() { let f = () -> f() }",
            "fn main() { let mut f: FnOnce<(), i32>; f = () -> f() }",
        ] {
            let analysis = analyze(source);
            assert!(analysis.diagnostics.iter().any(|diagnostic| {
                diagnostic
                    .message
                    .contains("recursive closure cycle through `f`")
            }));
        }
    }

    #[test]
    fn second_fnonce_call_is_rejected() {
        let analysis = analyze(
            "fn main() { let value = 1; let f = () -> value; let one = f(); let two = f() }",
        );
        assert!(analysis.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("closure values can be called once")
        }));
    }

    #[test]
    fn loop_and_match_bindings_are_local_to_the_closure() {
        let analysis = analyze(
            r#"
fn main() {
  let values = [1, 2]
  let f = () -> {
    for value in values { let copy = value }
    match item { Some(payload) => payload, None => 0 }
  }
}
"#,
        );
        assert_eq!(
            analysis.closures[0]
                .captures
                .iter()
                .map(|capture| capture.name.as_str())
                .collect::<Vec<_>>(),
            ["values"]
        );
    }
}
