use super::*;

use glyph_core::thread::{private_thread_handle_type, private_thread_scope_type, unit_task_type};
use glyph_core::types::BorrowedCallableKind;
use std::collections::{HashSet, VecDeque};

const SPAWN_SYMBOL: &str = "glyph_thread_spawn";
const SPAWN_RESULT_SYMBOL: &str = "glyph_thread_spawn_result";
const JOIN_SYMBOL: &str = "glyph_thread_join";
const JOIN_RESULT_SYMBOL: &str = "glyph_thread_join_result";
const DETACH_SYMBOL: &str = "glyph_thread_detach";
const SCOPE_CREATE_SYMBOL: &str = "glyph_thread_scope_create";
const SCOPE_SPAWN_SYMBOL: &str = "glyph_thread_scope_spawn";
const SCOPE_SPAWN_RESULT_SYMBOL: &str = "glyph_thread_scope_spawn_result";
const SCOPE_JOIN_SYMBOL: &str = "glyph_thread_scope_join";
const SCOPE_JOIN_RESULT_SYMBOL: &str = "glyph_thread_scope_join_result";
const SCOPE_DRAIN_SYMBOL: &str = "glyph_thread_scope_drain";
const SCOPE_DRAIN_OR_ABORT_SYMBOL: &str = "glyph_thread_scope_drain_or_abort";
const SCOPE_JOIN_ALL_SYMBOL: &str = "glyph_thread_scope_join_all";

impl CodegenContext {
    pub(super) fn validate_scoped_thread_function(&self, func: &MirFunction) -> Result<()> {
        fn type_may_escape_scope(ty: &Type) -> bool {
            matches!(ty, Type::BorrowedFunction { .. })
                || matches!(ty, Type::Ref(..))
                || ty == &glyph_core::thread::canonical_thread_scope_type()
                || ty == &private_thread_scope_type()
                || glyph_core::thread::is_canonical_scoped_thread_handle(ty)
                || glyph_core::thread::private_scoped_thread_handle_result(ty).is_some()
                || match ty {
                    Type::App { args, .. } | Type::Tuple(args) => {
                        args.iter().any(type_may_escape_scope)
                    }
                    Type::Array(inner, _)
                    | Type::Own(inner)
                    | Type::RawPtr(inner)
                    | Type::Shared(inner) => type_may_escape_scope(inner),
                    _ => false,
                }
        }

        fn type_contains_borrowed_callable(ty: &Type) -> bool {
            matches!(ty, Type::BorrowedFunction { .. })
                || match ty {
                    Type::App { args, .. } | Type::Tuple(args) => {
                        args.iter().any(type_contains_borrowed_callable)
                    }
                    Type::Array(inner, _)
                    | Type::Own(inner)
                    | Type::RawPtr(inner)
                    | Type::Shared(inner) => type_contains_borrowed_callable(inner),
                    _ => false,
                }
        }

        let mut fnmut_spawns = HashSet::new();
        let mut public_spawn_scopes = HashSet::new();
        for block in &func.blocks {
            for inst in &block.insts {
                let local_has_borrowed_callable = |local: LocalId| {
                    func.locals
                        .get(local.0 as usize)
                        .and_then(|local| local.ty.as_ref())
                        .is_some_and(type_contains_borrowed_callable)
                };
                let value_has_borrowed_callable = |value: &MirValue| matches!(value, MirValue::Local(local) if local_has_borrowed_callable(*local));
                if let MirInst::Assign { value, .. } = inst {
                    let stores_borrowed = match value {
                        Rvalue::StructLit { field_values, .. } => field_values
                            .iter()
                            .any(|(_, value)| value_has_borrowed_callable(value)),
                        Rvalue::ArrayLit { elements, .. } => {
                            elements.iter().any(value_has_borrowed_callable)
                        }
                        Rvalue::EnumConstruct {
                            payload: Some(value),
                            ..
                        } => value_has_borrowed_callable(value),
                        Rvalue::VecPush { value, .. }
                        | Rvalue::OwnNew { value, .. }
                        | Rvalue::SharedNew { value, .. }
                        | Rvalue::ArcNew { value, .. }
                        | Rvalue::MutexNew { value, .. } => value_has_borrowed_callable(value),
                        Rvalue::MapAdd { key, value, .. }
                        | Rvalue::MapUpdate { key, value, .. } => {
                            value_has_borrowed_callable(key) || value_has_borrowed_callable(value)
                        }
                        Rvalue::MakeClosure { captures, .. } => captures
                            .iter()
                            .any(|capture| local_has_borrowed_callable(capture.local)),
                        _ => false,
                    };
                    if stores_borrowed {
                        bail!(
                            "borrowed callables cannot be stored in aggregates, containers, allocations, or owned closures"
                        );
                    }
                    let task = match value {
                        Rvalue::ScopedThreadSpawnUnit { task, .. }
                        | Rvalue::ScopedThreadSpawnResult { task, .. } => Some(*task),
                        _ => None,
                    };
                    if let Some(task) = task {
                        let task_type = func
                            .locals
                            .get(task.0 as usize)
                            .and_then(|local| local.ty.as_ref());
                        if matches!(
                            task_type,
                            Some(Type::BorrowedFunction {
                                kind: BorrowedCallableKind::FnMut,
                                ..
                            })
                        ) && !fnmut_spawns.insert(task)
                        {
                            bail!(
                                "borrowed FnMut local {task:?} cannot have more than one live scoped spawn"
                            );
                        }
                    }
                    let scope = match value {
                        Rvalue::ScopedThreadSpawnUnit { scope, .. }
                        | Rvalue::ScopedThreadSpawnResult { scope, .. } => Some(*scope),
                        _ => None,
                    };
                    if let Some(scope) = scope {
                        if func
                            .locals
                            .get(scope.0 as usize)
                            .and_then(|local| local.ty.as_ref())
                            == Some(&glyph_core::thread::canonical_thread_scope_type())
                        {
                            public_spawn_scopes.insert(scope);
                        }
                    }
                    if let Rvalue::Move(source) = value {
                        let source_type = func
                            .locals
                            .get(source.0 as usize)
                            .and_then(|local| local.ty.as_ref());
                        if matches!(
                            source_type,
                            Some(Type::BorrowedFunction {
                                kind: BorrowedCallableKind::FnMut,
                                ..
                            })
                        ) {
                            bail!("borrowed FnMut local {source:?} cannot be copied or relabeled");
                        }
                    }
                }
                if let MirInst::AssignField { value, .. } = inst
                    && matches!(value, Rvalue::Move(local) if local_has_borrowed_callable(*local))
                {
                    bail!("borrowed callables cannot be stored in struct fields");
                }
                if let MirInst::Return(Some(MirValue::Local(local))) = inst {
                    if func
                        .locals
                        .get(local.0 as usize)
                        .and_then(|local| local.ty.as_ref())
                        .is_some_and(type_may_escape_scope)
                    {
                        bail!("scoped or borrowed value cannot escape through return");
                    }
                }
            }
        }

        if !public_spawn_scopes.is_empty() {
            for block in &func.blocks {
                let mut drained = HashSet::new();
                for inst in &block.insts {
                    if let MirInst::DrainThreadScope(scope) = inst {
                        drained.insert(*scope);
                    }
                    if matches!(inst, MirInst::Return(_))
                        && !public_spawn_scopes.is_subset(&drained)
                    {
                        bail!(
                            "scope callback return is missing DrainThreadScope for {:?}",
                            public_spawn_scopes.difference(&drained).collect::<Vec<_>>()
                        );
                    }
                }
            }
        }

        if func.blocks.is_empty() {
            return Ok(());
        }
        let mut entries: Vec<Option<HashSet<LocalId>>> = vec![None; func.blocks.len()];
        entries[0] = Some(HashSet::new());
        let mut work = VecDeque::from([0_usize]);
        while let Some(index) = work.pop_front() {
            let Some(mut active) = entries[index].clone() else {
                continue;
            };
            let block = &func.blocks[index];
            let mut successors = Vec::new();
            for inst in &block.insts {
                match inst {
                    MirInst::Assign {
                        value: Rvalue::ThreadScopeCreate { out_scope },
                        ..
                    } => {
                        active.insert(*out_scope);
                    }
                    MirInst::DropThreadScope(scope) => {
                        active.remove(scope);
                    }
                    MirInst::Return(_) => {
                        if !active.is_empty() {
                            bail!(
                                "thread scope cleanup is missing before return; active scopes: {active:?}"
                            );
                        }
                    }
                    MirInst::Goto(target) => successors.push(target.0 as usize),
                    MirInst::If {
                        then_bb, else_bb, ..
                    } => {
                        successors.push(then_bb.0 as usize);
                        successors.push(else_bb.0 as usize);
                    }
                    _ => {}
                }
            }
            for successor in successors {
                if successor >= entries.len() {
                    continue;
                }
                let changed = match entries[successor].as_mut() {
                    Some(existing) => {
                        let before = existing.len();
                        existing.extend(active.iter().copied());
                        existing.len() != before
                    }
                    None => {
                        entries[successor] = Some(active.clone());
                        true
                    }
                };
                if changed {
                    work.push_back(successor);
                }
            }
        }
        Ok(())
    }

    pub(super) fn codegen_thread_handle_from_raw(
        &mut self,
        raw: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let slot = self.thread_handle_slot(raw, func, local_map)?;
        let ptr_ty = unsafe { LLVMPointerType(LLVMInt8TypeInContext(self.context), 0) };
        unsafe {
            let value = LLVMBuildLoad2(
                self.builder,
                ptr_ty,
                slot,
                CString::new("thread.handle.wrap")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, LLVMConstPointerNull(ptr_ty), slot);
            Ok(value)
        }
    }

    pub(super) fn codegen_thread_handle_into_raw(
        &mut self,
        handle: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let ty = func
            .locals
            .get(handle.0 as usize)
            .and_then(|local| local.ty.as_ref())
            .ok_or_else(|| anyhow!("public thread handle {:?} has no type", handle))?;
        if !glyph_core::thread::is_canonical_thread_handle(ty) {
            bail!(
                "thread handle unwrap requires canonical std::thread::JoinHandle<T>, found {ty:?}"
            );
        }
        let slot = local_map
            .get(&handle)
            .copied()
            .ok_or_else(|| anyhow!("missing storage for public thread handle {:?}", handle))?;
        let ptr_ty = unsafe { LLVMPointerType(LLVMInt8TypeInContext(self.context), 0) };
        unsafe {
            let value = LLVMBuildLoad2(
                self.builder,
                ptr_ty,
                slot,
                CString::new("thread.handle.unwrap")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, LLVMConstPointerNull(ptr_ty), slot);
            Ok(value)
        }
    }

    pub(super) fn codegen_scoped_thread_handle_from_raw(
        &mut self,
        raw: LocalId,
        result_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let slot = self.scoped_thread_handle_slot(raw, Some(result_type), func, local_map)?;
        let ptr_ty = unsafe { LLVMPointerType(LLVMInt8TypeInContext(self.context), 0) };
        unsafe {
            let value = LLVMBuildLoad2(
                self.builder,
                ptr_ty,
                slot,
                CString::new("scoped.thread.handle.wrap")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, LLVMConstPointerNull(ptr_ty), slot);
            Ok(value)
        }
    }

    pub(super) fn codegen_scoped_thread_handle_into_raw(
        &mut self,
        handle: LocalId,
        result_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let ty = func
            .locals
            .get(handle.0 as usize)
            .and_then(|local| local.ty.as_ref())
            .ok_or_else(|| anyhow!("public scoped thread handle {:?} has no type", handle))?;
        if glyph_core::thread::canonical_scoped_thread_handle_result(ty) != Some(result_type) {
            bail!(
                "scoped thread handle unwrap requires canonical std::thread::ScopedJoinHandle<T>, found {ty:?}"
            );
        }
        let slot = local_map.get(&handle).copied().ok_or_else(|| {
            anyhow!(
                "missing storage for public scoped thread handle {:?}",
                handle
            )
        })?;
        let ptr_ty = unsafe { LLVMPointerType(LLVMInt8TypeInContext(self.context), 0) };
        unsafe {
            let value = LLVMBuildLoad2(
                self.builder,
                ptr_ty,
                slot,
                CString::new("scoped.thread.handle.unwrap")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, LLVMConstPointerNull(ptr_ty), slot);
            Ok(value)
        }
    }

    fn validate_unit_task_local(&self, task: LocalId, func: &MirFunction) -> Result<()> {
        let ty = func
            .locals
            .get(task.0 as usize)
            .and_then(|local| local.ty.as_ref())
            .ok_or_else(|| anyhow!("thread task {:?} has no type", task))?;
        let (params, ret) = ty
            .function_signature()
            .ok_or_else(|| anyhow!("thread task must be FnOnce() -> (), found {ty:?}"))?;
        if !params.is_empty() || !Self::is_unit_type(ret) {
            bail!("thread task must be FnOnce() -> (), found {ty:?}");
        }
        Ok(())
    }

    fn validate_result_task_local(
        &self,
        task: LocalId,
        result_type: &Type,
        func: &MirFunction,
    ) -> Result<Type> {
        let ty = func
            .locals
            .get(task.0 as usize)
            .and_then(|local| local.ty.as_ref())
            .ok_or_else(|| anyhow!("thread task {:?} has no type", task))?;
        let (params, ret) = ty.function_signature().ok_or_else(|| {
            anyhow!("thread task must be FnOnce() -> {result_type:?}, found {ty:?}")
        })?;
        let result_matches =
            ret == result_type || (Self::is_unit_type(ret) && Self::is_unit_type(result_type));
        if !params.is_empty() || !result_matches {
            bail!("thread task must be FnOnce() -> {result_type:?}, found {ty:?}");
        }
        Ok(ty.clone())
    }

    fn thread_handle_slot(
        &self,
        handle: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let ty = func
            .locals
            .get(handle.0 as usize)
            .and_then(|local| local.ty.as_ref())
            .ok_or_else(|| anyhow!("thread handle {:?} has no type", handle))?;
        if ty != &private_thread_handle_type() {
            bail!("internal thread handle must use private RawPtr<I8> MIR storage, found {ty:?}");
        }
        local_map
            .get(&handle)
            .copied()
            .ok_or_else(|| anyhow!("missing storage for thread handle {:?}", handle))
    }

    fn thread_scope_slot(
        &self,
        scope: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let ty = func
            .locals
            .get(scope.0 as usize)
            .and_then(|local| local.ty.as_ref())
            .ok_or_else(|| anyhow!("thread scope {:?} has no type", scope))?;
        if ty != &private_thread_scope_type() {
            bail!("internal thread scope must use private RawPtr<I8> MIR storage, found {ty:?}");
        }
        local_map
            .get(&scope)
            .copied()
            .ok_or_else(|| anyhow!("missing storage for thread scope {:?}", scope))
    }

    fn scoped_thread_handle_slot(
        &self,
        handle: LocalId,
        expected_result: Option<&Type>,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let ty = func
            .locals
            .get(handle.0 as usize)
            .and_then(|local| local.ty.as_ref())
            .ok_or_else(|| anyhow!("scoped thread handle {:?} has no type", handle))?;
        let actual_result = glyph_core::thread::private_scoped_thread_handle_result(ty);
        if actual_result.is_none()
            || expected_result.is_some_and(|expected| actual_result != Some(expected))
        {
            bail!(
                "internal scoped thread handle has wrong result provenance: expected {expected_result:?}, found {ty:?}"
            );
        }
        local_map
            .get(&handle)
            .copied()
            .ok_or_else(|| anyhow!("missing storage for scoped thread handle {:?}", handle))
    }

    fn validate_scoped_task_local(
        &self,
        task: LocalId,
        result_type: &Type,
        func: &MirFunction,
    ) -> Result<Type> {
        let ty = func
            .locals
            .get(task.0 as usize)
            .and_then(|local| local.ty.as_ref())
            .ok_or_else(|| anyhow!("scoped thread task {:?} has no type", task))?;
        let Type::BorrowedFunction { kind, params, ret } = ty else {
            bail!(
                "scoped thread task must be a borrowed Fn/FnMut() -> {result_type:?}, found {ty:?}"
            );
        };
        if !matches!(kind, BorrowedCallableKind::Fn | BorrowedCallableKind::FnMut)
            || !params.is_empty()
            || !(ret.as_ref() == result_type
                || (Self::is_unit_type(ret) && Self::is_unit_type(result_type)))
        {
            bail!(
                "scoped thread task must be a borrowed Fn/FnMut() -> {result_type:?}, found {ty:?}"
            );
        }
        Ok(ty.clone())
    }

    fn thread_entry_type(&self) -> LLVMTypeRef {
        unsafe {
            let ptr = LLVMPointerType(LLVMInt8TypeInContext(self.context), 0);
            let mut params = [ptr];
            LLVMFunctionType(
                LLVMVoidTypeInContext(self.context),
                params.as_mut_ptr(),
                1,
                0,
            )
        }
    }

    fn thread_consume_type(&self) -> LLVMTypeRef {
        unsafe {
            let ptr = LLVMPointerType(LLVMInt8TypeInContext(self.context), 0);
            let ptr_ptr = LLVMPointerType(ptr, 0);
            let mut params = [ptr_ptr];
            LLVMFunctionType(
                LLVMInt32TypeInContext(self.context),
                params.as_mut_ptr(),
                1,
                0,
            )
        }
    }

    fn thread_spawn_type(&self) -> LLVMTypeRef {
        unsafe {
            let ptr = LLVMPointerType(LLVMInt8TypeInContext(self.context), 0);
            let ptr_ptr = LLVMPointerType(ptr, 0);
            let callback = LLVMPointerType(self.thread_entry_type(), 0);
            let mut params = [ptr_ptr, callback, ptr, callback];
            LLVMFunctionType(
                LLVMInt32TypeInContext(self.context),
                params.as_mut_ptr(),
                params.len() as u32,
                0,
            )
        }
    }

    fn thread_result_entry_type(&self) -> LLVMTypeRef {
        unsafe {
            let ptr = LLVMPointerType(LLVMInt8TypeInContext(self.context), 0);
            let mut params = [ptr, ptr, ptr];
            LLVMFunctionType(
                LLVMVoidTypeInContext(self.context),
                params.as_mut_ptr(),
                params.len() as u32,
                0,
            )
        }
    }

    fn thread_spawn_result_type(&self) -> LLVMTypeRef {
        unsafe {
            let ptr = LLVMPointerType(LLVMInt8TypeInContext(self.context), 0);
            let ptr_ptr = LLVMPointerType(ptr, 0);
            let result_entry = LLVMPointerType(self.thread_result_entry_type(), 0);
            let drop_callback = LLVMPointerType(self.thread_entry_type(), 0);
            let usize_ty = LLVMInt64TypeInContext(self.context);
            let mut params = [
                ptr_ptr,
                result_entry,
                ptr,
                ptr,
                drop_callback,
                usize_ty,
                drop_callback,
            ];
            LLVMFunctionType(
                LLVMInt32TypeInContext(self.context),
                params.as_mut_ptr(),
                params.len() as u32,
                0,
            )
        }
    }

    fn thread_join_result_type(&self) -> LLVMTypeRef {
        unsafe {
            let ptr = LLVMPointerType(LLVMInt8TypeInContext(self.context), 0);
            let ptr_ptr = LLVMPointerType(ptr, 0);
            let mut params = [ptr_ptr, ptr];
            LLVMFunctionType(
                LLVMInt32TypeInContext(self.context),
                params.as_mut_ptr(),
                params.len() as u32,
                0,
            )
        }
    }

    fn thread_scope_drain_type(&self) -> LLVMTypeRef {
        unsafe {
            let ptr = LLVMPointerType(LLVMInt8TypeInContext(self.context), 0);
            let mut params = [ptr];
            LLVMFunctionType(
                LLVMInt32TypeInContext(self.context),
                params.as_mut_ptr(),
                1,
                0,
            )
        }
    }

    fn thread_scope_drain_or_abort_type(&self) -> LLVMTypeRef {
        unsafe {
            let ptr = LLVMPointerType(LLVMInt8TypeInContext(self.context), 0);
            let mut params = [ptr];
            LLVMFunctionType(
                LLVMVoidTypeInContext(self.context),
                params.as_mut_ptr(),
                1,
                0,
            )
        }
    }

    fn scoped_thread_spawn_type(&self) -> LLVMTypeRef {
        unsafe {
            let ptr = LLVMPointerType(LLVMInt8TypeInContext(self.context), 0);
            let ptr_ptr = LLVMPointerType(ptr, 0);
            let callback = LLVMPointerType(self.thread_entry_type(), 0);
            let mut params = [ptr, ptr_ptr, callback, ptr];
            LLVMFunctionType(
                LLVMInt32TypeInContext(self.context),
                params.as_mut_ptr(),
                params.len() as u32,
                0,
            )
        }
    }

    fn scoped_thread_spawn_result_type(&self) -> LLVMTypeRef {
        unsafe {
            let ptr = LLVMPointerType(LLVMInt8TypeInContext(self.context), 0);
            let ptr_ptr = LLVMPointerType(ptr, 0);
            let result_entry = LLVMPointerType(self.thread_result_entry_type(), 0);
            let drop_callback = LLVMPointerType(self.thread_entry_type(), 0);
            let usize_ty = LLVMInt64TypeInContext(self.context);
            let mut params = [
                ptr,
                ptr_ptr,
                result_entry,
                ptr,
                ptr,
                usize_ty,
                drop_callback,
            ];
            LLVMFunctionType(
                LLVMInt32TypeInContext(self.context),
                params.as_mut_ptr(),
                params.len() as u32,
                0,
            )
        }
    }

    fn get_or_declare_thread_runtime(
        &self,
        symbol: &str,
        function_type: LLVMTypeRef,
    ) -> Result<LLVMValueRef> {
        let name = CString::new(symbol)?;
        unsafe {
            let existing = LLVMGetNamedFunction(self.module, name.as_ptr());
            if !existing.is_null() {
                return Ok(existing);
            }
            Ok(LLVMAddFunction(self.module, name.as_ptr(), function_type))
        }
    }

    fn ensure_thread_result_entry_adapter(
        &mut self,
        signature: &Type,
        result_type: &Type,
    ) -> Result<LLVMValueRef> {
        let name = CString::new(format!(
            "__glyph_thread_result_entry_{}",
            self.type_key(result_type)
        ))?;
        let existing = unsafe { LLVMGetNamedFunction(self.module, name.as_ptr()) };
        if !existing.is_null() {
            return Ok(existing);
        }

        let adapter_ty = self.thread_result_entry_type();
        let adapter = unsafe { LLVMAddFunction(self.module, name.as_ptr(), adapter_ty) };
        unsafe { LLVMSetLinkage(adapter, LLVMLinkage::LLVMPrivateLinkage) };
        let saved_block = unsafe { LLVMGetInsertBlock(self.builder) };
        let generated = (|| {
            let entry = unsafe {
                LLVMAppendBasicBlockInContext(
                    self.context,
                    adapter,
                    CString::new("entry")?.as_ptr(),
                )
            };
            unsafe { LLVMPositionBuilderAtEnd(self.builder, entry) };

            let invoke = unsafe { LLVMGetParam(adapter, 0) };
            let env = unsafe { LLVMGetParam(adapter, 1) };
            let out_result = unsafe { LLVMGetParam(adapter, 2) };
            let (invoke_ty, uses_sret) = self.callable_invoke_type(signature)?;
            let mut args = if uses_sret {
                vec![out_result, env]
            } else {
                vec![env]
            };
            let call = self.build_call2(invoke_ty, invoke, &mut args, "thread.task.result")?;
            if uses_sret {
                self.add_sret_call_attribute(call, self.get_llvm_type(result_type)?);
            } else if !Self::is_unit_type(result_type) {
                unsafe { LLVMBuildStore(self.builder, call, out_result) };
            }
            unsafe { LLVMBuildRetVoid(self.builder) };
            Ok::<(), anyhow::Error>(())
        })();
        if !saved_block.is_null() {
            unsafe { LLVMPositionBuilderAtEnd(self.builder, saved_block) };
        }
        generated?;
        Ok(adapter)
    }

    fn ensure_thread_result_drop_thunk(&mut self, result_type: &Type) -> Result<LLVMValueRef> {
        let name = CString::new(format!(
            "__glyph_thread_result_drop_{}",
            self.type_key(result_type)
        ))?;
        let existing = unsafe { LLVMGetNamedFunction(self.module, name.as_ptr()) };
        if !existing.is_null() {
            return Ok(existing);
        }

        let drop_ty = self.thread_entry_type();
        let drop_fn = unsafe { LLVMAddFunction(self.module, name.as_ptr(), drop_ty) };
        unsafe { LLVMSetLinkage(drop_fn, LLVMLinkage::LLVMPrivateLinkage) };
        let saved_block = unsafe { LLVMGetInsertBlock(self.builder) };
        let generated = (|| {
            let entry = unsafe {
                LLVMAppendBasicBlockInContext(
                    self.context,
                    drop_fn,
                    CString::new("entry")?.as_ptr(),
                )
            };
            unsafe { LLVMPositionBuilderAtEnd(self.builder, entry) };
            let result = unsafe { LLVMGetParam(drop_fn, 0) };
            self.codegen_drop_elem_slot(result, result_type)?;
            unsafe { LLVMBuildRetVoid(self.builder) };
            Ok::<(), anyhow::Error>(())
        })();
        if !saved_block.is_null() {
            unsafe { LLVMPositionBuilderAtEnd(self.builder, saved_block) };
        }
        generated?;
        Ok(drop_fn)
    }

    pub(super) fn codegen_thread_spawn_unit(
        &mut self,
        task: LocalId,
        out_handle: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        self.validate_unit_task_local(task, func)?;
        let handle_slot = self.thread_handle_slot(out_handle, func, local_map)?;
        let task_slot = local_map
            .get(&task)
            .copied()
            .ok_or_else(|| anyhow!("missing storage for thread task {:?}", task))?;

        let carrier_ty = self.get_llvm_type(&unit_task_type())?;
        let ptr_ty = unsafe { LLVMPointerType(LLVMInt8TypeInContext(self.context), 0) };
        let carrier = unsafe {
            LLVMBuildLoad2(
                self.builder,
                carrier_ty,
                task_slot,
                CString::new("thread.task")?.as_ptr(),
            )
        };
        let env = unsafe {
            LLVMBuildExtractValue(
                self.builder,
                carrier,
                0,
                CString::new("thread.task.env")?.as_ptr(),
            )
        };
        let invoke = unsafe {
            LLVMBuildExtractValue(
                self.builder,
                carrier,
                1,
                CString::new("thread.task.invoke")?.as_ptr(),
            )
        };
        let drop = unsafe {
            LLVMBuildExtractValue(
                self.builder,
                carrier,
                2,
                CString::new("thread.task.drop")?.as_ptr(),
            )
        };

        // Ownership leaves compiled code before the runtime call. Both the
        // success path (worker entry) and failure path (drop_unstarted) are
        // exclusively owned by the runtime from this point forward.
        unsafe {
            LLVMBuildStore(self.builder, LLVMConstNull(carrier_ty), task_slot);
            LLVMBuildStore(self.builder, LLVMConstPointerNull(ptr_ty), handle_slot);
        }

        let spawn_ty = self.thread_spawn_type();
        let spawn = self.get_or_declare_thread_runtime(SPAWN_SYMBOL, spawn_ty)?;
        let callback_ty = unsafe { LLVMPointerType(self.thread_entry_type(), 0) };
        let invoke = unsafe {
            LLVMBuildBitCast(
                self.builder,
                invoke,
                callback_ty,
                CString::new("thread.entry")?.as_ptr(),
            )
        };
        let drop = unsafe {
            LLVMBuildBitCast(
                self.builder,
                drop,
                callback_ty,
                CString::new("thread.drop_unstarted")?.as_ptr(),
            )
        };
        let mut args = [handle_slot, invoke, env, drop];
        self.build_call2(spawn_ty, spawn, &mut args, "thread.spawn.status")
    }

    pub(super) fn codegen_thread_spawn_result(
        &mut self,
        task: LocalId,
        out_handle: LocalId,
        result_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let signature = self.validate_result_task_local(task, result_type, func)?;
        let handle_slot = self.thread_handle_slot(out_handle, func, local_map)?;
        let task_slot = local_map
            .get(&task)
            .copied()
            .ok_or_else(|| anyhow!("missing storage for thread task {:?}", task))?;
        let carrier_ty = self.get_llvm_type(&signature)?;
        let ptr_ty = unsafe { LLVMPointerType(LLVMInt8TypeInContext(self.context), 0) };
        let carrier = unsafe {
            LLVMBuildLoad2(
                self.builder,
                carrier_ty,
                task_slot,
                CString::new("thread.task")?.as_ptr(),
            )
        };
        let env = unsafe {
            LLVMBuildExtractValue(
                self.builder,
                carrier,
                0,
                CString::new("thread.task.env")?.as_ptr(),
            )
        };
        let invoke = unsafe {
            LLVMBuildExtractValue(
                self.builder,
                carrier,
                1,
                CString::new("thread.task.invoke")?.as_ptr(),
            )
        };
        let drop = unsafe {
            LLVMBuildExtractValue(
                self.builder,
                carrier,
                2,
                CString::new("thread.task.drop")?.as_ptr(),
            )
        };
        let entry = self.ensure_thread_result_entry_adapter(&signature, result_type)?;
        let drop_result = self.ensure_thread_result_drop_thunk(result_type)?;
        let llvm_result_type = self.get_llvm_type(result_type)?;
        let result_size = unsafe { LLVMSizeOf(llvm_result_type) };

        // As with unit spawn, the runtime owns the callable before the call so
        // every failure path has a single, explicit unstarted-drop owner.
        unsafe {
            LLVMBuildStore(self.builder, LLVMConstNull(carrier_ty), task_slot);
            LLVMBuildStore(self.builder, LLVMConstPointerNull(ptr_ty), handle_slot);
        }

        let spawn_ty = self.thread_spawn_result_type();
        let spawn = self.get_or_declare_thread_runtime(SPAWN_RESULT_SYMBOL, spawn_ty)?;
        let mut args = [
            handle_slot,
            entry,
            invoke,
            env,
            drop,
            result_size,
            drop_result,
        ];
        self.build_call2(spawn_ty, spawn, &mut args, "thread.spawn.result.status")
    }

    fn load_thread_scope(
        &self,
        scope: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<(LLVMValueRef, LLVMValueRef)> {
        let ty = func
            .locals
            .get(scope.0 as usize)
            .and_then(|local| local.ty.as_ref())
            .ok_or_else(|| anyhow!("thread scope {:?} has no type", scope))?;
        if ty != &private_thread_scope_type()
            && ty != &glyph_core::thread::canonical_thread_scope_type()
        {
            bail!("scoped spawn requires a canonical Scope or private scope owner, found {ty:?}");
        }
        let slot = local_map
            .get(&scope)
            .copied()
            .ok_or_else(|| anyhow!("missing storage for thread scope {:?}", scope))?;
        let ptr_ty = unsafe { LLVMPointerType(LLVMInt8TypeInContext(self.context), 0) };
        let value = unsafe {
            LLVMBuildLoad2(
                self.builder,
                ptr_ty,
                slot,
                CString::new("thread.scope")?.as_ptr(),
            )
        };
        Ok((slot, value))
    }

    pub(super) fn codegen_thread_scope_create(
        &mut self,
        out_scope: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let scope_slot = self.thread_scope_slot(out_scope, func, local_map)?;
        let ptr_ty = unsafe { LLVMPointerType(LLVMInt8TypeInContext(self.context), 0) };
        unsafe { LLVMBuildStore(self.builder, LLVMConstPointerNull(ptr_ty), scope_slot) };
        let function_type = self.thread_consume_type();
        let function = self.get_or_declare_thread_runtime(SCOPE_CREATE_SYMBOL, function_type)?;
        self.build_call2(
            function_type,
            function,
            &mut [scope_slot],
            "thread.scope.create.status",
        )
    }

    pub(super) fn codegen_thread_scope_exit(
        &mut self,
        scope: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let scope_slot = self.thread_scope_slot(scope, func, local_map)?;
        let function_type = self.thread_consume_type();
        let function = self.get_or_declare_thread_runtime(SCOPE_JOIN_ALL_SYMBOL, function_type)?;
        self.build_call2(
            function_type,
            function,
            &mut [scope_slot],
            "thread.scope.exit.status",
        )
    }

    pub(super) fn codegen_thread_scope_from_raw(
        &mut self,
        raw: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let slot = self.thread_scope_slot(raw, func, local_map)?;
        let ptr_ty = unsafe { LLVMPointerType(LLVMInt8TypeInContext(self.context), 0) };
        Ok(unsafe {
            LLVMBuildLoad2(
                self.builder,
                ptr_ty,
                slot,
                CString::new("thread.scope.view")?.as_ptr(),
            )
        })
    }

    pub(super) fn codegen_thread_scope_drain(
        &mut self,
        scope: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let (_, value) = self.load_thread_scope(scope, func, local_map)?;
        let function_type = self.thread_scope_drain_type();
        let function = self.get_or_declare_thread_runtime(SCOPE_DRAIN_SYMBOL, function_type)?;
        self.build_call2(
            function_type,
            function,
            &mut [value],
            "thread.scope.drain.status",
        )
    }

    pub(super) fn codegen_drain_thread_scope(
        &mut self,
        scope: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<()> {
        let (_, value) = self.load_thread_scope(scope, func, local_map)?;
        let function_type = self.thread_scope_drain_or_abort_type();
        let function =
            self.get_or_declare_thread_runtime(SCOPE_DRAIN_OR_ABORT_SYMBOL, function_type)?;
        self.build_call2(function_type, function, &mut [value], "")?;
        Ok(())
    }

    pub(super) fn codegen_scoped_thread_spawn_unit(
        &mut self,
        scope: LocalId,
        task: LocalId,
        out_handle: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let signature = self.validate_scoped_task_local(task, &Type::Void, func)?;
        let (_, scope_value) = self.load_thread_scope(scope, func, local_map)?;
        let handle_slot =
            self.scoped_thread_handle_slot(out_handle, Some(&Type::Void), func, local_map)?;
        let task_slot = local_map
            .get(&task)
            .copied()
            .ok_or_else(|| anyhow!("missing storage for scoped thread task {:?}", task))?;
        let carrier_ty = self.get_llvm_type(&signature)?;
        let carrier = unsafe {
            LLVMBuildLoad2(
                self.builder,
                carrier_ty,
                task_slot,
                CString::new("scoped.thread.task")?.as_ptr(),
            )
        };
        let env = unsafe {
            LLVMBuildExtractValue(
                self.builder,
                carrier,
                0,
                CString::new("scoped.thread.task.env")?.as_ptr(),
            )
        };
        let invoke = unsafe {
            LLVMBuildExtractValue(
                self.builder,
                carrier,
                1,
                CString::new("scoped.thread.task.invoke")?.as_ptr(),
            )
        };
        let ptr_ty = unsafe { LLVMPointerType(LLVMInt8TypeInContext(self.context), 0) };
        unsafe { LLVMBuildStore(self.builder, LLVMConstPointerNull(ptr_ty), handle_slot) };
        let function_type = self.scoped_thread_spawn_type();
        let function = self.get_or_declare_thread_runtime(SCOPE_SPAWN_SYMBOL, function_type)?;
        let callback_ty = unsafe { LLVMPointerType(self.thread_entry_type(), 0) };
        let entry = unsafe {
            LLVMBuildBitCast(
                self.builder,
                invoke,
                callback_ty,
                CString::new("scoped.thread.entry")?.as_ptr(),
            )
        };
        self.build_call2(
            function_type,
            function,
            &mut [scope_value, handle_slot, entry, env],
            "scoped.thread.spawn.status",
        )
    }

    pub(super) fn codegen_scoped_thread_spawn_result(
        &mut self,
        scope: LocalId,
        task: LocalId,
        out_handle: LocalId,
        result_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let signature = self.validate_scoped_task_local(task, result_type, func)?;
        let (_, scope_value) = self.load_thread_scope(scope, func, local_map)?;
        let handle_slot =
            self.scoped_thread_handle_slot(out_handle, Some(result_type), func, local_map)?;
        let task_slot = local_map
            .get(&task)
            .copied()
            .ok_or_else(|| anyhow!("missing storage for scoped thread task {:?}", task))?;
        let carrier_ty = self.get_llvm_type(&signature)?;
        let carrier = unsafe {
            LLVMBuildLoad2(
                self.builder,
                carrier_ty,
                task_slot,
                CString::new("scoped.thread.task")?.as_ptr(),
            )
        };
        let env = unsafe {
            LLVMBuildExtractValue(
                self.builder,
                carrier,
                0,
                CString::new("scoped.thread.task.env")?.as_ptr(),
            )
        };
        let invoke = unsafe {
            LLVMBuildExtractValue(
                self.builder,
                carrier,
                1,
                CString::new("scoped.thread.task.invoke")?.as_ptr(),
            )
        };
        let entry = self.ensure_thread_result_entry_adapter(&signature, result_type)?;
        let drop_result = self.ensure_thread_result_drop_thunk(result_type)?;
        let result_size = unsafe { LLVMSizeOf(self.get_llvm_type(result_type)?) };
        let ptr_ty = unsafe { LLVMPointerType(LLVMInt8TypeInContext(self.context), 0) };
        unsafe { LLVMBuildStore(self.builder, LLVMConstPointerNull(ptr_ty), handle_slot) };
        let function_type = self.scoped_thread_spawn_result_type();
        let function =
            self.get_or_declare_thread_runtime(SCOPE_SPAWN_RESULT_SYMBOL, function_type)?;
        self.build_call2(
            function_type,
            function,
            &mut [
                scope_value,
                handle_slot,
                entry,
                invoke,
                env,
                result_size,
                drop_result,
            ],
            "scoped.thread.spawn.result.status",
        )
    }

    pub(super) fn codegen_scoped_thread_join_unit(
        &mut self,
        handle: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let handle_slot =
            self.scoped_thread_handle_slot(handle, Some(&Type::Void), func, local_map)?;
        let function_type = self.thread_consume_type();
        let function = self.get_or_declare_thread_runtime(SCOPE_JOIN_SYMBOL, function_type)?;
        self.build_call2(
            function_type,
            function,
            &mut [handle_slot],
            "scoped.thread.join.status",
        )
    }

    pub(super) fn codegen_scoped_thread_join_result(
        &mut self,
        handle: LocalId,
        out_result: LocalId,
        result_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let handle_slot =
            self.scoped_thread_handle_slot(handle, Some(result_type), func, local_map)?;
        let actual_result = func
            .locals
            .get(out_result.0 as usize)
            .and_then(|local| local.ty.as_ref())
            .ok_or_else(|| anyhow!("scoped thread result local {:?} has no type", out_result))?;
        if actual_result != result_type
            && !(Self::is_unit_type(actual_result) && Self::is_unit_type(result_type))
        {
            bail!(
                "scoped thread join result local has type {actual_result:?}, expected {result_type:?}"
            );
        }
        let result_slot = local_map
            .get(&out_result)
            .copied()
            .ok_or_else(|| anyhow!("missing storage for scoped thread result {:?}", out_result))?;
        let function_type = self.thread_join_result_type();
        let function =
            self.get_or_declare_thread_runtime(SCOPE_JOIN_RESULT_SYMBOL, function_type)?;
        self.build_call2(
            function_type,
            function,
            &mut [handle_slot, result_slot],
            "scoped.thread.join.result.status",
        )
    }

    pub(super) fn codegen_drop_scoped_thread_handle(
        &mut self,
        handle: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<()> {
        let slot = self.scoped_thread_handle_slot(handle, None, func, local_map)?;
        self.codegen_forget_scoped_thread_handle_slot(slot)
    }

    pub(super) fn codegen_forget_scoped_thread_handle_slot(
        &mut self,
        slot: LLVMValueRef,
    ) -> Result<()> {
        let ptr_ty = unsafe { LLVMPointerType(LLVMInt8TypeInContext(self.context), 0) };
        unsafe { LLVMBuildStore(self.builder, LLVMConstPointerNull(ptr_ty), slot) };
        Ok(())
    }

    pub(super) fn codegen_drop_thread_scope(
        &mut self,
        scope: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<()> {
        let (scope_slot, scope_value) = self.load_thread_scope(scope, func, local_map)?;
        let current = unsafe { LLVMGetInsertBlock(self.builder) };
        if current.is_null() {
            bail!("thread scope drop requires an active basic block");
        }
        let parent = unsafe { LLVMGetBasicBlockParent(current) };
        let join_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("thread.scope.drop.join")?.as_ptr(),
            )
        };
        let done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("thread.scope.drop.done")?.as_ptr(),
            )
        };
        let is_null = unsafe {
            LLVMBuildIsNull(
                self.builder,
                scope_value,
                CString::new("thread.scope.drop.isnull")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, is_null, done_bb, join_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, join_bb) };
        let function_type = self.thread_consume_type();
        let function = self.get_or_declare_thread_runtime(SCOPE_JOIN_ALL_SYMBOL, function_type)?;
        let first = self.build_call2(
            function_type,
            function,
            &mut [scope_slot],
            "thread.scope.drop.status",
        )?;
        let retry_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("thread.scope.drop.retry")?.as_ptr(),
            )
        };
        let first_ok = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                first,
                LLVMConstInt(LLVMInt32TypeInContext(self.context), 0, 0),
                CString::new("thread.scope.drop.ok")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, first_ok, done_bb, retry_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, retry_bb) };
        let retry = self.build_call2(
            function_type,
            function,
            &mut [scope_slot],
            "thread.scope.drop.retry.status",
        )?;
        let fatal_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("thread.scope.drop.fatal")?.as_ptr(),
            )
        };
        let retry_ok = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                retry,
                LLVMConstInt(LLVMInt32TypeInContext(self.context), 0, 0),
                CString::new("thread.scope.drop.retry.ok")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, retry_ok, done_bb, fatal_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, fatal_bb) };
        let abort_name = CString::new("abort")?;
        let abort_ty = unsafe {
            LLVMFunctionType(
                LLVMVoidTypeInContext(self.context),
                std::ptr::null_mut(),
                0,
                0,
            )
        };
        let abort = unsafe {
            let existing = LLVMGetNamedFunction(self.module, abort_name.as_ptr());
            if existing.is_null() {
                LLVMAddFunction(self.module, abort_name.as_ptr(), abort_ty)
            } else {
                existing
            }
        };
        self.build_call2(abort_ty, abort, &mut [], "")?;
        unsafe {
            LLVMBuildUnreachable(self.builder);
            LLVMPositionBuilderAtEnd(self.builder, done_bb);
        }
        Ok(())
    }

    fn codegen_thread_consume_unit(
        &mut self,
        handle: LocalId,
        symbol: &str,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let handle_slot = self.thread_handle_slot(handle, func, local_map)?;
        let function_type = self.thread_consume_type();
        let function = self.get_or_declare_thread_runtime(symbol, function_type)?;
        let mut args = [handle_slot];
        self.build_call2(function_type, function, &mut args, "thread.consume.status")
    }

    pub(super) fn codegen_thread_join_unit(
        &mut self,
        handle: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        self.codegen_thread_consume_unit(handle, JOIN_SYMBOL, func, local_map)
    }

    pub(super) fn codegen_thread_join_result(
        &mut self,
        handle: LocalId,
        out_result: LocalId,
        result_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let handle_slot = self.thread_handle_slot(handle, func, local_map)?;
        let actual_result = func
            .locals
            .get(out_result.0 as usize)
            .and_then(|local| local.ty.as_ref())
            .ok_or_else(|| anyhow!("thread result local {:?} has no type", out_result))?;
        let result_matches = actual_result == result_type
            || (Self::is_unit_type(actual_result) && Self::is_unit_type(result_type));
        if !result_matches {
            bail!("thread join result local has type {actual_result:?}, expected {result_type:?}");
        }
        let result_slot = local_map
            .get(&out_result)
            .copied()
            .ok_or_else(|| anyhow!("missing storage for thread result {:?}", out_result))?;
        let function_type = self.thread_join_result_type();
        let function = self.get_or_declare_thread_runtime(JOIN_RESULT_SYMBOL, function_type)?;
        let mut args = [handle_slot, result_slot];
        self.build_call2(
            function_type,
            function,
            &mut args,
            "thread.join.result.status",
        )
    }

    pub(super) fn codegen_thread_detach_unit(
        &mut self,
        handle: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        self.codegen_thread_consume_unit(handle, DETACH_SYMBOL, func, local_map)
    }

    pub(super) fn codegen_drop_thread_handle(
        &mut self,
        handle: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<()> {
        let handle_slot = self.thread_handle_slot(handle, func, local_map)?;
        self.codegen_drop_thread_handle_slot(handle_slot)
    }

    pub(super) fn codegen_drop_thread_handle_slot(
        &mut self,
        handle_slot: LLVMValueRef,
    ) -> Result<()> {
        let ptr_ty = unsafe { LLVMPointerType(LLVMInt8TypeInContext(self.context), 0) };
        let value = unsafe {
            LLVMBuildLoad2(
                self.builder,
                ptr_ty,
                handle_slot,
                CString::new("thread.drop.handle")?.as_ptr(),
            )
        };
        let current = unsafe { LLVMGetInsertBlock(self.builder) };
        if current.is_null() {
            bail!("thread handle drop requires an active basic block");
        }
        let parent = unsafe { LLVMGetBasicBlockParent(current) };
        let detach_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("thread.drop.detach")?.as_ptr(),
            )
        };
        let done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("thread.drop.done")?.as_ptr(),
            )
        };
        let is_null = unsafe {
            LLVMBuildIsNull(
                self.builder,
                value,
                CString::new("thread.drop.isnull")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, is_null, done_bb, detach_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, detach_bb) };
        let function_type = self.thread_consume_type();
        let function = self.get_or_declare_thread_runtime(DETACH_SYMBOL, function_type)?;
        let first_status = self.build_call2(
            function_type,
            function,
            &mut [handle_slot],
            "thread.consume.status",
        )?;
        let retry_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("thread.drop.retry")?.as_ptr(),
            )
        };
        let first_ok = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                first_status,
                LLVMConstInt(LLVMInt32TypeInContext(self.context), 0, 0),
                CString::new("thread.drop.detach.ok")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, first_ok, done_bb, retry_bb) };

        // A detach failure cannot be silently discarded: the handle remains
        // live and would leak. Retry once for a transient failure (including
        // the deterministic runtime test hook), then fail fast on a persistent
        // runtime invariant violation rather than returning with lost state.
        unsafe { LLVMPositionBuilderAtEnd(self.builder, retry_bb) };
        let retry_status = self.build_call2(
            function_type,
            function,
            &mut [handle_slot],
            "thread.consume.status",
        )?;
        let fatal_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("thread.drop.fatal")?.as_ptr(),
            )
        };
        let retry_ok = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                retry_status,
                LLVMConstInt(LLVMInt32TypeInContext(self.context), 0, 0),
                CString::new("thread.drop.retry.ok")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, retry_ok, done_bb, fatal_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, fatal_bb) };
        let abort_name = CString::new("abort")?;
        let abort = unsafe {
            let existing = LLVMGetNamedFunction(self.module, abort_name.as_ptr());
            if existing.is_null() {
                let abort_ty = LLVMFunctionType(
                    LLVMVoidTypeInContext(self.context),
                    std::ptr::null_mut(),
                    0,
                    0,
                );
                LLVMAddFunction(self.module, abort_name.as_ptr(), abort_ty)
            } else {
                existing
            }
        };
        let abort_ty = unsafe {
            LLVMFunctionType(
                LLVMVoidTypeInContext(self.context),
                std::ptr::null_mut(),
                0,
                0,
            )
        };
        self.build_call2(abort_ty, abort, &mut [], "")?;
        unsafe {
            LLVMBuildUnreachable(self.builder);
            LLVMPositionBuilderAtEnd(self.builder, done_bb);
        }
        Ok(())
    }
}
