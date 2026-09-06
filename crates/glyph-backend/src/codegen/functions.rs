use super::*;

impl CodegenContext {
    pub fn codegen_module(&mut self, mir_module: &MirModule) -> Result<()> {
        // GLYPH-3: verify MIR invariants backend codegen otherwise trusts
        // silently (in-range block/local ids, terminators, valid declared
        // types, enum variant bounds, ...) before any LLVM lowering starts.
        // This is the single chokepoint every LLVM path passes through: the
        // `LlvmBackend` trait impl, the CLI's direct `CodegenContext`
        // construction sites, `thread_runtime`, and every codegen test
        // harness all call `codegen_module`.
        let verify_errors = glyph_core::mir_verify::verify_module(mir_module);
        if !verify_errors.is_empty() {
            bail!(
                "MIR verification failed before codegen ({} error(s)):\n{}",
                verify_errors.len(),
                glyph_core::mir_verify::format_errors(&verify_errors)
            );
        }

        self.init_target_data()?;
        self.debug_log("create_named_types start");
        self.create_named_types(mir_module)?;
        self.debug_log("register_struct_types start");
        self.register_struct_types(mir_module)?;
        self.debug_log("register_enum_types start");
        self.register_enum_types(mir_module)?;

        let needs_sys_argv = self.mir_uses_sys_argv(mir_module);

        // Declare all functions up-front so calls can reference any order.
        self.debug_log("declare_functions start");
        let functions = self.declare_functions(mir_module, needs_sys_argv)?;

        for func in &mir_module.functions {
            self.debug_log(&format!("codegen_function_body start: {}", func.name));
            let llvm_func = *functions
                .get(&func.name)
                .ok_or_else(|| anyhow!("missing declared function {}", func.name))?;
            self.codegen_function_body(func, llvm_func, &functions, mir_module)?;
            self.debug_log(&format!("codegen_function_body done: {}", func.name));
        }

        if needs_sys_argv {
            if let Some(main_func) = functions.get("main").copied() {
                self.codegen_main_wrapper(mir_module, main_func)?;
            }
        }

        Ok(())
    }

    pub(super) fn declare_functions(
        &mut self,
        mir_module: &MirModule,
        needs_sys_argv: bool,
    ) -> Result<HashMap<String, LLVMValueRef>> {
        let mut functions = HashMap::new();

        for func in &mir_module.functions {
            let (func_type, uses_sret) = self.llvm_function_type(func)?;
            let llvm_name = if needs_sys_argv && func.name == "main" && func.params.is_empty() {
                "__glyph_main"
            } else {
                func.name.as_str()
            };
            let func_name = CString::new(llvm_name)?;
            let llvm_func = unsafe { LLVMAddFunction(self.module, func_name.as_ptr(), func_type) };
            self.function_types.insert(func.name.clone(), func_type);
            if uses_sret {
                let ret_ty = func
                    .ret_type
                    .as_ref()
                    .ok_or_else(|| anyhow!("sret function missing return type"))?;
                let llvm_ret_ty = self.get_llvm_type(ret_ty)?;
                self.add_sret_attribute(llvm_func, llvm_ret_ty);
                self.sret_functions
                    .insert(func.name.clone(), ret_ty.clone());
            }
            functions.insert(func.name.clone(), llvm_func);
        }

        for func in &mir_module.extern_functions {
            if functions.contains_key(&func.name) {
                continue;
            }
            let (func_type, uses_sret) = self.llvm_extern_function_type(func)?;
            let symbol_name = func.link_name.as_ref().unwrap_or(&func.name);
            let func_name = CString::new(symbol_name.as_str())?;
            let llvm_func = unsafe { LLVMAddFunction(self.module, func_name.as_ptr(), func_type) };
            self.function_types.insert(func.name.clone(), func_type);
            unsafe { LLVMSetLinkage(llvm_func, LLVMLinkage::LLVMExternalLinkage) };
            if symbol_name == "strdup" && self.strdup_fn.is_none() {
                self.strdup_fn = Some(llvm_func);
            }
            if uses_sret {
                let ret_ty = func
                    .ret_type
                    .as_ref()
                    .ok_or_else(|| anyhow!("sret extern missing return type"))?;
                let llvm_ret_ty = self.get_llvm_type(ret_ty)?;
                self.add_sret_attribute(llvm_func, llvm_ret_ty);
                self.sret_functions
                    .insert(func.name.clone(), ret_ty.clone());
            }
            // ABI mapping (v0: only "C" supported; default LLVM CC is C).
            if let Some(abi) = &func.abi {
                if abi != "C" {
                    bail!("unsupported ABI '{}': only \"C\" is supported", abi);
                }
                // If additional ABIs are added, map them via LLVMSetFunctionCallConv here.
            }
            functions.insert(func.name.clone(), llvm_func);
        }

        Ok(functions)
    }

    pub(super) fn llvm_function_type(&self, func: &MirFunction) -> Result<(LLVMTypeRef, bool)> {
        // Determine return type
        let mut uses_sret = false;
        let ret_type = if let Some(ret_ty) = func.ret_type.as_ref() {
            if self.ret_uses_sret(ret_ty)? {
                uses_sret = true;
                unsafe { LLVMVoidTypeInContext(self.context) }
            } else {
                self.get_llvm_type(ret_ty)?
            }
        } else {
            unsafe { LLVMVoidTypeInContext(self.context) }
        };

        // Build parameter types
        let mut param_types: Vec<LLVMTypeRef> = Vec::new();
        if uses_sret {
            let ret_ty = func
                .ret_type
                .as_ref()
                .ok_or_else(|| anyhow!("sret function missing return type"))?;
            let llvm_ret_ty = self.get_llvm_type(ret_ty)?;
            let sret_ptr_ty = unsafe { LLVMPointerType(llvm_ret_ty, 0) };
            param_types.push(sret_ptr_ty);
        }
        for &param_id in &func.params {
            let local = &func.locals[param_id.0 as usize];
            let param_ty = local
                .ty
                .as_ref()
                .map(|t| self.get_llvm_type(t))
                .transpose()?
                .unwrap_or_else(|| unsafe { LLVMInt32TypeInContext(self.context) });
            param_types.push(param_ty);
        }

        Ok((
            unsafe {
                LLVMFunctionType(
                    ret_type,
                    param_types.as_mut_ptr(),
                    param_types.len() as u32,
                    0, // not variadic
                )
            },
            uses_sret,
        ))
    }

    pub(super) fn llvm_extern_function_type(
        &self,
        func: &MirExternFunction,
    ) -> Result<(LLVMTypeRef, bool)> {
        let mut uses_sret = false;
        let ret_type = if let Some(ret_ty) = func.ret_type.as_ref() {
            if self.ret_uses_sret(ret_ty)? {
                uses_sret = true;
                unsafe { LLVMVoidTypeInContext(self.context) }
            } else {
                self.get_llvm_type(ret_ty)?
            }
        } else {
            unsafe { LLVMVoidTypeInContext(self.context) }
        };

        let mut param_types: Vec<LLVMTypeRef> = Vec::new();
        if uses_sret {
            let ret_ty = func
                .ret_type
                .as_ref()
                .ok_or_else(|| anyhow!("sret extern missing return type"))?;
            let llvm_ret_ty = self.get_llvm_type(ret_ty)?;
            let sret_ptr_ty = unsafe { LLVMPointerType(llvm_ret_ty, 0) };
            param_types.push(sret_ptr_ty);
        }
        for param_ty in &func.params {
            param_types.push(self.get_llvm_type(param_ty)?);
        }

        Ok((
            unsafe {
                LLVMFunctionType(
                    ret_type,
                    param_types.as_mut_ptr(),
                    param_types.len() as u32,
                    0, // not variadic in v0
                )
            },
            uses_sret,
        ))
    }

    pub(super) fn codegen_function_body(
        &mut self,
        func: &MirFunction,
        llvm_func: LLVMValueRef,
        functions: &HashMap<String, LLVMValueRef>,
        mir_module: &MirModule,
    ) -> Result<()> {
        self.validate_scoped_thread_function(func)?;
        // Create basic blocks
        let mut bb_map: HashMap<BlockId, LLVMBasicBlockRef> = HashMap::new();
        for (i, _) in func.blocks.iter().enumerate() {
            let bb_name = CString::new(format!("bb{}", i))?;
            let bb =
                unsafe { LLVMAppendBasicBlockInContext(self.context, llvm_func, bb_name.as_ptr()) };
            bb_map.insert(BlockId(i as u32), bb);
        }

        self.debug_log("codegen_function_body allocas start");

        let sret_ptr = self
            .sret_functions
            .get(&func.name)
            .map(|_| unsafe { LLVMGetParam(llvm_func, 0) });

        // Create local allocas
        let mut local_map: HashMap<LocalId, LLVMValueRef> = HashMap::new();

        // Set up entry block for allocas
        if let Some(&entry_bb) = bb_map.get(&BlockId(0)) {
            unsafe { LLVMPositionBuilderAtEnd(self.builder, entry_bb) };

            for (i, local) in func.locals.iter().enumerate() {
                let local_id = LocalId(i as u32);

                self.debug_log(&format!("alloca local {} ty={:?}", i, local.ty));

                let local_ty = local
                    .ty
                    .as_ref()
                    .map(|t| {
                        // Unit-typed locals (Void or empty tuple, e.g. the
                        // binding in `Ok(_u)` on Result<(), E>) get an i8
                        // slot; their real LLVM type has no storable size.
                        if Self::is_unit_type(t) {
                            Ok(unsafe { LLVMInt8TypeInContext(self.context) })
                        } else {
                            self.get_llvm_type(t)
                        }
                    })
                    .transpose()?
                    .unwrap_or_else(|| unsafe { LLVMInt32TypeInContext(self.context) });

                let local_name = local.name.clone().unwrap_or_else(|| format!("tmp{}", i));
                let local_name = CString::new(local_name)?;

                let alloca =
                    unsafe { LLVMBuildAlloca(self.builder, local_ty, local_name.as_ptr()) };
                if let Some(Type::Atomic(scalar)) = local.ty.as_ref() {
                    unsafe { LLVMSetAlignment(alloca, self.atomic_alignment(*scalar)?) };
                }
                local_map.insert(local_id, alloca);
            }

            let param_offset = if sret_ptr.is_some() { 1 } else { 0 };
            for (i, &param_id) in func.params.iter().enumerate() {
                let param_val = unsafe { LLVMGetParam(llvm_func, (i + param_offset) as u32) };
                let slot = local_map
                    .get(&param_id)
                    .ok_or_else(|| anyhow!("missing storage for param {:?}", param_id))?;
                unsafe {
                    LLVMBuildStore(self.builder, param_val, *slot);
                }
            }
        }

        self.debug_log("codegen_function_body allocas done");

        // Codegen each basic block
        for (i, block) in func.blocks.iter().enumerate() {
            self.debug_log(&format!("codegen_block start bb{}", i));
            let bb = bb_map.get(&BlockId(i as u32)).unwrap();
            unsafe { LLVMPositionBuilderAtEnd(self.builder, *bb) };
            self.codegen_block(
                func, block, &local_map, &bb_map, functions, mir_module, sret_ptr,
            )?;
            self.debug_log(&format!("codegen_block done bb{}", i));
        }

        Ok(())
    }

    pub(super) fn codegen_block(
        &mut self,
        func: &MirFunction,
        block: &MirBlock,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        bb_map: &HashMap<BlockId, LLVMBasicBlockRef>,
        functions: &HashMap<String, LLVMValueRef>,
        mir_module: &MirModule,
        sret_ptr: Option<LLVMValueRef>,
    ) -> Result<()> {
        for inst in &block.insts {
            self.codegen_inst(
                func, inst, local_map, bb_map, functions, mir_module, sret_ptr,
            )?;
        }
        Ok(())
    }

    pub(super) fn codegen_inst(
        &mut self,
        func: &MirFunction,
        inst: &MirInst,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        bb_map: &HashMap<BlockId, LLVMBasicBlockRef>,
        functions: &HashMap<String, LLVMValueRef>,
        mir_module: &MirModule,
        sret_ptr: Option<LLVMValueRef>,
    ) -> Result<()> {
        unsafe {
            match inst {
                MirInst::Assign { local, value } => {
                    self.debug_log(&format!("codegen_inst assign {}", self.rvalue_tag(value)));
                    let val = self.codegen_rvalue_for_local(
                        value,
                        func,
                        local_map,
                        functions,
                        mir_module,
                        Some(*local),
                    )?;
                    let local_ty = func
                        .locals
                        .get(local.0 as usize)
                        .and_then(|l| l.ty.as_ref());
                    let is_void = matches!(local_ty, Some(Type::Void))
                        || matches!(local_ty, Some(Type::Tuple(elem_types)) if elem_types.is_empty());
                    if !is_void {
                        let local_ptr = local_map
                            .get(local)
                            .ok_or_else(|| anyhow!("undefined local {:?}", local))?;
                        let target_ty = self.local_llvm_type(func, *local)?;
                        // Extension follows the RVALUE's own source
                        // signedness when one is available (a plain move or
                        // deref), not the destination local's declared type
                        // (GLYPH-73). Other rvalue kinds (Binary, Call,
                        // Cast, literals, ...) already evaluate to their
                        // destination's natural width, so fall back to the
                        // old destination-derived rule for those.
                        let src_ty = self.rvalue_source_type(value, func);
                        let signed = src_ty.as_ref().map_or_else(
                            || {
                                matches!(
                                    local_ty,
                                    Some(Type::I8 | Type::I16 | Type::I32 | Type::I64)
                                )
                            },
                            |ty| Self::int_ext_is_signed(ty),
                        );
                        let val = self.coerce_int_value(val, target_ty, signed);
                        let store = LLVMBuildStore(self.builder, val, *local_ptr);
                        if let Some(Type::Atomic(scalar)) = local_ty {
                            // AtomicNew initializes private storage. Every
                            // later wrapper move remains an atomic access.
                            if !matches!(value, Rvalue::AtomicNew { .. }) {
                                LLVMSetOrdering(
                                    store,
                                    llvm_sys::LLVMAtomicOrdering::LLVMAtomicOrderingSequentiallyConsistent,
                                );
                            }
                            LLVMSetAlignment(store, self.atomic_alignment(*scalar)?);
                        }
                    }
                }
                MirInst::AssignField {
                    base,
                    field_index,
                    value,
                    ..
                } => {
                    self.debug_log(&format!(
                        "codegen_inst assign_field {}",
                        self.rvalue_tag(value)
                    ));
                    let val = self.codegen_rvalue(value, func, local_map, functions, mir_module)?;

                    let (struct_name, struct_ptr) =
                        self.struct_pointer_for_local(*base, func, local_map)?;
                    let llvm_struct = self.get_struct_type(struct_name.as_str())?;
                    let field_ty = {
                        let layout = self
                            .struct_layouts
                            .get(&struct_name)
                            .ok_or_else(|| anyhow!("missing layout for struct {}", struct_name))?;
                        layout
                            .fields
                            .get(*field_index as usize)
                            .map(|(_, ty)| ty.clone())
                            .ok_or_else(|| {
                                anyhow!("invalid field index {} for {}", field_index, struct_name)
                            })?
                    };

                    let is_void = matches!(field_ty, Type::Void)
                        || matches!(&field_ty, Type::Tuple(elem_types) if elem_types.is_empty());
                    if !is_void {
                        let gep_name =
                            CString::new(format!("{}.field{}", struct_name, field_index))?;
                        let field_ptr = LLVMBuildStructGEP2(
                            self.builder,
                            llvm_struct,
                            struct_ptr,
                            *field_index,
                            gep_name.as_ptr(),
                        );
                        let llvm_field_ty = self.get_llvm_type(&field_ty)?;
                        // See the Assign case above: extend per the
                        // rvalue's own source signedness when known
                        // (GLYPH-73), else fall back to the field's type.
                        let src_ty = self.rvalue_source_type(value, func);
                        let signed = src_ty.as_ref().map_or_else(
                            || matches!(field_ty, Type::I8 | Type::I16 | Type::I32 | Type::I64),
                            |ty| Self::int_ext_is_signed(ty),
                        );
                        let val = self.coerce_int_value(val, llvm_field_ty, signed);
                        LLVMBuildStore(self.builder, val, field_ptr);
                    }
                }
                MirInst::AssignIndex { base, index, value } => {
                    self.debug_log(&format!(
                        "codegen_inst assign_index {}",
                        self.rvalue_tag(value)
                    ));

                    let base_ty = func
                        .locals
                        .get(base.0 as usize)
                        .and_then(|l| l.ty.clone())
                        .ok_or_else(|| anyhow!("indexed assignment base has unknown type"))?;
                    let mut inner_ty = &base_ty;
                    while let Type::Ref(inner, _) = inner_ty {
                        inner_ty = inner.as_ref();
                    }

                    // Resolve the element type and a pointer to the element slot.
                    let (elem_ty, elem_ptr) = match inner_ty {
                        Type::Array(elem, size) => {
                            let elem = elem.as_ref().clone();
                            let size = *size;
                            let slot = *local_map
                                .get(base)
                                .ok_or_else(|| anyhow!("undefined local {:?}", base))?;
                            // A reference local's slot holds a pointer to the
                            // array; a direct local's slot IS the array alloca.
                            let array_ptr = if matches!(base_ty, Type::Ref(_, _)) {
                                let ref_llvm_ty = self.get_llvm_type(&base_ty)?;
                                LLVMBuildLoad2(
                                    self.builder,
                                    ref_llvm_ty,
                                    slot,
                                    CString::new("idx.assign.deref")?.as_ptr(),
                                )
                            } else {
                                slot
                            };
                            let llvm_array_ty = self.get_llvm_type(inner_ty)?;
                            let mut index_val = self.codegen_value(index, func, local_map)?;
                            // Array bounds checks compare in i32.
                            let i32_index_ty = LLVMInt32TypeInContext(self.context);
                            index_val = self.coerce_int_value(index_val, i32_index_ty, true);
                            self.emit_bounds_check(index_val, size)?;
                            let i32_ty = LLVMInt32TypeInContext(self.context);
                            let zero = LLVMConstInt(i32_ty, 0, 0);
                            let mut indices = vec![zero, index_val];
                            let elem_ptr = LLVMBuildInBoundsGEP2(
                                self.builder,
                                llvm_array_ty,
                                array_ptr,
                                indices.as_mut_ptr(),
                                indices.len() as u32,
                                CString::new("idx.assign.elem")?.as_ptr(),
                            );
                            (elem, elem_ptr)
                        }
                        Type::App { base: b, args } if b == "Vec" || b.ends_with("::Vec") => {
                            let elem = args
                                .first()
                                .cloned()
                                .ok_or_else(|| anyhow!("Vec type missing element type"))?;
                            let ptr = self.codegen_vec_index_ref(
                                *base, &elem, index, true, func, local_map,
                            )?;
                            (elem, ptr)
                        }
                        Type::Named(name) if name.starts_with("Vec$") => {
                            let elem = {
                                let layout = self.struct_layouts.get(name).ok_or_else(|| {
                                    anyhow!("missing vec layout for {}", name)
                                })?;
                                match &layout.fields.first() {
                                    Some((_, Type::RawPtr(elem))) => elem.as_ref().clone(),
                                    _ => bail!("malformed Vec layout for {}", name),
                                }
                            };
                            let ptr = self.codegen_vec_index_ref(
                                *base, &elem, index, true, func, local_map,
                            )?;
                            (elem, ptr)
                        }
                        other => bail!(
                            "indexed assignment target must be a Vec or array, got {:?}",
                            other
                        ),
                    };

                    // Deep-clone non-owning views entering the container, then
                    // drop the element being overwritten so droppable payloads
                    // free exactly once.
                    let mut val = if let Rvalue::Move(src) = value {
                        let is_view = func
                            .locals
                            .get(src.0 as usize)
                            .map_or(false, |l| l.skip_drop);
                        let raw = self.codegen_rvalue(value, func, local_map, functions, mir_module)?;
                        if is_view && Self::type_needs_clone(&elem_ty) {
                            self.codegen_deep_clone_value(&elem_ty, raw)?
                        } else {
                            raw
                        }
                    } else {
                        self.codegen_rvalue(value, func, local_map, functions, mir_module)?
                    };

                    if Self::field_type_has_drop_glue(&elem_ty) {
                        self.codegen_drop_elem_slot(elem_ptr, &elem_ty)?;
                    }

                    let llvm_elem_ty = self.get_llvm_type(&elem_ty)?;
                    // See the Assign case above: extend per the rvalue's
                    // own source signedness when known (GLYPH-73), else
                    // fall back to the element's type.
                    let src_ty = self.rvalue_source_type(value, func);
                    let signed = src_ty.as_ref().map_or_else(
                        || matches!(elem_ty, Type::I8 | Type::I16 | Type::I32 | Type::I64),
                        |ty| Self::int_ext_is_signed(ty),
                    );
                    val = self.coerce_int_value(val, llvm_elem_ty, signed);
                    // Width-coerce float stores (f64 literal into f32 slot).
                    let val_kind = LLVMGetTypeKind(LLVMTypeOf(val));
                    let elem_kind = LLVMGetTypeKind(llvm_elem_ty);
                    if Self::is_float_type_kind(val_kind)
                        && Self::is_float_type_kind(elem_kind)
                        && LLVMTypeOf(val) != llvm_elem_ty
                    {
                        val = if matches!(elem_ty, Type::F64) {
                            LLVMBuildFPExt(
                                self.builder,
                                val,
                                llvm_elem_ty,
                                CString::new("idx.assign.fpext")?.as_ptr(),
                            )
                        } else {
                            LLVMBuildFPTrunc(
                                self.builder,
                                val,
                                llvm_elem_ty,
                                CString::new("idx.assign.fptrunc")?.as_ptr(),
                            )
                        };
                    }
                    LLVMBuildStore(self.builder, val, elem_ptr);
                }
                MirInst::Return(val) => {
                    // Returning a non-owning view (skip_drop) hands ownership
                    // to the caller; deep-clone so the caller's drop doesn't
                    // free container-owned data.
                    let returns_view = |func: &MirFunction, v: &Option<MirValue>| {
                        matches!(v, Some(MirValue::Local(id))
                            if func.locals.get(id.0 as usize).map_or(false, |l| l.skip_drop))
                    };
                    if let Some(sret_ptr) = sret_ptr {
                        if let Some(v) = val {
                            let mut ret_val = self.codegen_value(v, func, local_map)?;
                            if returns_view(func, val) {
                                if let Some(ret_ty) = func.ret_type.as_ref() {
                                    if Self::type_needs_clone(ret_ty) {
                                        ret_val = self.codegen_deep_clone_value(ret_ty, ret_val)?;
                                    }
                                }
                            }
                            LLVMBuildStore(self.builder, ret_val, sret_ptr);
                        } else if let Some(ret_ty) = func.ret_type.as_ref() {
                            let llvm_ret_ty = self.get_llvm_type(ret_ty)?;
                            let zero = LLVMConstNull(llvm_ret_ty);
                            LLVMBuildStore(self.builder, zero, sret_ptr);
                        }
                        LLVMBuildRetVoid(self.builder);
                    } else if let Some(v) = val {
                        let mut ret_val = self.codegen_value(v, func, local_map)?;
                        if returns_view(func, val) {
                            if let Some(ret_ty) = func.ret_type.as_ref() {
                                if Self::type_needs_clone(ret_ty) {
                                    ret_val = self.codegen_deep_clone_value(ret_ty, ret_val)?;
                                }
                            }
                        }
                        if let Some(ret_ty) = func.ret_type.as_ref() {
                            let llvm_ret_ty = self.get_llvm_type(ret_ty)?;
                            // Extension follows the RETURNED VALUE's own
                            // signedness, not the declared return type's
                            // (GLYPH-73).
                            let src_ty = self.mir_value_type(v, func);
                            let signed = src_ty.as_ref().map_or_else(
                                || matches!(ret_ty, Type::I8 | Type::I16 | Type::I32 | Type::I64),
                                |ty| Self::int_ext_is_signed(ty),
                            );
                            ret_val = self.coerce_int_value(ret_val, llvm_ret_ty, signed);
                        }
                        LLVMBuildRet(self.builder, ret_val);
                    } else {
                        match func.ret_type.as_ref() {
                            None | Some(Type::Void) => {
                                LLVMBuildRetVoid(self.builder);
                            }
                            Some(Type::Tuple(elem_types)) if elem_types.is_empty() => {
                                LLVMBuildRetVoid(self.builder);
                            }
                            Some(ret_ty) => {
                                let llvm_ret_ty = self.get_llvm_type(ret_ty)?;
                                let zero = LLVMConstNull(llvm_ret_ty);
                                LLVMBuildRet(self.builder, zero);
                            }
                        }
                    }
                }
                MirInst::Goto(target) => {
                    let target_bb = bb_map
                        .get(target)
                        .ok_or_else(|| anyhow!("undefined block {:?}", target))?;
                    LLVMBuildBr(self.builder, *target_bb);
                }
                MirInst::If {
                    cond,
                    then_bb,
                    else_bb,
                } => {
                    let cond_val = self.codegen_value(cond, func, local_map)?;
                    // Ensure condition is i1 for LLVMBuildCondBr.
                    // Bare booleans loaded from untyped locals come back as i32;
                    // truncate to i1 so LLVM doesn't reject the branch.
                    let i1_ty = LLVMInt1TypeInContext(self.context);
                    let cond_val = if LLVMTypeOf(cond_val) != i1_ty {
                        let name = std::ffi::CString::new("bool.trunc").unwrap();
                        LLVMBuildTrunc(self.builder, cond_val, i1_ty, name.as_ptr())
                    } else {
                        cond_val
                    };
                    let then_block = bb_map
                        .get(then_bb)
                        .ok_or_else(|| anyhow!("undefined block {:?}", then_bb))?;
                    let else_block = bb_map
                        .get(else_bb)
                        .ok_or_else(|| anyhow!("undefined block {:?}", else_bb))?;
                    LLVMBuildCondBr(self.builder, cond_val, *then_block, *else_block);
                }
                MirInst::Drop(local) => {
                    self.codegen_drop_local(*local, func, local_map)?;
                }
                MirInst::DropThreadHandle(handle) => {
                    self.codegen_drop_thread_handle(*handle, func, local_map)?;
                }
                MirInst::DropThreadScope(scope) => {
                    self.codegen_drop_thread_scope(*scope, func, local_map)?;
                }
                MirInst::DrainThreadScope(scope) => {
                    self.codegen_drain_thread_scope(*scope, func, local_map)?;
                }
                MirInst::DropScopedThreadHandle(handle) => {
                    self.codegen_drop_scoped_thread_handle(*handle, func, local_map)?;
                }
                MirInst::Nop => {}
            }
        }
        Ok(())
    }
}
