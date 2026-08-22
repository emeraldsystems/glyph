//! Compiler/runtime bridge for `Mutex<T>` and lexical `MutexGuard<T>`.
//!
//! The compiler owns a stable allocation `{ native: ptr, value: T }`. The
//! runtime owns only the opaque native mutex behind `native`, so pthread
//! layout never enters the language ABI and payload drop glue stays typed.

use super::*;

const CREATE_SYMBOL: &str = "glyph_mutex_create";
const LOCK_SYMBOL: &str = "glyph_mutex_lock";
const TRY_LOCK_SYMBOL: &str = "glyph_mutex_try_lock";
const UNLOCK_SYMBOL: &str = "glyph_mutex_unlock";
const DESTROY_SYMBOL: &str = "glyph_mutex_destroy";

impl CodegenContext {
    pub(super) fn mutex_control_block_type(&self, elem_type: &Type) -> Result<LLVMTypeRef> {
        unsafe {
            let ptr = LLVMPointerType(LLVMInt8TypeInContext(self.context), 0);
            let mut fields = [ptr, self.get_llvm_type(elem_type)?];
            Ok(LLVMStructTypeInContext(
                self.context,
                fields.as_mut_ptr(),
                fields.len() as u32,
                0,
            ))
        }
    }

    pub(super) fn mutex_pointer_type(&self, elem_type: &Type) -> Result<LLVMTypeRef> {
        Ok(unsafe { LLVMPointerType(self.mutex_control_block_type(elem_type)?, 0) })
    }

    fn mutex_status_function_type(&self, takes_pointer_to_pointer: bool) -> LLVMTypeRef {
        unsafe {
            let ptr = LLVMPointerType(LLVMInt8TypeInContext(self.context), 0);
            let param = if takes_pointer_to_pointer {
                LLVMPointerType(ptr, 0)
            } else {
                ptr
            };
            let mut params = [param];
            LLVMFunctionType(
                LLVMInt32TypeInContext(self.context),
                params.as_mut_ptr(),
                1,
                0,
            )
        }
    }

    fn mutex_runtime(
        &self,
        symbol: &str,
        takes_pointer_to_pointer: bool,
    ) -> Result<(LLVMValueRef, LLVMTypeRef)> {
        let function_type = self.mutex_status_function_type(takes_pointer_to_pointer);
        let name = CString::new(symbol)?;
        unsafe {
            let existing = LLVMGetNamedFunction(self.module, name.as_ptr());
            let function = if existing.is_null() {
                LLVMAddFunction(self.module, name.as_ptr(), function_type)
            } else {
                existing
            };
            Ok((function, function_type))
        }
    }

    fn emit_mutex_abort(&mut self) -> Result<()> {
        let name = CString::new("abort")?;
        let function_type = unsafe {
            LLVMFunctionType(
                LLVMVoidTypeInContext(self.context),
                std::ptr::null_mut(),
                0,
                0,
            )
        };
        let abort = unsafe {
            let existing = LLVMGetNamedFunction(self.module, name.as_ptr());
            if existing.is_null() {
                LLVMAddFunction(self.module, name.as_ptr(), function_type)
            } else {
                existing
            }
        };
        self.build_call2(function_type, abort, &mut [], "")?;
        unsafe { LLVMBuildUnreachable(self.builder) };
        Ok(())
    }

    fn require_zero_status(&mut self, status: LLVMValueRef, label: &str) -> Result<()> {
        let current = unsafe { LLVMGetInsertBlock(self.builder) };
        let parent = unsafe { LLVMGetBasicBlockParent(current) };
        if parent.is_null() {
            bail!("mutex status check requires a parent function");
        }
        let fatal = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new(format!("mutex.{label}.fatal"))?.as_ptr(),
            )
        };
        let done = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new(format!("mutex.{label}.done"))?.as_ptr(),
            )
        };
        let ok = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                status,
                LLVMConstInt(LLVMInt32TypeInContext(self.context), 0, 0),
                CString::new(format!("mutex.{label}.ok"))?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, ok, done, fatal) };
        unsafe { LLVMPositionBuilderAtEnd(self.builder, fatal) };
        self.emit_mutex_abort()?;
        unsafe { LLVMPositionBuilderAtEnd(self.builder, done) };
        Ok(())
    }

    fn require_live_mutex_pointer(&mut self, pointer: LLVMValueRef, label: &str) -> Result<()> {
        let current = unsafe { LLVMGetInsertBlock(self.builder) };
        let parent = unsafe { LLVMGetBasicBlockParent(current) };
        let fatal = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new(format!("mutex.{label}.null"))?.as_ptr(),
            )
        };
        let live = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new(format!("mutex.{label}.live"))?.as_ptr(),
            )
        };
        let is_null = unsafe {
            LLVMBuildIsNull(
                self.builder,
                pointer,
                CString::new(format!("mutex.{label}.isnull"))?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, is_null, fatal, live) };
        unsafe { LLVMPositionBuilderAtEnd(self.builder, fatal) };
        self.emit_mutex_abort()?;
        unsafe { LLVMPositionBuilderAtEnd(self.builder, live) };
        Ok(())
    }

    fn mutex_pointer_from_local(
        &mut self,
        base: LocalId,
        elem_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let actual = func
            .locals
            .get(base.0 as usize)
            .and_then(|local| local.ty.as_ref())
            .ok_or_else(|| anyhow!("mutex local {:?} has no type", base))?;
        let expected = Type::mutex(elem_type.clone());
        let is_ref = matches!(actual, Type::Ref(inner, _) if inner.as_ref() == &expected);
        if actual != &expected && !is_ref {
            bail!(
                "Mutex MIR references local {:?} as Mutex<{:?}>, but its type is {:?}",
                base,
                elem_type,
                actual
            );
        }
        let slot = local_map
            .get(&base)
            .copied()
            .ok_or_else(|| anyhow!("missing storage for mutex local {:?}", base))?;
        let pointer_type = self.mutex_pointer_type(elem_type)?;
        let owner_slot = if is_ref {
            unsafe {
                LLVMBuildLoad2(
                    self.builder,
                    LLVMPointerType(pointer_type, 0),
                    slot,
                    CString::new("mutex.ref.slot")?.as_ptr(),
                )
            }
        } else {
            slot
        };
        let pointer = unsafe {
            LLVMBuildLoad2(
                self.builder,
                pointer_type,
                owner_slot,
                CString::new("mutex.ptr")?.as_ptr(),
            )
        };
        self.require_live_mutex_pointer(pointer, "access")?;
        Ok(pointer)
    }

    fn native_mutex_pointer(
        &self,
        control: LLVMValueRef,
        elem_type: &Type,
        label: &str,
    ) -> Result<LLVMValueRef> {
        let field = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                self.mutex_control_block_type(elem_type)?,
                control,
                0,
                CString::new(format!("mutex.{label}.native.ptr"))?.as_ptr(),
            )
        };
        let ptr = unsafe { LLVMPointerType(LLVMInt8TypeInContext(self.context), 0) };
        Ok(unsafe {
            LLVMBuildLoad2(
                self.builder,
                ptr,
                field,
                CString::new(format!("mutex.{label}.native"))?.as_ptr(),
            )
        })
    }

    pub(super) fn codegen_mutex_new(
        &mut self,
        value: &MirValue,
        elem_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let control_type = self.mutex_control_block_type(elem_type)?;
        let malloc = self.ensure_malloc_fn()?;
        let mut malloc_args = [unsafe { LLVMSizeOf(control_type) }];
        let raw = self.build_call2(
            self.malloc_function_type(),
            malloc,
            &mut malloc_args,
            "mutex.alloc",
        )?;
        let control = unsafe {
            LLVMBuildBitCast(
                self.builder,
                raw,
                self.mutex_pointer_type(elem_type)?,
                CString::new("mutex.control")?.as_ptr(),
            )
        };
        self.require_live_mutex_pointer(control, "alloc")?;

        let native_field = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                control_type,
                control,
                0,
                CString::new("mutex.native.slot")?.as_ptr(),
            )
        };
        let (create, create_type) = self.mutex_runtime(CREATE_SYMBOL, true)?;
        let mut create_args = [native_field];
        let status =
            self.build_call2(create_type, create, &mut create_args, "mutex.create.status")?;
        self.require_zero_status(status, "create")?;

        let payload_field = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                control_type,
                control,
                1,
                CString::new("mutex.value.ptr")?.as_ptr(),
            )
        };
        let payload = self.codegen_value_owned(value, elem_type, func, local_map)?;
        unsafe { LLVMBuildStore(self.builder, payload, payload_field) };
        Ok(control)
    }

    fn codegen_mutex_acquire(
        &mut self,
        base: LocalId,
        elem_type: &Type,
        nonblocking: bool,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let control = self.mutex_pointer_from_local(base, elem_type, func, local_map)?;
        let native = self.native_mutex_pointer(control, elem_type, "lock")?;
        let symbol = if nonblocking {
            TRY_LOCK_SYMBOL
        } else {
            LOCK_SYMBOL
        };
        let (lock, lock_type) = self.mutex_runtime(symbol, false)?;
        let mut args = [native];
        let status = self.build_call2(lock_type, lock, &mut args, "mutex.lock.status")?;
        if !nonblocking {
            self.require_zero_status(status, "lock")?;
            return Ok(control);
        }

        let current = unsafe { LLVMGetInsertBlock(self.builder) };
        let parent = unsafe { LLVMGetBasicBlockParent(current) };
        let busy = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("mutex.try.busy")?.as_ptr(),
            )
        };
        let check_error = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("mutex.try.check_error")?.as_ptr(),
            )
        };
        let acquired = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("mutex.try.acquired")?.as_ptr(),
            )
        };
        let done = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("mutex.try.done")?.as_ptr(),
            )
        };
        let is_busy = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                status,
                LLVMConstInt(LLVMInt32TypeInContext(self.context), 1, 0),
                CString::new("mutex.try.is_busy")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, is_busy, busy, check_error) };
        unsafe { LLVMPositionBuilderAtEnd(self.builder, busy) };
        unsafe { LLVMBuildBr(self.builder, done) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, check_error) };
        let is_ok = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                status,
                LLVMConstInt(LLVMInt32TypeInContext(self.context), 0, 0),
                CString::new("mutex.try.is_ok")?.as_ptr(),
            )
        };
        let fatal = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("mutex.try.fatal")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, is_ok, acquired, fatal) };
        unsafe { LLVMPositionBuilderAtEnd(self.builder, fatal) };
        self.emit_mutex_abort()?;
        unsafe { LLVMPositionBuilderAtEnd(self.builder, acquired) };
        unsafe { LLVMBuildBr(self.builder, done) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, done) };
        let pointer_type = self.mutex_pointer_type(elem_type)?;
        let result = unsafe {
            LLVMBuildPhi(
                self.builder,
                pointer_type,
                CString::new("mutex.try.guard")?.as_ptr(),
            )
        };
        let mut values = [unsafe { LLVMConstPointerNull(pointer_type) }, control];
        let mut blocks = [busy, acquired];
        unsafe { LLVMAddIncoming(result, values.as_mut_ptr(), blocks.as_mut_ptr(), 2) };
        Ok(result)
    }

    pub(super) fn codegen_mutex_lock(
        &mut self,
        base: LocalId,
        elem_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        self.codegen_mutex_acquire(base, elem_type, false, func, local_map)
    }

    pub(super) fn codegen_mutex_try_lock(
        &mut self,
        base: LocalId,
        elem_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        self.codegen_mutex_acquire(base, elem_type, true, func, local_map)
    }

    fn guard_pointer_from_local(
        &mut self,
        guard: LocalId,
        elem_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        require_acquired: bool,
    ) -> Result<LLVMValueRef> {
        let actual = func
            .locals
            .get(guard.0 as usize)
            .and_then(|local| local.ty.as_ref());
        let expected = Type::mutex_guard(elem_type.clone());
        if actual != Some(&expected) {
            bail!("MutexGuard MIR type mismatch: expected {expected:?}, found {actual:?}");
        }
        let slot = local_map
            .get(&guard)
            .copied()
            .ok_or_else(|| anyhow!("missing storage for mutex guard {:?}", guard))?;
        let pointer = unsafe {
            LLVMBuildLoad2(
                self.builder,
                self.mutex_pointer_type(elem_type)?,
                slot,
                CString::new("mutex.guard.ptr")?.as_ptr(),
            )
        };
        if require_acquired {
            self.require_live_mutex_pointer(pointer, "guard")?;
        }
        Ok(pointer)
    }

    pub(super) fn codegen_mutex_guard_is_acquired(
        &mut self,
        guard: LocalId,
        elem_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let pointer = self.guard_pointer_from_local(guard, elem_type, func, local_map, false)?;
        Ok(unsafe {
            LLVMBuildIsNotNull(
                self.builder,
                pointer,
                CString::new("mutex.guard.acquired")?.as_ptr(),
            )
        })
    }

    pub(super) fn codegen_mutex_guard_borrow(
        &mut self,
        guard: LocalId,
        elem_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let pointer = self.guard_pointer_from_local(guard, elem_type, func, local_map, true)?;
        Ok(unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                self.mutex_control_block_type(elem_type)?,
                pointer,
                1,
                CString::new("mutex.guard.value")?.as_ptr(),
            )
        })
    }

    pub(super) fn codegen_drop_mutex_guard_slot(
        &mut self,
        slot: LLVMValueRef,
        elem_type: &Type,
    ) -> Result<()> {
        let pointer_type = self.mutex_pointer_type(elem_type)?;
        let pointer = unsafe {
            LLVMBuildLoad2(
                self.builder,
                pointer_type,
                slot,
                CString::new("mutex.guard.drop.ptr")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, LLVMConstPointerNull(pointer_type), slot) };
        let current = unsafe { LLVMGetInsertBlock(self.builder) };
        let parent = unsafe { LLVMGetBasicBlockParent(current) };
        let unlock = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("mutex.guard.drop.unlock")?.as_ptr(),
            )
        };
        let done = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("mutex.guard.drop.done")?.as_ptr(),
            )
        };
        let is_null = unsafe {
            LLVMBuildIsNull(
                self.builder,
                pointer,
                CString::new("mutex.guard.drop.isnull")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, is_null, done, unlock) };
        unsafe { LLVMPositionBuilderAtEnd(self.builder, unlock) };
        let native = self.native_mutex_pointer(pointer, elem_type, "unlock")?;
        let (function, function_type) = self.mutex_runtime(UNLOCK_SYMBOL, false)?;
        let mut args = [native];
        let status = self.build_call2(function_type, function, &mut args, "mutex.unlock.status")?;
        self.require_zero_status(status, "unlock")?;
        unsafe { LLVMBuildBr(self.builder, done) };
        unsafe { LLVMPositionBuilderAtEnd(self.builder, done) };
        Ok(())
    }

    pub(super) fn codegen_drop_mutex_slot(
        &mut self,
        slot: LLVMValueRef,
        elem_type: &Type,
    ) -> Result<()> {
        let pointer_type = self.mutex_pointer_type(elem_type)?;
        let pointer = unsafe {
            LLVMBuildLoad2(
                self.builder,
                pointer_type,
                slot,
                CString::new("mutex.drop.ptr")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, LLVMConstPointerNull(pointer_type), slot) };
        let current = unsafe { LLVMGetInsertBlock(self.builder) };
        let parent = unsafe { LLVMGetBasicBlockParent(current) };
        let destroy = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("mutex.drop.destroy")?.as_ptr(),
            )
        };
        let done = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("mutex.drop.done")?.as_ptr(),
            )
        };
        let is_null = unsafe {
            LLVMBuildIsNull(
                self.builder,
                pointer,
                CString::new("mutex.drop.isnull")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, is_null, done, destroy) };
        unsafe { LLVMPositionBuilderAtEnd(self.builder, destroy) };
        let control_type = self.mutex_control_block_type(elem_type)?;
        let native_field = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                control_type,
                pointer,
                0,
                CString::new("mutex.drop.native.slot")?.as_ptr(),
            )
        };
        let (function, function_type) = self.mutex_runtime(DESTROY_SYMBOL, true)?;
        let mut args = [native_field];
        let status =
            self.build_call2(function_type, function, &mut args, "mutex.destroy.status")?;
        self.require_zero_status(status, "destroy")?;
        let payload = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                control_type,
                pointer,
                1,
                CString::new("mutex.drop.value")?.as_ptr(),
            )
        };
        self.codegen_drop_elem_slot(payload, elem_type)?;
        self.codegen_free(pointer)?;
        unsafe { LLVMBuildBr(self.builder, done) };
        unsafe { LLVMPositionBuilderAtEnd(self.builder, done) };
        Ok(())
    }
}
