use std::collections::HashMap;

use glyph_core::ast::Module;
use glyph_core::diag::Diagnostic;
use glyph_core::mir::{BlockId, Local, LocalId, MirBlock, MirInst, MirValue, Rvalue};
use glyph_core::span::Span;
use glyph_core::types::Type;

use crate::resolver::ResolverContext;

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

pub(crate) struct LowerCtx<'a> {
    pub(crate) resolver: &'a ResolverContext,
    pub(crate) module: &'a Module,
    pub(crate) fn_sigs: &'a HashMap<String, FnSig>,
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) locals: Vec<Local>,
    pub(crate) bindings: HashMap<&'a str, LocalId>,
    pub(crate) next_local: u32,
    pub(crate) function_name: String,
    pub(crate) fn_ret_type: Option<Type>,
    pub(crate) blocks: Vec<MirBlock>,
    pub(crate) current: BlockId,
    pub(crate) loop_stack: Vec<LoopContext>,
    pub(crate) scope_stack: Vec<Vec<LocalId>>,
    pub(crate) local_states: Vec<LocalState>,
    pub(crate) string_counter: u32,
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
        Self {
            resolver,
            module,
            fn_sigs,
            diagnostics: Vec::new(),
            locals: Vec::new(),
            bindings: HashMap::new(),
            next_local: 0,
            function_name,
            fn_ret_type: None,
            blocks,
            current: BlockId(0),
            loop_stack: Vec::new(),
            scope_stack: vec![Vec::new()],
            local_states: Vec::new(),
            string_counter: 0,
        }
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
                self.handle_reassign(*local);
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
        let scopes = self.scope_stack.clone();
        for scope in scopes.iter().skip(depth).rev() {
            for &local in scope.iter().rev() {
                self.drop_local_if_needed(local);
            }
        }
    }

    pub(crate) fn handle_reassign(&mut self, local: LocalId) {
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
            self.emit_drop(local);
        }
    }

    pub(crate) fn enter_scope(&mut self) {
        self.scope_stack.push(Vec::new());
    }

    pub(crate) fn exit_scope(&mut self) {
        if self.scope_stack.len() <= 1 {
            return;
        }
        if let Some(locals) = self.scope_stack.pop() {
            for local in locals.into_iter().rev() {
                self.drop_local_if_needed(local);
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
        let scopes: Vec<Vec<LocalId>> = self.scope_stack.clone();
        for scope in scopes.iter().rev() {
            for &local in scope.iter().rev() {
                self.drop_local_if_needed(local);
            }
        }
    }

    pub(crate) fn emit_drop(&mut self, local: LocalId) {
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
    }

    pub(crate) fn local_ty(&self, local: LocalId) -> Option<&Type> {
        self.locals
            .get(local.0 as usize)
            .and_then(|l| l.ty.as_ref())
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
            Type::App { base, .. } => base == "Vec" || base == "Map",
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
            _ => false,
        }
    }

    fn type_is_guard(ty: &Type) -> bool {
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
}
