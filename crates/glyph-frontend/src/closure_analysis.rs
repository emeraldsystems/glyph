//! Lexical free-variable discovery for closure conversion.
//!
//! This pass intentionally answers only the lexical question: which local
//! bindings must a closure environment carry? Ownership classification and
//! cross-thread `Send` policy consume this metadata in later passes.

use std::collections::HashMap;

use glyph_core::{
    ast::{Block, CaptureMode, Expr, Function, InterpSegment, MatchPattern, Param, Stmt, TypeExpr},
    span::Span,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosureCapture {
    pub binding_id: u32,
    pub name: String,
    pub declaration_span: Span,
    pub first_use_span: Span,
    pub declared_type: Option<TypeExpr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosureInfo {
    pub span: Span,
    pub capture_mode: CaptureMode,
    pub captures: Vec<ClosureCapture>,
}

#[derive(Debug, Clone)]
struct Binding {
    id: u32,
    name: String,
    declaration_span: Span,
    declared_type: Option<TypeExpr>,
    declaration_order: u32,
}

#[derive(Debug, Clone)]
struct CapturedBinding {
    binding: Binding,
    first_use_span: Span,
}

#[derive(Default)]
struct WalkState {
    scopes: Vec<Vec<Binding>>,
    outer_visible: HashMap<String, Binding>,
    captures: HashMap<u32, CapturedBinding>,
}

impl WalkState {
    fn top_level() -> Self {
        Self {
            scopes: vec![Vec::new()],
            ..Self::default()
        }
    }

    fn for_closure(outer_visible: HashMap<String, Binding>) -> Self {
        Self {
            scopes: vec![Vec::new()],
            outer_visible,
            captures: HashMap::new(),
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
            .expect("walk state always has a scope")
            .push(binding);
    }

    fn local_binding(&self, name: &str) -> Option<&Binding> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|scope| scope.iter().rev())
            .find(|binding| binding.name == name)
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

    fn reference_name(&mut self, name: &str, span: Span) {
        if self.local_binding(name).is_some() {
            return;
        }
        let Some(binding) = self.outer_visible.get(name).cloned() else {
            // Function items, constants, enum constructors, and unresolved
            // names are not lexical captures.
            return;
        };
        self.capture(binding, span);
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

    fn capture(&mut self, binding: Binding, span: Span) {
        self.captures.entry(binding.id).or_insert(CapturedBinding {
            binding,
            first_use_span: span,
        });
    }

    fn ordered_captures(&self) -> Vec<CapturedBinding> {
        let mut captures: Vec<_> = self.captures.values().cloned().collect();
        captures.sort_by_key(|capture| capture.binding.declaration_order);
        captures
    }
}

#[derive(Default)]
struct Analyzer {
    next_binding_id: u32,
    next_declaration_order: u32,
    closures: Vec<ClosureInfo>,
}

impl Analyzer {
    fn binding(&mut self, name: &str, span: Span, declared_type: Option<&TypeExpr>) -> Binding {
        let binding = Binding {
            id: self.next_binding_id,
            name: name.to_string(),
            declaration_span: span,
            declared_type: declared_type.cloned(),
            declaration_order: self.next_declaration_order,
        };
        self.next_binding_id += 1;
        self.next_declaration_order += 1;
        binding
    }

    fn declare_param(&mut self, state: &mut WalkState, param: &Param) {
        state.declare(self.binding(&param.name.0, param.span, param.ty.as_ref()));
    }

    fn visit_block(&mut self, block: &Block, state: &mut WalkState, scoped: bool) {
        if scoped {
            state.push_scope();
        }
        for stmt in &block.stmts {
            self.visit_stmt(stmt, state);
        }
        if scoped {
            state.pop_scope();
        }
    }

    fn visit_stmt(&mut self, stmt: &Stmt, state: &mut WalkState) {
        match stmt {
            Stmt::Expr(expr, _) => self.visit_expr(expr, state),
            Stmt::Ret(expr, _) => {
                if let Some(expr) = expr {
                    self.visit_expr(expr, state);
                }
            }
            Stmt::Let {
                name,
                ty,
                value,
                span,
                ..
            } => {
                // The initializer is evaluated before the new binding enters
                // scope, so `let x = x` can capture an outer `x`.
                if let Some(value) = value {
                    self.visit_expr(value, state);
                }
                state.declare(self.binding(&name.0, *span, ty.as_ref()));
            }
            Stmt::Assign { target, value, .. } => {
                self.visit_expr(target, state);
                self.visit_expr(value, state);
            }
            Stmt::Break(_) | Stmt::Continue(_) => {}
        }
    }

    fn visit_expr(&mut self, expr: &Expr, state: &mut WalkState) {
        match expr {
            Expr::Lit(_, _) => {}
            Expr::InterpString { segments, .. } => {
                for segment in segments {
                    if let InterpSegment::Expr(expr) = segment {
                        self.visit_expr(expr, state);
                    }
                }
            }
            Expr::Ident(name, span) => state.reference_name(&name.0, *span),
            Expr::Unary { expr, .. }
            | Expr::Try { expr, .. }
            | Expr::Cast { expr, .. }
            | Expr::Ref { expr, .. } => self.visit_expr(expr, state),
            Expr::Binary { lhs, rhs, .. } => {
                self.visit_expr(lhs, state);
                self.visit_expr(rhs, state);
            }
            Expr::Call { callee, args, .. } => {
                self.visit_expr(callee, state);
                for arg in args {
                    self.visit_expr(arg, state);
                }
            }
            Expr::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.visit_expr(cond, state);
                self.visit_block(then_block, state, true);
                if let Some(block) = else_block {
                    self.visit_block(block, state, true);
                }
            }
            Expr::Block(block) => self.visit_block(block, state, true),
            Expr::StructLit { fields, .. } => {
                for (_, value) in fields {
                    self.visit_expr(value, state);
                }
            }
            Expr::FieldAccess { base, .. } => self.visit_expr(base, state),
            Expr::While { cond, body, .. } => {
                self.visit_expr(cond, state);
                self.visit_block(body, state, true);
            }
            Expr::For {
                var,
                start,
                end,
                body,
                span,
            } => {
                self.visit_expr(start, state);
                self.visit_expr(end, state);
                state.push_scope();
                state.declare(self.binding(&var.0, *span, None));
                self.visit_block(body, state, false);
                state.pop_scope();
            }
            Expr::ForIn {
                var,
                iter,
                body,
                span,
            } => {
                self.visit_expr(iter, state);
                state.push_scope();
                state.declare(self.binding(&var.0, *span, None));
                self.visit_block(body, state, false);
                state.pop_scope();
            }
            Expr::ArrayLit { elements, .. } | Expr::Tuple { elements, .. } => {
                for element in elements {
                    self.visit_expr(element, state);
                }
            }
            Expr::Index { base, index, .. } => {
                self.visit_expr(base, state);
                self.visit_expr(index, state);
            }
            Expr::MethodCall { receiver, args, .. } => {
                self.visit_expr(receiver, state);
                for arg in args {
                    self.visit_expr(arg, state);
                }
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.visit_expr(scrutinee, state);
                for arm in arms {
                    state.push_scope();
                    if let MatchPattern::Variant {
                        binding: Some(binding),
                        ..
                    } = &arm.pattern
                    {
                        state.declare(self.binding(&binding.0, arm.span, None));
                    }
                    self.visit_expr(&arm.expr, state);
                    state.pop_scope();
                }
            }
            Expr::Closure {
                capture,
                params,
                body,
                span,
            } => {
                let nested =
                    self.analyze_closure(*capture, params, body, *span, state.visible_bindings());
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
    ) -> Vec<CapturedBinding> {
        // Reserve the slot so the public result stays in source/preorder even
        // though nested closures complete analysis before their parent.
        let info_index = self.closures.len();
        self.closures.push(ClosureInfo {
            span,
            capture_mode,
            captures: Vec::new(),
        });

        let mut state = WalkState::for_closure(outer_visible);
        for param in params {
            self.declare_param(&mut state, param);
        }
        self.visit_expr(body, &mut state);

        let captures = state.ordered_captures();
        self.closures[info_index].captures = captures
            .iter()
            .map(|capture| ClosureCapture {
                binding_id: capture.binding.id,
                name: capture.binding.name.clone(),
                declaration_span: capture.binding.declaration_span,
                first_use_span: capture.first_use_span,
                declared_type: capture.binding.declared_type.clone(),
            })
            .collect();
        captures
    }
}

pub fn analyze_function_closures(function: &Function) -> Vec<ClosureInfo> {
    let mut analyzer = Analyzer::default();
    let mut state = WalkState::top_level();
    for param in &function.params {
        analyzer.declare_param(&mut state, param);
    }
    analyzer.visit_block(&function.body, &mut state, false);
    analyzer.closures
}

#[cfg(test)]
mod tests {
    use glyph_core::ast::{Item, TypeExpr};

    use super::analyze_function_closures;
    use crate::{lex, parse};

    fn analyze(source: &str) -> Vec<super::ClosureInfo> {
        let lexed = lex(source);
        assert!(lexed.diagnostics.is_empty(), "{:?}", lexed.diagnostics);
        let parsed = parse(&lexed.tokens, source);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let Item::Function(function) = &parsed.module.items[0] else {
            panic!("expected function")
        };
        analyze_function_closures(function)
    }

    #[test]
    fn captures_are_deduplicated_in_declaration_order() {
        let infos = analyze(
            "fn main() { let first: i32 = 1; let second: i32 = 2; let f = x -> second + first + second + x }",
        );
        assert_eq!(infos.len(), 1);
        assert_eq!(
            infos[0]
                .captures
                .iter()
                .map(|capture| capture.name.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert!(matches!(
            infos[0].captures[0].declared_type,
            Some(TypeExpr::Path { .. })
        ));
    }

    #[test]
    fn parameters_and_inner_bindings_shadow_outer_locals() {
        let infos = analyze(
            "fn main() { let value = 1; let f = value -> { let inner = value; let value = 3; inner + value } }",
        );
        assert_eq!(infos.len(), 1);
        assert!(infos[0].captures.is_empty());
    }

    #[test]
    fn initializer_can_capture_the_binding_shadowed_by_its_let() {
        let infos =
            analyze("fn main() { let value = 1; let f = () -> { let value = value + 1; value } }");
        assert_eq!(infos[0].captures.len(), 1);
        assert_eq!(infos[0].captures[0].name, "value");
    }

    #[test]
    fn nested_closure_captures_propagate_to_the_parent_environment() {
        let infos = analyze(
            "fn main() { let outside = 1; let outer = p -> { let inner = q -> outside + p + q; inner } }",
        );
        assert_eq!(infos.len(), 2);
        assert_eq!(
            infos[0]
                .captures
                .iter()
                .map(|capture| capture.name.as_str())
                .collect::<Vec<_>>(),
            ["outside"]
        );
        assert_eq!(
            infos[1]
                .captures
                .iter()
                .map(|capture| capture.name.as_str())
                .collect::<Vec<_>>(),
            ["outside", "p"]
        );
    }

    #[test]
    fn loop_and_match_bindings_are_local_to_the_closure() {
        let infos = analyze(
            r#"
fn main() {
  let values = [1, 2];
  let f = () -> {
    for value in values { let copy = value }
    match item { Some(payload) => payload, None => 0 }
  }
}
"#,
        );
        assert_eq!(
            infos[0]
                .captures
                .iter()
                .map(|capture| capture.name.as_str())
                .collect::<Vec<_>>(),
            ["values"]
        );
    }
}
