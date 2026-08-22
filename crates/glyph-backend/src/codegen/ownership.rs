use super::*;

impl CodegenContext {
    fn type_leaf_name(name: &str) -> &str {
        name.rsplit("::").next().unwrap_or(name)
    }

    pub(super) fn codegen_drop_local(
        &mut self,
        local: LocalId,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<()> {
        let ty = match func
            .locals
            .get(local.0 as usize)
            .and_then(|l| l.ty.as_ref())
        {
            Some(ty) => ty,
            None => return Ok(()),
        };
        match ty {
            Type::Own(inner) => {
                let slot = local_map
                    .get(&local)
                    .ok_or_else(|| anyhow!("undefined local {:?}", local))?;
                self.codegen_drop_own_slot(*slot, inner)
            }
            Type::Shared(inner) => {
                let slot = local_map
                    .get(&local)
                    .ok_or_else(|| anyhow!("undefined local {:?}", local))?;
                self.codegen_drop_shared_slot(*slot, inner)
            }
            Type::String => {
                let slot = local_map
                    .get(&local)
                    .ok_or_else(|| anyhow!("undefined local {:?}", local))?;
                self.codegen_drop_string_slot(*slot)
            }
            Type::App { base, args } if base == "Vec" => {
                let elem = args.get(0).cloned().unwrap_or(Type::I32);
                self.codegen_drop_vec(local, &elem, func, local_map)
            }
            Type::App { base, args } if base == "Map" => {
                let key_ty = args.get(0).cloned().unwrap_or(Type::I32);
                let val_ty = args.get(1).cloned().unwrap_or(Type::I32);
                self.codegen_drop_map(local, &key_ty, &val_ty, func, local_map)
            }
            Type::Named(name) => {
                let slot = local_map
                    .get(&local)
                    .ok_or_else(|| anyhow!("undefined local {:?}", local))?;
                self.codegen_drop_named_slot(*slot, name)
            }
            Type::Enum(name) => {
                let slot = local_map
                    .get(&local)
                    .ok_or_else(|| anyhow!("undefined local {:?}", local))?;
                self.codegen_drop_enum_slot(*slot, name)
            }
            Type::Function { .. } => {
                let slot = local_map
                    .get(&local)
                    .ok_or_else(|| anyhow!("undefined local {:?}", local))?;
                self.codegen_drop_callable_slot(*slot, ty)
            }
            _ => Ok(()),
        }
    }

    pub(super) fn codegen_drop_callable_slot(
        &mut self,
        slot: LLVMValueRef,
        callable_ty: &Type,
    ) -> Result<()> {
        let carrier_ty = self.get_llvm_type(callable_ty)?;
        unsafe {
            let carrier = LLVMBuildLoad2(
                self.builder,
                carrier_ty,
                slot,
                CString::new("callable.drop.load")?.as_ptr(),
            );
            let env = LLVMBuildExtractValue(
                self.builder,
                carrier,
                0,
                CString::new("callable.drop.env")?.as_ptr(),
            );
            let drop_fn = LLVMBuildExtractValue(
                self.builder,
                carrier,
                2,
                CString::new("callable.drop.fn")?.as_ptr(),
            );
            // Clear first: a drop thunk that re-enters cleanup cannot observe
            // an owned callable twice.
            LLVMBuildStore(self.builder, LLVMConstNull(carrier_ty), slot);

            let parent = LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder));
            if parent.is_null() {
                bail!("callable drop parent function missing");
            }
            let call_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("callable.drop.call")?.as_ptr(),
            );
            let done_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("callable.drop.done")?.as_ptr(),
            );
            let is_null = LLVMBuildIsNull(
                self.builder,
                drop_fn,
                CString::new("callable.drop.isnull")?.as_ptr(),
            );
            LLVMBuildCondBr(self.builder, is_null, done_bb, call_bb);

            LLVMPositionBuilderAtEnd(self.builder, call_bb);
            let ptr_ty = LLVMPointerType(LLVMInt8TypeInContext(self.context), 0);
            let mut drop_params = [ptr_ty];
            let drop_ty = LLVMFunctionType(
                LLVMVoidTypeInContext(self.context),
                drop_params.as_mut_ptr(),
                1,
                0,
            );
            let mut args = [env];
            LLVMBuildCall2(
                self.builder,
                drop_ty,
                drop_fn,
                args.as_mut_ptr(),
                1,
                CString::new("")?.as_ptr(),
            );
            LLVMBuildBr(self.builder, done_bb);
            LLVMPositionBuilderAtEnd(self.builder, done_bb);
        }
        Ok(())
    }

    pub(super) fn codegen_drop_own_slot(&mut self, slot: LLVMValueRef, inner: &Type) -> Result<()> {
        unsafe {
            let ptr_ty = self.get_llvm_type(&Type::Own(Box::new(inner.clone())))?;
            let load_name = CString::new("own.ptr")?;
            let ptr_val = LLVMBuildLoad2(self.builder, ptr_ty, slot, load_name.as_ptr());
            let null_ptr = LLVMConstPointerNull(ptr_ty);
            let cmp_name = CString::new("own.isnull")?;
            let is_null = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                ptr_val,
                null_ptr,
                cmp_name.as_ptr(),
            );

            let current_bb = LLVMGetInsertBlock(self.builder);
            if current_bb.is_null() {
                bail!("builder not positioned in a block for drop");
            }
            let parent_func = LLVMGetBasicBlockParent(current_bb);
            if parent_func.is_null() {
                bail!("drop parent function missing");
            }

            let drop_name = CString::new("own.drop")?;
            let cont_name = CString::new("own.drop.cont")?;
            let drop_bb =
                LLVMAppendBasicBlockInContext(self.context, parent_func, drop_name.as_ptr());
            let cont_bb =
                LLVMAppendBasicBlockInContext(self.context, parent_func, cont_name.as_ptr());

            LLVMBuildCondBr(self.builder, is_null, cont_bb, drop_bb);

            LLVMPositionBuilderAtEnd(self.builder, drop_bb);
            self.codegen_free(ptr_val)?;
            LLVMBuildStore(self.builder, null_ptr, slot);
            LLVMBuildBr(self.builder, cont_bb);

            LLVMPositionBuilderAtEnd(self.builder, cont_bb);
        }
        Ok(())
    }

    pub(super) fn codegen_drop_shared_slot(
        &mut self,
        slot: LLVMValueRef,
        elem_type: &Type,
    ) -> Result<()> {
        unsafe {
            // 1. Load pointer
            let ptr_ty = self.get_llvm_type(&Type::Shared(Box::new(elem_type.clone())))?;
            let load_name = CString::new("shared.drop.load")?;
            let ptr_val = LLVMBuildLoad2(self.builder, ptr_ty, slot, load_name.as_ptr());

            // 2. Null check (pointer might already be consumed)
            let null_ptr = LLVMConstPointerNull(ptr_ty);
            let is_null_name = CString::new("shared.drop.is_null")?;
            let is_null = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                ptr_val,
                null_ptr,
                is_null_name.as_ptr(),
            );

            // 3. Create basic blocks
            let func = LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder));
            let dec_bb = LLVMAppendBasicBlockInContext(
                self.context,
                func,
                CString::new("shared.dec")?.as_ptr(),
            );
            let free_bb = LLVMAppendBasicBlockInContext(
                self.context,
                func,
                CString::new("shared.free")?.as_ptr(),
            );
            let cont_bb = LLVMAppendBasicBlockInContext(
                self.context,
                func,
                CString::new("shared.cont")?.as_ptr(),
            );

            // If null, skip to continue
            LLVMBuildCondBr(self.builder, is_null, cont_bb, dec_bb);

            // 4. Decrement block
            LLVMPositionBuilderAtEnd(self.builder, dec_bb);

            // Get struct type
            let usize_ty = LLVMInt64TypeInContext(self.context);
            let elem_llvm_ty = self.get_llvm_type(elem_type)?;
            let mut field_tys = vec![usize_ty, elem_llvm_ty];
            let rc_struct = LLVMStructTypeInContext(self.context, field_tys.as_mut_ptr(), 2, 0);

            // Get refcount pointer
            let refcount_ptr = LLVMBuildStructGEP2(
                self.builder,
                rc_struct,
                ptr_val,
                0,
                CString::new("shared.rc.ptr")?.as_ptr(),
            );

            // Load old refcount
            let old_count = LLVMBuildLoad2(
                self.builder,
                usize_ty,
                refcount_ptr,
                CString::new("shared.rc.old")?.as_ptr(),
            );

            // Decrement refcount
            let one = LLVMConstInt(usize_ty, 1, 0);
            let new_count = LLVMBuildSub(
                self.builder,
                old_count,
                one,
                CString::new("shared.rc.new")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, new_count, refcount_ptr);

            // Check if refcount reached zero
            let zero = LLVMConstInt(usize_ty, 0, 0);
            let is_zero = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                new_count,
                zero,
                CString::new("shared.rc.is_zero")?.as_ptr(),
            );

            // If zero, free; else continue
            LLVMBuildCondBr(self.builder, is_zero, free_bb, cont_bb);

            // 5. Free block (refcount is zero)
            LLVMPositionBuilderAtEnd(self.builder, free_bb);
            self.codegen_free(ptr_val)?;

            // Set slot to null
            LLVMBuildStore(self.builder, null_ptr, slot);
            LLVMBuildBr(self.builder, cont_bb);

            // 6. Continue block
            LLVMPositionBuilderAtEnd(self.builder, cont_bb);

            Ok(())
        }
    }

    pub(super) fn codegen_drop_string_slot(&mut self, slot: LLVMValueRef) -> Result<()> {
        unsafe {
            let ptr_ty = self.get_llvm_type(&Type::String)?;
            let load_name = CString::new("string.ptr")?;
            let ptr_val = LLVMBuildLoad2(self.builder, ptr_ty, slot, load_name.as_ptr());
            let null_ptr = LLVMConstPointerNull(ptr_ty);
            let cmp_name = CString::new("string.isnull")?;
            let is_null = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                ptr_val,
                null_ptr,
                cmp_name.as_ptr(),
            );

            let func = LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder));
            let drop_bb = LLVMAppendBasicBlockInContext(
                self.context,
                func,
                CString::new("string.drop")?.as_ptr(),
            );
            let cont_bb = LLVMAppendBasicBlockInContext(
                self.context,
                func,
                CString::new("string.drop.cont")?.as_ptr(),
            );

            LLVMBuildCondBr(self.builder, is_null, cont_bb, drop_bb);

            LLVMPositionBuilderAtEnd(self.builder, drop_bb);
            self.codegen_free(ptr_val)?;
            LLVMBuildStore(self.builder, null_ptr, slot);
            LLVMBuildBr(self.builder, cont_bb);

            LLVMPositionBuilderAtEnd(self.builder, cont_bb);
        }
        Ok(())
    }

    pub(super) fn codegen_drop_file_slot(&mut self, slot: LLVMValueRef) -> Result<()> {
        unsafe {
            let file_ty = self.get_struct_type("File")?;
            let layout = self
                .struct_layouts
                .get("File")
                .ok_or_else(|| anyhow!("missing layout for struct File"))?;
            let handle_field = layout
                .fields
                .first()
                .ok_or_else(|| anyhow!("missing File handle field"))?;
            let handle_ty = self.get_llvm_type(&handle_field.1)?;
            let handle_ptr = LLVMBuildStructGEP2(
                self.builder,
                file_ty,
                slot,
                0,
                CString::new("file.handle")?.as_ptr(),
            );
            let handle_val = LLVMBuildLoad2(
                self.builder,
                handle_ty,
                handle_ptr,
                CString::new("file.handle.load")?.as_ptr(),
            );
            let null_ptr = LLVMConstNull(handle_ty);
            let is_null = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                handle_val,
                null_ptr,
                CString::new("file.handle.null")?.as_ptr(),
            );

            let func = LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder));
            let drop_bb = LLVMAppendBasicBlockInContext(
                self.context,
                func,
                CString::new("file.drop")?.as_ptr(),
            );
            let cont_bb = LLVMAppendBasicBlockInContext(
                self.context,
                func,
                CString::new("file.drop.cont")?.as_ptr(),
            );
            LLVMBuildCondBr(self.builder, is_null, cont_bb, drop_bb);

            LLVMPositionBuilderAtEnd(self.builder, drop_bb);
            let fclose_fn = LLVMGetNamedFunction(self.module, CString::new("fclose")?.as_ptr());
            if !fclose_fn.is_null() {
                let mut args = vec![handle_val];
                if let Ok(fn_ty) = self.function_type_for("fclose") {
                    LLVMBuildCall2(
                        self.builder,
                        fn_ty,
                        fclose_fn,
                        args.as_mut_ptr(),
                        args.len() as u32,
                        CString::new("file.drop.close")?.as_ptr(),
                    );
                }
            }
            LLVMBuildStore(self.builder, null_ptr, handle_ptr);
            LLVMBuildBr(self.builder, cont_bb);

            LLVMPositionBuilderAtEnd(self.builder, cont_bb);
        }
        Ok(())
    }

    pub(super) fn codegen_drop_ui_session_guard_slot(
        &mut self,
        slot: LLVMValueRef,
        struct_name: &str,
    ) -> Result<()> {
        unsafe {
            let guard_ty = self.get_struct_type(struct_name)?;
            let layout = self
                .struct_layouts
                .get(struct_name)
                .ok_or_else(|| anyhow!("missing layout for struct {}", struct_name))?;

            let terminal_field_index = layout
                .fields
                .iter()
                .position(|(field_name, _)| field_name == "terminal_id")
                .ok_or_else(|| {
                    anyhow!(
                        "missing 'terminal_id' field on UiSessionGuard layout '{}'",
                        struct_name
                    )
                })?;
            let active_field_index = layout
                .fields
                .iter()
                .position(|(field_name, _)| field_name == "active")
                .ok_or_else(|| {
                    anyhow!(
                        "missing 'active' field on UiSessionGuard layout '{}'",
                        struct_name
                    )
                })?;

            let terminal_ty = self.get_llvm_type(&layout.fields[terminal_field_index].1)?;
            let active_ty = self.get_llvm_type(&layout.fields[active_field_index].1)?;

            let terminal_ptr = LLVMBuildStructGEP2(
                self.builder,
                guard_ty,
                slot,
                terminal_field_index as u32,
                CString::new("term.guard.terminal.ptr")?.as_ptr(),
            );
            let active_ptr = LLVMBuildStructGEP2(
                self.builder,
                guard_ty,
                slot,
                active_field_index as u32,
                CString::new("term.guard.active.ptr")?.as_ptr(),
            );
            let active_val = LLVMBuildLoad2(
                self.builder,
                active_ty,
                active_ptr,
                CString::new("term.guard.active.load")?.as_ptr(),
            );
            let is_inactive = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                active_val,
                LLVMConstInt(active_ty, 0, 0),
                CString::new("term.guard.inactive")?.as_ptr(),
            );

            let func = LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder));
            let drop_bb = LLVMAppendBasicBlockInContext(
                self.context,
                func,
                CString::new("term.guard.drop")?.as_ptr(),
            );
            let cont_bb = LLVMAppendBasicBlockInContext(
                self.context,
                func,
                CString::new("term.guard.drop.cont")?.as_ptr(),
            );
            LLVMBuildCondBr(self.builder, is_inactive, cont_bb, drop_bb);

            LLVMPositionBuilderAtEnd(self.builder, drop_bb);
            let end_fn = LLVMGetNamedFunction(
                self.module,
                CString::new("glyph_term_session_end")?.as_ptr(),
            );
            if !end_fn.is_null() {
                let terminal_id = LLVMBuildLoad2(
                    self.builder,
                    terminal_ty,
                    terminal_ptr,
                    CString::new("term.guard.terminal.load")?.as_ptr(),
                );
                let mut args = vec![terminal_id];
                if let Ok(fn_ty) = self.function_type_for("glyph_term_session_end") {
                    LLVMBuildCall2(
                        self.builder,
                        fn_ty,
                        end_fn,
                        args.as_mut_ptr(),
                        args.len() as u32,
                        CString::new("term.guard.drop.end")?.as_ptr(),
                    );
                }
            }
            LLVMBuildStore(self.builder, LLVMConstInt(active_ty, 0, 0), active_ptr);
            LLVMBuildBr(self.builder, cont_bb);

            LLVMPositionBuilderAtEnd(self.builder, cont_bb);
        }
        Ok(())
    }

    pub(super) fn codegen_drop_named_slot(&mut self, slot: LLVMValueRef, name: &str) -> Result<()> {
        if self.enum_layouts.contains_key(name) {
            return self.codegen_drop_enum_slot(slot, name);
        }

        let leaf_name = Self::type_leaf_name(name);

        // Runtime-managed RAII type: ensure file handles are closed on scope exit
        // when users rely on implicit cleanup instead of explicit `close()`.
        if leaf_name == "File" {
            return self.codegen_drop_file_slot(slot);
        }
        if leaf_name == "UiSessionGuard" {
            return self.codegen_drop_ui_session_guard_slot(slot, name);
        }

        let layout = match self.struct_layouts.get(name) {
            Some(l) => l.clone(),
            None => return Ok(()),
        };

        // Detect Vec by name prefix (Vec$ and Map$ both have 3 fields
        // with RawPtr first, so we can't distinguish by layout alone).
        if name.starts_with("Vec$") {
            if layout.fields.len() == 3 {
                if let Type::RawPtr(ref elem_type) = layout.fields[0].1 {
                    return self.codegen_drop_vec_from_ptr(slot, name, elem_type);
                }
            }
        }

        if name.starts_with("Map$") {
            let (key_type, value_type) = self.map_key_value_types_from_map(name)?;
            return self.codegen_drop_map_from_ptr(slot, name, &key_type, &value_type);
        }

        // Generic struct: iterate fields and recursively drop each
        let llvm_struct = self.get_struct_type(name)?;
        for (field_index, (_field_name, field_type)) in layout.fields.iter().enumerate() {
            if !Self::field_type_has_drop_glue(field_type) {
                continue;
            }
            let gep_name = CString::new(format!("{}.drop.field{}", name, field_index))?;
            let field_ptr = unsafe {
                LLVMBuildStructGEP2(
                    self.builder,
                    llvm_struct,
                    slot,
                    field_index as u32,
                    gep_name.as_ptr(),
                )
            };
            self.codegen_drop_elem_slot(field_ptr, field_type)?;
        }
        Ok(())
    }

    pub(super) fn field_type_has_drop_glue(ty: &Type) -> bool {
        matches!(
            ty,
            Type::Own(_)
                | Type::Shared(_)
                | Type::String
                | Type::Named(_)
                | Type::Enum(_)
                | Type::Function { .. }
                | Type::App { .. }
        )
    }

    pub(super) fn type_display_for_mono(ty: &Type) -> String {
        match ty {
            Type::String => "String".to_string(),
            Type::I8 => "i8".to_string(),
            Type::I32 => "i32".to_string(),
            Type::I64 => "i64".to_string(),
            Type::U8 => "u8".to_string(),
            Type::U32 => "u32".to_string(),
            Type::U64 => "u64".to_string(),
            Type::Usize => "usize".to_string(),
            Type::F32 => "f32".to_string(),
            Type::F64 => "f64".to_string(),
            Type::Bool => "bool".to_string(),
            Type::Named(n) => n.clone(),
            _ => format!("{:?}", ty),
        }
    }

    pub(super) fn codegen_drop_elem_slot(
        &mut self,
        slot: LLVMValueRef,
        elem_type: &Type,
    ) -> Result<()> {
        match elem_type {
            Type::Own(inner) => self.codegen_drop_own_slot(slot, inner),
            Type::Shared(inner) => self.codegen_drop_shared_slot(slot, inner),
            Type::String => self.codegen_drop_string_slot(slot),
            Type::Named(name) => self.codegen_drop_named_slot(slot, name),
            Type::Enum(name) => self.codegen_drop_enum_slot(slot, name),
            Type::Function { .. } => self.codegen_drop_callable_slot(slot, elem_type),
            Type::App { base, args } if base == "Vec" => {
                let elem = args.first().cloned().unwrap_or(Type::I32);
                let vec_name = format!("Vec${}", Self::type_display_for_mono(&elem));
                self.codegen_drop_vec_from_ptr(slot, &vec_name, &elem)
            }
            Type::App { base, args } if base == "Map" => {
                let key_type = args.first().cloned().unwrap_or(Type::I32);
                let value_type = args.get(1).cloned().unwrap_or(Type::I32);
                let map_name = format!(
                    "Map${}__{}",
                    self.type_key(&key_type),
                    self.type_key(&value_type)
                );
                self.codegen_drop_map_from_ptr(slot, &map_name, &key_type, &value_type)
            }
            _ => Ok(()),
        }
    }

    pub(super) fn codegen_drop_enum_slot(
        &mut self,
        slot: LLVMValueRef,
        enum_name: &str,
    ) -> Result<()> {
        let guard_key = format!("enum:{}", enum_name);
        if !self.drop_in_progress.insert(guard_key.clone()) {
            return Ok(());
        }
        let result = (|| {
            let layout = match self.enum_layouts.get(enum_name) {
                Some(layout) => layout.clone(),
                None => return Ok(()),
            };

            let droppable_variants: Vec<(u32, Type)> = layout
                .variants
                .iter()
                .enumerate()
                .filter_map(|(index, variant)| {
                    let payload = variant.payload.clone()?;
                    if Self::field_type_has_drop_glue(&payload) {
                        Some((index as u32, payload))
                    } else {
                        None
                    }
                })
                .collect();
            if droppable_variants.is_empty() {
                return Ok(());
            }

            let llvm_enum = self.get_enum_type(enum_name)?;
            let tag_ptr = unsafe {
                LLVMBuildStructGEP2(
                    self.builder,
                    llvm_enum,
                    slot,
                    0,
                    CString::new("enum.drop.tag")?.as_ptr(),
                )
            };
            let tag_val = unsafe {
                LLVMBuildLoad2(
                    self.builder,
                    LLVMInt32TypeInContext(self.context),
                    tag_ptr,
                    CString::new("enum.drop.tag.load")?.as_ptr(),
                )
            };

            let current_bb = unsafe { LLVMGetInsertBlock(self.builder) };
            if current_bb.is_null() {
                bail!("builder not positioned in a block for enum drop");
            }
            let parent_fn = unsafe { LLVMGetBasicBlockParent(current_bb) };
            if parent_fn.is_null() {
                bail!("enum drop parent function missing");
            }

            let mut check_bbs = Vec::new();
            let mut drop_bbs = Vec::new();
            for (variant_index, _) in &droppable_variants {
                check_bbs.push(unsafe {
                    LLVMAppendBasicBlockInContext(
                        self.context,
                        parent_fn,
                        CString::new(format!("enum.drop.check.{}", variant_index))?.as_ptr(),
                    )
                });
                drop_bbs.push(unsafe {
                    LLVMAppendBasicBlockInContext(
                        self.context,
                        parent_fn,
                        CString::new(format!("enum.drop.variant.{}", variant_index))?.as_ptr(),
                    )
                });
            }
            let done_bb = unsafe {
                LLVMAppendBasicBlockInContext(
                    self.context,
                    parent_fn,
                    CString::new("enum.drop.done")?.as_ptr(),
                )
            };

            unsafe { LLVMBuildBr(self.builder, check_bbs[0]) };

            for (i, ((variant_index, payload_type), (&check_bb, &drop_bb))) in droppable_variants
                .iter()
                .zip(check_bbs.iter().zip(drop_bbs.iter()))
                .enumerate()
            {
                unsafe { LLVMPositionBuilderAtEnd(self.builder, check_bb) };
                let cmp = unsafe {
                    LLVMBuildICmp(
                        self.builder,
                        llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                        tag_val,
                        LLVMConstInt(
                            LLVMInt32TypeInContext(self.context),
                            *variant_index as u64,
                            0,
                        ),
                        CString::new(format!("enum.drop.is.{}", variant_index))?.as_ptr(),
                    )
                };
                let next_bb = if i + 1 < check_bbs.len() {
                    check_bbs[i + 1]
                } else {
                    done_bb
                };
                unsafe { LLVMBuildCondBr(self.builder, cmp, drop_bb, next_bb) };

                unsafe { LLVMPositionBuilderAtEnd(self.builder, drop_bb) };
                let payload_ptr = unsafe {
                    LLVMBuildStructGEP2(
                        self.builder,
                        llvm_enum,
                        slot,
                        1 + *variant_index,
                        CString::new(format!("enum.drop.payload.{}", variant_index))?.as_ptr(),
                    )
                };
                self.codegen_drop_elem_slot(payload_ptr, payload_type)?;
                unsafe { LLVMBuildBr(self.builder, done_bb) };
            }

            unsafe { LLVMPositionBuilderAtEnd(self.builder, done_bb) };
            Ok(())
        })();

        self.drop_in_progress.remove(&guard_key);
        result
    }

    pub(super) fn codegen_own_new(
        &mut self,
        elem_type: &Type,
        value: &MirValue,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        unsafe {
            let llvm_elem_ty = self.get_llvm_type(elem_type)?;
            let size_val = LLVMSizeOf(llvm_elem_ty);
            let malloc_fn = self.ensure_malloc_fn()?;
            let malloc_ty = self.malloc_function_type();
            let mut args = vec![size_val];
            let call_name = CString::new("own.alloc")?;
            let raw_ptr = LLVMBuildCall2(
                self.builder,
                malloc_ty,
                malloc_fn,
                args.as_mut_ptr(),
                args.len() as u32,
                call_name.as_ptr(),
            );
            let typed_ptr_ty = LLVMPointerType(llvm_elem_ty, 0);
            let cast_name = CString::new("own.ptr")?;
            let typed_ptr =
                LLVMBuildBitCast(self.builder, raw_ptr, typed_ptr_ty, cast_name.as_ptr());
            let value_val = self.codegen_value(value, func, local_map)?;
            LLVMBuildStore(self.builder, value_val, typed_ptr);
            Ok(typed_ptr)
        }
    }

    pub(super) fn codegen_own_into_raw(
        &mut self,
        base: LocalId,
        elem_type: &Type,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let slot = local_map
            .get(&base)
            .ok_or_else(|| anyhow!("undefined local {:?}", base))?;
        let llvm_ty = self.get_llvm_type(&Type::Own(Box::new(elem_type.clone())))?;
        unsafe {
            let load_name = CString::new("own.into")?;
            let ptr_val = LLVMBuildLoad2(self.builder, llvm_ty, *slot, load_name.as_ptr());
            let null_ptr = LLVMConstPointerNull(llvm_ty);
            LLVMBuildStore(self.builder, null_ptr, *slot);
            Ok(ptr_val)
        }
    }

    pub(super) fn codegen_own_from_raw(
        &mut self,
        ptr: &MirValue,
        elem_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let value = self.codegen_value(ptr, func, local_map)?;
        let desired_ty = self.get_llvm_type(&Type::Own(Box::new(elem_type.clone())))?;
        unsafe {
            if LLVMTypeOf(value) == desired_ty {
                Ok(value)
            } else {
                let cast_name = CString::new("own.from.raw")?;
                Ok(LLVMBuildBitCast(
                    self.builder,
                    value,
                    desired_ty,
                    cast_name.as_ptr(),
                ))
            }
        }
    }

    pub(super) fn codegen_raw_ptr_null(&mut self, elem_type: &Type) -> Result<LLVMValueRef> {
        let ty = self.get_llvm_type(&Type::RawPtr(Box::new(elem_type.clone())))?;
        Ok(unsafe { LLVMConstPointerNull(ty) })
    }

    pub(super) fn codegen_shared_new(
        &mut self,
        elem_type: &Type,
        value: &MirValue,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        unsafe {
            // 1. Create struct type [refcount: usize, data: T]
            let usize_ty = LLVMInt64TypeInContext(self.context);
            let elem_llvm_ty = self.get_llvm_type(elem_type)?;
            let mut field_tys = vec![usize_ty, elem_llvm_ty];
            let rc_struct = LLVMStructTypeInContext(
                self.context,
                field_tys.as_mut_ptr(),
                2,
                0, // not packed
            );

            // 2. Allocate memory
            let size_val = LLVMSizeOf(rc_struct);
            let malloc_fn = self.ensure_malloc_fn()?;
            let malloc_ty = self.malloc_function_type();
            let mut args = vec![size_val];
            let call_name = CString::new("shared.alloc")?;
            let raw_ptr = LLVMBuildCall2(
                self.builder,
                malloc_ty,
                malloc_fn,
                args.as_mut_ptr(),
                args.len() as u32,
                call_name.as_ptr(),
            );

            // 3. Cast to typed pointer
            let typed_ptr_ty = LLVMPointerType(rc_struct, 0);
            let cast_name = CString::new("shared.ptr")?;
            let typed_ptr =
                LLVMBuildBitCast(self.builder, raw_ptr, typed_ptr_ty, cast_name.as_ptr());

            // 4. Initialize refcount to 1
            let refcount_ptr = LLVMBuildStructGEP2(
                self.builder,
                rc_struct,
                typed_ptr,
                0,
                CString::new("shared.rc")?.as_ptr(),
            );
            let one = LLVMConstInt(usize_ty, 1, 0);
            LLVMBuildStore(self.builder, one, refcount_ptr);

            // 5. Store the data value
            let data_ptr = LLVMBuildStructGEP2(
                self.builder,
                rc_struct,
                typed_ptr,
                1,
                CString::new("shared.data")?.as_ptr(),
            );
            let value_val = self.codegen_value(value, func, local_map)?;
            LLVMBuildStore(self.builder, value_val, data_ptr);

            // 6. Return pointer (API returns pointer to whole struct)
            Ok(typed_ptr)
        }
    }

    pub(super) fn codegen_shared_clone(
        &mut self,
        base: LocalId,
        elem_type: &Type,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        unsafe {
            // 1. Load the Shared<T> pointer
            let slot = local_map
                .get(&base)
                .ok_or_else(|| anyhow!("undefined local {:?}", base))?;
            let shared_ty = self.get_llvm_type(&Type::Shared(Box::new(elem_type.clone())))?;
            let load_name = CString::new("shared.load")?;
            let ptr_val = LLVMBuildLoad2(self.builder, shared_ty, *slot, load_name.as_ptr());

            // 2. Get struct type for GEP
            let usize_ty = LLVMInt64TypeInContext(self.context);
            let elem_llvm_ty = self.get_llvm_type(elem_type)?;
            let mut field_tys = vec![usize_ty, elem_llvm_ty];
            let rc_struct = LLVMStructTypeInContext(self.context, field_tys.as_mut_ptr(), 2, 0);

            // 3. Get pointer to refcount field
            let refcount_ptr = LLVMBuildStructGEP2(
                self.builder,
                rc_struct,
                ptr_val,
                0,
                CString::new("shared.rc.ptr")?.as_ptr(),
            );

            // 4. Load current refcount
            let rc_name = CString::new("shared.rc.old")?;
            let old_count = LLVMBuildLoad2(self.builder, usize_ty, refcount_ptr, rc_name.as_ptr());

            // 5. Increment refcount
            let one = LLVMConstInt(usize_ty, 1, 0);
            let inc_name = CString::new("shared.rc.new")?;
            let new_count = LLVMBuildAdd(self.builder, old_count, one, inc_name.as_ptr());
            LLVMBuildStore(self.builder, new_count, refcount_ptr);

            // 6. Return the same pointer (refcount incremented)
            Ok(ptr_val)
        }
    }
}
