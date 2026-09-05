use super::*;

impl CodegenContext {
    pub(super) fn cast_int_to_u64(
        &mut self,
        val: LLVMValueRef,
        signed: bool,
    ) -> Result<LLVMValueRef> {
        let u64_ty = unsafe { LLVMInt64TypeInContext(self.context) };
        unsafe {
            if LLVMTypeOf(val) == u64_ty {
                Ok(val)
            } else {
                let name = CString::new("hash.cast")?;
                let cast = if signed {
                    LLVMBuildSExt(self.builder, val, u64_ty, name.as_ptr())
                } else {
                    LLVMBuildZExt(self.builder, val, u64_ty, name.as_ptr())
                };
                Ok(cast)
            }
        }
    }

    pub(super) fn codegen_map_hash(
        &mut self,
        key_type: &Type,
        key: &MirValue,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        functions: &HashMap<String, LLVMValueRef>,
        mir_module: &MirModule,
    ) -> Result<LLVMValueRef> {
        match key_type {
            Type::I8 | Type::I16 | Type::I32 | Type::I64 => {
                let val = self.codegen_value(key, func, local_map)?;
                self.cast_int_to_u64(val, true)
            }
            Type::U8
            | Type::U16
            | Type::U32
            | Type::U64
            | Type::Usize
            | Type::Bool
            | Type::Char => {
                let val = self.codegen_value(key, func, local_map)?;
                self.cast_int_to_u64(val, false)
            }
            Type::String | Type::Str => {
                let val = self.codegen_value(key, func, local_map)?;
                let len = self.codegen_string_len_value(val, functions)?;
                self.codegen_hash_bytes(val, len)
            }
            Type::Ref(_, _) | Type::RawPtr(_) | Type::Own(_) | Type::Shared(_) => {
                let val = self.codegen_value(key, func, local_map)?;
                let u64_ty = unsafe { LLVMInt64TypeInContext(self.context) };
                let name = CString::new("hash.ptr")?;
                Ok(unsafe { LLVMBuildPtrToInt(self.builder, val, u64_ty, name.as_ptr()) })
            }
            Type::Named(name) => {
                let hash_name = format!("{}::Hash::hash", name);
                let callee = functions
                    .get(&hash_name)
                    .copied()
                    .ok_or_else(|| anyhow!("missing hash method {}", hash_name))?;

                let fn_ty = if let Some(target_func) =
                    mir_module.functions.iter().find(|f| f.name == hash_name)
                {
                    self.llvm_function_type(target_func)?.0
                } else {
                    bail!("missing hash function {}", hash_name);
                };

                let arg_ptr = match key {
                    MirValue::Local(local_id) => *local_map
                        .get(local_id)
                        .ok_or_else(|| anyhow!("missing local for hash {:?}", local_id))?,
                    _ => {
                        let value = self.codegen_value(key, func, local_map)?;
                        let elem_ty = self.get_llvm_type(key_type)?;
                        let tmp = unsafe {
                            self.build_entry_alloca(
                                self.builder,
                                elem_ty,
                                CString::new("hash.key.tmp")?.as_ptr(),
                            )
                        };
                        unsafe { LLVMBuildStore(self.builder, value, tmp) };
                        tmp
                    }
                };

                let mut args = vec![arg_ptr];
                let call_name = CString::new("hash.call")?;
                Ok(unsafe {
                    LLVMBuildCall2(
                        self.builder,
                        fn_ty,
                        callee,
                        args.as_mut_ptr(),
                        args.len() as u32,
                        call_name.as_ptr(),
                    )
                })
            }
            _ => bail!("unsupported Map key type for hashing: {:?}", key_type),
        }
    }

    pub(super) fn codegen_map_hash_from_ptr(
        &mut self,
        key_type: &Type,
        key_ptr: LLVMValueRef,
        functions: &HashMap<String, LLVMValueRef>,
        mir_module: &MirModule,
    ) -> Result<LLVMValueRef> {
        match key_type {
            Type::I8 | Type::I16 | Type::I32 | Type::I64 => {
                let llvm_ty = self.get_llvm_type(key_type)?;
                let val = unsafe {
                    LLVMBuildLoad2(
                        self.builder,
                        llvm_ty,
                        key_ptr,
                        CString::new("hash.load")?.as_ptr(),
                    )
                };
                self.cast_int_to_u64(val, true)
            }
            Type::U8
            | Type::U16
            | Type::U32
            | Type::U64
            | Type::Usize
            | Type::Bool
            | Type::Char => {
                let llvm_ty = self.get_llvm_type(key_type)?;
                let val = unsafe {
                    LLVMBuildLoad2(
                        self.builder,
                        llvm_ty,
                        key_ptr,
                        CString::new("hash.load")?.as_ptr(),
                    )
                };
                self.cast_int_to_u64(val, false)
            }
            Type::String | Type::Str => {
                let val = unsafe {
                    LLVMBuildLoad2(
                        self.builder,
                        self.get_llvm_type(key_type)?,
                        key_ptr,
                        CString::new("hash.str.load")?.as_ptr(),
                    )
                };
                let len = self.codegen_string_len_value(val, functions)?;
                self.codegen_hash_bytes(val, len)
            }
            Type::Ref(_, _) | Type::RawPtr(_) | Type::Own(_) | Type::Shared(_) => {
                let raw_val = unsafe {
                    LLVMBuildLoad2(
                        self.builder,
                        self.get_llvm_type(key_type)?,
                        key_ptr,
                        CString::new("hash.ptr.load")?.as_ptr(),
                    )
                };
                let u64_ty = unsafe { LLVMInt64TypeInContext(self.context) };
                let name = CString::new("hash.ptr")?;
                Ok(unsafe { LLVMBuildPtrToInt(self.builder, raw_val, u64_ty, name.as_ptr()) })
            }
            Type::Named(name) => {
                let hash_name = format!("{}::Hash::hash", name);
                let callee = functions
                    .get(&hash_name)
                    .copied()
                    .ok_or_else(|| anyhow!("missing hash method {}", hash_name))?;

                let fn_ty = if let Some(target_func) =
                    mir_module.functions.iter().find(|f| f.name == hash_name)
                {
                    self.llvm_function_type(target_func)?.0
                } else {
                    bail!("missing hash function {}", hash_name);
                };

                let mut args = vec![key_ptr];
                let call_name = CString::new("hash.call")?;
                Ok(unsafe {
                    LLVMBuildCall2(
                        self.builder,
                        fn_ty,
                        callee,
                        args.as_mut_ptr(),
                        args.len() as u32,
                        call_name.as_ptr(),
                    )
                })
            }
            _ => bail!("unsupported Map key type for hashing: {:?}", key_type),
        }
    }
}
