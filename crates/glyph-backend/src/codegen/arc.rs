//! Thread-safe atomic reference-counted ownership.
//!
//! `Arc<T>` is deliberately separate from the legacy, single-threaded
//! `Shared<T>` representation. An Arc value is a pointer to one allocation:
//!
//! ```text
//! { strong: AtomicUsize, value: T }
//! ```
//!
//! Publishing an Arc never moves that allocation. Cloning uses a relaxed
//! atomic increment; releasing uses a release decrement and an acquire fence
//! before the last owner destroys `T`. This is the standard ref-count
//! synchronization pattern: destruction observes all writes sequenced before
//! releases by previous owners without imposing SeqCst on every clone.

use super::*;
use glyph_core::atomic::{AtomicOrdering, AtomicRmwOp, AtomicScalar};

impl CodegenContext {
    pub(super) fn arc_control_block_type(&self, elem_type: &Type) -> Result<LLVMTypeRef> {
        unsafe {
            let strong = self.atomic_storage_type(AtomicScalar::Usize)?;
            let value = self.get_llvm_type(elem_type)?;
            let mut fields = [strong, value];
            Ok(LLVMStructTypeInContext(
                self.context,
                fields.as_mut_ptr(),
                fields.len() as u32,
                0,
            ))
        }
    }

    pub(super) fn arc_pointer_type(&self, elem_type: &Type) -> Result<LLVMTypeRef> {
        Ok(unsafe { LLVMPointerType(self.arc_control_block_type(elem_type)?, 0) })
    }

    fn validate_arc_local(
        &self,
        base: LocalId,
        elem_type: &Type,
        func: &MirFunction,
    ) -> Result<()> {
        let actual = func
            .locals
            .get(base.0 as usize)
            .and_then(|local| local.ty.as_ref());
        let expected = Type::arc(elem_type.clone());
        if actual != Some(&expected) {
            bail!(
                "Arc MIR references local {:?} as Arc<{:?}>, but its type is {:?}",
                base,
                elem_type,
                actual
            );
        }
        Ok(())
    }

    fn arc_pointer_from_local(
        &self,
        base: LocalId,
        elem_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        self.validate_arc_local(base, elem_type, func)?;
        let slot = local_map
            .get(&base)
            .copied()
            .ok_or_else(|| anyhow!("missing storage for Arc local {:?}", base))?;
        Ok(unsafe {
            LLVMBuildLoad2(
                self.builder,
                self.arc_pointer_type(elem_type)?,
                slot,
                CString::new("arc.ptr")?.as_ptr(),
            )
        })
    }

    fn abort_function_type(&self) -> LLVMTypeRef {
        unsafe {
            LLVMFunctionType(
                LLVMVoidTypeInContext(self.context),
                std::ptr::null_mut(),
                0,
                0,
            )
        }
    }

    fn emit_arc_abort(&mut self) -> Result<()> {
        let name = CString::new("abort")?;
        let abort = unsafe {
            let existing = LLVMGetNamedFunction(self.module, name.as_ptr());
            if existing.is_null() {
                LLVMAddFunction(self.module, name.as_ptr(), self.abort_function_type())
            } else {
                existing
            }
        };
        self.build_call2(self.abort_function_type(), abort, &mut [], "")?;
        unsafe { LLVMBuildUnreachable(self.builder) };
        Ok(())
    }

    /// Branch to a fatal block when an operation observes a consumed/null Arc.
    /// Returns the continuation block, with the builder positioned at its end.
    fn require_live_arc(&mut self, ptr: LLVMValueRef, label: &str) -> Result<()> {
        let current = unsafe { LLVMGetInsertBlock(self.builder) };
        if current.is_null() {
            bail!("builder not positioned while validating Arc pointer");
        }
        let parent = unsafe { LLVMGetBasicBlockParent(current) };
        let fatal = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new(format!("arc.{label}.null"))?.as_ptr(),
            )
        };
        let live = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new(format!("arc.{label}.live"))?.as_ptr(),
            )
        };
        let is_null = unsafe {
            LLVMBuildIsNull(
                self.builder,
                ptr,
                CString::new(format!("arc.{label}.isnull"))?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, is_null, fatal, live) };
        unsafe { LLVMPositionBuilderAtEnd(self.builder, fatal) };
        self.emit_arc_abort()?;
        unsafe { LLVMPositionBuilderAtEnd(self.builder, live) };
        Ok(())
    }

    pub(super) fn codegen_arc_new(
        &mut self,
        value: &MirValue,
        elem_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let control = self.arc_control_block_type(elem_type)?;
        let size = unsafe { LLVMSizeOf(control) };
        let malloc = self.ensure_malloc_fn()?;
        let mut args = [size];
        let raw = self.build_call2(self.malloc_function_type(), malloc, &mut args, "arc.alloc")?;
        let ptr = unsafe {
            LLVMBuildBitCast(
                self.builder,
                raw,
                self.arc_pointer_type(elem_type)?,
                CString::new("arc.control")?.as_ptr(),
            )
        };
        self.require_live_arc(ptr, "alloc")?;

        let strong_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                control,
                ptr,
                0,
                CString::new("arc.strong.ptr")?.as_ptr(),
            )
        };
        // Initialization is non-atomic because the allocation is still
        // private. Every access after publication goes through atomic glue.
        let strong_ty = self.atomic_storage_type(AtomicScalar::Usize)?;
        unsafe { LLVMBuildStore(self.builder, LLVMConstInt(strong_ty, 1, 0), strong_ptr) };

        let payload_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                control,
                ptr,
                1,
                CString::new("arc.value.ptr")?.as_ptr(),
            )
        };
        let payload = self.codegen_value_owned(value, elem_type, func, local_map)?;
        unsafe { LLVMBuildStore(self.builder, payload, payload_ptr) };
        Ok(ptr)
    }

    fn codegen_arc_clone_pointer(
        &mut self,
        ptr: LLVMValueRef,
        elem_type: &Type,
    ) -> Result<LLVMValueRef> {
        self.require_live_arc(ptr, "clone")?;
        let control = self.arc_control_block_type(elem_type)?;
        let strong_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                control,
                ptr,
                0,
                CString::new("arc.clone.strong.ptr")?.as_ptr(),
            )
        };
        let strong_ty = self.atomic_storage_type(AtomicScalar::Usize)?;
        let one = unsafe { LLVMConstInt(strong_ty, 1, 0) };
        let old = self.build_atomic_rmw_value(
            strong_ptr,
            one,
            AtomicScalar::Usize,
            AtomicRmwOp::Add,
            AtomicOrdering::Relaxed,
        )?;

        // Overflow is fatal. Although the fetch-add has wrapped by this
        // point, `abort` prevents any wrapped count from becoming observable
        // to safe code or allowing premature destruction.
        let overflow = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                old,
                LLVMConstAllOnes(strong_ty),
                CString::new("arc.clone.overflow")?.as_ptr(),
            )
        };
        let current = unsafe { LLVMGetInsertBlock(self.builder) };
        let parent = unsafe { LLVMGetBasicBlockParent(current) };
        let fatal = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("arc.clone.fatal")?.as_ptr(),
            )
        };
        let done = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("arc.clone.done")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, overflow, fatal, done) };
        unsafe { LLVMPositionBuilderAtEnd(self.builder, fatal) };
        self.emit_arc_abort()?;
        unsafe { LLVMPositionBuilderAtEnd(self.builder, done) };
        Ok(ptr)
    }

    pub(super) fn codegen_arc_clone(
        &mut self,
        base: LocalId,
        elem_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let ptr = self.arc_pointer_from_local(base, elem_type, func, local_map)?;
        self.codegen_arc_clone_pointer(ptr, elem_type)
    }

    pub(super) fn codegen_arc_borrow(
        &mut self,
        base: LocalId,
        elem_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let ptr = self.arc_pointer_from_local(base, elem_type, func, local_map)?;
        self.require_live_arc(ptr, "borrow")?;
        Ok(unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                self.arc_control_block_type(elem_type)?,
                ptr,
                1,
                CString::new("arc.borrow.value")?.as_ptr(),
            )
        })
    }

    pub(super) fn codegen_drop_arc_slot(
        &mut self,
        slot: LLVMValueRef,
        elem_type: &Type,
    ) -> Result<()> {
        let ptr_ty = self.arc_pointer_type(elem_type)?;
        let ptr = unsafe {
            LLVMBuildLoad2(
                self.builder,
                ptr_ty,
                slot,
                CString::new("arc.drop.ptr")?.as_ptr(),
            )
        };
        // Clear before entering any user-visible payload drop glue. Reentrant
        // cleanup therefore cannot release this owner twice.
        unsafe { LLVMBuildStore(self.builder, LLVMConstPointerNull(ptr_ty), slot) };

        let current = unsafe { LLVMGetInsertBlock(self.builder) };
        if current.is_null() {
            bail!("builder not positioned while dropping Arc");
        }
        let parent = unsafe { LLVMGetBasicBlockParent(current) };
        let release = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("arc.drop.release")?.as_ptr(),
            )
        };
        let last = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("arc.drop.last")?.as_ptr(),
            )
        };
        let fatal = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("arc.drop.underflow")?.as_ptr(),
            )
        };
        let done = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("arc.drop.done")?.as_ptr(),
            )
        };
        let is_null = unsafe {
            LLVMBuildIsNull(self.builder, ptr, CString::new("arc.drop.isnull")?.as_ptr())
        };
        unsafe { LLVMBuildCondBr(self.builder, is_null, done, release) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, release) };
        let control = self.arc_control_block_type(elem_type)?;
        let strong_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                control,
                ptr,
                0,
                CString::new("arc.drop.strong.ptr")?.as_ptr(),
            )
        };
        let strong_ty = self.atomic_storage_type(AtomicScalar::Usize)?;
        let one = unsafe { LLVMConstInt(strong_ty, 1, 0) };
        let old = self.build_atomic_rmw_value(
            strong_ptr,
            one,
            AtomicScalar::Usize,
            AtomicRmwOp::Sub,
            AtomicOrdering::Release,
        )?;
        let was_zero = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                old,
                LLVMConstInt(strong_ty, 0, 0),
                CString::new("arc.drop.was_zero")?.as_ptr(),
            )
        };
        let check_last = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("arc.drop.check_last")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, was_zero, fatal, check_last) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, check_last) };
        let was_last = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                old,
                one,
                CString::new("arc.drop.was_last")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, was_last, last, done) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, fatal) };
        self.emit_arc_abort()?;

        unsafe { LLVMPositionBuilderAtEnd(self.builder, last) };
        self.build_atomic_fence_value(AtomicOrdering::Acquire)?;
        let payload = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                control,
                ptr,
                1,
                CString::new("arc.drop.value")?.as_ptr(),
            )
        };
        self.codegen_drop_elem_slot(payload, elem_type)?;
        self.codegen_free(ptr)?;
        unsafe { LLVMBuildBr(self.builder, done) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, done) };
        Ok(())
    }

    pub(super) fn emit_clone_arc(
        &mut self,
        dst: LLVMValueRef,
        src: LLVMValueRef,
        elem_type: &Type,
    ) -> Result<()> {
        let ptr_ty = self.arc_pointer_type(elem_type)?;
        let ptr = unsafe {
            LLVMBuildLoad2(
                self.builder,
                ptr_ty,
                src,
                CString::new("clone.arc.load")?.as_ptr(),
            )
        };
        let cloned = self.codegen_arc_clone_pointer(ptr, elem_type)?;
        unsafe { LLVMBuildStore(self.builder, cloned, dst) };
        Ok(())
    }
}
