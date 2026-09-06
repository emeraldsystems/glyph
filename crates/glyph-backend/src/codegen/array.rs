use super::*;

impl CodegenContext {
    pub(super) fn codegen_array_literal(
        &mut self,
        elem_type: &Type,
        elements: &[MirValue],
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        if elements.is_empty() {
            bail!("array literal must have at least one element");
        }

        let array_ty = Type::Array(Box::new(elem_type.clone()), elements.len());
        let llvm_array_ty = self.get_llvm_type(&array_ty)?;
        let alloca_name = CString::new("array.tmp")?;
        let alloca = unsafe { self.build_entry_alloca(self.builder, llvm_array_ty, alloca_name.as_ptr()) };

        let i32_ty = unsafe { LLVMInt32TypeInContext(self.context) };
        let zero = unsafe { LLVMConstInt(i32_ty, 0, 0) };
        let llvm_elem_ty = self.get_llvm_type(elem_type)?;
        let elem_signed = matches!(elem_type, Type::I8 | Type::I16 | Type::I32 | Type::I64);

        for (idx, element) in elements.iter().enumerate() {
            let mut elem_val = self.codegen_value(element, func, local_map)?;
            // An element's own computed value can come out at a different
            // integer width than the array's declared element type (e.g. a
            // negated literal, still computed at i32, going into an `[i8; N]`
            // literal) - coerce it to match before storing, the same way
            // `codegen_struct_literal` coerces a field value to its declared
            // field type. A no-op when the widths already agree.
            elem_val = self.coerce_int_value(elem_val, llvm_elem_ty, elem_signed);
            let index_const = unsafe { LLVMConstInt(i32_ty, idx as u64, 0) };
            let mut indices = vec![zero, index_const];
            let gep_name = CString::new(format!("array.elem{}", idx))?;
            let elem_ptr = unsafe {
                LLVMBuildInBoundsGEP2(
                    self.builder,
                    llvm_array_ty,
                    alloca,
                    indices.as_mut_ptr(),
                    indices.len() as u32,
                    gep_name.as_ptr(),
                )
            };
            unsafe {
                LLVMBuildStore(self.builder, elem_val, elem_ptr);
            }
        }

        let load_name = CString::new("array.value")?;
        Ok(unsafe { LLVMBuildLoad2(self.builder, llvm_array_ty, alloca, load_name.as_ptr()) })
    }

    pub(super) fn codegen_array_index(
        &mut self,
        base: LocalId,
        index: &MirValue,
        bounds_check: bool,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let base_ptr = *local_map
            .get(&base)
            .ok_or_else(|| anyhow!("undefined local {:?}", base))?;

        let base_ty = func
            .locals
            .get(base.0 as usize)
            .and_then(|local| local.ty.as_ref())
            .ok_or_else(|| anyhow!("array index base has unknown type"))?;

        let (elem_ty, size) = match base_ty {
            Type::Array(elem, size) => (elem.as_ref().clone(), *size),
            _ => bail!("array index base is not an array"),
        };

        let llvm_array_ty = self.get_llvm_type(base_ty)?;
        let elem_llvm_ty = self.get_llvm_type(&elem_ty)?;
        let index_val = self.codegen_value(index, func, local_map)?;

        if bounds_check {
            self.emit_bounds_check(index_val, size)?;
        }

        let i32_ty = unsafe { LLVMInt32TypeInContext(self.context) };
        let zero = unsafe { LLVMConstInt(i32_ty, 0, 0) };
        let mut indices = vec![zero, index_val];
        let gep_name = CString::new("array.index")?;
        let elem_ptr = unsafe {
            LLVMBuildInBoundsGEP2(
                self.builder,
                llvm_array_ty,
                base_ptr,
                indices.as_mut_ptr(),
                indices.len() as u32,
                gep_name.as_ptr(),
            )
        };
        let load_name = CString::new("array.elem.load")?;
        Ok(unsafe { LLVMBuildLoad2(self.builder, elem_llvm_ty, elem_ptr, load_name.as_ptr()) })
    }

    pub(super) fn codegen_array_len(
        &mut self,
        base: LocalId,
        func: &MirFunction,
    ) -> Result<LLVMValueRef> {
        let base_ty = func
            .locals
            .get(base.0 as usize)
            .and_then(|local| local.ty.as_ref())
            .ok_or_else(|| anyhow!("array length base has unknown type"))?;

        let size = match base_ty {
            Type::Array(_, size) => *size,
            _ => bail!(".len() is only supported on arrays"),
        };

        let i32_ty = unsafe { LLVMInt32TypeInContext(self.context) };
        Ok(unsafe { LLVMConstInt(i32_ty, size as u64, 0) })
    }

    pub(super) fn emit_bounds_check(
        &mut self,
        index_val: LLVMValueRef,
        array_size: usize,
    ) -> Result<()> {
        unsafe {
            let i32_ty = LLVMInt32TypeInContext(self.context);
            let zero = LLVMConstInt(i32_ty, 0, 0);
            let nonneg_name = CString::new("idx.nonneg")?;
            let non_negative = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntSGE,
                index_val,
                zero,
                nonneg_name.as_ptr(),
            );

            let size_const = LLVMConstInt(i32_ty, array_size as u64, 0);
            let upper_name = CString::new("idx.upper")?;
            let within_upper = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntSLT,
                index_val,
                size_const,
                upper_name.as_ptr(),
            );

            let cond_name = CString::new("idx.in_bounds")?;
            let cond = LLVMBuildAnd(self.builder, non_negative, within_upper, cond_name.as_ptr());

            let current_block = LLVMGetInsertBlock(self.builder);
            if current_block.is_null() {
                bail!("bounds check requires valid insertion block");
            }
            let parent_func = LLVMGetBasicBlockParent(current_block);
            if parent_func.is_null() {
                bail!("bounds check parent function missing");
            }

            let ok_name = CString::new("bounds.ok")?;
            let panic_name = CString::new("bounds.panic")?;
            let ok_bb = LLVMAppendBasicBlockInContext(self.context, parent_func, ok_name.as_ptr());
            let panic_bb =
                LLVMAppendBasicBlockInContext(self.context, parent_func, panic_name.as_ptr());

            LLVMBuildCondBr(self.builder, cond, ok_bb, panic_bb);

            LLVMPositionBuilderAtEnd(self.builder, panic_bb);
            self.emit_panic()?;
            LLVMBuildUnreachable(self.builder);

            LLVMPositionBuilderAtEnd(self.builder, ok_bb);
        }
        Ok(())
    }

    pub(super) fn emit_vec_bounds_check(
        &mut self,
        index_val: LLVMValueRef,
        len_val: LLVMValueRef,
        len_ty: LLVMTypeRef,
    ) -> Result<()> {
        unsafe {
            let zero = LLVMConstInt(len_ty, 0, 0);
            let nonneg_name = CString::new("idx.nonneg")?;
            let non_negative = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntSGE,
                index_val,
                zero,
                nonneg_name.as_ptr(),
            );

            let upper_name = CString::new("idx.upper")?;
            let within_upper = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntSLT,
                index_val,
                len_val,
                upper_name.as_ptr(),
            );

            let cond_name = CString::new("idx.in_bounds")?;
            let cond = LLVMBuildAnd(self.builder, non_negative, within_upper, cond_name.as_ptr());

            let current_block = LLVMGetInsertBlock(self.builder);
            if current_block.is_null() {
                bail!("bounds check requires valid insertion block");
            }
            let parent_func = LLVMGetBasicBlockParent(current_block);
            if parent_func.is_null() {
                bail!("bounds check parent function missing");
            }

            let ok_name = CString::new("bounds.ok")?;
            let panic_name = CString::new("bounds.panic")?;
            let ok_bb = LLVMAppendBasicBlockInContext(self.context, parent_func, ok_name.as_ptr());
            let panic_bb =
                LLVMAppendBasicBlockInContext(self.context, parent_func, panic_name.as_ptr());

            LLVMBuildCondBr(self.builder, cond, ok_bb, panic_bb);

            LLVMPositionBuilderAtEnd(self.builder, panic_bb);
            self.emit_panic()?;
            LLVMBuildUnreachable(self.builder);

            LLVMPositionBuilderAtEnd(self.builder, ok_bb);
        }
        Ok(())
    }

    pub(super) fn emit_panic(&mut self) -> Result<()> {
        unsafe {
            let trap_name = CString::new("llvm.trap")?;
            let void_ty = LLVMVoidTypeInContext(self.context);
            let trap_ty = LLVMFunctionType(void_ty, std::ptr::null_mut(), 0, 0);
            let mut trap_fn = LLVMGetNamedFunction(self.module, trap_name.as_ptr());
            if trap_fn.is_null() {
                trap_fn = LLVMAddFunction(self.module, trap_name.as_ptr(), trap_ty);
            }
            let mut args: Vec<LLVMValueRef> = Vec::new();
            let _ = self.build_call2(trap_ty, trap_fn, &mut args, "panic.trap")?;
        }
        Ok(())
    }
}
