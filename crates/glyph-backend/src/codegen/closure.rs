use super::*;
use glyph_core::mir::{
    BorrowCaptureSource, BorrowKind, CaptureTransfer, MirBorrowCapture, MirCapture,
};

impl CodegenContext {
    fn borrowed_capture_type(capture: &MirBorrowCapture) -> Type {
        Type::Ref(
            Box::new(capture.ty.clone()),
            match capture.borrow {
                BorrowKind::Shared => Mutability::Immutable,
                BorrowKind::Mutable => Mutability::Mutable,
            },
        )
    }

    fn closure_capture_is_copy(ty: &Type) -> bool {
        match ty {
            Type::I8
            | Type::I32
            | Type::I64
            | Type::U8
            | Type::U32
            | Type::U64
            | Type::Usize
            | Type::F32
            | Type::F64
            | Type::Bool
            | Type::Char
            | Type::Str
            | Type::Void
            | Type::Ref(..)
            | Type::RawPtr(_) => true,
            Type::Array(element, _) => Self::closure_capture_is_copy(element),
            Type::Tuple(elements) => elements.iter().all(Self::closure_capture_is_copy),
            _ => false,
        }
    }

    fn stable_closure_hash(value: &str) -> u64 {
        // FNV-1a keeps synthesized names stable across compiler processes;
        // `DefaultHasher` deliberately makes no such compatibility promise.
        value
            .as_bytes()
            .iter()
            .fold(0xcbf29ce484222325, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
            })
    }

    fn closure_artifact_key(
        &self,
        function: &str,
        signature: &Type,
        captures: &[MirCapture],
    ) -> String {
        let captures = captures
            .iter()
            .map(|capture| self.type_key(&capture.ty))
            .collect::<Vec<_>>()
            .join("_");
        format!("{}::{}::{}", function, self.type_key(signature), captures)
    }

    fn closure_env_type(&mut self, captures: &[MirCapture]) -> Result<LLVMTypeRef> {
        let mut fields = captures
            .iter()
            .map(|capture| self.get_llvm_type(&capture.ty))
            .collect::<Result<Vec<_>>>()?;
        Ok(unsafe {
            LLVMStructTypeInContext(self.context, fields.as_mut_ptr(), fields.len() as u32, 0)
        })
    }

    fn borrowed_closure_env_type(&mut self, captures: &[MirBorrowCapture]) -> Result<LLVMTypeRef> {
        let mut fields = captures
            .iter()
            .map(|capture| self.get_llvm_type(&Self::borrowed_capture_type(capture)))
            .collect::<Result<Vec<_>>>()?;
        Ok(unsafe {
            LLVMStructTypeInContext(self.context, fields.as_mut_ptr(), fields.len() as u32, 0)
        })
    }

    fn closure_free_env(&mut self, env: LLVMValueRef) -> Result<()> {
        let free = self.ensure_free_fn()?;
        let free_ty = self.free_function_type();
        let mut args = [env];
        self.build_call2(free_ty, free, &mut args, "closure.env.free")?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn build_closure_invoke_thunk(
        &mut self,
        function: &str,
        signature: &Type,
        captures: &[MirCapture],
        env_ty: LLVMTypeRef,
        functions: &HashMap<String, LLVMValueRef>,
        mir_module: &MirModule,
        suffix: &str,
    ) -> Result<LLVMValueRef> {
        let (params, ret) = signature
            .function_signature()
            .ok_or_else(|| anyhow!("closure signature is not callable: {signature:?}"))?;
        let (body_params, body_ret) = self.function_signature_in_module(function, mir_module)?;
        let expected_params = captures
            .iter()
            .map(|capture| capture.ty.clone())
            .chain(params.iter().cloned())
            .collect::<Vec<_>>();
        let unit_compatible = |left: &Type, right: &Type| {
            left == right || (Self::is_unit_type(left) && Self::is_unit_type(right))
        };
        if body_params != expected_params || !unit_compatible(ret, &body_ret) {
            bail!(
                "lifted closure {} has signature {:?} -> {:?}, expected {:?} -> {:?}",
                function,
                body_params,
                body_ret,
                expected_params,
                ret
            );
        }

        let target = functions
            .get(function)
            .copied()
            .ok_or_else(|| anyhow!("lifted closure function {function} was not declared"))?;
        let target_ty =
            self.function_types.get(function).copied().ok_or_else(|| {
                anyhow!("missing LLVM function type for lifted closure {function}")
            })?;
        let (invoke_ty, uses_sret) = self.callable_invoke_type(signature)?;
        let name = CString::new(format!(
            "__glyph_closure_invoke_{}_{}",
            self.sanitize(function),
            suffix
        ))?;
        let thunk = unsafe { LLVMAddFunction(self.module, name.as_ptr(), invoke_ty) };
        unsafe { LLVMSetLinkage(thunk, LLVMLinkage::LLVMPrivateLinkage) };
        if uses_sret {
            self.add_sret_attribute(thunk, self.get_llvm_type(ret)?);
        }

        let entry = unsafe {
            LLVMAppendBasicBlockInContext(self.context, thunk, CString::new("entry")?.as_ptr())
        };
        unsafe { LLVMPositionBuilderAtEnd(self.builder, entry) };

        let sret_offset = usize::from(uses_sret);
        let env = unsafe { LLVMGetParam(thunk, sret_offset as u32) };
        let env_ptr_ty = unsafe { LLVMPointerType(env_ty, 0) };
        let typed_env = unsafe {
            LLVMBuildBitCast(
                self.builder,
                env,
                env_ptr_ty,
                CString::new("closure.env.typed")?.as_ptr(),
            )
        };

        let mut body_args = Vec::with_capacity(expected_params.len() + sret_offset);
        if uses_sret {
            body_args.push(unsafe { LLVMGetParam(thunk, 0) });
        }
        for (index, capture) in captures.iter().enumerate() {
            let field = unsafe {
                LLVMBuildStructGEP2(
                    self.builder,
                    env_ty,
                    typed_env,
                    index as u32,
                    CString::new(format!("closure.capture.{index}"))?.as_ptr(),
                )
            };
            let value = unsafe {
                let load = LLVMBuildLoad2(
                    self.builder,
                    self.get_llvm_type(&capture.ty)?,
                    field,
                    CString::new(format!("closure.capture.load.{index}"))?.as_ptr(),
                );
                if let Type::Atomic(scalar) = capture.ty {
                    LLVMSetOrdering(
                        load,
                        llvm_sys::LLVMAtomicOrdering::LLVMAtomicOrderingSequentiallyConsistent,
                    );
                    LLVMSetAlignment(load, self.atomic_alignment(scalar)?);
                }
                load
            };
            body_args.push(value);
        }
        for index in 0..params.len() {
            body_args.push(unsafe { LLVMGetParam(thunk, (sret_offset + 1 + index) as u32) });
        }

        let call = self.build_call2(target_ty, target, &mut body_args, "closure.body.call")?;
        if uses_sret {
            self.add_sret_call_attribute(call, self.get_llvm_type(ret)?);
        }
        self.closure_free_env(env)?;
        unsafe {
            if uses_sret || Self::is_unit_type(ret) {
                LLVMBuildRetVoid(self.builder);
            } else {
                LLVMBuildRet(self.builder, call);
            }
        }
        Ok(thunk)
    }

    fn build_closure_drop_thunk(
        &mut self,
        function: &str,
        captures: &[MirCapture],
        env_ty: LLVMTypeRef,
        suffix: &str,
    ) -> Result<LLVMValueRef> {
        let ptr_ty = unsafe { LLVMPointerType(LLVMInt8TypeInContext(self.context), 0) };
        let mut params = [ptr_ty];
        let drop_ty = unsafe {
            LLVMFunctionType(
                LLVMVoidTypeInContext(self.context),
                params.as_mut_ptr(),
                1,
                0,
            )
        };
        let name = CString::new(format!(
            "__glyph_closure_drop_{}_{}",
            self.sanitize(function),
            suffix
        ))?;
        let thunk = unsafe { LLVMAddFunction(self.module, name.as_ptr(), drop_ty) };
        unsafe { LLVMSetLinkage(thunk, LLVMLinkage::LLVMPrivateLinkage) };
        let entry = unsafe {
            LLVMAppendBasicBlockInContext(self.context, thunk, CString::new("entry")?.as_ptr())
        };
        unsafe { LLVMPositionBuilderAtEnd(self.builder, entry) };

        let env = unsafe { LLVMGetParam(thunk, 0) };
        let env_ptr_ty = unsafe { LLVMPointerType(env_ty, 0) };
        let typed_env = unsafe {
            LLVMBuildBitCast(
                self.builder,
                env,
                env_ptr_ty,
                CString::new("closure.drop.env")?.as_ptr(),
            )
        };
        for (index, capture) in captures.iter().enumerate().rev() {
            let field = unsafe {
                LLVMBuildStructGEP2(
                    self.builder,
                    env_ty,
                    typed_env,
                    index as u32,
                    CString::new(format!("closure.drop.capture.{index}"))?.as_ptr(),
                )
            };
            self.codegen_drop_elem_slot(field, &capture.ty)?;
        }
        self.closure_free_env(env)?;
        unsafe { LLVMBuildRetVoid(self.builder) };
        Ok(thunk)
    }

    #[allow(clippy::too_many_arguments)]
    fn build_borrowed_closure_invoke_thunk(
        &mut self,
        function: &str,
        signature: &Type,
        captures: &[MirBorrowCapture],
        env_ty: LLVMTypeRef,
        functions: &HashMap<String, LLVMValueRef>,
        mir_module: &MirModule,
        suffix: &str,
    ) -> Result<LLVMValueRef> {
        let Type::BorrowedFunction { params, ret, .. } = signature else {
            bail!("borrowed closure signature is not Fn/FnMut: {signature:?}");
        };
        let (body_params, body_ret) = self.function_signature_in_module(function, mir_module)?;
        let expected_params = captures
            .iter()
            .map(Self::borrowed_capture_type)
            .chain(params.iter().cloned())
            .collect::<Vec<_>>();
        let unit_compatible = |left: &Type, right: &Type| {
            left == right || (Self::is_unit_type(left) && Self::is_unit_type(right))
        };
        if body_params != expected_params || !unit_compatible(ret, &body_ret) {
            bail!(
                "lifted borrowed closure {} has signature {:?} -> {:?}, expected {:?} -> {:?}",
                function,
                body_params,
                body_ret,
                expected_params,
                ret
            );
        }

        let target = functions.get(function).copied().ok_or_else(|| {
            anyhow!("lifted borrowed closure function {function} was not declared")
        })?;
        let target_ty = self.function_types.get(function).copied().ok_or_else(|| {
            anyhow!("missing LLVM function type for lifted borrowed closure {function}")
        })?;
        let (invoke_ty, uses_sret) = self.callable_invoke_type(signature)?;
        let name = CString::new(format!(
            "__glyph_borrowed_closure_invoke_{}_{}",
            self.sanitize(function),
            suffix
        ))?;
        let thunk = unsafe { LLVMAddFunction(self.module, name.as_ptr(), invoke_ty) };
        unsafe { LLVMSetLinkage(thunk, LLVMLinkage::LLVMPrivateLinkage) };
        if uses_sret {
            self.add_sret_attribute(thunk, self.get_llvm_type(ret)?);
        }

        let entry = unsafe {
            LLVMAppendBasicBlockInContext(self.context, thunk, CString::new("entry")?.as_ptr())
        };
        unsafe { LLVMPositionBuilderAtEnd(self.builder, entry) };

        let sret_offset = usize::from(uses_sret);
        let env = unsafe { LLVMGetParam(thunk, sret_offset as u32) };
        let env_ptr_ty = unsafe { LLVMPointerType(env_ty, 0) };
        let typed_env = unsafe {
            LLVMBuildBitCast(
                self.builder,
                env,
                env_ptr_ty,
                CString::new("borrowed.closure.env.typed")?.as_ptr(),
            )
        };

        let mut body_args = Vec::with_capacity(expected_params.len() + sret_offset);
        if uses_sret {
            body_args.push(unsafe { LLVMGetParam(thunk, 0) });
        }
        for (index, capture) in captures.iter().enumerate() {
            let field = unsafe {
                LLVMBuildStructGEP2(
                    self.builder,
                    env_ty,
                    typed_env,
                    index as u32,
                    CString::new(format!("borrowed.closure.capture.{index}"))?.as_ptr(),
                )
            };
            let capture_ty = Self::borrowed_capture_type(capture);
            let value = unsafe {
                LLVMBuildLoad2(
                    self.builder,
                    self.get_llvm_type(&capture_ty)?,
                    field,
                    CString::new(format!("borrowed.closure.capture.load.{index}"))?.as_ptr(),
                )
            };
            body_args.push(value);
        }
        for index in 0..params.len() {
            body_args.push(unsafe { LLVMGetParam(thunk, (sret_offset + 1 + index) as u32) });
        }

        let call = self.build_call2(
            target_ty,
            target,
            &mut body_args,
            "borrowed.closure.body.call",
        )?;
        if uses_sret {
            self.add_sret_call_attribute(call, self.get_llvm_type(ret)?);
        }
        unsafe {
            if uses_sret || Self::is_unit_type(ret) {
                LLVMBuildRetVoid(self.builder);
            } else {
                LLVMBuildRet(self.builder, call);
            }
        }
        Ok(thunk)
    }

    #[allow(clippy::too_many_arguments)]
    fn ensure_borrowed_closure_artifacts(
        &mut self,
        function: &str,
        signature: &Type,
        captures: &[MirBorrowCapture],
        functions: &HashMap<String, LLVMValueRef>,
        mir_module: &MirModule,
    ) -> Result<BorrowedClosureArtifacts> {
        let captures_key = captures
            .iter()
            .map(|capture| {
                format!(
                    "{}:{}",
                    match capture.borrow {
                        BorrowKind::Shared => "shared",
                        BorrowKind::Mutable => "mut",
                    },
                    self.type_key(&capture.ty)
                )
            })
            .collect::<Vec<_>>()
            .join("_");
        let key = format!(
            "{}::{}::{}",
            function,
            self.type_key(signature),
            captures_key
        );
        if let Some(artifacts) = self.borrowed_closure_artifacts.get(&key).copied() {
            return Ok(artifacts);
        }
        let suffix = format!(
            "{}_{:016x}",
            self.sanitize(function),
            Self::stable_closure_hash(&key)
        );
        let env_type = self.borrowed_closure_env_type(captures)?;
        let saved_block = unsafe { LLVMGetInsertBlock(self.builder) };
        let generated = self.build_borrowed_closure_invoke_thunk(
            function, signature, captures, env_type, functions, mir_module, &suffix,
        );
        if !saved_block.is_null() {
            unsafe { LLVMPositionBuilderAtEnd(self.builder, saved_block) };
        }
        let invoke = generated?;
        let artifacts = BorrowedClosureArtifacts { env_type, invoke };
        self.borrowed_closure_artifacts.insert(key, artifacts);
        Ok(artifacts)
    }

    #[allow(clippy::too_many_arguments)]
    fn ensure_closure_artifacts(
        &mut self,
        function: &str,
        signature: &Type,
        captures: &[MirCapture],
        functions: &HashMap<String, LLVMValueRef>,
        mir_module: &MirModule,
    ) -> Result<ClosureArtifacts> {
        let key = self.closure_artifact_key(function, signature, captures);
        if let Some(artifacts) = self.closure_artifacts.get(&key).copied() {
            return Ok(artifacts);
        }
        let suffix = format!(
            "{}_{:016x}",
            self.sanitize(function),
            Self::stable_closure_hash(&key)
        );
        let env_type = self.closure_env_type(captures)?;
        let saved_block = unsafe { LLVMGetInsertBlock(self.builder) };
        let generated = (|| {
            let invoke = self.build_closure_invoke_thunk(
                function, signature, captures, env_type, functions, mir_module, &suffix,
            )?;
            let drop = self.build_closure_drop_thunk(function, captures, env_type, &suffix)?;
            Ok::<_, anyhow::Error>((invoke, drop))
        })();
        if !saved_block.is_null() {
            unsafe { LLVMPositionBuilderAtEnd(self.builder, saved_block) };
        }
        let (invoke, drop) = generated?;
        let artifacts = ClosureArtifacts {
            env_type,
            invoke,
            drop,
        };
        self.closure_artifacts.insert(key, artifacts);
        Ok(artifacts)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn codegen_make_closure(
        &mut self,
        function: &str,
        signature: &Type,
        captures: &[MirCapture],
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        functions: &HashMap<String, LLVMValueRef>,
        mir_module: &MirModule,
    ) -> Result<LLVMValueRef> {
        if !matches!(signature, Type::Function { .. }) {
            bail!("MakeClosure requires an owned FnOnce signature, found {signature:?}");
        }
        if captures.is_empty() {
            return self.codegen_function_ref(function, signature, functions, mir_module);
        }
        for capture in captures {
            let actual = func
                .locals
                .get(capture.local.0 as usize)
                .and_then(|local| local.ty.as_ref());
            if actual != Some(&capture.ty) {
                bail!(
                    "closure capture `{}` expects {:?}, local {:?} has {:?}",
                    capture.name,
                    capture.ty,
                    capture.local,
                    actual
                );
            }
            if capture.transfer == CaptureTransfer::Copy
                && !Self::closure_capture_is_copy(&capture.ty)
            {
                bail!(
                    "closure capture `{}` cannot copy ownership-bearing type {:?}",
                    capture.name,
                    capture.ty
                );
            }
        }

        let artifacts =
            self.ensure_closure_artifacts(function, signature, captures, functions, mir_module)?;
        let target_data = self
            .target_data
            .ok_or_else(|| anyhow!("missing target data while allocating closure environment"))?;
        let size = unsafe { LLVMABISizeOfType(target_data, artifacts.env_type) };
        let size_value =
            unsafe { LLVMConstInt(LLVMInt64TypeInContext(self.context), size.max(1), 0) };
        let malloc = self.ensure_malloc_fn()?;
        let mut malloc_args = [size_value];
        let env = self.build_call2(
            self.malloc_function_type(),
            malloc,
            &mut malloc_args,
            "closure.env.malloc",
        )?;
        let typed_env = unsafe {
            LLVMBuildBitCast(
                self.builder,
                env,
                LLVMPointerType(artifacts.env_type, 0),
                CString::new("closure.env")?.as_ptr(),
            )
        };
        for (index, capture) in captures.iter().enumerate() {
            let field = unsafe {
                LLVMBuildStructGEP2(
                    self.builder,
                    artifacts.env_type,
                    typed_env,
                    index as u32,
                    CString::new(format!("closure.env.field.{index}"))?.as_ptr(),
                )
            };
            let value = self.codegen_value(&MirValue::Local(capture.local), func, local_map)?;
            unsafe {
                LLVMBuildStore(self.builder, value, field);
                if capture.transfer == CaptureTransfer::Move {
                    let source = local_map.get(&capture.local).copied().ok_or_else(|| {
                        anyhow!("missing storage for closure capture {:?}", capture.local)
                    })?;
                    LLVMBuildStore(
                        self.builder,
                        LLVMConstNull(self.get_llvm_type(&capture.ty)?),
                        source,
                    );
                }
            }
        }

        let ptr_ty = unsafe { LLVMPointerType(LLVMInt8TypeInContext(self.context), 0) };
        let invoke = unsafe {
            LLVMBuildBitCast(
                self.builder,
                artifacts.invoke,
                ptr_ty,
                CString::new("closure.invoke")?.as_ptr(),
            )
        };
        let drop = unsafe {
            LLVMBuildBitCast(
                self.builder,
                artifacts.drop,
                ptr_ty,
                CString::new("closure.drop")?.as_ptr(),
            )
        };
        let carrier_ty = self.get_llvm_type(signature)?;
        let carrier = unsafe { LLVMGetUndef(carrier_ty) };
        let carrier = unsafe {
            LLVMBuildInsertValue(
                self.builder,
                carrier,
                env,
                0,
                CString::new("closure.carrier.env")?.as_ptr(),
            )
        };
        let carrier = unsafe {
            LLVMBuildInsertValue(
                self.builder,
                carrier,
                invoke,
                1,
                CString::new("closure.carrier.invoke")?.as_ptr(),
            )
        };
        Ok(unsafe {
            LLVMBuildInsertValue(
                self.builder,
                carrier,
                drop,
                2,
                CString::new("closure.carrier.drop")?.as_ptr(),
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn codegen_make_borrowed_closure(
        &mut self,
        function: &str,
        signature: &Type,
        captures: &[MirBorrowCapture],
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        functions: &HashMap<String, LLVMValueRef>,
        mir_module: &MirModule,
    ) -> Result<LLVMValueRef> {
        let Type::BorrowedFunction { kind, .. } = signature else {
            bail!("MakeBorrowedClosure requires an Fn/FnMut signature, found {signature:?}");
        };
        if *kind == BorrowedCallableKind::Fn
            && captures
                .iter()
                .any(|capture| capture.borrow == BorrowKind::Mutable)
        {
            bail!("Fn borrowed closure cannot contain a mutable capture");
        }
        if captures.is_empty() {
            return self.codegen_function_ref(function, signature, functions, mir_module);
        }
        for capture in captures {
            let actual = func
                .locals
                .get(capture.local.0 as usize)
                .and_then(|local| local.ty.as_ref());
            let compatible = match (capture.source, actual) {
                (BorrowCaptureSource::Local, Some(actual)) => actual == &capture.ty,
                (BorrowCaptureSource::Reborrow, Some(Type::Ref(inner, mutability))) => {
                    inner.as_ref() == &capture.ty
                        && (capture.borrow == BorrowKind::Shared
                            || *mutability == Mutability::Mutable)
                }
                _ => false,
            };
            if !compatible {
                bail!(
                    "borrowed closure capture `{}` expects {:?} referent {:?}, local {:?} has {:?}",
                    capture.name,
                    capture.source,
                    capture.ty,
                    capture.local,
                    actual
                );
            }
        }

        let artifacts = self.ensure_borrowed_closure_artifacts(
            function, signature, captures, functions, mir_module,
        )?;
        let typed_env = unsafe {
            LLVMBuildAlloca(
                self.builder,
                artifacts.env_type,
                CString::new("borrowed.closure.env")?.as_ptr(),
            )
        };
        for (index, capture) in captures.iter().enumerate() {
            let field = unsafe {
                LLVMBuildStructGEP2(
                    self.builder,
                    artifacts.env_type,
                    typed_env,
                    index as u32,
                    CString::new(format!("borrowed.closure.env.field.{index}"))?.as_ptr(),
                )
            };
            let source_slot = local_map.get(&capture.local).copied().ok_or_else(|| {
                anyhow!(
                    "missing storage for borrowed closure capture {:?}",
                    capture.local
                )
            })?;
            let source = match capture.source {
                BorrowCaptureSource::Local => source_slot,
                BorrowCaptureSource::Reborrow => unsafe {
                    LLVMBuildLoad2(
                        self.builder,
                        self.get_llvm_type(&Self::borrowed_capture_type(capture))?,
                        source_slot,
                        CString::new(format!("borrowed.closure.reborrow.{index}"))?.as_ptr(),
                    )
                },
            };
            unsafe { LLVMBuildStore(self.builder, source, field) };
        }

        let ptr_ty = unsafe { LLVMPointerType(LLVMInt8TypeInContext(self.context), 0) };
        let env = unsafe {
            LLVMBuildBitCast(
                self.builder,
                typed_env,
                ptr_ty,
                CString::new("borrowed.closure.env.erased")?.as_ptr(),
            )
        };
        let invoke = unsafe {
            LLVMBuildBitCast(
                self.builder,
                artifacts.invoke,
                ptr_ty,
                CString::new("borrowed.closure.invoke")?.as_ptr(),
            )
        };
        let null = unsafe { LLVMConstPointerNull(ptr_ty) };
        let carrier_ty = self.get_llvm_type(signature)?;
        let carrier = unsafe { LLVMGetUndef(carrier_ty) };
        let carrier = unsafe {
            LLVMBuildInsertValue(
                self.builder,
                carrier,
                env,
                0,
                CString::new("borrowed.closure.carrier.env")?.as_ptr(),
            )
        };
        let carrier = unsafe {
            LLVMBuildInsertValue(
                self.builder,
                carrier,
                invoke,
                1,
                CString::new("borrowed.closure.carrier.invoke")?.as_ptr(),
            )
        };
        Ok(unsafe {
            LLVMBuildInsertValue(
                self.builder,
                carrier,
                null,
                2,
                CString::new("borrowed.closure.carrier.drop")?.as_ptr(),
            )
        })
    }
}
