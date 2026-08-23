use super::*;

impl CodegenContext {
    pub(super) fn codegen_string_slice(
        &mut self,
        base: LocalId,
        start: &MirValue,
        len: &MirValue,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        functions: &HashMap<String, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let base_val = self.codegen_string_base_value(base, func, local_map)?;
        let start_val = self.codegen_value(start, func, local_map)?;
        let len_val = self.codegen_value(len, func, local_map)?;
        let usize_ty = unsafe { LLVMInt64TypeInContext(self.context) };
        let start_val = self.ensure_usize(start_val, usize_ty)?;
        let len_val = self.ensure_usize(len_val, usize_ty)?;
        let base_len = self.codegen_string_len_value(base_val, functions)?;

        if std::env::var("GLYPH_DEBUG_MAP").is_ok() {
            let printf_fn = self.ensure_printf_fn()?;
            let printf_ty = self.printf_function_type();
            let fmt_ptr = self.codegen_string_literal(
                "[str.slice] base_len=%llu start=%llu len=%llu first=%llu\n",
                ".str.slice.debug",
            )?;
            let i8_ty = unsafe { LLVMInt8TypeInContext(self.context) };
            let first_ptr = unsafe {
                LLVMBuildGEP2(
                    self.builder,
                    i8_ty,
                    base_val,
                    vec![LLVMConstInt(usize_ty, 0, 0)].as_mut_ptr(),
                    1,
                    CString::new("str.slice.first")?.as_ptr(),
                )
            };
            let first_val = unsafe {
                LLVMBuildLoad2(
                    self.builder,
                    i8_ty,
                    first_ptr,
                    CString::new("str.slice.first.load")?.as_ptr(),
                )
            };
            let first_u64 = unsafe {
                LLVMBuildZExt(
                    self.builder,
                    first_val,
                    usize_ty,
                    CString::new("str.slice.first.u64")?.as_ptr(),
                )
            };
            let base_len_u64 = self.ensure_usize(base_len, usize_ty)?;
            let mut args = vec![fmt_ptr, base_len_u64, start_val, len_val, first_u64];
            unsafe {
                LLVMBuildCall2(
                    self.builder,
                    printf_ty,
                    printf_fn,
                    args.as_mut_ptr(),
                    args.len() as u32,
                    CString::new("str.slice.debug")?.as_ptr(),
                );
            }
        }

        let parent_bb = unsafe { LLVMGetInsertBlock(self.builder) };
        let parent_fn = unsafe { LLVMGetBasicBlockParent(parent_bb) };
        let empty_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.slice.empty")?.as_ptr(),
            )
        };
        let body_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.slice.body")?.as_ptr(),
            )
        };
        let done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.slice.done")?.as_ptr(),
            )
        };
        let start_ge = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntUGE,
                start_val,
                base_len,
                CString::new("str.slice.start.ge")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, start_ge, empty_bb, body_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, empty_bb) };
        let empty_val = self.codegen_string_empty(functions)?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, body_bb) };
        let remaining = unsafe {
            LLVMBuildSub(
                self.builder,
                base_len,
                start_val,
                CString::new("str.slice.rem")?.as_ptr(),
            )
        };
        let len_gt = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntUGT,
                len_val,
                remaining,
                CString::new("str.slice.len.gt")?.as_ptr(),
            )
        };
        let actual_len = unsafe {
            LLVMBuildSelect(
                self.builder,
                len_gt,
                remaining,
                len_val,
                CString::new("str.slice.len")?.as_ptr(),
            )
        };
        let i8_ty = unsafe { LLVMInt8TypeInContext(self.context) };
        let base_i8 = unsafe {
            LLVMBuildBitCast(
                self.builder,
                base_val,
                LLVMPointerType(i8_ty, 0),
                CString::new("str.slice.cast")?.as_ptr(),
            )
        };
        let slice_ptr = unsafe {
            LLVMBuildGEP2(
                self.builder,
                i8_ty,
                base_i8,
                vec![start_val].as_mut_ptr(),
                1,
                CString::new("str.slice.ptr")?.as_ptr(),
            )
        };
        let slice_val = self.codegen_string_copy_from_ptr_len(slice_ptr, actual_len, functions)?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, done_bb) };
        let phi = unsafe {
            let phi_name = CString::new("str.slice.result")?;
            let phi_node = LLVMBuildPhi(
                self.builder,
                self.get_llvm_type(&Type::String)?,
                phi_name.as_ptr(),
            );
            let mut vals = vec![empty_val, slice_val];
            let mut bbs = vec![empty_bb, body_bb];
            LLVMAddIncoming(
                phi_node,
                vals.as_mut_ptr(),
                bbs.as_mut_ptr(),
                vals.len() as u32,
            );
            phi_node
        };
        Ok(phi)
    }

    pub(super) fn codegen_string_trim(
        &mut self,
        base: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        functions: &HashMap<String, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let base_val = self.codegen_string_base_value(base, func, local_map)?;
        let len_val = self.codegen_string_len_value(base_val, functions)?;
        let usize_ty = unsafe { LLVMInt64TypeInContext(self.context) };
        let zero = unsafe { LLVMConstInt(usize_ty, 0, 0) };

        let result = unsafe {
            let parent_bb = LLVMGetInsertBlock(self.builder);
            let parent_fn = LLVMGetBasicBlockParent(parent_bb);
            let empty_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.trim.empty")?.as_ptr(),
            );
            let start_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.trim.start")?.as_ptr(),
            );
            let done_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.trim.done")?.as_ptr(),
            );
            let len_zero = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                len_val,
                zero,
                CString::new("str.trim.len.zero")?.as_ptr(),
            );
            LLVMBuildCondBr(self.builder, len_zero, empty_bb, start_bb);

            LLVMPositionBuilderAtEnd(self.builder, empty_bb);
            let empty_val = self.codegen_string_empty(functions)?;
            LLVMBuildBr(self.builder, done_bb);

            LLVMPositionBuilderAtEnd(self.builder, start_bb);
            let start_slot = self.build_entry_alloca(
                self.builder,
                usize_ty,
                CString::new("str.trim.start.idx")?.as_ptr(),
            );
            let end_slot = self.build_entry_alloca(
                self.builder,
                usize_ty,
                CString::new("str.trim.end.idx")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, zero, start_slot);
            LLVMBuildStore(self.builder, len_val, end_slot);

            let i8_ty = LLVMInt8TypeInContext(self.context);
            let base_i8 = LLVMBuildBitCast(
                self.builder,
                base_val,
                LLVMPointerType(i8_ty, 0),
                CString::new("str.trim.cast")?.as_ptr(),
            );
            let isspace_fn = self.get_extern_function(functions, "isspace")?;
            let space_ty = self.function_type_for("isspace")?;

            let loop_start_check = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.trim.start.check")?.as_ptr(),
            );
            let loop_start_body = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.trim.start.body")?.as_ptr(),
            );
            let loop_start_done = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.trim.start.done")?.as_ptr(),
            );
            LLVMBuildBr(self.builder, loop_start_check);

            LLVMPositionBuilderAtEnd(self.builder, loop_start_check);
            let start_val = LLVMBuildLoad2(
                self.builder,
                usize_ty,
                start_slot,
                CString::new("str.trim.start.load")?.as_ptr(),
            );
            let end_val = LLVMBuildLoad2(
                self.builder,
                usize_ty,
                end_slot,
                CString::new("str.trim.end.load")?.as_ptr(),
            );
            let start_lt = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntULT,
                start_val,
                end_val,
                CString::new("str.trim.start.lt")?.as_ptr(),
            );
            LLVMBuildCondBr(self.builder, start_lt, loop_start_body, loop_start_done);

            LLVMPositionBuilderAtEnd(self.builder, loop_start_body);
            let char_ptr = LLVMBuildGEP2(
                self.builder,
                i8_ty,
                base_i8,
                vec![start_val].as_mut_ptr(),
                1,
                CString::new("str.trim.char")?.as_ptr(),
            );
            let ch = LLVMBuildLoad2(
                self.builder,
                i8_ty,
                char_ptr,
                CString::new("str.trim.ch")?.as_ptr(),
            );
            let ch_i32 = LLVMBuildSExt(
                self.builder,
                ch,
                LLVMInt32TypeInContext(self.context),
                CString::new("str.trim.ch.i32")?.as_ptr(),
            );
            let mut space_args = vec![ch_i32];
            let space_res = LLVMBuildCall2(
                self.builder,
                space_ty,
                isspace_fn,
                space_args.as_mut_ptr(),
                space_args.len() as u32,
                CString::new("str.trim.space")?.as_ptr(),
            );
            let is_space = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntNE,
                space_res,
                LLVMConstInt(LLVMInt32TypeInContext(self.context), 0, 0),
                CString::new("str.trim.is_space")?.as_ptr(),
            );
            let inc_start_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.trim.start.inc")?.as_ptr(),
            );
            let keep_start_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.trim.start.keep")?.as_ptr(),
            );
            LLVMBuildCondBr(self.builder, is_space, inc_start_bb, keep_start_bb);

            LLVMPositionBuilderAtEnd(self.builder, inc_start_bb);
            let start_next = LLVMBuildAdd(
                self.builder,
                start_val,
                LLVMConstInt(usize_ty, 1, 0),
                CString::new("str.trim.start.next")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, start_next, start_slot);
            LLVMBuildBr(self.builder, loop_start_check);

            LLVMPositionBuilderAtEnd(self.builder, keep_start_bb);
            LLVMBuildBr(self.builder, loop_start_done);

            LLVMPositionBuilderAtEnd(self.builder, loop_start_done);
            let loop_end_check = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.trim.end.check")?.as_ptr(),
            );
            let loop_end_body = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.trim.end.body")?.as_ptr(),
            );
            let loop_end_done = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.trim.end.done")?.as_ptr(),
            );
            LLVMBuildBr(self.builder, loop_end_check);

            LLVMPositionBuilderAtEnd(self.builder, loop_end_check);
            let start_val2 = LLVMBuildLoad2(
                self.builder,
                usize_ty,
                start_slot,
                CString::new("str.trim.start.load2")?.as_ptr(),
            );
            let end_val2 = LLVMBuildLoad2(
                self.builder,
                usize_ty,
                end_slot,
                CString::new("str.trim.end.load2")?.as_ptr(),
            );
            let end_gt = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntUGT,
                end_val2,
                start_val2,
                CString::new("str.trim.end.gt")?.as_ptr(),
            );
            LLVMBuildCondBr(self.builder, end_gt, loop_end_body, loop_end_done);

            LLVMPositionBuilderAtEnd(self.builder, loop_end_body);
            let end_minus = LLVMBuildSub(
                self.builder,
                end_val2,
                LLVMConstInt(usize_ty, 1, 0),
                CString::new("str.trim.end.minus")?.as_ptr(),
            );
            let end_char_ptr = LLVMBuildGEP2(
                self.builder,
                i8_ty,
                base_i8,
                vec![end_minus].as_mut_ptr(),
                1,
                CString::new("str.trim.end.char")?.as_ptr(),
            );
            let end_ch = LLVMBuildLoad2(
                self.builder,
                i8_ty,
                end_char_ptr,
                CString::new("str.trim.end.ch")?.as_ptr(),
            );
            let end_ch_i32 = LLVMBuildSExt(
                self.builder,
                end_ch,
                LLVMInt32TypeInContext(self.context),
                CString::new("str.trim.end.ch.i32")?.as_ptr(),
            );
            let mut end_space_args = vec![end_ch_i32];
            let end_space_res = LLVMBuildCall2(
                self.builder,
                space_ty,
                isspace_fn,
                end_space_args.as_mut_ptr(),
                end_space_args.len() as u32,
                CString::new("str.trim.end.space")?.as_ptr(),
            );
            let end_is_space = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntNE,
                end_space_res,
                LLVMConstInt(LLVMInt32TypeInContext(self.context), 0, 0),
                CString::new("str.trim.end.is_space")?.as_ptr(),
            );
            let dec_end_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.trim.end.dec")?.as_ptr(),
            );
            let keep_end_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.trim.end.keep")?.as_ptr(),
            );
            LLVMBuildCondBr(self.builder, end_is_space, dec_end_bb, keep_end_bb);

            LLVMPositionBuilderAtEnd(self.builder, dec_end_bb);
            LLVMBuildStore(self.builder, end_minus, end_slot);
            LLVMBuildBr(self.builder, loop_end_check);

            LLVMPositionBuilderAtEnd(self.builder, keep_end_bb);
            LLVMBuildBr(self.builder, loop_end_done);

            LLVMPositionBuilderAtEnd(self.builder, loop_end_done);
            let final_start = LLVMBuildLoad2(
                self.builder,
                usize_ty,
                start_slot,
                CString::new("str.trim.start.final")?.as_ptr(),
            );
            let final_end = LLVMBuildLoad2(
                self.builder,
                usize_ty,
                end_slot,
                CString::new("str.trim.end.final")?.as_ptr(),
            );
            let final_len = LLVMBuildSub(
                self.builder,
                final_end,
                final_start,
                CString::new("str.trim.len")?.as_ptr(),
            );
            let trim_ptr = LLVMBuildGEP2(
                self.builder,
                i8_ty,
                base_i8,
                vec![final_start].as_mut_ptr(),
                1,
                CString::new("str.trim.ptr")?.as_ptr(),
            );
            let trim_val = self.codegen_string_copy_from_ptr_len(trim_ptr, final_len, functions)?;
            LLVMBuildBr(self.builder, done_bb);

            LLVMPositionBuilderAtEnd(self.builder, done_bb);
            let phi = LLVMBuildPhi(
                self.builder,
                self.get_llvm_type(&Type::String)?,
                CString::new("str.trim.result")?.as_ptr(),
            );
            let mut vals = vec![empty_val, trim_val];
            let mut bbs = vec![empty_bb, loop_end_done];
            LLVMAddIncoming(phi, vals.as_mut_ptr(), bbs.as_mut_ptr(), vals.len() as u32);
            Ok(phi)
        };
        result
    }

    pub(super) fn codegen_string_split(
        &mut self,
        base: LocalId,
        sep: &MirValue,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        functions: &HashMap<String, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let base_val = self.codegen_string_base_value(base, func, local_map)?;
        let sep_val = self.codegen_value(sep, func, local_map)?;
        let base_len = self.codegen_string_len_value(base_val, functions)?;
        let sep_len = self.codegen_string_len_value(sep_val, functions)?;

        let cap = unsafe {
            LLVMBuildAdd(
                self.builder,
                base_len,
                LLVMConstInt(LLVMTypeOf(base_len), 1, 0),
                CString::new("str.split.cap")?.as_ptr(),
            )
        };
        let vec_val = self.codegen_vec_init_with_capacity_value(&Type::String, cap)?;
        let vec_name = format!("Vec${}", self.type_key(&Type::String));
        let llvm_vec_ty = self.get_struct_type(&vec_name)?;
        let vec_ptr = unsafe {
            let alloca = self.build_entry_alloca(
                self.builder,
                llvm_vec_ty,
                CString::new("str.split.vec")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, vec_val, alloca);
            alloca
        };

        let parent_bb = unsafe { LLVMGetInsertBlock(self.builder) };
        let parent_fn = unsafe { LLVMGetBasicBlockParent(parent_bb) };
        let sep_zero = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                sep_len,
                LLVMConstInt(LLVMTypeOf(sep_len), 0, 0),
                CString::new("str.split.sep.zero")?.as_ptr(),
            )
        };
        let sep_zero_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.split.sep.zero.bb")?.as_ptr(),
            )
        };
        let loop_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.split.loop")?.as_ptr(),
            )
        };
        let done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.split.done")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, sep_zero, sep_zero_bb, loop_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, sep_zero_bb) };
        let dup_val = self.codegen_string_copy_from_ptr_len(base_val, base_len, functions)?;
        self.codegen_vec_push_no_grow(vec_ptr, &Type::String, dup_val)?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, loop_bb) };
        let current_slot = unsafe {
            self.build_entry_alloca(
                self.builder,
                self.get_llvm_type(&Type::String)?,
                CString::new("str.split.curr")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, base_val, current_slot) };

        let loop_check = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.split.check")?.as_ptr(),
            )
        };
        let loop_body = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.split.body")?.as_ptr(),
            )
        };
        let loop_end = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.split.end")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildBr(self.builder, loop_check) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, loop_check) };
        let current_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                self.get_llvm_type(&Type::String)?,
                current_slot,
                CString::new("str.split.curr.load")?.as_ptr(),
            )
        };
        let strstr_fn = self.get_extern_function(functions, "strstr")?;
        let mut strstr_args = vec![current_val, sep_val];
        let strstr_ty = self.function_type_for("strstr")?;
        let found = unsafe {
            LLVMBuildCall2(
                self.builder,
                strstr_ty,
                strstr_fn,
                strstr_args.as_mut_ptr(),
                strstr_args.len() as u32,
                CString::new("str.split.find")?.as_ptr(),
            )
        };
        let null_ptr = unsafe { LLVMConstNull(LLVMTypeOf(found)) };
        let found_null = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                found,
                null_ptr,
                CString::new("str.split.found.null")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, found_null, loop_end, loop_body) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, loop_body) };
        let current_int = unsafe {
            LLVMBuildPtrToInt(
                self.builder,
                current_val,
                LLVMInt64TypeInContext(self.context),
                CString::new("str.split.curr.int")?.as_ptr(),
            )
        };
        let found_int = unsafe {
            LLVMBuildPtrToInt(
                self.builder,
                found,
                LLVMInt64TypeInContext(self.context),
                CString::new("str.split.found.int")?.as_ptr(),
            )
        };
        let seg_len = unsafe {
            LLVMBuildSub(
                self.builder,
                found_int,
                current_int,
                CString::new("str.split.seg.len")?.as_ptr(),
            )
        };
        let seg_val = self.codegen_string_copy_from_ptr_len(current_val, seg_len, functions)?;
        self.codegen_vec_push_no_grow(vec_ptr, &Type::String, seg_val)?;
        let next_ptr = unsafe {
            LLVMBuildGEP2(
                self.builder,
                LLVMInt8TypeInContext(self.context),
                found,
                vec![sep_len].as_mut_ptr(),
                1,
                CString::new("str.split.next")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, next_ptr, current_slot) };
        unsafe { LLVMBuildBr(self.builder, loop_check) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, loop_end) };
        let tail_len = self.codegen_string_len_value(current_val, functions)?;
        let tail_val = self.codegen_string_copy_from_ptr_len(current_val, tail_len, functions)?;
        self.codegen_vec_push_no_grow(vec_ptr, &Type::String, tail_val)?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, done_bb) };
        let vec_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                llvm_vec_ty,
                vec_ptr,
                CString::new("str.split.load")?.as_ptr(),
            )
        };
        Ok(vec_val)
    }

    pub(super) fn codegen_hash_bytes(
        &mut self,
        ptr: LLVMValueRef,
        len: LLVMValueRef,
    ) -> Result<LLVMValueRef> {
        let u64_ty = unsafe { LLVMInt64TypeInContext(self.context) };
        let usize_ty = unsafe { LLVMInt64TypeInContext(self.context) };
        let i8_ty = unsafe { LLVMInt8TypeInContext(self.context) };
        let i8_ptr_ty = unsafe { LLVMPointerType(i8_ty, 0) };

        let len_val = self.ensure_usize(len, usize_ty)?;
        let ptr_val = unsafe {
            if LLVMTypeOf(ptr) == i8_ptr_ty {
                ptr
            } else {
                LLVMBuildBitCast(
                    self.builder,
                    ptr,
                    i8_ptr_ty,
                    CString::new("hash.ptr.cast")?.as_ptr(),
                )
            }
        };

        let offset = unsafe { LLVMConstInt(u64_ty, 14695981039346656037, 0) };
        let prime = unsafe { LLVMConstInt(u64_ty, 1099511628211, 0) };
        let zero = unsafe { LLVMConstInt(usize_ty, 0, 0) };

        let hash_slot =
            unsafe { self.build_entry_alloca(self.builder, u64_ty, CString::new("hash.state")?.as_ptr()) };
        let idx_slot =
            unsafe { self.build_entry_alloca(self.builder, usize_ty, CString::new("hash.idx")?.as_ptr()) };
        unsafe {
            LLVMBuildStore(self.builder, offset, hash_slot);
            LLVMBuildStore(self.builder, zero, idx_slot);
        }

        let parent_bb = unsafe { LLVMGetInsertBlock(self.builder) };
        let parent_fn = unsafe { LLVMGetBasicBlockParent(parent_bb) };
        let check_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("hash.check")?.as_ptr(),
            )
        };
        let body_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("hash.body")?.as_ptr(),
            )
        };
        let done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("hash.done")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildBr(self.builder, check_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, check_bb) };
        let idx_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                idx_slot,
                CString::new("hash.idx.load")?.as_ptr(),
            )
        };
        let in_bounds = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntULT,
                idx_val,
                len_val,
                CString::new("hash.idx.lt")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, in_bounds, body_bb, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, body_bb) };
        let byte_ptr = unsafe {
            LLVMBuildGEP2(
                self.builder,
                i8_ty,
                ptr_val,
                vec![idx_val].as_mut_ptr(),
                1,
                CString::new("hash.byte.ptr")?.as_ptr(),
            )
        };
        let byte_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                i8_ty,
                byte_ptr,
                CString::new("hash.byte")?.as_ptr(),
            )
        };
        let byte_u64 = unsafe {
            LLVMBuildZExt(
                self.builder,
                byte_val,
                u64_ty,
                CString::new("hash.byte.u64")?.as_ptr(),
            )
        };
        let hash_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                u64_ty,
                hash_slot,
                CString::new("hash.load")?.as_ptr(),
            )
        };
        let xor_val = unsafe {
            LLVMBuildXor(
                self.builder,
                hash_val,
                byte_u64,
                CString::new("hash.xor")?.as_ptr(),
            )
        };
        let mul_val = unsafe {
            LLVMBuildMul(
                self.builder,
                xor_val,
                prime,
                CString::new("hash.mul")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, mul_val, hash_slot) };
        let next_idx = unsafe {
            LLVMBuildAdd(
                self.builder,
                idx_val,
                LLVMConstInt(usize_ty, 1, 0),
                CString::new("hash.idx.next")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, next_idx, idx_slot) };
        unsafe { LLVMBuildBr(self.builder, check_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, done_bb) };
        Ok(unsafe {
            LLVMBuildLoad2(
                self.builder,
                u64_ty,
                hash_slot,
                CString::new("hash.result")?.as_ptr(),
            )
        })
    }

    pub(super) fn codegen_string_starts_with(
        &mut self,
        base: LocalId,
        needle: &MirValue,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        functions: &HashMap<String, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let base_val = self.codegen_string_base_value(base, func, local_map)?;
        let needle_val = self.codegen_value(needle, func, local_map)?;
        let base_len = self.codegen_string_len_value(base_val, functions)?;
        let needle_len = self.codegen_string_len_value(needle_val, functions)?;
        let result = unsafe {
            let too_long = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntUGT,
                needle_len,
                base_len,
                CString::new("str.starts.gt")?.as_ptr(),
            );
            let parent_bb = LLVMGetInsertBlock(self.builder);
            let parent_fn = LLVMGetBasicBlockParent(parent_bb);
            let fail_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.starts.fail")?.as_ptr(),
            );
            let body_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.starts.body")?.as_ptr(),
            );
            let done_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.starts.done")?.as_ptr(),
            );
            LLVMBuildCondBr(self.builder, too_long, fail_bb, body_bb);

            LLVMPositionBuilderAtEnd(self.builder, fail_bb);
            let false_val = LLVMConstInt(LLVMInt1TypeInContext(self.context), 0, 0);
            LLVMBuildBr(self.builder, done_bb);

            LLVMPositionBuilderAtEnd(self.builder, body_bb);
            let cmp_val =
                self.codegen_string_compare(base_val, needle_val, needle_len, functions)?;
            let cmp_zero = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                cmp_val,
                LLVMConstInt(LLVMInt32TypeInContext(self.context), 0, 0),
                CString::new("str.starts.eq")?.as_ptr(),
            );
            LLVMBuildBr(self.builder, done_bb);

            LLVMPositionBuilderAtEnd(self.builder, done_bb);
            let phi = {
                let phi_name = CString::new("str.starts.result")?;
                let phi_node = LLVMBuildPhi(
                    self.builder,
                    LLVMInt1TypeInContext(self.context),
                    phi_name.as_ptr(),
                );
                let mut vals = vec![false_val, cmp_zero];
                let mut bbs = vec![fail_bb, body_bb];
                LLVMAddIncoming(
                    phi_node,
                    vals.as_mut_ptr(),
                    bbs.as_mut_ptr(),
                    vals.len() as u32,
                );
                phi_node
            };
            Ok(phi)
        };
        result
    }

    pub(super) fn codegen_string_ends_with(
        &mut self,
        base: LocalId,
        needle: &MirValue,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        functions: &HashMap<String, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let base_val = self.codegen_string_base_value(base, func, local_map)?;
        let needle_val = self.codegen_value(needle, func, local_map)?;
        let base_len = self.codegen_string_len_value(base_val, functions)?;
        let needle_len = self.codegen_string_len_value(needle_val, functions)?;
        let result = unsafe {
            let too_long = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntUGT,
                needle_len,
                base_len,
                CString::new("str.ends.gt")?.as_ptr(),
            );
            let parent_bb = LLVMGetInsertBlock(self.builder);
            let parent_fn = LLVMGetBasicBlockParent(parent_bb);
            let fail_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.ends.fail")?.as_ptr(),
            );
            let body_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.ends.body")?.as_ptr(),
            );
            let done_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("str.ends.done")?.as_ptr(),
            );
            LLVMBuildCondBr(self.builder, too_long, fail_bb, body_bb);

            LLVMPositionBuilderAtEnd(self.builder, fail_bb);
            let false_val = LLVMConstInt(LLVMInt1TypeInContext(self.context), 0, 0);
            LLVMBuildBr(self.builder, done_bb);

            LLVMPositionBuilderAtEnd(self.builder, body_bb);
            let i8_ty = LLVMInt8TypeInContext(self.context);
            let base_i8 = LLVMBuildBitCast(
                self.builder,
                base_val,
                LLVMPointerType(i8_ty, 0),
                CString::new("str.ends.cast")?.as_ptr(),
            );
            let offset = LLVMBuildSub(
                self.builder,
                base_len,
                needle_len,
                CString::new("str.ends.offset")?.as_ptr(),
            );
            let tail_ptr = LLVMBuildGEP2(
                self.builder,
                i8_ty,
                base_i8,
                vec![offset].as_mut_ptr(),
                1,
                CString::new("str.ends.ptr")?.as_ptr(),
            );
            let cmp_val =
                self.codegen_string_compare(tail_ptr, needle_val, needle_len, functions)?;
            let cmp_zero = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                cmp_val,
                LLVMConstInt(LLVMInt32TypeInContext(self.context), 0, 0),
                CString::new("str.ends.eq")?.as_ptr(),
            );
            LLVMBuildBr(self.builder, done_bb);

            LLVMPositionBuilderAtEnd(self.builder, done_bb);
            let phi = LLVMBuildPhi(
                self.builder,
                LLVMInt1TypeInContext(self.context),
                CString::new("str.ends.result")?.as_ptr(),
            );
            let mut vals = vec![false_val, cmp_zero];
            let mut bbs = vec![fail_bb, body_bb];
            LLVMAddIncoming(phi, vals.as_mut_ptr(), bbs.as_mut_ptr(), vals.len() as u32);
            Ok(phi)
        };
        result
    }
}
