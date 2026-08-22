use super::*;

impl CodegenContext {
    fn callable_signature<'a>(&self, signature: &'a Type) -> Result<(&'a [Type], &'a Type)> {
        signature
            .function_signature()
            .ok_or_else(|| anyhow!("indirect call signature is not callable: {:?}", signature))
    }

    fn callable_invoke_type(&self, signature: &Type) -> Result<(LLVMTypeRef, bool)> {
        let (params, ret) = self.callable_signature(signature)?;
        let uses_sret = self.ret_uses_sret(ret)?;
        let llvm_ret = if uses_sret || Self::is_unit_type(ret) {
            unsafe { LLVMVoidTypeInContext(self.context) }
        } else {
            self.get_llvm_type(ret)?
        };

        let ptr_ty = unsafe { LLVMPointerType(LLVMInt8TypeInContext(self.context), 0) };
        let mut llvm_params = Vec::with_capacity(params.len() + 2);
        if uses_sret {
            let llvm_ret_ty = self.get_llvm_type(ret)?;
            llvm_params.push(unsafe { LLVMPointerType(llvm_ret_ty, 0) });
        }
        // The environment is the first non-ABI parameter for every callable,
        // including non-capturing function references (where it is null).
        llvm_params.push(ptr_ty);
        for param in params {
            llvm_params.push(self.get_llvm_type(param)?);
        }

        Ok((
            unsafe {
                LLVMFunctionType(
                    llvm_ret,
                    llvm_params.as_mut_ptr(),
                    llvm_params.len() as u32,
                    0,
                )
            },
            uses_sret,
        ))
    }

    fn function_signature_in_module(
        &self,
        name: &str,
        mir_module: &MirModule,
    ) -> Result<(Vec<Type>, Type)> {
        if let Some(function) = mir_module
            .functions
            .iter()
            .find(|function| function.name == name)
        {
            let params = function
                .params
                .iter()
                .map(|id| {
                    function
                        .locals
                        .get(id.0 as usize)
                        .and_then(|local| local.ty.clone())
                        .ok_or_else(|| anyhow!("function {} has an untyped parameter", name))
                })
                .collect::<Result<Vec<_>>>()?;
            return Ok((params, function.ret_type.clone().unwrap_or(Type::Void)));
        }
        if let Some(function) = mir_module
            .extern_functions
            .iter()
            .find(|function| function.name == name)
        {
            return Ok((
                function.params.clone(),
                function.ret_type.clone().unwrap_or(Type::Void),
            ));
        }
        bail!("function reference target {} not found in MIR module", name)
    }

    fn unit_compatible(lhs: &Type, rhs: &Type) -> bool {
        lhs == rhs || (Self::is_unit_type(lhs) && Self::is_unit_type(rhs))
    }

    fn ensure_function_ref_thunk(
        &mut self,
        name: &str,
        signature: &Type,
        functions: &HashMap<String, LLVMValueRef>,
        mir_module: &MirModule,
    ) -> Result<LLVMValueRef> {
        let key = format!("{}::{}", name, self.type_key(signature));
        if let Some(&thunk) = self.function_ref_thunks.get(&key) {
            return Ok(thunk);
        }

        let (params, ret) = self.callable_signature(signature)?;
        let (actual_params, actual_ret) = self.function_signature_in_module(name, mir_module)?;
        if params != actual_params || !Self::unit_compatible(ret, &actual_ret) {
            bail!(
                "function reference signature mismatch for {}: expected {:?} -> {:?}, found {:?} -> {:?}",
                name,
                params,
                ret,
                actual_params,
                actual_ret
            );
        }

        let target = functions
            .get(name)
            .copied()
            .ok_or_else(|| anyhow!("function reference target {} was not declared", name))?;
        let target_ty = self
            .function_types
            .get(name)
            .copied()
            .ok_or_else(|| anyhow!("missing LLVM function type for {}", name))?;
        let (thunk_ty, uses_sret) = self.callable_invoke_type(signature)?;
        let thunk_name = format!(
            "__glyph_fnref_{}_{}",
            self.sanitize(name),
            self.type_key(signature)
        );
        let thunk_name = CString::new(thunk_name)?;
        let thunk = unsafe { LLVMAddFunction(self.module, thunk_name.as_ptr(), thunk_ty) };
        unsafe { LLVMSetLinkage(thunk, LLVMLinkage::LLVMPrivateLinkage) };
        if uses_sret {
            self.add_sret_attribute(thunk, self.get_llvm_type(ret)?);
        }
        // Insert before building the body, so recursively encountered uses do
        // not try to create a duplicate adapter.
        self.function_ref_thunks.insert(key, thunk);

        let saved_bb = unsafe { LLVMGetInsertBlock(self.builder) };
        let entry = unsafe {
            LLVMAppendBasicBlockInContext(self.context, thunk, CString::new("entry")?.as_ptr())
        };
        unsafe { LLVMPositionBuilderAtEnd(self.builder, entry) };

        let mut target_args = Vec::with_capacity(params.len() + usize::from(uses_sret));
        let env_index = if uses_sret {
            let sret = unsafe { LLVMGetParam(thunk, 0) };
            target_args.push(sret);
            1
        } else {
            0
        };
        // env_index is intentionally skipped: named-function adapters have no
        // capture environment.
        for index in 0..params.len() {
            target_args.push(unsafe { LLVMGetParam(thunk, (env_index + 1 + index) as u32) });
        }

        let call = self.build_call2(target_ty, target, &mut target_args, "fnref.call")?;
        if uses_sret {
            self.add_sret_call_attribute(call, self.get_llvm_type(ret)?);
            unsafe { LLVMBuildRetVoid(self.builder) };
        } else if Self::is_unit_type(ret) {
            unsafe { LLVMBuildRetVoid(self.builder) };
        } else {
            unsafe { LLVMBuildRet(self.builder, call) };
        }

        if !saved_bb.is_null() {
            unsafe { LLVMPositionBuilderAtEnd(self.builder, saved_bb) };
        }
        Ok(thunk)
    }

    pub(super) fn codegen_function_ref(
        &mut self,
        name: &str,
        signature: &Type,
        functions: &HashMap<String, LLVMValueRef>,
        mir_module: &MirModule,
    ) -> Result<LLVMValueRef> {
        let thunk = self.ensure_function_ref_thunk(name, signature, functions, mir_module)?;
        let carrier_ty = self.get_llvm_type(signature)?;
        let ptr_ty = unsafe { LLVMPointerType(LLVMInt8TypeInContext(self.context), 0) };
        let null = unsafe { LLVMConstPointerNull(ptr_ty) };
        let invoke = if unsafe { LLVMTypeOf(thunk) } == ptr_ty {
            thunk
        } else {
            unsafe {
                LLVMBuildBitCast(
                    self.builder,
                    thunk,
                    ptr_ty,
                    CString::new("fnref.invoke")?.as_ptr(),
                )
            }
        };

        let carrier = unsafe { LLVMGetUndef(carrier_ty) };
        let carrier = unsafe {
            LLVMBuildInsertValue(
                self.builder,
                carrier,
                null,
                0,
                CString::new("fnref.env")?.as_ptr(),
            )
        };
        let carrier = unsafe {
            LLVMBuildInsertValue(
                self.builder,
                carrier,
                invoke,
                1,
                CString::new("fnref.invoke")?.as_ptr(),
            )
        };
        Ok(unsafe {
            LLVMBuildInsertValue(
                self.builder,
                carrier,
                null,
                2,
                CString::new("fnref.drop")?.as_ptr(),
            )
        })
    }

    fn codegen_callable_argument(
        &mut self,
        arg: &MirValue,
        param_ty: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let mut arg_val = self.codegen_value_owned(arg, param_ty, func, local_map)?;
        let arg_ty = self.mir_value_type(arg, func);

        unsafe {
            if let Some(Type::Ref(inner, _)) = arg_ty.as_ref() {
                if inner.as_ref() == param_ty {
                    arg_val = LLVMBuildLoad2(
                        self.builder,
                        self.get_llvm_type(param_ty)?,
                        arg_val,
                        CString::new("callable.arg.deref")?.as_ptr(),
                    );
                }
            }
            if let Type::Ref(inner, _) = param_ty {
                if arg_ty.as_ref() == Some(inner.as_ref()) {
                    let slot = LLVMBuildAlloca(
                        self.builder,
                        self.get_llvm_type(inner)?,
                        CString::new("callable.arg.addr")?.as_ptr(),
                    );
                    LLVMBuildStore(self.builder, arg_val, slot);
                    arg_val = slot;
                }
            }

            let expected = self.get_llvm_type(param_ty)?;
            let actual = LLVMTypeOf(arg_val);
            let expected_kind = LLVMGetTypeKind(expected);
            let actual_kind = LLVMGetTypeKind(actual);
            if expected_kind == llvm_sys::LLVMTypeKind::LLVMIntegerTypeKind
                && actual_kind == llvm_sys::LLVMTypeKind::LLVMIntegerTypeKind
            {
                return Ok(self.coerce_int_value(
                    arg_val,
                    expected,
                    matches!(param_ty, Type::I8 | Type::I32 | Type::I64),
                ));
            }
            if Self::is_float_type_kind(expected_kind)
                && Self::is_float_type_kind(actual_kind)
                && expected != actual
            {
                return Ok(if matches!(param_ty, Type::F64) {
                    LLVMBuildFPExt(
                        self.builder,
                        arg_val,
                        expected,
                        CString::new("callable.arg.fp.ext")?.as_ptr(),
                    )
                } else {
                    LLVMBuildFPTrunc(
                        self.builder,
                        arg_val,
                        expected,
                        CString::new("callable.arg.fp.trunc")?.as_ptr(),
                    )
                });
            }
            if Self::is_float_type_kind(expected_kind)
                && actual_kind == llvm_sys::LLVMTypeKind::LLVMIntegerTypeKind
            {
                let unsigned =
                    matches!(arg_ty, Some(Type::U8 | Type::U32 | Type::U64 | Type::Usize));
                return Ok(if unsigned {
                    LLVMBuildUIToFP(
                        self.builder,
                        arg_val,
                        expected,
                        CString::new("callable.arg.ui.fp")?.as_ptr(),
                    )
                } else {
                    LLVMBuildSIToFP(
                        self.builder,
                        arg_val,
                        expected,
                        CString::new("callable.arg.si.fp")?.as_ptr(),
                    )
                });
            }
            if expected_kind == llvm_sys::LLVMTypeKind::LLVMPointerTypeKind
                && actual_kind == llvm_sys::LLVMTypeKind::LLVMPointerTypeKind
                && expected != actual
            {
                return Ok(LLVMBuildBitCast(
                    self.builder,
                    arg_val,
                    expected,
                    CString::new("callable.arg.ptr")?.as_ptr(),
                ));
            }
        }
        Ok(arg_val)
    }

    pub(super) fn codegen_call_indirect(
        &mut self,
        callee: LocalId,
        signature: &Type,
        args: &[MirValue],
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let (params, ret) = self.callable_signature(signature)?;
        let params = params.to_vec();
        let ret = ret.clone();
        if args.len() != params.len() {
            bail!(
                "indirect call expected {} arguments, found {}",
                params.len(),
                args.len()
            );
        }
        let local_ty = func
            .locals
            .get(callee.0 as usize)
            .and_then(|local| local.ty.as_ref())
            .ok_or_else(|| anyhow!("indirect callee {:?} has no type", callee))?;
        if local_ty != signature {
            bail!(
                "indirect callee type mismatch: local has {:?}, call uses {:?}",
                local_ty,
                signature
            );
        }

        let carrier = self.codegen_value(&MirValue::Local(callee), func, local_map)?;
        let env = unsafe {
            LLVMBuildExtractValue(
                self.builder,
                carrier,
                0,
                CString::new("callable.env")?.as_ptr(),
            )
        };
        let invoke = unsafe {
            LLVMBuildExtractValue(
                self.builder,
                carrier,
                1,
                CString::new("callable.invoke")?.as_ptr(),
            )
        };

        // CallIndirect is consuming. Clear storage before invoking so generic
        // scope cleanup cannot run the callable's drop thunk a second time.
        let callee_slot = local_map
            .get(&callee)
            .copied()
            .ok_or_else(|| anyhow!("missing storage for indirect callee {:?}", callee))?;
        unsafe {
            LLVMBuildStore(
                self.builder,
                LLVMConstNull(self.get_llvm_type(signature)?),
                callee_slot,
            );
        }

        let (invoke_ty, uses_sret) = self.callable_invoke_type(signature)?;
        let mut llvm_args = Vec::with_capacity(args.len() + 2);
        let mut sret_slot = None;
        if uses_sret {
            let ret_ty = self.get_llvm_type(&ret)?;
            let slot = unsafe {
                LLVMBuildAlloca(
                    self.builder,
                    ret_ty,
                    CString::new("callable.sret.tmp")?.as_ptr(),
                )
            };
            llvm_args.push(slot);
            sret_slot = Some(slot);
        }
        llvm_args.push(env);
        for (arg, param_ty) in args.iter().zip(&params) {
            llvm_args.push(self.codegen_callable_argument(arg, param_ty, func, local_map)?);
        }

        let call = self.build_call2(invoke_ty, invoke, &mut llvm_args, "call.indirect")?;
        if let Some(slot) = sret_slot {
            let ret_ty = self.get_llvm_type(&ret)?;
            self.add_sret_call_attribute(call, ret_ty);
            return Ok(unsafe {
                LLVMBuildLoad2(
                    self.builder,
                    ret_ty,
                    slot,
                    CString::new("callable.sret.load")?.as_ptr(),
                )
            });
        }
        Ok(call)
    }
}
