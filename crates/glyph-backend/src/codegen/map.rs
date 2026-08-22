use super::*;

const MAP_INITIAL_BUCKETS: u64 = 8;

impl CodegenContext {
    pub(super) fn codegen_map_bucket_index(
        &mut self,
        map: LocalId,
        key_type: &Type,
        key: &MirValue,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        functions: &HashMap<String, LLVMValueRef>,
        mir_module: &MirModule,
    ) -> Result<LLVMValueRef> {
        let (struct_name, map_ptr) = self.struct_pointer_for_local(map, func, local_map)?;
        let llvm_map_ty = self.get_struct_type(&struct_name)?;
        let cap_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                map_ptr,
                1,
                CString::new("map.cap.ptr")?.as_ptr(),
            )
        };
        let usize_ty = unsafe { LLVMInt64TypeInContext(self.context) };
        let cap_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                cap_ptr,
                CString::new("map.cap")?.as_ptr(),
            )
        };

        let hash_val =
            self.codegen_map_hash(key_type, key, func, local_map, functions, mir_module)?;
        let hash_val = self.ensure_usize(hash_val, usize_ty)?;
        let zero = unsafe { LLVMConstInt(usize_ty, 0, 0) };
        let cap_is_zero = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                cap_val,
                zero,
                CString::new("map.cap.zero")?.as_ptr(),
            )
        };
        let rem = unsafe {
            LLVMBuildURem(
                self.builder,
                hash_val,
                cap_val,
                CString::new("map.bucket.idx")?.as_ptr(),
            )
        };
        Ok(unsafe {
            LLVMBuildSelect(
                self.builder,
                cap_is_zero,
                zero,
                rem,
                CString::new("map.bucket.sel")?.as_ptr(),
            )
        })
    }

    pub(super) fn codegen_map_init(
        &mut self,
        key_type: &Type,
        value_type: &Type,
        rvalue: &Rvalue,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        self.debug_log(&format!(
            "codegen_map_init start: key={:?} value={:?}",
            key_type, value_type
        ));
        let inst_name = format!(
            "Map${}__{}",
            self.type_key(key_type),
            self.type_key(value_type)
        );
        let (llvm_map_ty, usize_ty) = self
            .get_map_layout(&inst_name)
            .ok_or_else(|| anyhow!("missing map layout for {}", inst_name))?;

        let alloca_name = CString::new("map.tmp")?;
        let alloca = unsafe { LLVMBuildAlloca(self.builder, llvm_map_ty, alloca_name.as_ptr()) };

        let zero = unsafe { LLVMConstInt(usize_ty, 0, 0) };
        let cap_val = match rvalue {
            Rvalue::MapWithCapacity { capacity, .. } => {
                self.codegen_value(capacity, func, local_map)?
            }
            _ => zero,
        };
        let cap_val = self.ensure_usize(cap_val, usize_ty)?;

        let buckets_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                alloca,
                0,
                CString::new("map.buckets").unwrap().as_ptr(),
            )
        };

        let bucket_name = self.map_bucket_name(key_type, value_type);
        let bucket_head_ty = Type::RawPtr(Box::new(Type::Named(bucket_name)));
        let bucket_array_ty = Type::RawPtr(Box::new(bucket_head_ty.clone()));
        let bucket_array_llvm_ty = self.get_llvm_type(&bucket_array_ty)?;

        let cap_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                alloca,
                1,
                CString::new("map.cap").unwrap().as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, cap_val, cap_ptr) };

        let len_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                alloca,
                2,
                CString::new("map.len").unwrap().as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, zero, len_ptr) };

        let cap_is_zero = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                cap_val,
                zero,
                CString::new("map.cap.zero")?.as_ptr(),
            )
        };

        let parent_bb = unsafe { LLVMGetInsertBlock(self.builder) };
        let parent_fn = unsafe { LLVMGetBasicBlockParent(parent_bb) };
        let alloc_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.cap.alloc")?.as_ptr(),
            )
        };
        let zero_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.cap.zero")?.as_ptr(),
            )
        };
        let done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.cap.done")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, cap_is_zero, zero_bb, alloc_bb) };

        // alloc path
        unsafe { LLVMPositionBuilderAtEnd(self.builder, alloc_bb) };
        let bucket_head_llvm_ty = self.get_llvm_type(&bucket_head_ty)?;
        let elem_size = unsafe { LLVMSizeOf(bucket_head_llvm_ty) };
        let alloc_size = unsafe {
            LLVMBuildMul(
                self.builder,
                cap_val,
                elem_size,
                CString::new("map.cap.alloc.size")?.as_ptr(),
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
                CString::new("map.cap.malloc")?.as_ptr(),
            )
        };
        let bucket_array = unsafe {
            LLVMBuildBitCast(
                self.builder,
                raw_ptr,
                bucket_array_llvm_ty,
                CString::new("map.cap.cast")?.as_ptr(),
            )
        };

        let i8_ty = unsafe { LLVMInt8TypeInContext(self.context) };
        let i8_ptr_ty = unsafe { LLVMPointerType(i8_ty, 0) };
        let memset_ptr = unsafe {
            LLVMBuildBitCast(
                self.builder,
                bucket_array,
                i8_ptr_ty,
                CString::new("map.cap.memset.ptr")?.as_ptr(),
            )
        };
        let zero_byte = unsafe { LLVMConstInt(i8_ty, 0, 0) };
        unsafe {
            LLVMBuildMemSet(self.builder, memset_ptr, zero_byte, alloc_size, 0);
        }

        unsafe { LLVMBuildStore(self.builder, bucket_array, buckets_ptr) };
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        // zero-cap path
        unsafe { LLVMPositionBuilderAtEnd(self.builder, zero_bb) };
        let null_buckets = unsafe { LLVMConstNull(bucket_array_llvm_ty) };
        unsafe { LLVMBuildStore(self.builder, null_buckets, buckets_ptr) };
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        // done
        unsafe { LLVMPositionBuilderAtEnd(self.builder, done_bb) };

        let load_name = CString::new("map.value")?;
        let result =
            unsafe { LLVMBuildLoad2(self.builder, llvm_map_ty, alloca, load_name.as_ptr()) };
        self.debug_log(&format!(
            "codegen_map_init done: key={:?} value={:?}",
            key_type, value_type
        ));
        Ok(result)
    }

    pub(super) fn codegen_map_add(
        &mut self,
        map: LocalId,
        key_type: &Type,
        key: &MirValue,
        value_type: &Type,
        value: &MirValue,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        functions: &HashMap<String, LLVMValueRef>,
        mir_module: &MirModule,
    ) -> Result<LLVMValueRef> {
        self.debug_log(&format!(
            "codegen_map_add start: key={:?} value={:?}",
            key_type, value_type
        ));
        // Ensure bucket type is fully registered before any GEP/load operations
        self.ensure_map_bucket_type(key_type, value_type)?;

        let (struct_name, map_ptr) = self.struct_pointer_for_local(map, func, local_map)?;
        let (llvm_map_ty, usize_ty) = self
            .get_map_layout(&struct_name)
            .ok_or_else(|| anyhow!("missing map layout for {}", struct_name))?;

        let buckets_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                map_ptr,
                0,
                CString::new("map.buckets").unwrap().as_ptr(),
            )
        };
        let cap_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                map_ptr,
                1,
                CString::new("map.cap").unwrap().as_ptr(),
            )
        };
        let len_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                map_ptr,
                2,
                CString::new("map.len").unwrap().as_ptr(),
            )
        };
        let cap_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                cap_ptr,
                CString::new("map.cap.load")?.as_ptr(),
            )
        };
        let zero = unsafe { LLVMConstInt(usize_ty, 0, 0) };
        let cap_is_zero = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                cap_val,
                zero,
                CString::new("map.cap.zero")?.as_ptr(),
            )
        };

        let parent_bb = unsafe { LLVMGetInsertBlock(self.builder) };
        let parent_fn = unsafe { LLVMGetBasicBlockParent(parent_bb) };
        let alloc_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.ensure.alloc")?.as_ptr(),
            )
        };
        let cont_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.ensure.cont")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, cap_is_zero, alloc_bb, cont_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, alloc_bb) };
        let init_cap = unsafe { LLVMConstInt(usize_ty, MAP_INITIAL_BUCKETS, 0) };
        self.codegen_map_allocate_buckets(buckets_ptr, cap_ptr, init_cap, key_type, value_type)?;
        unsafe { LLVMBuildBr(self.builder, cont_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, cont_bb) };
        let mut key_val = self.codegen_value(key, func, local_map)?;
        if matches!(key_type, Type::String | Type::Str) {
            let key_len = self.codegen_string_len_value(key_val, functions)?;
            key_val = self.codegen_string_copy_from_ptr_len(key_val, key_len, functions)?;
        }
        let value_val = self.codegen_value_owned(value, value_type, func, local_map)?;

        let cap_val_cont = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                cap_ptr,
                CString::new("map.cap.load")?.as_ptr(),
            )
        };
        let (bucket_index, hash_dbg, key_len_dbg) = if matches!(key_type, Type::String | Type::Str)
        {
            let key_len = self.codegen_string_len_value(key_val, functions)?;
            let hash_val = self.codegen_hash_bytes(key_val, key_len)?;
            let hash_val = self.ensure_usize(hash_val, usize_ty)?;
            let zero = unsafe { LLVMConstInt(usize_ty, 0, 0) };
            let cap_is_zero = unsafe {
                LLVMBuildICmp(
                    self.builder,
                    llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                    cap_val_cont,
                    zero,
                    CString::new("map.cap.zero")?.as_ptr(),
                )
            };
            let rem = unsafe {
                LLVMBuildURem(
                    self.builder,
                    hash_val,
                    cap_val_cont,
                    CString::new("map.bucket.idx")?.as_ptr(),
                )
            };
            let bucket_index = unsafe {
                LLVMBuildSelect(
                    self.builder,
                    cap_is_zero,
                    zero,
                    rem,
                    CString::new("map.bucket.sel")?.as_ptr(),
                )
            };
            (bucket_index, hash_val, key_len)
        } else {
            let idx = self.codegen_map_bucket_index(
                map, key_type, key, func, local_map, functions, mir_module,
            )?;
            let zero = unsafe { LLVMConstInt(usize_ty, 0, 0) };
            (idx, zero, zero)
        };

        let bucket_name = self.map_bucket_name(key_type, value_type);
        let bucket_head_ty = Type::RawPtr(Box::new(Type::Named(bucket_name.clone())));
        let bucket_head_llvm_ty = self.get_llvm_type(&bucket_head_ty)?;
        let bucket_array_ty = Type::RawPtr(Box::new(bucket_head_ty.clone()));
        let bucket_array_llvm_ty = self.get_llvm_type(&bucket_array_ty)?;

        let bucket_array = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_array_llvm_ty,
                buckets_ptr,
                CString::new("map.buckets.load")?.as_ptr(),
            )
        };
        let bucket_slot_ptr = unsafe {
            LLVMBuildGEP2(
                self.builder,
                bucket_head_llvm_ty,
                bucket_array,
                vec![bucket_index].as_mut_ptr(),
                1,
                CString::new("map.bucket.slot")?.as_ptr(),
            )
        };
        let head_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_head_llvm_ty,
                bucket_slot_ptr,
                CString::new("map.bucket.head")?.as_ptr(),
            )
        };
        let cap_dbg = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                cap_ptr,
                CString::new("map.cap.dbg")?.as_ptr(),
            )
        };
        let len_dbg = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                len_ptr,
                CString::new("map.len.dbg")?.as_ptr(),
            )
        };

        self.debug_map_log("add", cap_dbg, len_dbg, head_val)?;
        self.debug_map_log_index(
            "add.idx",
            cap_dbg,
            len_dbg,
            bucket_array,
            head_val,
            bucket_index,
            hash_dbg,
            key_len_dbg,
        )?;
        if matches!(key_type, Type::String | Type::Str) {
            self.debug_map_key("add.key", key_val, key_len_dbg)?;
        }

        let current_slot = unsafe {
            LLVMBuildAlloca(
                self.builder,
                bucket_head_llvm_ty,
                CString::new("map.curr")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, head_val, current_slot) };

        let input_key_val = match key_type {
            Type::Named(_) => None,
            _ => Some(key_val),
        };
        let input_hash = match key_type {
            Type::Named(_) => {
                Some(self.codegen_map_hash(key_type, key, func, local_map, functions, mir_module)?)
            }
            _ => None,
        };

        let check_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.add.check")?.as_ptr(),
            )
        };
        let body_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.add.body")?.as_ptr(),
            )
        };
        let next_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.add.next")?.as_ptr(),
            )
        };
        let found_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.add.found")?.as_ptr(),
            )
        };
        let insert_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.add.insert")?.as_ptr(),
            )
        };
        let done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.add.done")?.as_ptr(),
            )
        };

        unsafe { LLVMBuildBr(self.builder, check_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, check_bb) };
        let current_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_head_llvm_ty,
                current_slot,
                CString::new("map.curr.load")?.as_ptr(),
            )
        };
        let null_bucket = unsafe { LLVMConstNull(bucket_head_llvm_ty) };
        let is_null = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                current_val,
                null_bucket,
                CString::new("map.curr.null")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, is_null, insert_bb, body_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, body_bb) };
        let llvm_bucket_ty = self.get_struct_type(&bucket_name)?;
        let key_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                current_val,
                0,
                CString::new("map.bucket.key")?.as_ptr(),
            )
        };
        let key_match = match key_type {
            Type::Named(_) => {
                let bucket_hash =
                    self.codegen_map_hash_from_ptr(key_type, key_ptr, functions, mir_module)?;
                self.debug_map_hash_compare(
                    "get.hash",
                    input_hash.expect("hash for named key"),
                    bucket_hash,
                )?;
                unsafe {
                    LLVMBuildICmp(
                        self.builder,
                        llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                        bucket_hash,
                        input_hash.expect("hash for named key"),
                        CString::new("map.key.eq")?.as_ptr(),
                    )
                }
            }
            _ => {
                let key_llvm_ty = self.get_llvm_type(key_type)?;
                let bucket_key_val = unsafe {
                    LLVMBuildLoad2(
                        self.builder,
                        key_llvm_ty,
                        key_ptr,
                        CString::new("map.key.load")?.as_ptr(),
                    )
                };
                if matches!(key_type, Type::String | Type::Str) {
                    let input_val = input_key_val.expect("key value");
                    let input_len = self.codegen_string_len_value(input_val, functions)?;
                    let bucket_len = self.codegen_string_len_value(bucket_key_val, functions)?;
                    let eq_val = self.codegen_string_eq(bucket_key_val, input_val, functions)?;
                    self.debug_map_key_compare(
                        "get.cmp",
                        input_val,
                        bucket_key_val,
                        input_len,
                        bucket_len,
                        eq_val,
                    )?;
                    eq_val
                } else {
                    unsafe {
                        LLVMBuildICmp(
                            self.builder,
                            llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                            bucket_key_val,
                            input_key_val.expect("key value"),
                            CString::new("map.key.eq")?.as_ptr(),
                        )
                    }
                }
            }
        };
        unsafe { LLVMBuildCondBr(self.builder, key_match, found_bb, next_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, next_bb) };
        let next_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                current_val,
                2,
                CString::new("map.bucket.next")?.as_ptr(),
            )
        };
        let next_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_head_llvm_ty,
                next_ptr,
                CString::new("map.bucket.next.load")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, next_val, current_slot) };
        unsafe { LLVMBuildBr(self.builder, check_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, found_bb) };
        let err_val = self.codegen_err_value("key exists")?;
        let err_ty = Type::Named("Err".into());
        let ok_ty = Type::Void;
        let exists_val = self.codegen_result_err(&ok_ty, &err_ty, Some(err_val))?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, insert_bb) };
        let bucket_size = unsafe { LLVMSizeOf(llvm_bucket_ty) };
        let malloc_fn = self.ensure_malloc_fn()?;
        let malloc_ty = self.malloc_function_type();
        let mut malloc_args = vec![bucket_size];
        let raw_ptr = unsafe {
            LLVMBuildCall2(
                self.builder,
                malloc_ty,
                malloc_fn,
                malloc_args.as_mut_ptr(),
                malloc_args.len() as u32,
                CString::new("map.bucket.malloc")?.as_ptr(),
            )
        };
        let new_bucket = unsafe {
            LLVMBuildBitCast(
                self.builder,
                raw_ptr,
                bucket_head_llvm_ty,
                CString::new("map.bucket.cast")?.as_ptr(),
            )
        };
        let new_key_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                new_bucket,
                0,
                CString::new("map.bucket.key")?.as_ptr(),
            )
        };
        let new_val_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                new_bucket,
                1,
                CString::new("map.bucket.val")?.as_ptr(),
            )
        };
        let new_next_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                new_bucket,
                2,
                CString::new("map.bucket.next")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, key_val, new_key_ptr) };
        unsafe { LLVMBuildStore(self.builder, value_val, new_val_ptr) };
        unsafe { LLVMBuildStore(self.builder, head_val, new_next_ptr) };
        unsafe { LLVMBuildStore(self.builder, new_bucket, bucket_slot_ptr) };

        let len_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                len_ptr,
                CString::new("map.len.load")?.as_ptr(),
            )
        };
        let new_len = unsafe {
            LLVMBuildAdd(
                self.builder,
                len_val,
                LLVMConstInt(usize_ty, 1, 0),
                CString::new("map.len.inc")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, new_len, len_ptr) };
        let ok_val = self.codegen_result_ok(&ok_ty, &err_ty, None)?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, done_bb) };
        let result_ty = self.get_enum_type(&format!(
            "Result${}__{}",
            self.type_key(&ok_ty),
            self.type_key(&err_ty)
        ))?;
        let phi = unsafe {
            let phi_name = CString::new("map.add.result")?;
            let phi_node = LLVMBuildPhi(self.builder, result_ty, phi_name.as_ptr());
            let mut incoming_vals = vec![exists_val, ok_val];
            let mut incoming_bbs = vec![found_bb, insert_bb];
            LLVMAddIncoming(
                phi_node,
                incoming_vals.as_mut_ptr(),
                incoming_bbs.as_mut_ptr(),
                incoming_vals.len() as u32,
            );
            phi_node
        };
        self.debug_log(&format!(
            "codegen_map_add done: key={:?} value={:?}",
            key_type, value_type
        ));
        Ok(phi)
    }

    pub(super) fn codegen_map_update(
        &mut self,
        map: LocalId,
        key_type: &Type,
        key: &MirValue,
        value_type: &Type,
        value: &MirValue,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        functions: &HashMap<String, LLVMValueRef>,
        mir_module: &MirModule,
    ) -> Result<LLVMValueRef> {
        self.debug_log(&format!(
            "codegen_map_update start: key={:?} value={:?}",
            key_type, value_type
        ));
        // Ensure bucket type is fully registered before any GEP/load operations
        self.ensure_map_bucket_type(key_type, value_type)?;

        let (struct_name, map_ptr) = self.struct_pointer_for_local(map, func, local_map)?;
        let (llvm_map_ty, usize_ty) = self
            .get_map_layout(&struct_name)
            .ok_or_else(|| anyhow!("missing map layout for {}", struct_name))?;
        let buckets_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                map_ptr,
                0,
                CString::new("map.buckets").unwrap().as_ptr(),
            )
        };
        let cap_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                map_ptr,
                1,
                CString::new("map.cap").unwrap().as_ptr(),
            )
        };
        let len_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                map_ptr,
                2,
                CString::new("map.len").unwrap().as_ptr(),
            )
        };
        let cap_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                cap_ptr,
                CString::new("map.cap.load")?.as_ptr(),
            )
        };
        let len_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                len_ptr,
                CString::new("map.len.load")?.as_ptr(),
            )
        };
        let zero = unsafe { LLVMConstInt(usize_ty, 0, 0) };
        let cap_is_zero = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                cap_val,
                zero,
                CString::new("map.cap.zero")?.as_ptr(),
            )
        };

        let parent_bb = unsafe { LLVMGetInsertBlock(self.builder) };
        let parent_fn = unsafe { LLVMGetBasicBlockParent(parent_bb) };
        let missing_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.update.missing")?.as_ptr(),
            )
        };
        let cont_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.update.cont")?.as_ptr(),
            )
        };
        let done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.update.done")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, cap_is_zero, missing_bb, cont_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, missing_bb) };
        let err_val = self.codegen_err_value("key missing")?;
        let err_ty = Type::Named("Err".into());
        let ok_ty = Type::Void;
        let missing_val = self.codegen_result_err(&ok_ty, &err_ty, Some(err_val))?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, cont_bb) };
        let (bucket_index, hash_dbg, key_len_dbg) = if matches!(key_type, Type::String | Type::Str)
        {
            let key_val = self.codegen_value(key, func, local_map)?;
            let key_len = self.codegen_string_len_value(key_val, functions)?;
            let hash_val = self.codegen_hash_bytes(key_val, key_len)?;
            let hash_val = self.ensure_usize(hash_val, usize_ty)?;
            let zero = unsafe { LLVMConstInt(usize_ty, 0, 0) };
            let cap_is_zero = unsafe {
                LLVMBuildICmp(
                    self.builder,
                    llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                    cap_val,
                    zero,
                    CString::new("map.cap.zero")?.as_ptr(),
                )
            };
            let rem = unsafe {
                LLVMBuildURem(
                    self.builder,
                    hash_val,
                    cap_val,
                    CString::new("map.bucket.idx")?.as_ptr(),
                )
            };
            let bucket_index = unsafe {
                LLVMBuildSelect(
                    self.builder,
                    cap_is_zero,
                    zero,
                    rem,
                    CString::new("map.bucket.sel")?.as_ptr(),
                )
            };
            (bucket_index, hash_val, key_len)
        } else {
            let idx = self.codegen_map_bucket_index(
                map, key_type, key, func, local_map, functions, mir_module,
            )?;
            let zero = unsafe { LLVMConstInt(usize_ty, 0, 0) };
            (idx, zero, zero)
        };

        let bucket_name = self.map_bucket_name(key_type, value_type);
        let bucket_head_ty = Type::RawPtr(Box::new(Type::Named(bucket_name.clone())));
        let bucket_head_llvm_ty = self.get_llvm_type(&bucket_head_ty)?;
        let bucket_array_ty = Type::RawPtr(Box::new(bucket_head_ty.clone()));
        let bucket_array_llvm_ty = self.get_llvm_type(&bucket_array_ty)?;

        let bucket_array = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_array_llvm_ty,
                buckets_ptr,
                CString::new("map.buckets.load")?.as_ptr(),
            )
        };
        let bucket_slot_ptr = unsafe {
            LLVMBuildGEP2(
                self.builder,
                bucket_head_llvm_ty,
                bucket_array,
                vec![bucket_index].as_mut_ptr(),
                1,
                CString::new("map.bucket.slot")?.as_ptr(),
            )
        };
        let head_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_head_llvm_ty,
                bucket_slot_ptr,
                CString::new("map.bucket.head")?.as_ptr(),
            )
        };

        self.debug_map_log_index(
            "get.idx",
            cap_val,
            len_val,
            bucket_array,
            head_val,
            bucket_index,
            hash_dbg,
            key_len_dbg,
        )?;

        self.debug_map_log("get", cap_val, len_val, head_val)?;

        let current_slot = unsafe {
            LLVMBuildAlloca(
                self.builder,
                bucket_head_llvm_ty,
                CString::new("map.curr")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, head_val, current_slot) };

        let input_key_val = match key_type {
            Type::Named(_) => None,
            _ => Some(self.codegen_value(key, func, local_map)?),
        };
        let input_hash = match key_type {
            Type::Named(_) => {
                Some(self.codegen_map_hash(key_type, key, func, local_map, functions, mir_module)?)
            }
            _ => None,
        };
        let value_val = self.codegen_value_owned(value, value_type, func, local_map)?;

        let check_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.update.check")?.as_ptr(),
            )
        };
        let body_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.update.body")?.as_ptr(),
            )
        };
        let next_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.update.next")?.as_ptr(),
            )
        };
        let found_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.update.found")?.as_ptr(),
            )
        };

        unsafe { LLVMBuildBr(self.builder, check_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, check_bb) };
        let current_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_head_llvm_ty,
                current_slot,
                CString::new("map.curr.load")?.as_ptr(),
            )
        };
        let null_bucket = unsafe { LLVMConstNull(bucket_head_llvm_ty) };
        let is_null = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                current_val,
                null_bucket,
                CString::new("map.curr.null")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, is_null, missing_bb, body_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, body_bb) };
        let llvm_bucket_ty = self.get_struct_type(&bucket_name)?;
        let key_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                current_val,
                0,
                CString::new("map.bucket.key")?.as_ptr(),
            )
        };
        let key_match = match key_type {
            Type::Named(_) => {
                let bucket_hash =
                    self.codegen_map_hash_from_ptr(key_type, key_ptr, functions, mir_module)?;
                unsafe {
                    LLVMBuildICmp(
                        self.builder,
                        llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                        bucket_hash,
                        input_hash.expect("hash for named key"),
                        CString::new("map.key.eq")?.as_ptr(),
                    )
                }
            }
            _ => {
                let key_llvm_ty = self.get_llvm_type(key_type)?;
                let bucket_key_val = unsafe {
                    LLVMBuildLoad2(
                        self.builder,
                        key_llvm_ty,
                        key_ptr,
                        CString::new("map.key.load")?.as_ptr(),
                    )
                };
                if matches!(key_type, Type::String | Type::Str) {
                    let input_val = input_key_val.expect("key value");
                    let input_len = self.codegen_string_len_value(input_val, functions)?;
                    let bucket_len = self.codegen_string_len_value(bucket_key_val, functions)?;
                    let eq_val = self.codegen_string_eq(bucket_key_val, input_val, functions)?;
                    let label = format!("get.cmp.{}", self.type_key(key_type));
                    self.debug_map_key_compare(
                        &label,
                        input_val,
                        bucket_key_val,
                        input_len,
                        bucket_len,
                        eq_val,
                    )?;
                    eq_val
                } else {
                    unsafe {
                        LLVMBuildICmp(
                            self.builder,
                            llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                            bucket_key_val,
                            input_key_val.expect("key value"),
                            CString::new("map.key.eq")?.as_ptr(),
                        )
                    }
                }
            }
        };
        unsafe { LLVMBuildCondBr(self.builder, key_match, found_bb, next_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, next_bb) };
        let next_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                current_val,
                2,
                CString::new("map.bucket.next")?.as_ptr(),
            )
        };
        let next_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_head_llvm_ty,
                next_ptr,
                CString::new("map.bucket.next.load")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, next_val, current_slot) };
        unsafe { LLVMBuildBr(self.builder, check_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, found_bb) };
        let value_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                current_val,
                1,
                CString::new("map.bucket.val")?.as_ptr(),
            )
        };
        self.codegen_drop_elem_slot(value_ptr, value_type)?;
        unsafe { LLVMBuildStore(self.builder, value_val, value_ptr) };
        let ok_val = self.codegen_result_ok(&ok_ty, &err_ty, None)?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, done_bb) };
        let result_ty = self.get_enum_type(&format!(
            "Result${}__{}",
            self.type_key(&ok_ty),
            self.type_key(&err_ty)
        ))?;
        let phi = unsafe {
            let phi_name = CString::new("map.update.result")?;
            let phi_node = LLVMBuildPhi(self.builder, result_ty, phi_name.as_ptr());
            let mut incoming_vals = vec![missing_val, ok_val];
            let mut incoming_bbs = vec![missing_bb, found_bb];
            LLVMAddIncoming(
                phi_node,
                incoming_vals.as_mut_ptr(),
                incoming_bbs.as_mut_ptr(),
                incoming_vals.len() as u32,
            );
            phi_node
        };
        self.debug_log(&format!(
            "codegen_map_update done: key={:?} value={:?}",
            key_type, value_type
        ));
        Ok(phi)
    }

    pub(super) fn codegen_map_del(
        &mut self,
        map: LocalId,
        key_type: &Type,
        value_type: &Type,
        key: &MirValue,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        functions: &HashMap<String, LLVMValueRef>,
        mir_module: &MirModule,
    ) -> Result<LLVMValueRef> {
        self.debug_log(&format!(
            "codegen_map_del start: key={:?} value={:?}",
            key_type, value_type
        ));
        // Ensure bucket type is fully registered before any GEP/load operations
        self.ensure_map_bucket_type(key_type, value_type)?;

        let (struct_name, map_ptr) = self.struct_pointer_for_local(map, func, local_map)?;
        let (llvm_map_ty, usize_ty) = self
            .get_map_layout(&struct_name)
            .ok_or_else(|| anyhow!("missing map layout for {}", struct_name))?;
        let buckets_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                map_ptr,
                0,
                CString::new("map.buckets").unwrap().as_ptr(),
            )
        };
        let cap_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                map_ptr,
                1,
                CString::new("map.cap").unwrap().as_ptr(),
            )
        };
        let len_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                map_ptr,
                2,
                CString::new("map.len").unwrap().as_ptr(),
            )
        };
        let cap_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                cap_ptr,
                CString::new("map.cap.load")?.as_ptr(),
            )
        };
        let zero = unsafe { LLVMConstInt(usize_ty, 0, 0) };
        let cap_is_zero = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                cap_val,
                zero,
                CString::new("map.cap.zero")?.as_ptr(),
            )
        };

        let parent_bb = unsafe { LLVMGetInsertBlock(self.builder) };
        let parent_fn = unsafe { LLVMGetBasicBlockParent(parent_bb) };
        let missing_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.del.missing")?.as_ptr(),
            )
        };
        let cont_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.del.cont")?.as_ptr(),
            )
        };
        let done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.del.done")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, cap_is_zero, missing_bb, cont_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, missing_bb) };
        let err_val = self.codegen_err_value("key missing")?;
        let err_ty = Type::Named("Err".into());
        let ok_ty = value_type.clone();
        let missing_val = self.codegen_result_err(&ok_ty, &err_ty, Some(err_val))?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, cont_bb) };
        let bucket_index = self
            .codegen_map_bucket_index(map, key_type, key, func, local_map, functions, mir_module)?;

        let bucket_name = self.map_bucket_name(key_type, value_type);
        let bucket_head_ty = Type::RawPtr(Box::new(Type::Named(bucket_name.clone())));
        let bucket_head_llvm_ty = self.get_llvm_type(&bucket_head_ty)?;
        let bucket_array_ty = Type::RawPtr(Box::new(bucket_head_ty.clone()));
        let bucket_array_llvm_ty = self.get_llvm_type(&bucket_array_ty)?;

        let bucket_array = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_array_llvm_ty,
                buckets_ptr,
                CString::new("map.buckets.load")?.as_ptr(),
            )
        };
        let bucket_slot_ptr = unsafe {
            LLVMBuildGEP2(
                self.builder,
                bucket_head_llvm_ty,
                bucket_array,
                vec![bucket_index].as_mut_ptr(),
                1,
                CString::new("map.bucket.slot")?.as_ptr(),
            )
        };
        let head_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_head_llvm_ty,
                bucket_slot_ptr,
                CString::new("map.bucket.head")?.as_ptr(),
            )
        };

        let current_slot = unsafe {
            LLVMBuildAlloca(
                self.builder,
                bucket_head_llvm_ty,
                CString::new("map.curr")?.as_ptr(),
            )
        };
        let prev_slot = unsafe {
            LLVMBuildAlloca(
                self.builder,
                bucket_head_llvm_ty,
                CString::new("map.prev")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, head_val, current_slot) };
        unsafe { LLVMBuildStore(self.builder, LLVMConstNull(bucket_head_llvm_ty), prev_slot) };

        let input_key_val = match key_type {
            Type::Named(_) => None,
            _ => Some(self.codegen_value(key, func, local_map)?),
        };
        let input_hash = match key_type {
            Type::Named(_) => {
                Some(self.codegen_map_hash(key_type, key, func, local_map, functions, mir_module)?)
            }
            _ => None,
        };

        let check_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.del.check")?.as_ptr(),
            )
        };
        let body_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.del.body")?.as_ptr(),
            )
        };
        let next_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.del.next")?.as_ptr(),
            )
        };
        let found_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.del.found")?.as_ptr(),
            )
        };
        let unlink_head_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.del.unlink.head")?.as_ptr(),
            )
        };
        let unlink_prev_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.del.unlink.prev")?.as_ptr(),
            )
        };
        let unlink_done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.del.unlink.done")?.as_ptr(),
            )
        };

        unsafe { LLVMBuildBr(self.builder, check_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, check_bb) };
        let current_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_head_llvm_ty,
                current_slot,
                CString::new("map.curr.load")?.as_ptr(),
            )
        };
        let null_bucket = unsafe { LLVMConstNull(bucket_head_llvm_ty) };
        let is_null = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                current_val,
                null_bucket,
                CString::new("map.curr.null")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, is_null, missing_bb, body_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, body_bb) };
        let llvm_bucket_ty = self.get_struct_type(&bucket_name)?;
        let key_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                current_val,
                0,
                CString::new("map.bucket.key")?.as_ptr(),
            )
        };
        let key_match = match key_type {
            Type::Named(_) => {
                let bucket_hash =
                    self.codegen_map_hash_from_ptr(key_type, key_ptr, functions, mir_module)?;
                self.debug_map_hash_compare(
                    "get.hash",
                    input_hash.expect("hash for named key"),
                    bucket_hash,
                )?;
                unsafe {
                    LLVMBuildICmp(
                        self.builder,
                        llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                        bucket_hash,
                        input_hash.expect("hash for named key"),
                        CString::new("map.key.eq")?.as_ptr(),
                    )
                }
            }
            _ => {
                let key_llvm_ty = self.get_llvm_type(key_type)?;
                let bucket_key_val = unsafe {
                    LLVMBuildLoad2(
                        self.builder,
                        key_llvm_ty,
                        key_ptr,
                        CString::new("map.key.load")?.as_ptr(),
                    )
                };
                if matches!(key_type, Type::String | Type::Str) {
                    let input_val = input_key_val.expect("key value");
                    let input_len = self.codegen_string_len_value(input_val, functions)?;
                    let bucket_len = self.codegen_string_len_value(bucket_key_val, functions)?;
                    let eq_val = self.codegen_string_eq(bucket_key_val, input_val, functions)?;
                    let label = format!("get.cmp.{}", self.type_key(key_type));
                    self.debug_map_key_compare(
                        &label,
                        input_val,
                        bucket_key_val,
                        input_len,
                        bucket_len,
                        eq_val,
                    )?;
                    eq_val
                } else {
                    unsafe {
                        LLVMBuildICmp(
                            self.builder,
                            llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                            bucket_key_val,
                            input_key_val.expect("key value"),
                            CString::new("map.key.eq")?.as_ptr(),
                        )
                    }
                }
            }
        };
        unsafe { LLVMBuildCondBr(self.builder, key_match, found_bb, next_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, next_bb) };
        let next_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                current_val,
                2,
                CString::new("map.bucket.next")?.as_ptr(),
            )
        };
        let next_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_head_llvm_ty,
                next_ptr,
                CString::new("map.bucket.next.load")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, current_val, prev_slot) };
        unsafe { LLVMBuildStore(self.builder, next_val, current_slot) };
        unsafe { LLVMBuildBr(self.builder, check_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, found_bb) };
        let next_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                current_val,
                2,
                CString::new("map.bucket.next")?.as_ptr(),
            )
        };
        let next_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_head_llvm_ty,
                next_ptr,
                CString::new("map.bucket.next.load")?.as_ptr(),
            )
        };
        let prev_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_head_llvm_ty,
                prev_slot,
                CString::new("map.prev.load")?.as_ptr(),
            )
        };
        let prev_is_null = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                prev_val,
                null_bucket,
                CString::new("map.prev.null")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, prev_is_null, unlink_head_bb, unlink_prev_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, unlink_head_bb) };
        unsafe { LLVMBuildStore(self.builder, next_val, bucket_slot_ptr) };
        unsafe { LLVMBuildBr(self.builder, unlink_done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, unlink_prev_bb) };
        let prev_next_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                prev_val,
                2,
                CString::new("map.prev.next")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, next_val, prev_next_ptr) };
        unsafe { LLVMBuildBr(self.builder, unlink_done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, unlink_done_bb) };
        let value_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                current_val,
                1,
                CString::new("map.bucket.val")?.as_ptr(),
            )
        };
        let value_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                self.get_llvm_type(value_type)?,
                value_ptr,
                CString::new("map.bucket.val.load")?.as_ptr(),
            )
        };
        self.codegen_drop_elem_slot(key_ptr, key_type)?;
        self.codegen_free(current_val)?;
        let len_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                len_ptr,
                CString::new("map.len.load")?.as_ptr(),
            )
        };
        let new_len = unsafe {
            LLVMBuildSub(
                self.builder,
                len_val,
                LLVMConstInt(usize_ty, 1, 0),
                CString::new("map.len.dec")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, new_len, len_ptr) };
        let ok_val = self.codegen_result_ok(&ok_ty, &err_ty, Some(value_val))?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, done_bb) };
        let result_ty = self.get_enum_type(&format!(
            "Result${}__{}",
            self.type_key(&ok_ty),
            self.type_key(&err_ty)
        ))?;
        let phi = unsafe {
            let phi_name = CString::new("map.del.result")?;
            let phi_node = LLVMBuildPhi(self.builder, result_ty, phi_name.as_ptr());
            let mut incoming_vals = vec![missing_val, ok_val];
            let mut incoming_bbs = vec![missing_bb, unlink_done_bb];
            LLVMAddIncoming(
                phi_node,
                incoming_vals.as_mut_ptr(),
                incoming_bbs.as_mut_ptr(),
                incoming_vals.len() as u32,
            );
            phi_node
        };
        self.debug_log(&format!(
            "codegen_map_del done: key={:?} value={:?}",
            key_type, value_type
        ));
        Ok(phi)
    }

    pub(super) fn codegen_map_get(
        &mut self,
        map: LocalId,
        key_type: &Type,
        value_type: &Type,
        key: &MirValue,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        functions: &HashMap<String, LLVMValueRef>,
        mir_module: &MirModule,
    ) -> Result<LLVMValueRef> {
        self.debug_log(&format!(
            "codegen_map_get start: key={:?} value={:?}",
            key_type, value_type
        ));
        // Ensure bucket type is fully registered before any GEP/load operations
        self.ensure_map_bucket_type(key_type, value_type)?;

        let (struct_name, map_ptr) = self.struct_pointer_for_local(map, func, local_map)?;
        let (llvm_map_ty, usize_ty) = self
            .get_map_layout(&struct_name)
            .ok_or_else(|| anyhow!("missing map layout for {}", struct_name))?;
        let buckets_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                map_ptr,
                0,
                CString::new("map.buckets").unwrap().as_ptr(),
            )
        };
        let cap_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                map_ptr,
                1,
                CString::new("map.cap").unwrap().as_ptr(),
            )
        };
        let len_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                map_ptr,
                2,
                CString::new("map.len").unwrap().as_ptr(),
            )
        };
        let cap_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                cap_ptr,
                CString::new("map.cap.load")?.as_ptr(),
            )
        };
        let len_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                len_ptr,
                CString::new("map.len.load")?.as_ptr(),
            )
        };
        let zero = unsafe { LLVMConstInt(usize_ty, 0, 0) };
        let cap_is_zero = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                cap_val,
                zero,
                CString::new("map.cap.zero")?.as_ptr(),
            )
        };

        let parent_bb = unsafe { LLVMGetInsertBlock(self.builder) };
        let parent_fn = unsafe { LLVMGetBasicBlockParent(parent_bb) };
        let none_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.get.none")?.as_ptr(),
            )
        };
        let cont_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.get.cont")?.as_ptr(),
            )
        };
        let done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.get.done")?.as_ptr(),
            )
        };
        let null_head =
            unsafe { LLVMConstNull(LLVMPointerType(LLVMInt8TypeInContext(self.context), 0)) };
        self.debug_map_log("get.pre", cap_val, len_val, null_head)?;
        unsafe { LLVMBuildCondBr(self.builder, cap_is_zero, none_bb, cont_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, none_bb) };
        let null_head =
            unsafe { LLVMConstNull(LLVMPointerType(LLVMInt8TypeInContext(self.context), 0)) };
        self.debug_map_log("get.none", cap_val, len_val, null_head)?;
        let none_val = self.codegen_option_none(value_type, mir_module)?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, cont_bb) };
        let bucket_index = self
            .codegen_map_bucket_index(map, key_type, key, func, local_map, functions, mir_module)?;

        let bucket_name = self.map_bucket_name(key_type, value_type);
        let bucket_head_ty = Type::RawPtr(Box::new(Type::Named(bucket_name.clone())));
        let bucket_head_llvm_ty = self.get_llvm_type(&bucket_head_ty)?;
        let bucket_array_ty = Type::RawPtr(Box::new(bucket_head_ty.clone()));
        let bucket_array_llvm_ty = self.get_llvm_type(&bucket_array_ty)?;

        let bucket_array = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_array_llvm_ty,
                buckets_ptr,
                CString::new("map.buckets.load")?.as_ptr(),
            )
        };
        let bucket_slot_ptr = unsafe {
            LLVMBuildGEP2(
                self.builder,
                bucket_head_llvm_ty,
                bucket_array,
                vec![bucket_index].as_mut_ptr(),
                1,
                CString::new("map.bucket.slot")?.as_ptr(),
            )
        };
        let head_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_head_llvm_ty,
                bucket_slot_ptr,
                CString::new("map.bucket.head")?.as_ptr(),
            )
        };

        self.debug_map_log("get", cap_val, len_val, head_val)?;

        let current_slot = unsafe {
            LLVMBuildAlloca(
                self.builder,
                bucket_head_llvm_ty,
                CString::new("map.curr")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, head_val, current_slot) };

        let input_key_val = match key_type {
            Type::Named(_) => None,
            _ => Some(self.codegen_value(key, func, local_map)?),
        };
        let input_hash = match key_type {
            Type::Named(_) => {
                Some(self.codegen_map_hash(key_type, key, func, local_map, functions, mir_module)?)
            }
            _ => None,
        };

        let check_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.get.check")?.as_ptr(),
            )
        };
        let body_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.get.body")?.as_ptr(),
            )
        };
        let next_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.get.next")?.as_ptr(),
            )
        };
        let found_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.get.found")?.as_ptr(),
            )
        };

        unsafe { LLVMBuildBr(self.builder, check_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, check_bb) };
        let current_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_head_llvm_ty,
                current_slot,
                CString::new("map.curr.load")?.as_ptr(),
            )
        };
        let null_bucket = unsafe { LLVMConstNull(bucket_head_llvm_ty) };
        let is_null = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                current_val,
                null_bucket,
                CString::new("map.curr.null")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, is_null, none_bb, body_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, body_bb) };
        let llvm_bucket_ty = self.get_struct_type(&bucket_name)?;
        let key_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                current_val,
                0,
                CString::new("map.bucket.key")?.as_ptr(),
            )
        };
        let key_match = match key_type {
            Type::Named(_) => {
                let bucket_hash =
                    self.codegen_map_hash_from_ptr(key_type, key_ptr, functions, mir_module)?;
                self.debug_map_hash_compare(
                    "get.hash",
                    input_hash.expect("hash for named key"),
                    bucket_hash,
                )?;
                unsafe {
                    LLVMBuildICmp(
                        self.builder,
                        llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                        bucket_hash,
                        input_hash.expect("hash for named key"),
                        CString::new("map.key.eq")?.as_ptr(),
                    )
                }
            }
            _ => {
                let key_llvm_ty = self.get_llvm_type(key_type)?;
                let bucket_key_val = unsafe {
                    LLVMBuildLoad2(
                        self.builder,
                        key_llvm_ty,
                        key_ptr,
                        CString::new("map.key.load")?.as_ptr(),
                    )
                };
                if matches!(key_type, Type::String | Type::Str) {
                    let input_val = input_key_val.expect("key value");
                    let input_len = self.codegen_string_len_value(input_val, functions)?;
                    let bucket_len = self.codegen_string_len_value(bucket_key_val, functions)?;
                    let eq_val = self.codegen_string_eq(bucket_key_val, input_val, functions)?;
                    let label = format!("get.cmp.{}", self.type_key(key_type));
                    self.debug_map_key_compare(
                        &label,
                        input_val,
                        bucket_key_val,
                        input_len,
                        bucket_len,
                        eq_val,
                    )?;
                    eq_val
                } else {
                    unsafe {
                        LLVMBuildICmp(
                            self.builder,
                            llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                            bucket_key_val,
                            input_key_val.expect("key value"),
                            CString::new("map.key.eq")?.as_ptr(),
                        )
                    }
                }
            }
        };
        unsafe { LLVMBuildCondBr(self.builder, key_match, found_bb, next_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, next_bb) };
        let next_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                current_val,
                2,
                CString::new("map.bucket.next")?.as_ptr(),
            )
        };
        let next_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_head_llvm_ty,
                next_ptr,
                CString::new("map.bucket.next.load")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, next_val, current_slot) };
        unsafe { LLVMBuildBr(self.builder, check_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, found_bb) };
        let value_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                current_val,
                1,
                CString::new("map.bucket.val")?.as_ptr(),
            )
        };
        let value_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                self.get_llvm_type(value_type)?,
                value_ptr,
                CString::new("map.bucket.val.load")?.as_ptr(),
            )
        };
        if std::env::var("GLYPH_DEBUG_MAP").is_ok() {
            if matches!(value_type, Type::Enum(name) if name == "JsonValue") {
                self.debug_map_json_tag("get.val", value_val)?;
            }
        }
        let some_val = self.codegen_option_some(value_type, value_val, mir_module)?;
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, done_bb) };
        let option_name = format!("Option${}", self.type_key(value_type));
        let llvm_option_ty = self.get_enum_type(&option_name)?;
        let phi = unsafe {
            let phi_name = CString::new("map.get.result")?;
            let phi_node = LLVMBuildPhi(self.builder, llvm_option_ty, phi_name.as_ptr());
            let mut incoming_vals = vec![none_val, some_val];
            let mut incoming_bbs = vec![none_bb, found_bb];
            LLVMAddIncoming(
                phi_node,
                incoming_vals.as_mut_ptr(),
                incoming_bbs.as_mut_ptr(),
                incoming_vals.len() as u32,
            );
            phi_node
        };
        if std::env::var("GLYPH_DEBUG_MAP").is_ok() {
            let tmp = unsafe {
                LLVMBuildAlloca(
                    self.builder,
                    llvm_option_ty,
                    CString::new("map.get.opt.tmp")?.as_ptr(),
                )
            };
            unsafe { LLVMBuildStore(self.builder, phi, tmp) };
            let tag_ptr = unsafe {
                LLVMBuildStructGEP2(
                    self.builder,
                    llvm_option_ty,
                    tmp,
                    0,
                    CString::new("map.get.tag")?.as_ptr(),
                )
            };
            let tag_val = unsafe {
                LLVMBuildLoad2(
                    self.builder,
                    LLVMInt32TypeInContext(self.context),
                    tag_ptr,
                    CString::new("map.get.tag.load")?.as_ptr(),
                )
            };
            self.debug_map_tag("get.opt", tag_val)?;
        }
        self.debug_log(&format!(
            "codegen_map_get done: key={:?} value={:?}",
            key_type, value_type
        ));
        Ok(phi)
    }

    pub(super) fn codegen_map_has(
        &mut self,
        map: LocalId,
        key_type: &Type,
        key: &MirValue,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        functions: &HashMap<String, LLVMValueRef>,
        mir_module: &MirModule,
    ) -> Result<LLVMValueRef> {
        self.debug_log(&format!("codegen_map_has start: key={:?}", key_type));
        let (struct_name, map_ptr) = self.struct_pointer_for_local(map, func, local_map)?;
        // Ensure bucket type is fully registered before any GEP/load operations
        self.ensure_map_bucket_type_from_map(&struct_name, key_type)?;
        let (llvm_map_ty, usize_ty) = self
            .get_map_layout(&struct_name)
            .ok_or_else(|| anyhow!("missing map layout for {}", struct_name))?;
        let buckets_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                map_ptr,
                0,
                CString::new("map.buckets").unwrap().as_ptr(),
            )
        };
        let cap_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                map_ptr,
                1,
                CString::new("map.cap").unwrap().as_ptr(),
            )
        };
        let cap_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                cap_ptr,
                CString::new("map.cap.load")?.as_ptr(),
            )
        };
        let zero = unsafe { LLVMConstInt(usize_ty, 0, 0) };
        let cap_is_zero = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                cap_val,
                zero,
                CString::new("map.cap.zero")?.as_ptr(),
            )
        };

        let parent_bb = unsafe { LLVMGetInsertBlock(self.builder) };
        let parent_fn = unsafe { LLVMGetBasicBlockParent(parent_bb) };
        let false_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.has.false")?.as_ptr(),
            )
        };
        let cont_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.has.cont")?.as_ptr(),
            )
        };
        let done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.has.done")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, cap_is_zero, false_bb, cont_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, false_bb) };
        let false_val = unsafe { LLVMConstInt(LLVMInt1TypeInContext(self.context), 0, 0) };
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, cont_bb) };
        let bucket_index = self
            .codegen_map_bucket_index(map, key_type, key, func, local_map, functions, mir_module)?;

        let bucket_name = self.map_bucket_name_from_map(&struct_name)?;
        let bucket_head_ty = Type::RawPtr(Box::new(Type::Named(bucket_name.clone())));
        let bucket_head_llvm_ty = self.get_llvm_type(&bucket_head_ty)?;
        let bucket_array_ty = Type::RawPtr(Box::new(bucket_head_ty.clone()));
        let bucket_array_llvm_ty = self.get_llvm_type(&bucket_array_ty)?;

        let bucket_array = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_array_llvm_ty,
                buckets_ptr,
                CString::new("map.buckets.load")?.as_ptr(),
            )
        };
        let bucket_slot_ptr = unsafe {
            LLVMBuildGEP2(
                self.builder,
                bucket_head_llvm_ty,
                bucket_array,
                vec![bucket_index].as_mut_ptr(),
                1,
                CString::new("map.bucket.slot")?.as_ptr(),
            )
        };
        let head_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_head_llvm_ty,
                bucket_slot_ptr,
                CString::new("map.bucket.head")?.as_ptr(),
            )
        };

        let current_slot = unsafe {
            LLVMBuildAlloca(
                self.builder,
                bucket_head_llvm_ty,
                CString::new("map.curr")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, head_val, current_slot) };

        let input_key_val = match key_type {
            Type::Named(_) => None,
            _ => Some(self.codegen_value(key, func, local_map)?),
        };
        let input_hash = match key_type {
            Type::Named(_) => {
                Some(self.codegen_map_hash(key_type, key, func, local_map, functions, mir_module)?)
            }
            _ => None,
        };

        let check_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.has.check")?.as_ptr(),
            )
        };
        let body_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.has.body")?.as_ptr(),
            )
        };
        let next_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.has.next")?.as_ptr(),
            )
        };
        let found_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.has.found")?.as_ptr(),
            )
        };

        unsafe { LLVMBuildBr(self.builder, check_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, check_bb) };
        let current_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_head_llvm_ty,
                current_slot,
                CString::new("map.curr.load")?.as_ptr(),
            )
        };
        let null_bucket = unsafe { LLVMConstNull(bucket_head_llvm_ty) };
        let is_null = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                current_val,
                null_bucket,
                CString::new("map.curr.null")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, is_null, false_bb, body_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, body_bb) };
        let llvm_bucket_ty = self.get_struct_type(&bucket_name)?;
        let key_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                current_val,
                0,
                CString::new("map.bucket.key")?.as_ptr(),
            )
        };
        let key_match = match key_type {
            Type::Named(_) => {
                let bucket_hash =
                    self.codegen_map_hash_from_ptr(key_type, key_ptr, functions, mir_module)?;
                unsafe {
                    LLVMBuildICmp(
                        self.builder,
                        llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                        bucket_hash,
                        input_hash.expect("hash for named key"),
                        CString::new("map.key.eq")?.as_ptr(),
                    )
                }
            }
            _ => {
                let key_llvm_ty = self.get_llvm_type(key_type)?;
                let bucket_key_val = unsafe {
                    LLVMBuildLoad2(
                        self.builder,
                        key_llvm_ty,
                        key_ptr,
                        CString::new("map.key.load")?.as_ptr(),
                    )
                };
                if matches!(key_type, Type::String | Type::Str) {
                    self.codegen_string_eq(
                        bucket_key_val,
                        input_key_val.expect("key value"),
                        functions,
                    )?
                } else {
                    unsafe {
                        LLVMBuildICmp(
                            self.builder,
                            llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                            bucket_key_val,
                            input_key_val.expect("key value"),
                            CString::new("map.key.eq")?.as_ptr(),
                        )
                    }
                }
            }
        };
        unsafe { LLVMBuildCondBr(self.builder, key_match, found_bb, next_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, next_bb) };
        let next_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                current_val,
                2,
                CString::new("map.bucket.next")?.as_ptr(),
            )
        };
        let next_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_head_llvm_ty,
                next_ptr,
                CString::new("map.bucket.next.load")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, next_val, current_slot) };
        unsafe { LLVMBuildBr(self.builder, check_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, found_bb) };
        let true_val = unsafe { LLVMConstInt(LLVMInt1TypeInContext(self.context), 1, 0) };
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, done_bb) };
        let phi = unsafe {
            let phi_name = CString::new("map.has.result")?;
            let phi_node = LLVMBuildPhi(
                self.builder,
                LLVMInt1TypeInContext(self.context),
                phi_name.as_ptr(),
            );
            let mut incoming_vals = vec![false_val, true_val];
            let mut incoming_bbs = vec![false_bb, found_bb];
            LLVMAddIncoming(
                phi_node,
                incoming_vals.as_mut_ptr(),
                incoming_bbs.as_mut_ptr(),
                incoming_vals.len() as u32,
            );
            phi_node
        };
        self.debug_log(&format!("codegen_map_has done: key={:?}", key_type));
        Ok(phi)
    }

    pub(super) fn codegen_map_keys(
        &mut self,
        map: LocalId,
        key_type: &Type,
        value_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        _functions: &HashMap<String, LLVMValueRef>,
        _mir_module: &MirModule,
    ) -> Result<LLVMValueRef> {
        self.debug_log(&format!(
            "codegen_map_keys start: key={:?} value={:?}",
            key_type, value_type
        ));
        // Ensure bucket type is fully registered before any GEP/load operations
        self.ensure_map_bucket_type(key_type, value_type)?;

        let (struct_name, map_ptr) = self.struct_pointer_for_local(map, func, local_map)?;
        let (llvm_map_ty, usize_ty) = self
            .get_map_layout(&struct_name)
            .ok_or_else(|| anyhow!("missing map layout for {}", struct_name))?;
        let buckets_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                map_ptr,
                0,
                CString::new("map.buckets").unwrap().as_ptr(),
            )
        };
        let cap_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                map_ptr,
                1,
                CString::new("map.cap").unwrap().as_ptr(),
            )
        };
        let len_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                map_ptr,
                2,
                CString::new("map.len").unwrap().as_ptr(),
            )
        };
        let cap_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                cap_ptr,
                CString::new("map.cap.load")?.as_ptr(),
            )
        };
        let len_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                len_ptr,
                CString::new("map.len.load")?.as_ptr(),
            )
        };

        let vec_val = self.codegen_vec_init_with_capacity_value(key_type, len_val)?;
        let vec_name = format!("Vec${}", self.type_key(key_type));
        let llvm_vec_ty = self.get_struct_type(&vec_name)?;
        let vec_ptr = unsafe {
            let alloca = LLVMBuildAlloca(
                self.builder,
                llvm_vec_ty,
                CString::new("map.keys.vec")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, vec_val, alloca);
            alloca
        };

        let zero = unsafe { LLVMConstInt(usize_ty, 0, 0) };
        let len_is_zero = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                len_val,
                zero,
                CString::new("map.len.zero")?.as_ptr(),
            )
        };
        let cap_is_zero = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                cap_val,
                zero,
                CString::new("map.cap.zero")?.as_ptr(),
            )
        };
        let skip_loop = unsafe {
            LLVMBuildOr(
                self.builder,
                len_is_zero,
                cap_is_zero,
                CString::new("map.keys.skip")?.as_ptr(),
            )
        };

        let parent_bb = unsafe { LLVMGetInsertBlock(self.builder) };
        let parent_fn = unsafe { LLVMGetBasicBlockParent(parent_bb) };
        let loop_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.keys.loop")?.as_ptr(),
            )
        };
        let done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.keys.done")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, skip_loop, done_bb, loop_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, loop_bb) };
        let bucket_name = self.map_bucket_name_from_map(&struct_name)?;
        let bucket_head_ty = Type::RawPtr(Box::new(Type::Named(bucket_name.clone())));
        let bucket_head_llvm_ty = self.get_llvm_type(&bucket_head_ty)?;
        let bucket_array_ty = Type::RawPtr(Box::new(bucket_head_ty.clone()));
        let bucket_array_llvm_ty = self.get_llvm_type(&bucket_array_ty)?;
        let bucket_array = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_array_llvm_ty,
                buckets_ptr,
                CString::new("map.buckets.load")?.as_ptr(),
            )
        };

        let idx_slot = unsafe {
            LLVMBuildAlloca(
                self.builder,
                usize_ty,
                CString::new("map.keys.idx")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, zero, idx_slot) };
        let current_slot = unsafe {
            LLVMBuildAlloca(
                self.builder,
                bucket_head_llvm_ty,
                CString::new("map.keys.curr")?.as_ptr(),
            )
        };

        let outer_check_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.keys.check")?.as_ptr(),
            )
        };
        let outer_body_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.keys.body")?.as_ptr(),
            )
        };
        let outer_done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.keys.outer.done")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildBr(self.builder, outer_check_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, outer_check_bb) };
        let idx_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                idx_slot,
                CString::new("map.keys.idx.load")?.as_ptr(),
            )
        };
        let idx_in_bounds = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntULT,
                idx_val,
                cap_val,
                CString::new("map.keys.idx.lt")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, idx_in_bounds, outer_body_bb, outer_done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, outer_body_bb) };
        let bucket_slot_ptr = unsafe {
            LLVMBuildGEP2(
                self.builder,
                bucket_head_llvm_ty,
                bucket_array,
                vec![idx_val].as_mut_ptr(),
                1,
                CString::new("map.bucket.slot")?.as_ptr(),
            )
        };
        let head_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_head_llvm_ty,
                bucket_slot_ptr,
                CString::new("map.bucket.head")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, head_val, current_slot) };

        let inner_check_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.keys.inner.check")?.as_ptr(),
            )
        };
        let inner_body_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.keys.inner.body")?.as_ptr(),
            )
        };
        let inner_done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.keys.inner.done")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildBr(self.builder, inner_check_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, inner_check_bb) };
        let current_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_head_llvm_ty,
                current_slot,
                CString::new("map.keys.curr.load")?.as_ptr(),
            )
        };
        let null_bucket = unsafe { LLVMConstNull(bucket_head_llvm_ty) };
        let inner_done = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                current_val,
                null_bucket,
                CString::new("map.keys.curr.null")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, inner_done, inner_done_bb, inner_body_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, inner_body_bb) };
        let llvm_bucket_ty = self.get_struct_type(&bucket_name)?;
        let key_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                current_val,
                0,
                CString::new("map.bucket.key")?.as_ptr(),
            )
        };
        let key_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                self.get_llvm_type(key_type)?,
                key_ptr,
                CString::new("map.bucket.key.load")?.as_ptr(),
            )
        };
        self.codegen_vec_push_no_grow(vec_ptr, key_type, key_val)?;
        let next_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                current_val,
                2,
                CString::new("map.bucket.next")?.as_ptr(),
            )
        };
        let next_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_head_llvm_ty,
                next_ptr,
                CString::new("map.bucket.next.load")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, next_val, current_slot) };
        unsafe { LLVMBuildBr(self.builder, inner_check_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, inner_done_bb) };
        let new_idx = unsafe {
            LLVMBuildAdd(
                self.builder,
                idx_val,
                LLVMConstInt(usize_ty, 1, 0),
                CString::new("map.keys.idx.inc")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, new_idx, idx_slot) };
        unsafe { LLVMBuildBr(self.builder, outer_check_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, outer_done_bb) };
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, done_bb) };
        let vec_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                llvm_vec_ty,
                vec_ptr,
                CString::new("map.keys.load")?.as_ptr(),
            )
        };
        self.debug_log(&format!(
            "codegen_map_keys done: key={:?} value={:?}",
            key_type, value_type
        ));
        Ok(vec_val)
    }

    pub(super) fn codegen_map_vals(
        &mut self,
        map: LocalId,
        key_type: &Type,
        value_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        _functions: &HashMap<String, LLVMValueRef>,
        _mir_module: &MirModule,
    ) -> Result<LLVMValueRef> {
        self.debug_log(&format!(
            "codegen_map_vals start: key={:?} value={:?}",
            key_type, value_type
        ));
        // Ensure bucket type is fully registered before any GEP/load operations
        self.ensure_map_bucket_type(key_type, value_type)?;

        let (struct_name, map_ptr) = self.struct_pointer_for_local(map, func, local_map)?;
        let (llvm_map_ty, usize_ty) = self
            .get_map_layout(&struct_name)
            .ok_or_else(|| anyhow!("missing map layout for {}", struct_name))?;
        let buckets_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                map_ptr,
                0,
                CString::new("map.buckets").unwrap().as_ptr(),
            )
        };
        let cap_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                map_ptr,
                1,
                CString::new("map.cap").unwrap().as_ptr(),
            )
        };
        let len_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                map_ptr,
                2,
                CString::new("map.len").unwrap().as_ptr(),
            )
        };
        let cap_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                cap_ptr,
                CString::new("map.cap.load")?.as_ptr(),
            )
        };
        let len_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                len_ptr,
                CString::new("map.len.load")?.as_ptr(),
            )
        };

        let vec_val = self.codegen_vec_init_with_capacity_value(value_type, len_val)?;
        let vec_name = format!("Vec${}", self.type_key(value_type));
        let llvm_vec_ty = self.get_struct_type(&vec_name)?;
        let vec_ptr = unsafe {
            let alloca = LLVMBuildAlloca(
                self.builder,
                llvm_vec_ty,
                CString::new("map.vals.vec")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, vec_val, alloca);
            alloca
        };

        let zero = unsafe { LLVMConstInt(usize_ty, 0, 0) };
        let len_is_zero = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                len_val,
                zero,
                CString::new("map.len.zero")?.as_ptr(),
            )
        };
        let cap_is_zero = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                cap_val,
                zero,
                CString::new("map.cap.zero")?.as_ptr(),
            )
        };
        let skip_loop = unsafe {
            LLVMBuildOr(
                self.builder,
                len_is_zero,
                cap_is_zero,
                CString::new("map.vals.skip")?.as_ptr(),
            )
        };

        let parent_bb = unsafe { LLVMGetInsertBlock(self.builder) };
        let parent_fn = unsafe { LLVMGetBasicBlockParent(parent_bb) };
        let loop_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.vals.loop")?.as_ptr(),
            )
        };
        let done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.vals.done")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, skip_loop, done_bb, loop_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, loop_bb) };
        let bucket_name = self.map_bucket_name_from_map(&struct_name)?;
        let bucket_head_ty = Type::RawPtr(Box::new(Type::Named(bucket_name.clone())));
        let bucket_head_llvm_ty = self.get_llvm_type(&bucket_head_ty)?;
        let bucket_array_ty = Type::RawPtr(Box::new(bucket_head_ty.clone()));
        let bucket_array_llvm_ty = self.get_llvm_type(&bucket_array_ty)?;
        let bucket_array = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_array_llvm_ty,
                buckets_ptr,
                CString::new("map.buckets.load")?.as_ptr(),
            )
        };

        let idx_slot = unsafe {
            LLVMBuildAlloca(
                self.builder,
                usize_ty,
                CString::new("map.vals.idx")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, zero, idx_slot) };
        let current_slot = unsafe {
            LLVMBuildAlloca(
                self.builder,
                bucket_head_llvm_ty,
                CString::new("map.vals.curr")?.as_ptr(),
            )
        };

        let outer_check_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.vals.check")?.as_ptr(),
            )
        };
        let outer_body_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.vals.body")?.as_ptr(),
            )
        };
        let outer_done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.vals.outer.done")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildBr(self.builder, outer_check_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, outer_check_bb) };
        let idx_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                idx_slot,
                CString::new("map.vals.idx.load")?.as_ptr(),
            )
        };
        let idx_in_bounds = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntULT,
                idx_val,
                cap_val,
                CString::new("map.vals.idx.lt")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, idx_in_bounds, outer_body_bb, outer_done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, outer_body_bb) };
        let bucket_slot_ptr = unsafe {
            LLVMBuildGEP2(
                self.builder,
                bucket_head_llvm_ty,
                bucket_array,
                vec![idx_val].as_mut_ptr(),
                1,
                CString::new("map.bucket.slot")?.as_ptr(),
            )
        };
        let head_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_head_llvm_ty,
                bucket_slot_ptr,
                CString::new("map.bucket.head")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, head_val, current_slot) };

        let inner_check_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.vals.inner.check")?.as_ptr(),
            )
        };
        let inner_body_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.vals.inner.body")?.as_ptr(),
            )
        };
        let inner_done_bb = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("map.vals.inner.done")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildBr(self.builder, inner_check_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, inner_check_bb) };
        let current_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_head_llvm_ty,
                current_slot,
                CString::new("map.vals.curr.load")?.as_ptr(),
            )
        };
        let null_bucket = unsafe { LLVMConstNull(bucket_head_llvm_ty) };
        let inner_done = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                current_val,
                null_bucket,
                CString::new("map.vals.curr.null")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, inner_done, inner_done_bb, inner_body_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, inner_body_bb) };
        let llvm_bucket_ty = self.get_struct_type(&bucket_name)?;
        let val_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                current_val,
                1,
                CString::new("map.bucket.val")?.as_ptr(),
            )
        };
        let val_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                self.get_llvm_type(value_type)?,
                val_ptr,
                CString::new("map.bucket.val.load")?.as_ptr(),
            )
        };
        self.codegen_vec_push_no_grow(vec_ptr, value_type, val_val)?;
        let next_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                current_val,
                2,
                CString::new("map.bucket.next")?.as_ptr(),
            )
        };
        let next_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                bucket_head_llvm_ty,
                next_ptr,
                CString::new("map.bucket.next.load")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, next_val, current_slot) };
        unsafe { LLVMBuildBr(self.builder, inner_check_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, inner_done_bb) };
        let new_idx = unsafe {
            LLVMBuildAdd(
                self.builder,
                idx_val,
                LLVMConstInt(usize_ty, 1, 0),
                CString::new("map.vals.idx.inc")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, new_idx, idx_slot) };
        unsafe { LLVMBuildBr(self.builder, outer_check_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, outer_done_bb) };
        unsafe { LLVMBuildBr(self.builder, done_bb) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, done_bb) };
        let vec_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                llvm_vec_ty,
                vec_ptr,
                CString::new("map.vals.load")?.as_ptr(),
            )
        };
        self.debug_log(&format!(
            "codegen_map_vals done: key={:?} value={:?}",
            key_type, value_type
        ));
        Ok(vec_val)
    }

    pub(super) fn map_key_value_types_from_map(
        &self,
        map_struct_name: &str,
    ) -> Result<(Type, Type)> {
        let bucket_name = self.map_bucket_name_from_map(map_struct_name)?;
        let bucket_layout = self
            .struct_layouts
            .get(&bucket_name)
            .ok_or_else(|| anyhow!("missing bucket layout for {}", bucket_name))?;
        let key_type = bucket_layout
            .fields
            .first()
            .map(|(_, ty)| ty.clone())
            .ok_or_else(|| anyhow!("bucket layout missing key field: {}", bucket_name))?;
        let value_type = bucket_layout
            .fields
            .get(1)
            .map(|(_, ty)| ty.clone())
            .ok_or_else(|| anyhow!("bucket layout missing value field: {}", bucket_name))?;
        Ok((key_type, value_type))
    }

    pub(super) fn codegen_drop_map_from_ptr(
        &mut self,
        map_ptr: LLVMValueRef,
        struct_name: &str,
        key_type: &Type,
        value_type: &Type,
    ) -> Result<()> {
        let guard_key = format!("map:{}", struct_name);
        if !self.drop_in_progress.insert(guard_key.clone()) {
            return Ok(());
        }

        let result = (|| {
            self.ensure_map_bucket_type(key_type, value_type)?;

            let (llvm_map_ty, usize_ty) = self
                .get_map_layout(struct_name)
                .ok_or_else(|| anyhow!("missing map layout for {}", struct_name))?;
            let buckets_ptr = unsafe {
                LLVMBuildStructGEP2(
                    self.builder,
                    llvm_map_ty,
                    map_ptr,
                    0,
                    CString::new("map.drop.buckets")?.as_ptr(),
                )
            };
            let cap_ptr = unsafe {
                LLVMBuildStructGEP2(
                    self.builder,
                    llvm_map_ty,
                    map_ptr,
                    1,
                    CString::new("map.drop.cap")?.as_ptr(),
                )
            };
            let len_ptr = unsafe {
                LLVMBuildStructGEP2(
                    self.builder,
                    llvm_map_ty,
                    map_ptr,
                    2,
                    CString::new("map.drop.len")?.as_ptr(),
                )
            };
            let cap_val = unsafe {
                LLVMBuildLoad2(
                    self.builder,
                    usize_ty,
                    cap_ptr,
                    CString::new("map.drop.cap.load")?.as_ptr(),
                )
            };
            let zero = unsafe { LLVMConstInt(usize_ty, 0, 0) };
            let has_buckets = unsafe {
                LLVMBuildICmp(
                    self.builder,
                    llvm_sys::LLVMIntPredicate::LLVMIntUGT,
                    cap_val,
                    zero,
                    CString::new("map.drop.has_buckets")?.as_ptr(),
                )
            };

            let parent_bb = unsafe { LLVMGetInsertBlock(self.builder) };
            if parent_bb.is_null() {
                bail!("builder not positioned in block for map drop");
            }
            let parent_fn = unsafe { LLVMGetBasicBlockParent(parent_bb) };
            if parent_fn.is_null() {
                bail!("map drop parent function missing");
            }
            let drop_bb = unsafe {
                LLVMAppendBasicBlockInContext(
                    self.context,
                    parent_fn,
                    CString::new("map.drop.body")?.as_ptr(),
                )
            };
            let done_bb = unsafe {
                LLVMAppendBasicBlockInContext(
                    self.context,
                    parent_fn,
                    CString::new("map.drop.done")?.as_ptr(),
                )
            };
            unsafe { LLVMBuildCondBr(self.builder, has_buckets, drop_bb, done_bb) };

            unsafe { LLVMPositionBuilderAtEnd(self.builder, drop_bb) };
            let bucket_name = self.map_bucket_name_from_map(struct_name)?;
            let bucket_head_ty = Type::RawPtr(Box::new(Type::Named(bucket_name.clone())));
            let bucket_head_llvm_ty = self.get_llvm_type(&bucket_head_ty)?;
            let bucket_array_ty = Type::RawPtr(Box::new(bucket_head_ty.clone()));
            let bucket_array_llvm_ty = self.get_llvm_type(&bucket_array_ty)?;
            let bucket_array = unsafe {
                LLVMBuildLoad2(
                    self.builder,
                    bucket_array_llvm_ty,
                    buckets_ptr,
                    CString::new("map.drop.buckets.load")?.as_ptr(),
                )
            };

            let idx_slot = unsafe {
                LLVMBuildAlloca(
                    self.builder,
                    usize_ty,
                    CString::new("map.drop.idx")?.as_ptr(),
                )
            };
            unsafe { LLVMBuildStore(self.builder, zero, idx_slot) };

            let current_slot = unsafe {
                LLVMBuildAlloca(
                    self.builder,
                    bucket_head_llvm_ty,
                    CString::new("map.drop.curr")?.as_ptr(),
                )
            };
            unsafe {
                LLVMBuildStore(
                    self.builder,
                    LLVMConstNull(bucket_head_llvm_ty),
                    current_slot,
                )
            };

            let outer_check_bb = unsafe {
                LLVMAppendBasicBlockInContext(
                    self.context,
                    parent_fn,
                    CString::new("map.drop.outer.check")?.as_ptr(),
                )
            };
            let outer_body_bb = unsafe {
                LLVMAppendBasicBlockInContext(
                    self.context,
                    parent_fn,
                    CString::new("map.drop.outer.body")?.as_ptr(),
                )
            };
            let outer_done_bb = unsafe {
                LLVMAppendBasicBlockInContext(
                    self.context,
                    parent_fn,
                    CString::new("map.drop.outer.done")?.as_ptr(),
                )
            };
            let inner_check_bb = unsafe {
                LLVMAppendBasicBlockInContext(
                    self.context,
                    parent_fn,
                    CString::new("map.drop.inner.check")?.as_ptr(),
                )
            };
            let inner_body_bb = unsafe {
                LLVMAppendBasicBlockInContext(
                    self.context,
                    parent_fn,
                    CString::new("map.drop.inner.body")?.as_ptr(),
                )
            };
            let inner_done_bb = unsafe {
                LLVMAppendBasicBlockInContext(
                    self.context,
                    parent_fn,
                    CString::new("map.drop.inner.done")?.as_ptr(),
                )
            };

            unsafe { LLVMBuildBr(self.builder, outer_check_bb) };

            unsafe { LLVMPositionBuilderAtEnd(self.builder, outer_check_bb) };
            let idx_val = unsafe {
                LLVMBuildLoad2(
                    self.builder,
                    usize_ty,
                    idx_slot,
                    CString::new("map.drop.idx.load")?.as_ptr(),
                )
            };
            let idx_in_bounds = unsafe {
                LLVMBuildICmp(
                    self.builder,
                    llvm_sys::LLVMIntPredicate::LLVMIntULT,
                    idx_val,
                    cap_val,
                    CString::new("map.drop.idx.lt")?.as_ptr(),
                )
            };
            unsafe { LLVMBuildCondBr(self.builder, idx_in_bounds, outer_body_bb, outer_done_bb) };

            unsafe { LLVMPositionBuilderAtEnd(self.builder, outer_body_bb) };
            let bucket_slot_ptr = unsafe {
                LLVMBuildGEP2(
                    self.builder,
                    bucket_head_llvm_ty,
                    bucket_array,
                    vec![idx_val].as_mut_ptr(),
                    1,
                    CString::new("map.drop.bucket.slot")?.as_ptr(),
                )
            };
            let head_val = unsafe {
                LLVMBuildLoad2(
                    self.builder,
                    bucket_head_llvm_ty,
                    bucket_slot_ptr,
                    CString::new("map.drop.bucket.head")?.as_ptr(),
                )
            };
            unsafe { LLVMBuildStore(self.builder, head_val, current_slot) };
            unsafe { LLVMBuildBr(self.builder, inner_check_bb) };

            unsafe { LLVMPositionBuilderAtEnd(self.builder, inner_check_bb) };
            let current_val = unsafe {
                LLVMBuildLoad2(
                    self.builder,
                    bucket_head_llvm_ty,
                    current_slot,
                    CString::new("map.drop.curr.load")?.as_ptr(),
                )
            };
            let is_null = unsafe {
                LLVMBuildICmp(
                    self.builder,
                    llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                    current_val,
                    LLVMConstNull(bucket_head_llvm_ty),
                    CString::new("map.drop.curr.null")?.as_ptr(),
                )
            };
            unsafe { LLVMBuildCondBr(self.builder, is_null, inner_done_bb, inner_body_bb) };

            unsafe { LLVMPositionBuilderAtEnd(self.builder, inner_body_bb) };
            let llvm_bucket_ty = self.get_struct_type(&bucket_name)?;
            let key_ptr = unsafe {
                LLVMBuildStructGEP2(
                    self.builder,
                    llvm_bucket_ty,
                    current_val,
                    0,
                    CString::new("map.drop.key")?.as_ptr(),
                )
            };
            let value_ptr = unsafe {
                LLVMBuildStructGEP2(
                    self.builder,
                    llvm_bucket_ty,
                    current_val,
                    1,
                    CString::new("map.drop.value")?.as_ptr(),
                )
            };
            let next_ptr = unsafe {
                LLVMBuildStructGEP2(
                    self.builder,
                    llvm_bucket_ty,
                    current_val,
                    2,
                    CString::new("map.drop.next")?.as_ptr(),
                )
            };
            let next_val = unsafe {
                LLVMBuildLoad2(
                    self.builder,
                    bucket_head_llvm_ty,
                    next_ptr,
                    CString::new("map.drop.next.load")?.as_ptr(),
                )
            };
            self.codegen_drop_elem_slot(key_ptr, key_type)?;
            self.codegen_drop_elem_slot(value_ptr, value_type)?;
            self.codegen_free(current_val)?;
            unsafe { LLVMBuildStore(self.builder, next_val, current_slot) };
            unsafe { LLVMBuildBr(self.builder, inner_check_bb) };

            unsafe { LLVMPositionBuilderAtEnd(self.builder, inner_done_bb) };
            let next_idx = unsafe {
                LLVMBuildAdd(
                    self.builder,
                    idx_val,
                    LLVMConstInt(usize_ty, 1, 0),
                    CString::new("map.drop.idx.next")?.as_ptr(),
                )
            };
            unsafe { LLVMBuildStore(self.builder, next_idx, idx_slot) };
            unsafe { LLVMBuildBr(self.builder, outer_check_bb) };

            unsafe { LLVMPositionBuilderAtEnd(self.builder, outer_done_bb) };
            self.codegen_free(bucket_array)?;
            unsafe {
                LLVMBuildStore(
                    self.builder,
                    LLVMConstNull(bucket_array_llvm_ty),
                    buckets_ptr,
                )
            };
            unsafe { LLVMBuildStore(self.builder, zero, cap_ptr) };
            unsafe { LLVMBuildStore(self.builder, zero, len_ptr) };
            unsafe { LLVMBuildBr(self.builder, done_bb) };

            unsafe { LLVMPositionBuilderAtEnd(self.builder, done_bb) };
            Ok(())
        })();

        self.drop_in_progress.remove(&guard_key);
        result
    }

    pub(super) fn codegen_drop_map(
        &mut self,
        map: LocalId,
        key_type: &Type,
        value_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<()> {
        let (struct_name, map_ptr) = self.struct_pointer_for_local(map, func, local_map)?;
        self.codegen_drop_map_from_ptr(map_ptr, &struct_name, key_type, value_type)
    }
}
