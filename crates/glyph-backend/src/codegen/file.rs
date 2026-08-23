use super::*;

impl CodegenContext {
    pub(super) fn file_handle_ptr(
        &mut self,
        file: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<(LLVMTypeRef, LLVMValueRef)> {
        let (struct_name, file_ptr) = self.struct_pointer_for_local(file, func, local_map)?;
        let llvm_file_ty = self.get_struct_type(&struct_name)?;
        let layout = self
            .struct_layouts
            .get(&struct_name)
            .ok_or_else(|| anyhow!("missing layout for struct {}", struct_name))?;
        let handle_field = layout
            .fields
            .first()
            .ok_or_else(|| anyhow!("missing File handle field"))?;
        let handle_ty = self.get_llvm_type(&handle_field.1)?;
        let handle_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_file_ty,
                file_ptr,
                0,
                CString::new("file.handle")?.as_ptr(),
            )
        };
        Ok((handle_ty, handle_ptr))
    }

    pub(super) fn codegen_file_open(
        &mut self,
        path: &MirValue,
        create: bool,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        functions: &HashMap<String, LLVMValueRef>,
        _mir_module: &MirModule,
    ) -> Result<LLVMValueRef> {
        let path_val = self.codegen_value(path, func, local_map)?;
        let mode_str = if create { "w" } else { "r" };
        let mode_val = self.codegen_string_literal(mode_str, &format!(".str.file.{}", mode_str))?;
        let fopen_fn = self.get_extern_function(functions, "fopen")?;
        let mut args = vec![path_val, mode_val];
        let fn_ty = self.function_type_for("fopen")?;
        let call = unsafe {
            LLVMBuildCall2(
                self.builder,
                fn_ty,
                fopen_fn,
                args.as_mut_ptr(),
                args.len() as u32,
                CString::new("file.open")?.as_ptr(),
            )
        };
        let null_ptr = unsafe { LLVMConstNull(LLVMTypeOf(call)) };
        let is_null = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                call,
                null_ptr,
                CString::new("file.open.null")?.as_ptr(),
            )
        };

        let parent_bb = unsafe { LLVMGetInsertBlock(self.builder) };
        let parent_fn = unsafe { LLVMGetBasicBlockParent(parent_bb) };
        let ok_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.open.ok")?.as_ptr(),
            )
        };
        let err_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.open.err")?.as_ptr(),
            )
        };
        let done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.open.done")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, is_null, err_bb, ok_bb) };

        let ok_type = Type::Named("File".into());
        let err_type = Type::Named("Err".into());
        let result_name = format!(
            "Result${}__{}",
            self.type_key(&ok_type),
            self.type_key(&err_type)
        );
        let result_ty = self.get_enum_type(&result_name)?;

        unsafe { LLVMPositionBuilderAtEnd(self.builder, ok_bb) };
        let file_ty = self.get_struct_type("File")?;
        let file_alloca =
            unsafe { self.build_entry_alloca(self.builder, file_ty, CString::new("file.tmp")?.as_ptr()) };
        let handle_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                file_ty,
                file_alloca,
                0,
                CString::new("file.handle")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, call, handle_ptr) };
        let file_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                file_ty,
                file_alloca,
                CString::new("file.val")?.as_ptr(),
            )
        };
        let ok_val = self.codegen_result_ok(&ok_type, &err_type, Some(file_val))?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, err_bb) };
        let err_msg = if create {
            "failed to create file"
        } else {
            "failed to open file"
        };
        let err_payload = self.codegen_err_value(err_msg)?;
        let err_val = self.codegen_result_err(&ok_type, &err_type, Some(err_payload))?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, done_bb) };
        let phi = unsafe {
            let phi_name = CString::new("file.open.result")?;
            let phi_node = LLVMBuildPhi(self.builder, result_ty, phi_name.as_ptr());
            let mut vals = vec![err_val, ok_val];
            let mut bbs = vec![err_bb, ok_bb];
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

    pub(super) fn codegen_file_read_to_string(
        &mut self,
        file: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        functions: &HashMap<String, LLVMValueRef>,
        _mir_module: &MirModule,
    ) -> Result<LLVMValueRef> {
        let (handle_ty, handle_ptr) = self.file_handle_ptr(file, func, local_map)?;
        let handle_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                handle_ty,
                handle_ptr,
                CString::new("file.handle.load")?.as_ptr(),
            )
        };
        let null_ptr = unsafe { LLVMConstNull(handle_ty) };
        let is_null = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                handle_val,
                null_ptr,
                CString::new("file.null")?.as_ptr(),
            )
        };

        let ok_type = Type::String;
        let err_type = Type::Named("Err".into());
        let result_name = format!(
            "Result${}__{}",
            self.type_key(&ok_type),
            self.type_key(&err_type)
        );
        let result_ty = self.get_enum_type(&result_name)?;

        let parent_bb = unsafe { LLVMGetInsertBlock(self.builder) };
        let parent_fn = unsafe { LLVMGetBasicBlockParent(parent_bb) };
        let err_null_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.read.err.null")?.as_ptr(),
            )
        };
        let seek_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.read.seek")?.as_ptr(),
            )
        };
        let done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.read.done")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, is_null, err_null_bb, seek_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, err_null_bb) };
        let err_payload = self.codegen_err_value("file is closed")?;
        let err_val = self.codegen_result_err(&ok_type, &err_type, Some(err_payload))?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, seek_bb) };
        let fseek_fn = self.get_extern_function(functions, "fseek")?;
        let ftell_fn = self.get_extern_function(functions, "ftell")?;
        let rewind_fn = self.get_extern_function(functions, "rewind")?;
        let fread_fn = self.get_extern_function(functions, "fread")?;
        let i32_ty = unsafe { LLVMInt32TypeInContext(self.context) };
        let i64_ty = unsafe { LLVMInt64TypeInContext(self.context) };
        let zero_i64 = unsafe { LLVMConstInt(i64_ty, 0, 0) };
        let seek_end = unsafe { LLVMConstInt(i32_ty, 2, 0) };
        let mut seek_args = vec![handle_val, zero_i64, seek_end];
        let seek_ty = self.function_type_for("fseek")?;
        let seek_res = unsafe {
            LLVMBuildCall2(
                self.builder,
                seek_ty,
                fseek_fn,
                seek_args.as_mut_ptr(),
                seek_args.len() as u32,
                CString::new("file.fseek")?.as_ptr(),
            )
        };
        let seek_ok = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                seek_res,
                LLVMConstInt(i32_ty, 0, 0),
                CString::new("file.seek.ok")?.as_ptr(),
            )
        };

        let seek_err_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.read.seek.err")?.as_ptr(),
            )
        };
        let tell_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.read.tell")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, seek_ok, tell_bb, seek_err_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, seek_err_bb) };
        let seek_err_payload = self.codegen_err_value("failed to seek file")?;
        let seek_err_val = self.codegen_result_err(&ok_type, &err_type, Some(seek_err_payload))?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, tell_bb) };
        let mut tell_args = vec![handle_val];
        let tell_ty = self.function_type_for("ftell")?;
        let size_val = unsafe {
            LLVMBuildCall2(
                self.builder,
                tell_ty,
                ftell_fn,
                tell_args.as_mut_ptr(),
                tell_args.len() as u32,
                CString::new("file.ftell")?.as_ptr(),
            )
        };
        let size_neg = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntSLT,
                size_val,
                LLVMConstInt(i64_ty, 0, 1),
                CString::new("file.size.neg")?.as_ptr(),
            )
        };
        let size_err_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.read.size.err")?.as_ptr(),
            )
        };
        let alloc_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.read.alloc")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, size_neg, size_err_bb, alloc_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, size_err_bb) };
        let size_err_payload = self.codegen_err_value("failed to read file")?;
        let size_err_val = self.codegen_result_err(&ok_type, &err_type, Some(size_err_payload))?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, alloc_bb) };
        let mut rewind_args = vec![handle_val];
        let rewind_ty = self.function_type_for("rewind")?;
        let _ = self.build_call2(rewind_ty, rewind_fn, &mut rewind_args, "file.rewind")?;

        let size_plus_one = unsafe {
            LLVMBuildAdd(
                self.builder,
                size_val,
                LLVMConstInt(i64_ty, 1, 0),
                CString::new("file.size.plus")?.as_ptr(),
            )
        };
        let malloc_fn = self.ensure_malloc_fn()?;
        let malloc_ty = self.malloc_function_type();
        let mut malloc_args = vec![size_plus_one];
        let buf_ptr = unsafe {
            LLVMBuildCall2(
                self.builder,
                malloc_ty,
                malloc_fn,
                malloc_args.as_mut_ptr(),
                malloc_args.len() as u32,
                CString::new("file.buf")?.as_ptr(),
            )
        };
        let buf_null = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                buf_ptr,
                LLVMConstNull(LLVMTypeOf(buf_ptr)),
                CString::new("file.buf.null")?.as_ptr(),
            )
        };
        let buf_err_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.read.buf.err")?.as_ptr(),
            )
        };
        let read_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.read.body")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, buf_null, buf_err_bb, read_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, buf_err_bb) };
        let buf_err_payload = self.codegen_err_value("failed to allocate buffer")?;
        let buf_err_val = self.codegen_result_err(&ok_type, &err_type, Some(buf_err_payload))?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, read_bb) };
        let size_u32 = unsafe {
            LLVMBuildTrunc(
                self.builder,
                size_val,
                LLVMInt32TypeInContext(self.context),
                CString::new("file.size.u32")?.as_ptr(),
            )
        };
        let one_u32 = unsafe { LLVMConstInt(LLVMInt32TypeInContext(self.context), 1, 0) };
        let mut fread_args = vec![buf_ptr, one_u32, size_u32, handle_val];
        let fread_ty = self.function_type_for("fread")?;
        let read_count = unsafe {
            LLVMBuildCall2(
                self.builder,
                fread_ty,
                fread_fn,
                fread_args.as_mut_ptr(),
                fread_args.len() as u32,
                CString::new("file.fread")?.as_ptr(),
            )
        };
        let size_zero = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                size_u32,
                LLVMConstInt(LLVMInt32TypeInContext(self.context), 0, 0),
                CString::new("file.size.zero")?.as_ptr(),
            )
        };
        let read_zero = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                read_count,
                LLVMConstInt(LLVMInt32TypeInContext(self.context), 0, 0),
                CString::new("file.read.zero")?.as_ptr(),
            )
        };
        let read_err = unsafe {
            LLVMBuildAnd(
                self.builder,
                read_zero,
                LLVMBuildNot(
                    self.builder,
                    size_zero,
                    CString::new("file.size.notzero")?.as_ptr(),
                ),
                CString::new("file.read.err")?.as_ptr(),
            )
        };
        let read_err_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.read.fail")?.as_ptr(),
            )
        };
        let read_ok_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.read.ok")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, read_err, read_err_bb, read_ok_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, read_err_bb) };
        self.codegen_free(buf_ptr)?;
        let read_err_payload = self.codegen_err_value("failed to read file")?;
        let read_err_val = self.codegen_result_err(&ok_type, &err_type, Some(read_err_payload))?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, read_ok_bb) };
        let read_count64 = unsafe {
            LLVMBuildZExt(
                self.builder,
                read_count,
                i64_ty,
                CString::new("file.read.count64")?.as_ptr(),
            )
        };
        let i8_ty = unsafe { LLVMInt8TypeInContext(self.context) };
        let null_ptr = unsafe { LLVMPointerType(i8_ty, 0) };
        let buf_i8 = unsafe {
            LLVMBuildBitCast(
                self.builder,
                buf_ptr,
                null_ptr,
                CString::new("file.buf.cast")?.as_ptr(),
            )
        };
        let null_idx_ptr = unsafe {
            LLVMBuildGEP2(
                self.builder,
                i8_ty,
                buf_i8,
                vec![read_count64].as_mut_ptr(),
                1,
                CString::new("file.buf.term")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, LLVMConstInt(i8_ty, 0, 0), null_idx_ptr) };
        let ok_val = self.codegen_result_ok(&ok_type, &err_type, Some(buf_ptr))?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, done_bb) };
        let phi = unsafe {
            let phi_name = CString::new("file.read.result")?;
            let phi_node = LLVMBuildPhi(self.builder, result_ty, phi_name.as_ptr());
            let mut vals = vec![
                err_val,
                seek_err_val,
                size_err_val,
                buf_err_val,
                read_err_val,
                ok_val,
            ];
            let mut bbs = vec![
                err_null_bb,
                seek_err_bb,
                size_err_bb,
                buf_err_bb,
                read_err_bb,
                read_ok_bb,
            ];
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

    pub(super) fn codegen_file_write_string(
        &mut self,
        file: LocalId,
        contents: &MirValue,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        functions: &HashMap<String, LLVMValueRef>,
        _mir_module: &MirModule,
    ) -> Result<LLVMValueRef> {
        let (handle_ty, handle_ptr) = self.file_handle_ptr(file, func, local_map)?;
        let handle_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                handle_ty,
                handle_ptr,
                CString::new("file.handle.load")?.as_ptr(),
            )
        };
        let null_ptr = unsafe { LLVMConstNull(handle_ty) };

        let is_null = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                handle_val,
                null_ptr,
                CString::new("file.null")?.as_ptr(),
            )
        };

        let ok_type = Type::U32;
        let err_type = Type::Named("Err".into());
        let result_name = format!(
            "Result${}__{}",
            self.type_key(&ok_type),
            self.type_key(&err_type)
        );
        let result_ty = self.get_enum_type(&result_name)?;

        let parent_bb = unsafe { LLVMGetInsertBlock(self.builder) };
        let parent_fn = unsafe { LLVMGetBasicBlockParent(parent_bb) };
        let err_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.write.err")?.as_ptr(),
            )
        };
        let body_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.write.body")?.as_ptr(),
            )
        };
        let done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.write.done")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, is_null, err_bb, body_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, err_bb) };
        let err_payload = self.codegen_err_value("file is closed")?;
        let err_val = self.codegen_result_err(&ok_type, &err_type, Some(err_payload))?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, body_bb) };
        let contents_val = self.codegen_value(contents, func, local_map)?;
        let strlen_fn = self.get_extern_function(functions, "strlen")?;
        let mut strlen_args = vec![contents_val];
        let strlen_ty = self.function_type_for("strlen")?;
        let len_val = unsafe {
            LLVMBuildCall2(
                self.builder,
                strlen_ty,
                strlen_fn,
                strlen_args.as_mut_ptr(),
                strlen_args.len() as u32,
                CString::new("file.strlen")?.as_ptr(),
            )
        };
        let len_u32 = unsafe {
            LLVMBuildTrunc(
                self.builder,
                len_val,
                LLVMInt32TypeInContext(self.context),
                CString::new("file.len.u32")?.as_ptr(),
            )
        };
        let fwrite_fn = self.get_extern_function(functions, "write")?;
        let one_u32 = unsafe { LLVMConstInt(LLVMInt32TypeInContext(self.context), 1, 0) };
        let mut fwrite_args = vec![contents_val, one_u32, len_u32, handle_val];
        let fwrite_ty = self.function_type_for("write")?;
        let wrote = unsafe {
            LLVMBuildCall2(
                self.builder,
                fwrite_ty,
                fwrite_fn,
                fwrite_args.as_mut_ptr(),
                fwrite_args.len() as u32,
                CString::new("file.fwrite")?.as_ptr(),
            )
        };
        let wrote_zero = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                wrote,
                LLVMConstInt(LLVMInt32TypeInContext(self.context), 0, 0),
                CString::new("file.write.zero")?.as_ptr(),
            )
        };
        let len_zero = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                len_u32,
                LLVMConstInt(LLVMInt32TypeInContext(self.context), 0, 0),
                CString::new("file.len.zero")?.as_ptr(),
            )
        };
        let write_err = unsafe {
            LLVMBuildAnd(
                self.builder,
                wrote_zero,
                LLVMBuildNot(
                    self.builder,
                    len_zero,
                    CString::new("file.len.notzero")?.as_ptr(),
                ),
                CString::new("file.write.err")?.as_ptr(),
            )
        };
        let ok_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.write.ok")?.as_ptr(),
            )
        };
        let err_write_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.write.fail")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, write_err, err_write_bb, ok_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, err_write_bb) };
        let err_write_payload = self.codegen_err_value("failed to write file")?;
        let err_write_val =
            self.codegen_result_err(&ok_type, &err_type, Some(err_write_payload))?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, ok_bb) };
        let ok_val = self.codegen_result_ok(&ok_type, &err_type, Some(wrote))?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, done_bb) };
        let phi = unsafe {
            let phi_name = CString::new("file.write.result")?;
            let phi_node = LLVMBuildPhi(self.builder, result_ty, phi_name.as_ptr());
            let mut vals = vec![err_val, err_write_val, ok_val];
            let mut bbs = vec![err_bb, err_write_bb, ok_bb];
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

    pub(super) fn codegen_file_close(
        &mut self,
        file: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        functions: &HashMap<String, LLVMValueRef>,
        _mir_module: &MirModule,
    ) -> Result<LLVMValueRef> {
        let (handle_ty, handle_ptr) = self.file_handle_ptr(file, func, local_map)?;
        let handle_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                handle_ty,
                handle_ptr,
                CString::new("file.handle.load")?.as_ptr(),
            )
        };
        let null_ptr = unsafe { LLVMConstNull(handle_ty) };
        let is_null = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                handle_val,
                null_ptr,
                CString::new("file.null")?.as_ptr(),
            )
        };

        let ok_type = Type::I32;
        let err_type = Type::Named("Err".into());
        let result_name = format!(
            "Result${}__{}",
            self.type_key(&ok_type),
            self.type_key(&err_type)
        );
        let result_ty = self.get_enum_type(&result_name)?;

        let parent_bb = unsafe { LLVMGetInsertBlock(self.builder) };
        let parent_fn = unsafe { LLVMGetBasicBlockParent(parent_bb) };
        let err_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.close.err")?.as_ptr(),
            )
        };
        let body_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.close.body")?.as_ptr(),
            )
        };
        let done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.close.done")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, is_null, err_bb, body_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, err_bb) };
        let err_payload = self.codegen_err_value("file already closed")?;
        let err_val = self.codegen_result_err(&ok_type, &err_type, Some(err_payload))?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, body_bb) };
        let fclose_fn = self.get_extern_function(functions, "fclose")?;
        let mut fclose_args = vec![handle_val];
        let fclose_ty = self.function_type_for("fclose")?;
        let close_res = unsafe {
            LLVMBuildCall2(
                self.builder,
                fclose_ty,
                fclose_fn,
                fclose_args.as_mut_ptr(),
                fclose_args.len() as u32,
                CString::new("file.fclose")?.as_ptr(),
            )
        };
        let close_ok = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                close_res,
                LLVMConstInt(LLVMInt32TypeInContext(self.context), 0, 0),
                CString::new("file.close.ok")?.as_ptr(),
            )
        };
        let ok_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.close.ok.bb")?.as_ptr(),
            )
        };
        let err_close_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("file.close.fail")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, close_ok, ok_bb, err_close_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, err_close_bb) };
        let err_close_payload = self.codegen_err_value("failed to close file")?;
        let err_close_val =
            self.codegen_result_err(&ok_type, &err_type, Some(err_close_payload))?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, ok_bb) };
        unsafe { LLVMBuildStore(self.builder, null_ptr, handle_ptr) };
        let ok_val = self.codegen_result_ok(&ok_type, &err_type, Some(close_res))?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, done_bb) };
        let phi = unsafe {
            let phi_name = CString::new("file.close.result")?;
            let phi_node = LLVMBuildPhi(self.builder, result_ty, phi_name.as_ptr());
            let mut vals = vec![err_val, err_close_val, ok_val];
            let mut bbs = vec![err_bb, err_close_bb, ok_bb];
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
}
