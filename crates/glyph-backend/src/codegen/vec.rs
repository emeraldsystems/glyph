use super::*;

impl CodegenContext {
    pub(super) fn codegen_vec_init_with_capacity_value(
        &mut self,
        elem_type: &Type,
        cap_val: LLVMValueRef,
    ) -> Result<LLVMValueRef> {
        let inst_name = format!("Vec${}", self.type_key(elem_type));
        let (llvm_vec_ty, usize_ty) = self
            .get_vec_layout(&inst_name)
            .ok_or_else(|| anyhow!("missing vec layout for {}", inst_name))?;

        let alloca_name = CString::new("vec.tmp")?;
        let alloca = unsafe { self.build_entry_alloca(self.builder, llvm_vec_ty, alloca_name.as_ptr()) };

        let len_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                alloca,
                1,
                CString::new("vec.len").unwrap().as_ptr(),
            )
        };
        let zero = unsafe { LLVMConstInt(usize_ty, 0, 0) };
        unsafe { LLVMBuildStore(self.builder, zero, len_ptr) };

        let cap_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                alloca,
                2,
                CString::new("vec.cap").unwrap().as_ptr(),
            )
        };
        let cap_val = self.ensure_usize(cap_val, usize_ty)?;
        unsafe { LLVMBuildStore(self.builder, cap_val, cap_ptr) };

        let data_field_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                alloca,
                0,
                CString::new("vec.data").unwrap().as_ptr(),
            )
        };
        let elem_ptr_ty = self.get_llvm_type(&Type::RawPtr(Box::new(elem_type.clone())))?;
        let elem_llvm_ty = self.get_llvm_type(elem_type)?;
        let elem_size = unsafe { LLVMSizeOf(elem_llvm_ty) };
        let cap_is_zero = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                cap_val,
                zero,
                CString::new("vec.cap.zero")?.as_ptr(),
            )
        };

        let parent_bb = unsafe { LLVMGetInsertBlock(self.builder) };
        let parent_fn = unsafe { LLVMGetBasicBlockParent(parent_bb) };
        let alloc_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("vec.cap.alloc")?.as_ptr(),
            )
        };
        let zero_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("vec.cap.zero")?.as_ptr(),
            )
        };
        let done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("vec.cap.done")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, cap_is_zero, zero_bb, alloc_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, alloc_bb) };
        let alloc_size = unsafe {
            LLVMBuildMul(
                self.builder,
                cap_val,
                elem_size,
                CString::new("vec.cap.alloc.size")?.as_ptr(),
            )
        };
        let malloc_fn = self.ensure_malloc_fn()?;
        let malloc_ty = self.malloc_function_type();
        let mut malloc_args = vec![alloc_size];
        let raw_ptr = unsafe {
            LLVMBuildCall2(
                self.builder,
                malloc_ty,
                malloc_fn,
                malloc_args.as_mut_ptr(),
                malloc_args.len() as u32,
                CString::new("vec.cap.malloc")?.as_ptr(),
            )
        };
        let data_alloc = unsafe {
            LLVMBuildBitCast(
                self.builder,
                raw_ptr,
                elem_ptr_ty,
                CString::new("vec.cap.cast")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, data_alloc, data_field_ptr) };
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, zero_bb) };
        let null_data = unsafe { LLVMConstNull(elem_ptr_ty) };
        unsafe { LLVMBuildStore(self.builder, null_data, data_field_ptr) };
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, done_bb) };
        let load_name = CString::new("vec.value")?;
        Ok(unsafe { LLVMBuildLoad2(self.builder, llvm_vec_ty, alloca, load_name.as_ptr()) })
    }

    pub(super) fn codegen_vec_push_no_grow(
        &mut self,
        vec_ptr: LLVMValueRef,
        elem_type: &Type,
        value: LLVMValueRef,
    ) -> Result<()> {
        let inst_name = format!("Vec${}", self.type_key(elem_type));
        let (llvm_vec_ty, usize_ty) = self
            .get_vec_layout(&inst_name)
            .ok_or_else(|| anyhow!("missing vec layout for {}", inst_name))?;

        let data_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                vec_ptr,
                0,
                CString::new("vec.data").unwrap().as_ptr(),
            )
        };
        let len_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                vec_ptr,
                1,
                CString::new("vec.len").unwrap().as_ptr(),
            )
        };
        let current_len = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                len_ptr,
                CString::new("vec.len.load")?.as_ptr(),
            )
        };
        let raw_ptr_ty = self.get_llvm_type(&Type::RawPtr(Box::new(elem_type.clone())))?;
        let data_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                raw_ptr_ty,
                data_ptr,
                CString::new("vec.data.load")?.as_ptr(),
            )
        };
        let elem_llvm_ty = self.get_llvm_type(elem_type)?;
        let elem_ptr = unsafe {
            LLVMBuildGEP2(
                self.builder,
                elem_llvm_ty,
                data_val,
                vec![current_len].as_mut_ptr(),
                1,
                CString::new("vec.elem.ptr")?.as_ptr(),
            )
        };
        let value = self.coerce_vec_elem_for_store(value, elem_type, elem_llvm_ty)?;
        unsafe { LLVMBuildStore(self.builder, value, elem_ptr) };
        let new_len = unsafe {
            LLVMBuildAdd(
                self.builder,
                current_len,
                LLVMConstInt(usize_ty, 1, 0),
                CString::new("vec.len.inc")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, new_len, len_ptr) };
        Ok(())
    }

    /// Width-coerce a value about to be stored into a Vec element slot.
    ///
    /// `codegen_value`/`codegen_value_owned` materialize untyped integer
    /// literals as i32 and don't know the destination element width, so a
    /// `Vec<u8>.push(0)` would otherwise store a 4-byte i32 through a
    /// 1-byte-element pointer, overrunning the buffer by 3 bytes per element
    /// (GLYPH-72). LLVM's opaque pointers carry no pointee-type information,
    /// so a mismatched store like that is valid IR that silently corrupts
    /// adjacent heap memory instead of failing to compile or verify.
    ///
    /// Mirrors the coercion already applied to indexed-assignment stores in
    /// `functions.rs` (`xs[i] = v`, GLYPH-64).
    pub(super) fn coerce_vec_elem_for_store(
        &mut self,
        value: LLVMValueRef,
        elem_type: &Type,
        elem_llvm_ty: LLVMTypeRef,
    ) -> Result<LLVMValueRef> {
        let signed = matches!(elem_type, Type::I8 | Type::I16 | Type::I32 | Type::I64);
        let mut value = self.coerce_int_value(value, elem_llvm_ty, signed);
        unsafe {
            let val_kind = LLVMGetTypeKind(LLVMTypeOf(value));
            let elem_kind = LLVMGetTypeKind(elem_llvm_ty);
            if Self::is_float_type_kind(val_kind)
                && Self::is_float_type_kind(elem_kind)
                && LLVMTypeOf(value) != elem_llvm_ty
            {
                value = if matches!(elem_type, Type::F64) {
                    LLVMBuildFPExt(
                        self.builder,
                        value,
                        elem_llvm_ty,
                        CString::new("vec.push.fpext")?.as_ptr(),
                    )
                } else {
                    LLVMBuildFPTrunc(
                        self.builder,
                        value,
                        elem_llvm_ty,
                        CString::new("vec.push.fptrunc")?.as_ptr(),
                    )
                };
            }
        }
        if unsafe { LLVMTypeOf(value) } != elem_llvm_ty {
            bail!(
                "internal compiler error: Vec<{:?}> push value type does not match \
                 the element type after coercion; this indicates a codegen bug, not \
                 an error in the Glyph program",
                elem_type
            );
        }
        Ok(value)
    }

    pub(super) fn codegen_vec_struct_init(
        &mut self,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        elem_type: &Type,
        rvalue: &Rvalue,
    ) -> Result<LLVMValueRef> {
        let inst_name = format!("Vec${}", self.type_key(elem_type));
        let (llvm_vec_ty, usize_ty) = self
            .get_vec_layout(&inst_name)
            .ok_or_else(|| anyhow!("missing vec layout for {}", inst_name))?;

        let alloca_name = CString::new("vec.tmp")?;
        let alloca = unsafe { self.build_entry_alloca(self.builder, llvm_vec_ty, alloca_name.as_ptr()) };

        let elem_ptr_ty = self.get_llvm_type(&Type::RawPtr(Box::new(elem_type.clone())))?;

        // Field 1: len
        let len_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                alloca,
                1,
                CString::new("vec.len").unwrap().as_ptr(),
            )
        };
        let zero = unsafe { LLVMConstInt(usize_ty, 0, 0) };
        unsafe { LLVMBuildStore(self.builder, zero, len_ptr) };

        // Field 2: cap
        let cap_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                alloca,
                2,
                CString::new("vec.cap").unwrap().as_ptr(),
            )
        };
        let cap_val = match rvalue {
            Rvalue::VecWithCapacity { capacity, .. } => {
                self.codegen_value(capacity, func, local_map)?
            }
            _ => zero,
        };
        let cap_val = self.ensure_usize(cap_val, usize_ty)?;
        unsafe { LLVMBuildStore(self.builder, cap_val, cap_ptr) };

        // Field 0: data ptr (RawPtr<T>)
        let data_field_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                alloca,
                0,
                CString::new("vec.data").unwrap().as_ptr(),
            )
        };

        let elem_llvm_ty = self.get_llvm_type(elem_type)?;
        let elem_size = unsafe { LLVMSizeOf(elem_llvm_ty) };
        let cap_is_zero = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                cap_val,
                zero,
                CString::new("vec.cap.zero")?.as_ptr(),
            )
        };

        let parent_bb = unsafe { LLVMGetInsertBlock(self.builder) };
        let parent_fn = unsafe { LLVMGetBasicBlockParent(parent_bb) };
        let alloc_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("vec.cap.alloc")?.as_ptr(),
            )
        };
        let zero_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("vec.cap.zero")?.as_ptr(),
            )
        };
        let done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("vec.cap.done")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, cap_is_zero, zero_bb, alloc_bb) };

        // alloc path (cap > 0)
        unsafe { LLVMPositionBuilderAtEnd(self.builder, alloc_bb) };
        let alloc_size = unsafe {
            LLVMBuildMul(
                self.builder,
                cap_val,
                elem_size,
                CString::new("vec.cap.alloc.size")?.as_ptr(),
            )
        };
        let malloc_fn = self.ensure_malloc_fn()?;
        let malloc_ty = self.malloc_function_type();
        let mut malloc_args = vec![alloc_size];
        let raw_ptr = unsafe {
            LLVMBuildCall2(
                self.builder,
                malloc_ty,
                malloc_fn,
                malloc_args.as_mut_ptr(),
                malloc_args.len() as u32,
                CString::new("vec.cap.malloc")?.as_ptr(),
            )
        };
        let data_alloc = unsafe {
            LLVMBuildBitCast(
                self.builder,
                raw_ptr,
                elem_ptr_ty,
                CString::new("vec.cap.cast")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, data_alloc, data_field_ptr) };
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        // zero-cap path
        unsafe { LLVMPositionBuilderAtEnd(self.builder, zero_bb) };
        let null_data = unsafe { LLVMConstNull(elem_ptr_ty) };
        unsafe { LLVMBuildStore(self.builder, null_data, data_field_ptr) };
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        // done
        unsafe { LLVMPositionBuilderAtEnd(self.builder, done_bb) };

        let load_name = CString::new("vec.value")?;
        Ok(unsafe { LLVMBuildLoad2(self.builder, llvm_vec_ty, alloca, load_name.as_ptr()) })
    }

    pub(super) fn codegen_vec_len(
        &mut self,
        vec: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let (struct_name, vec_ptr) = self.struct_pointer_for_local(vec, func, local_map)?;
        let (llvm_vec_ty, _) = self
            .get_vec_layout(&struct_name)
            .ok_or_else(|| anyhow!("missing vec layout for {}", struct_name))?;
        let len_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                vec_ptr,
                1,
                CString::new("vec.len").unwrap().as_ptr(),
            )
        };
        let usize_ty = unsafe { LLVMInt64TypeInContext(self.context) };
        Ok(unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                len_ptr,
                CString::new("vec.len.load")?.as_ptr(),
            )
        })
    }

    pub(super) fn codegen_vec_index(
        &mut self,
        vec: LocalId,
        elem_type: &Type,
        index: &MirValue,
        bounds_check: bool,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let (struct_name, vec_ptr) = self.struct_pointer_for_local(vec, func, local_map)?;
        let (llvm_vec_ty, usize_ty) = self
            .get_vec_layout(&struct_name)
            .ok_or_else(|| anyhow!("missing vec layout for {}", struct_name))?;

        let index_val = self.codegen_value(index, func, local_map)?;

        // Load len
        let len_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                vec_ptr,
                1,
                CString::new("vec.len").unwrap().as_ptr(),
            )
        };
        let len_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                len_ptr,
                CString::new("vec.len.load")?.as_ptr(),
            )
        };

        let idx_usize = self.ensure_usize(index_val, usize_ty)?;

        if bounds_check {
            self.emit_vec_bounds_check(idx_usize, len_val, usize_ty)?;
        }

        // data pointer
        let data_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                vec_ptr,
                0,
                CString::new("vec.data").unwrap().as_ptr(),
            )
        };
        let raw_ptr_ty = self.get_llvm_type(&Type::RawPtr(Box::new(elem_type.clone())))?;
        let data_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                raw_ptr_ty,
                data_ptr,
                CString::new("vec.data.load")?.as_ptr(),
            )
        };

        let elem_llvm_ty = self.get_llvm_type(elem_type)?;
        let elem_ptr = unsafe {
            LLVMBuildGEP2(
                self.builder,
                elem_llvm_ty,
                data_val,
                vec![idx_usize].as_mut_ptr(),
                1,
                CString::new("vec.elem.ptr")?.as_ptr(),
            )
        };

        Ok(unsafe {
            LLVMBuildLoad2(
                self.builder,
                elem_llvm_ty,
                elem_ptr,
                CString::new("vec.elem.load")?.as_ptr(),
            )
        })
    }

    pub(super) fn codegen_vec_index_ref(
        &mut self,
        vec: LocalId,
        elem_type: &Type,
        index: &MirValue,
        bounds_check: bool,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let (struct_name, vec_ptr) = self.struct_pointer_for_local(vec, func, local_map)?;
        let (llvm_vec_ty, usize_ty) = self
            .get_vec_layout(&struct_name)
            .ok_or_else(|| anyhow!("missing vec layout for {}", struct_name))?;

        let index_val = self.codegen_value(index, func, local_map)?;

        let len_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                vec_ptr,
                1,
                CString::new("vec.len").unwrap().as_ptr(),
            )
        };
        let len_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                len_ptr,
                CString::new("vec.len.load")?.as_ptr(),
            )
        };

        let idx_usize = self.ensure_usize(index_val, usize_ty)?;

        if bounds_check {
            self.emit_vec_bounds_check(idx_usize, len_val, usize_ty)?;
        }

        let data_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                vec_ptr,
                0,
                CString::new("vec.data").unwrap().as_ptr(),
            )
        };
        let raw_ptr_ty = self.get_llvm_type(&Type::RawPtr(Box::new(elem_type.clone())))?;
        let data_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                raw_ptr_ty,
                data_ptr,
                CString::new("vec.data.load")?.as_ptr(),
            )
        };

        let elem_llvm_ty = self.get_llvm_type(elem_type)?;
        let elem_ptr = unsafe {
            LLVMBuildGEP2(
                self.builder,
                elem_llvm_ty,
                data_val,
                vec![idx_usize].as_mut_ptr(),
                1,
                CString::new("vec.elem.ptr")?.as_ptr(),
            )
        };

        Ok(elem_ptr)
    }

    pub(super) fn codegen_vec_push(
        &mut self,
        vec: LocalId,
        elem_type: &Type,
        value: &MirValue,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let (struct_name, vec_ptr) = self.struct_pointer_for_local(vec, func, local_map)?;
        let (llvm_vec_ty, usize_ty) = self
            .get_vec_layout(&struct_name)
            .ok_or_else(|| anyhow!("missing vec layout for {}", struct_name))?;

        // Field pointers
        let data_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                vec_ptr,
                0,
                CString::new("vec.data").unwrap().as_ptr(),
            )
        };
        let len_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                vec_ptr,
                1,
                CString::new("vec.len").unwrap().as_ptr(),
            )
        };
        let cap_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                vec_ptr,
                2,
                CString::new("vec.cap").unwrap().as_ptr(),
            )
        };

        let current_len = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                len_ptr,
                CString::new("vec.len.load")?.as_ptr(),
            )
        };
        let current_cap = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                cap_ptr,
                CString::new("vec.cap.load")?.as_ptr(),
            )
        };

        // Branch if growth needed
        let cmp_name = CString::new("vec.need_grow")?;
        let need_grow = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                current_len,
                current_cap,
                cmp_name.as_ptr(),
            )
        };

        let parent_bb = unsafe { LLVMGetInsertBlock(self.builder) };
        if parent_bb.is_null() {
            bail!("builder not positioned in block for vec push");
        }
        let parent_fn = unsafe { LLVMGetBasicBlockParent(parent_bb) };
        if parent_fn.is_null() {
            bail!("vec push parent function missing");
        }

        let grow_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("vec.grow")?.as_ptr(),
            )
        };
        let cont_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("vec.push.cont")?.as_ptr(),
            )
        };

        unsafe { LLVMBuildCondBr(self.builder, need_grow, grow_bb, cont_bb) };

        // ---- Grow block ----
        unsafe { LLVMPositionBuilderAtEnd(self.builder, grow_bb) };
        let one = unsafe { LLVMConstInt(usize_ty, 1, 0) };
        let two = unsafe { LLVMConstInt(usize_ty, 2, 0) };
        let zero = unsafe { LLVMConstInt(usize_ty, 0, 0) };
        let is_zero_cap = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                current_cap,
                zero,
                CString::new("vec.cap.zero")?.as_ptr(),
            )
        };
        let doubled = unsafe {
            LLVMBuildMul(
                self.builder,
                current_cap,
                two,
                CString::new("vec.cap.double")?.as_ptr(),
            )
        };
        let new_cap = unsafe {
            LLVMBuildSelect(
                self.builder,
                is_zero_cap,
                one,
                doubled,
                CString::new("vec.cap.new")?.as_ptr(),
            )
        };

        let elem_llvm_ty = self.get_llvm_type(elem_type)?;
        let elem_size = unsafe { LLVMSizeOf(elem_llvm_ty) };
        let alloc_size = unsafe {
            LLVMBuildMul(
                self.builder,
                new_cap,
                elem_size,
                CString::new("vec.alloc.size")?.as_ptr(),
            )
        };
        let malloc_fn = self.ensure_malloc_fn()?;
        let malloc_ty = self.malloc_function_type();
        let mut malloc_args = vec![alloc_size];
        let raw_ptr = unsafe {
            LLVMBuildCall2(
                self.builder,
                malloc_ty,
                malloc_fn,
                malloc_args.as_mut_ptr(),
                malloc_args.len() as u32,
                CString::new("vec.grow.malloc")?.as_ptr(),
            )
        };
        let new_data_ptr = unsafe {
            LLVMBuildBitCast(
                self.builder,
                raw_ptr,
                self.get_llvm_type(&Type::RawPtr(Box::new(elem_type.clone())))?,
                CString::new("vec.grow.cast")?.as_ptr(),
            )
        };

        // Copy old elements if any
        let cap_nonzero = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntUGT,
                current_cap,
                zero,
                CString::new("vec.cap.nonzero")?.as_ptr(),
            )
        };
        let copy_loop_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("vec.copy.loop")?.as_ptr(),
            )
        };
        let copy_done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("vec.copy.done")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, cap_nonzero, copy_loop_bb, copy_done_bb) };

        // loop: i in [0, current_len)
        unsafe { LLVMPositionBuilderAtEnd(self.builder, copy_loop_bb) };
        let idx_slot = unsafe {
            self.build_entry_alloca(self.builder, usize_ty, CString::new("vec.copy.i")?.as_ptr())
        };
        unsafe { LLVMBuildStore(self.builder, zero, idx_slot) };
        let loop_check_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("vec.copy.check")?.as_ptr(),
            )
        };
        let loop_body_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("vec.copy.body")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildBr(self.builder, loop_check_bb) };

        // check
        unsafe { LLVMPositionBuilderAtEnd(self.builder, loop_check_bb) };
        let idx_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                idx_slot,
                CString::new("vec.copy.i.load")?.as_ptr(),
            )
        };
        let cond = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntULT,
                idx_val,
                current_len,
                CString::new("vec.copy.cond")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, cond, loop_body_bb, copy_done_bb) };

        // body
        unsafe { LLVMPositionBuilderAtEnd(self.builder, loop_body_bb) };
        let old_data_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                self.get_llvm_type(&Type::RawPtr(Box::new(elem_type.clone())))?,
                data_ptr,
                CString::new("vec.data.old")?.as_ptr(),
            )
        };
        let elem_ty_loop = self.get_llvm_type(elem_type)?;
        let src_ptr = unsafe {
            LLVMBuildGEP2(
                self.builder,
                elem_ty_loop,
                old_data_val,
                vec![idx_val].as_mut_ptr(),
                1,
                CString::new("vec.copy.src")?.as_ptr(),
            )
        };
        let dst_ptr = unsafe {
            LLVMBuildGEP2(
                self.builder,
                elem_ty_loop,
                new_data_ptr,
                vec![idx_val].as_mut_ptr(),
                1,
                CString::new("vec.copy.dst")?.as_ptr(),
            )
        };
        let elem_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                elem_ty_loop,
                src_ptr,
                CString::new("vec.copy.load")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, elem_val, dst_ptr) };
        let inc = unsafe {
            LLVMBuildAdd(
                self.builder,
                idx_val,
                one,
                CString::new("vec.copy.inc")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, inc, idx_slot) };
        unsafe { LLVMBuildBr(self.builder, loop_check_bb) };

        // done: free old buffer if cap>0, then install new data/cap
        unsafe { LLVMPositionBuilderAtEnd(self.builder, copy_done_bb) };
        let free_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("vec.free.old")?.as_ptr(),
            )
        };
        let after_free_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("vec.free.cont")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, cap_nonzero, free_bb, after_free_bb) };
        unsafe { LLVMPositionBuilderAtEnd(self.builder, free_bb) };
        let old_data_val_free = unsafe {
            LLVMBuildLoad2(
                self.builder,
                self.get_llvm_type(&Type::RawPtr(Box::new(elem_type.clone())))?,
                data_ptr,
                CString::new("vec.data.old.free")?.as_ptr(),
            )
        };
        self.codegen_free(old_data_val_free)?;
        unsafe { LLVMBuildBr(self.builder, after_free_bb) };
        unsafe { LLVMPositionBuilderAtEnd(self.builder, after_free_bb) };
        unsafe { LLVMBuildStore(self.builder, new_data_ptr, data_ptr) };
        unsafe { LLVMBuildStore(self.builder, new_cap, cap_ptr) };
        unsafe { LLVMBuildBr(self.builder, cont_bb) };

        // ---- No-grow path falls through to cont_bb ----
        unsafe { LLVMPositionBuilderAtEnd(self.builder, cont_bb) };
        let data_after = unsafe {
            LLVMBuildLoad2(
                self.builder,
                self.get_llvm_type(&Type::RawPtr(Box::new(elem_type.clone())))?,
                data_ptr,
                CString::new("vec.data.after")?.as_ptr(),
            )
        };
        let elem_llvm_ty = self.get_llvm_type(elem_type)?;
        let elem_ptr = unsafe {
            LLVMBuildGEP2(
                self.builder,
                elem_llvm_ty,
                data_after,
                vec![current_len].as_mut_ptr(),
                1,
                CString::new("vec.push.dest")?.as_ptr(),
            )
        };
        let value_val = self.codegen_value_owned(value, elem_type, func, local_map)?;
        let value_val = self.coerce_vec_elem_for_store(value_val, elem_type, elem_llvm_ty)?;
        unsafe { LLVMBuildStore(self.builder, value_val, elem_ptr) };
        let new_len = unsafe {
            LLVMBuildAdd(
                self.builder,
                current_len,
                one,
                CString::new("vec.len.next")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, new_len, len_ptr) };

        let load_name = CString::new("vec.push.val")?;
        Ok(unsafe { LLVMBuildLoad2(self.builder, llvm_vec_ty, vec_ptr, load_name.as_ptr()) })
    }

    pub(super) fn codegen_vec_pop(
        &mut self,
        vec: LocalId,
        elem_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        mir_module: &MirModule,
    ) -> Result<LLVMValueRef> {
        let (struct_name, vec_ptr) = self.struct_pointer_for_local(vec, func, local_map)?;
        let (llvm_vec_ty, usize_ty) = self
            .get_vec_layout(&struct_name)
            .ok_or_else(|| anyhow!("missing vec layout for {}", struct_name))?;

        // Field pointers
        let data_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                vec_ptr,
                0,
                CString::new("vec.data").unwrap().as_ptr(),
            )
        };
        let len_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                vec_ptr,
                1,
                CString::new("vec.len").unwrap().as_ptr(),
            )
        };

        let len_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                len_ptr,
                CString::new("vec.len.load")?.as_ptr(),
            )
        };
        let zero = unsafe { LLVMConstInt(usize_ty, 0, 0) };
        let is_empty = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                len_val,
                zero,
                CString::new("vec.empty")?.as_ptr(),
            )
        };

        let parent_bb = unsafe { LLVMGetInsertBlock(self.builder) };
        if parent_bb.is_null() {
            bail!("builder not positioned in block for vec pop");
        }
        let parent_fn = unsafe { LLVMGetBasicBlockParent(parent_bb) };
        if parent_fn.is_null() {
            bail!("vec pop parent function missing");
        }

        let some_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("vec.pop.some")?.as_ptr(),
            )
        };
        let none_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("vec.pop.none")?.as_ptr(),
            )
        };
        let cont_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("vec.pop.cont")?.as_ptr(),
            )
        };

        unsafe { LLVMBuildCondBr(self.builder, is_empty, none_bb, some_bb) };

        // None branch
        unsafe { LLVMPositionBuilderAtEnd(self.builder, none_bb) };
        let none_val = self.codegen_option_none(elem_type, mir_module)?;
        unsafe { LLVMBuildBr(self.builder, cont_bb) };

        // Some branch
        unsafe { LLVMPositionBuilderAtEnd(self.builder, some_bb) };
        let new_len = unsafe {
            LLVMBuildSub(
                self.builder,
                len_val,
                LLVMConstInt(usize_ty, 1, 0),
                CString::new("vec.len.dec")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, new_len, len_ptr) };

        let raw_ptr_ty = self.get_llvm_type(&Type::RawPtr(Box::new(elem_type.clone())))?;
        let data_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                raw_ptr_ty,
                data_ptr,
                CString::new("vec.data.load")?.as_ptr(),
            )
        };
        let elem_llvm_ty = self.get_llvm_type(elem_type)?;
        let elem_ptr = unsafe {
            LLVMBuildGEP2(
                self.builder,
                elem_llvm_ty,
                data_val,
                vec![new_len].as_mut_ptr(),
                1,
                CString::new("vec.pop.ptr")?.as_ptr(),
            )
        };
        let popped_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                elem_llvm_ty,
                elem_ptr,
                CString::new("vec.pop.load")?.as_ptr(),
            )
        };
        let some_val = self.codegen_option_some(elem_type, popped_val, mir_module)?;
        unsafe { LLVMBuildBr(self.builder, cont_bb) };

        // Continuation phi
        unsafe { LLVMPositionBuilderAtEnd(self.builder, cont_bb) };
        let option_name = format!("Option${}", self.type_key(elem_type));
        let llvm_option_ty = self.get_enum_type(&option_name)?;
        let phi = unsafe {
            let phi_name = CString::new("vec.pop.phi")?;
            let phi_node = LLVMBuildPhi(self.builder, llvm_option_ty, phi_name.as_ptr());
            let mut incoming_vals = vec![none_val, some_val];
            let mut incoming_bbs = vec![none_bb, some_bb];
            LLVMAddIncoming(
                phi_node,
                incoming_vals.as_mut_ptr(),
                incoming_bbs.as_mut_ptr(),
                incoming_vals.len() as u32,
            );
            phi_node
        };

        Ok(phi)
    }

    pub(super) fn codegen_drop_vec(
        &mut self,
        vec: LocalId,
        elem_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<()> {
        let (struct_name, vec_ptr) = self.struct_pointer_for_local(vec, func, local_map)?;
        self.codegen_drop_vec_from_ptr(vec_ptr, &struct_name, elem_type)
    }

    pub(super) fn codegen_drop_vec_from_ptr(
        &mut self,
        vec_ptr: LLVMValueRef,
        struct_name: &str,
        elem_type: &Type,
    ) -> Result<()> {
        let (llvm_vec_ty, usize_ty) = self
            .get_vec_layout(struct_name)
            .ok_or_else(|| anyhow!("missing vec layout for {}", struct_name))?;

        // Field pointers
        let data_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                vec_ptr,
                0,
                CString::new("vec.data").unwrap().as_ptr(),
            )
        };
        let len_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                vec_ptr,
                1,
                CString::new("vec.len").unwrap().as_ptr(),
            )
        };
        let cap_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                vec_ptr,
                2,
                CString::new("vec.cap").unwrap().as_ptr(),
            )
        };

        let len_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                len_ptr,
                CString::new("vec.len.load")?.as_ptr(),
            )
        };
        let cap_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                cap_ptr,
                CString::new("vec.cap.load")?.as_ptr(),
            )
        };

        let zero = unsafe { LLVMConstInt(usize_ty, 0, 0) };
        let has_data = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntUGT,
                cap_val,
                zero,
                CString::new("vec.cap.nonzero")?.as_ptr(),
            )
        };

        let parent_bb = unsafe { LLVMGetInsertBlock(self.builder) };
        if parent_bb.is_null() {
            bail!("builder not positioned in block for vec drop");
        }
        let parent_fn = unsafe { LLVMGetBasicBlockParent(parent_bb) };
        if parent_fn.is_null() {
            bail!("vec drop parent function missing");
        }

        let drop_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("vec.drop.body")?.as_ptr(),
            )
        };
        let cont_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("vec.drop.cont")?.as_ptr(),
            )
        };

        unsafe { LLVMBuildCondBr(self.builder, has_data, drop_bb, cont_bb) };

        // Drop body
        unsafe { LLVMPositionBuilderAtEnd(self.builder, drop_bb) };
        let raw_ptr_ty = self.get_llvm_type(&Type::RawPtr(Box::new(elem_type.clone())))?;
        let data_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                raw_ptr_ty,
                data_ptr,
                CString::new("vec.data.load")?.as_ptr(),
            )
        };

        // loop over elements 0..len
        let idx_slot = unsafe {
            self.build_entry_alloca(self.builder, usize_ty, CString::new("vec.drop.i")?.as_ptr())
        };
        unsafe { LLVMBuildStore(self.builder, zero, idx_slot) };
        let loop_check_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("vec.drop.check")?.as_ptr(),
            )
        };
        let loop_body_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("vec.drop.body.loop")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildBr(self.builder, loop_check_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, loop_check_bb) };
        let idx_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                idx_slot,
                CString::new("vec.drop.i.load")?.as_ptr(),
            )
        };
        let cond = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntULT,
                idx_val,
                len_val,
                CString::new("vec.drop.cond")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, cond, loop_body_bb, cont_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, loop_body_bb) };
        let elem_llvm_ty = self.get_llvm_type(elem_type)?;
        let elem_ptr = unsafe {
            LLVMBuildGEP2(
                self.builder,
                elem_llvm_ty,
                data_val,
                vec![idx_val].as_mut_ptr(),
                1,
                CString::new("vec.drop.elem.ptr")?.as_ptr(),
            )
        };
        self.codegen_drop_elem_slot(elem_ptr, elem_type)?;
        let inc = unsafe {
            LLVMBuildAdd(
                self.builder,
                idx_val,
                LLVMConstInt(usize_ty, 1, 0),
                CString::new("vec.drop.inc")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, inc, idx_slot) };
        unsafe { LLVMBuildBr(self.builder, loop_check_bb) };

        // After loop: free buffer
        unsafe { LLVMPositionBuilderAtEnd(self.builder, cont_bb) };
        let data_to_free = unsafe {
            LLVMBuildLoad2(
                self.builder,
                raw_ptr_ty,
                data_ptr,
                CString::new("vec.data.free")?.as_ptr(),
            )
        };
        self.codegen_free(data_to_free)?;
        unsafe { LLVMBuildStore(self.builder, zero, len_ptr) };
        unsafe { LLVMBuildStore(self.builder, zero, cap_ptr) };
        let null_data = unsafe { LLVMConstNull(raw_ptr_ty) };
        unsafe { LLVMBuildStore(self.builder, null_data, data_ptr) };

        Ok(())
    }
}
