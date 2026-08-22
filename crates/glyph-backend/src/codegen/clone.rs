//! Deep-clone glue.
//!
//! Container reads (`Map.get`, `Vec[i]`, field access) return shallow
//! snapshots whose heap payloads are still owned by the container; MIR marks
//! the receiving local `skip_drop` so the view is not independently dropped.
//! That protection cannot follow a value across an ownership boundary (a
//! by-value call argument or a return), so codegen deep-clones the value at
//! those escape points and the receiver owns the copy.
//!
//! Clone glue is emitted as one private LLVM function per type
//! (`glyph.clone.<type_key>` with signature `void(ptr dst, ptr src)`),
//! memoized in `clone_fns` *before* the body is built so recursive types
//! (e.g. JsonValue containing Vec<JsonValue>) simply call back into the
//! function being defined.

use super::*;

impl CodegenContext {
    /// Returns true if passing `ty` by value transfers heap ownership and so
    /// requires a deep clone when the source is a non-owning view.
    pub(super) fn type_needs_clone(ty: &Type) -> bool {
        Self::field_type_has_drop_glue(ty)
    }

    /// Deep-clone `val` (of Glyph type `ty`) into a freshly owned value.
    /// Non-droppable types are returned unchanged.
    pub(super) fn codegen_deep_clone_value(
        &mut self,
        ty: &Type,
        val: LLVMValueRef,
    ) -> Result<LLVMValueRef> {
        if !Self::type_needs_clone(ty) {
            return Ok(val);
        }
        unsafe {
            let llvm_ty = LLVMTypeOf(val);
            let src = LLVMBuildAlloca(self.builder, llvm_ty, CString::new("clone.src")?.as_ptr());
            LLVMBuildStore(self.builder, val, src);
            let dst = LLVMBuildAlloca(self.builder, llvm_ty, CString::new("clone.dst")?.as_ptr());
            self.codegen_clone_slot(dst, src, ty)?;
            Ok(LLVMBuildLoad2(
                self.builder,
                llvm_ty,
                dst,
                CString::new("clone.val")?.as_ptr(),
            ))
        }
    }

    /// Copy `*src` into `*dst`, deep-cloning heap payloads. `dst` and `src`
    /// must not alias.
    pub(super) fn codegen_clone_slot(
        &mut self,
        dst: LLVMValueRef,
        src: LLVMValueRef,
        ty: &Type,
    ) -> Result<()> {
        if !Self::type_needs_clone(ty) {
            // Plain bit copy for non-droppable types.
            let llvm_ty = self.get_llvm_type(ty)?;
            unsafe {
                let v = LLVMBuildLoad2(
                    self.builder,
                    llvm_ty,
                    src,
                    CString::new("clone.raw.load")?.as_ptr(),
                );
                LLVMBuildStore(self.builder, v, dst);
            }
            return Ok(());
        }
        let clone_fn = self.ensure_clone_fn(ty)?;
        let fn_ty = self.clone_fn_type();
        let mut args = vec![dst, src];
        unsafe {
            LLVMBuildCall2(
                self.builder,
                fn_ty,
                clone_fn,
                args.as_mut_ptr(),
                args.len() as u32,
                CString::new("")?.as_ptr(),
            );
        }
        Ok(())
    }

    /// codegen_value, but deep-clones the result when the source local is a
    /// non-owning view (`skip_drop`) and the destination takes ownership.
    /// Use at ownership-transfer sites: container inserts, struct/enum
    /// construction, and by-value argument passing.
    pub(super) fn codegen_value_owned(
        &mut self,
        value: &MirValue,
        ty: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let v = self.codegen_value(value, func, local_map)?;
        if let MirValue::Local(id) = value {
            let is_view = func
                .locals
                .get(id.0 as usize)
                .map_or(false, |l| l.skip_drop);
            if is_view && Self::type_needs_clone(ty) {
                return self.codegen_deep_clone_value(ty, v);
            }
        }
        Ok(v)
    }

    fn clone_fn_type(&self) -> LLVMTypeRef {
        unsafe {
            let ptr_ty = LLVMPointerType(LLVMInt8TypeInContext(self.context), 0);
            let mut params = vec![ptr_ty, ptr_ty];
            LLVMFunctionType(
                LLVMVoidTypeInContext(self.context),
                params.as_mut_ptr(),
                params.len() as u32,
                0,
            )
        }
    }

    /// Resolve `Named` aliases to the structured type they stand for, so all
    /// spellings of a type share one clone function.
    fn canonical_clone_type(&self, ty: &Type) -> Type {
        match ty {
            Type::Named(name) if self.enum_layouts.contains_key(name) => {
                Type::Enum(name.clone())
            }
            _ => ty.clone(),
        }
    }

    fn ensure_clone_fn(&mut self, ty: &Type) -> Result<LLVMValueRef> {
        let ty = self.canonical_clone_type(ty);
        let key = format!("glyph.clone.{}", self.type_key(&ty));
        if let Some(&f) = self.clone_fns.get(&key) {
            return Ok(f);
        }

        let fn_ty = self.clone_fn_type();
        let name_c = CString::new(key.as_str())?;
        let f = unsafe { LLVMAddFunction(self.module, name_c.as_ptr(), fn_ty) };
        unsafe { LLVMSetLinkage(f, LLVMLinkage::LLVMPrivateLinkage) };
        // Memoize before emitting the body so recursive types call back into
        // the function under construction instead of recursing in Rust.
        self.clone_fns.insert(key, f);

        let saved_bb = unsafe { LLVMGetInsertBlock(self.builder) };
        let entry = unsafe {
            LLVMAppendBasicBlockInContext(self.context, f, CString::new("entry")?.as_ptr())
        };
        unsafe { LLVMPositionBuilderAtEnd(self.builder, entry) };

        let dst = unsafe { LLVMGetParam(f, 0) };
        let src = unsafe { LLVMGetParam(f, 1) };
        self.emit_clone_body(dst, src, &ty)?;
        unsafe { LLVMBuildRetVoid(self.builder) };

        if !saved_bb.is_null() {
            unsafe { LLVMPositionBuilderAtEnd(self.builder, saved_bb) };
        }
        Ok(f)
    }

    fn emit_clone_body(&mut self, dst: LLVMValueRef, src: LLVMValueRef, ty: &Type) -> Result<()> {
        match ty {
            Type::String => self.emit_clone_string(dst, src),
            Type::Own(inner) => self.emit_clone_own(dst, src, inner),
            Type::Shared(inner) => self.emit_clone_shared(dst, src, inner),
            Type::Enum(name) => self.emit_clone_enum(dst, src, name),
            Type::App { base, args } if base == "Vec" => {
                let elem = args.first().cloned().unwrap_or(Type::I32);
                let vec_name = format!("Vec${}", Self::type_display_for_mono(&elem));
                self.emit_clone_vec(dst, src, &vec_name, &elem)
            }
            Type::App { base, args } if base == "Map" => {
                let key_type = args.first().cloned().unwrap_or(Type::I32);
                let value_type = args.get(1).cloned().unwrap_or(Type::I32);
                let map_name = format!(
                    "Map${}__{}",
                    self.type_key(&key_type),
                    self.type_key(&value_type)
                );
                self.emit_clone_map(dst, src, &map_name, &key_type, &value_type)
            }
            Type::Named(name) => self.emit_clone_named(dst, src, name),
            Type::Tuple(elem_types) => self.emit_clone_tuple(dst, src, elem_types),
            // Remaining droppable classifications have no structured clone;
            // fall back to a bit copy (RawPtr/Ref/scalars never reach here).
            _ => {
                let llvm_ty = self.get_llvm_type(ty)?;
                unsafe {
                    let v = LLVMBuildLoad2(
                        self.builder,
                        llvm_ty,
                        src,
                        CString::new("clone.bits")?.as_ptr(),
                    );
                    LLVMBuildStore(self.builder, v, dst);
                }
                Ok(())
            }
        }
    }

    fn emit_clone_string(&mut self, dst: LLVMValueRef, src: LLVMValueRef) -> Result<()> {
        unsafe {
            let ptr_ty = LLVMPointerType(LLVMInt8TypeInContext(self.context), 0);
            let s = LLVMBuildLoad2(
                self.builder,
                ptr_ty,
                src,
                CString::new("clone.str.load")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, LLVMConstPointerNull(ptr_ty), dst);

            let is_null = LLVMBuildIsNull(
                self.builder,
                s,
                CString::new("clone.str.isnull")?.as_ptr(),
            );
            let parent_fn = LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder));
            let dup_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("clone.str.dup")?.as_ptr(),
            );
            let done_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("clone.str.done")?.as_ptr(),
            );
            LLVMBuildCondBr(self.builder, is_null, done_bb, dup_bb);

            LLVMPositionBuilderAtEnd(self.builder, dup_bb);
            let strdup_fn = self.ensure_strdup_fn()?;
            let strdup_ty = self.strdup_function_type();
            let mut args = vec![s];
            let dup = LLVMBuildCall2(
                self.builder,
                strdup_ty,
                strdup_fn,
                args.as_mut_ptr(),
                args.len() as u32,
                CString::new("clone.str.strdup")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, dup, dst);
            LLVMBuildBr(self.builder, done_bb);

            LLVMPositionBuilderAtEnd(self.builder, done_bb);
        }
        Ok(())
    }

    fn emit_clone_own(
        &mut self,
        dst: LLVMValueRef,
        src: LLVMValueRef,
        inner: &Type,
    ) -> Result<()> {
        unsafe {
            let ptr_ty = LLVMPointerType(LLVMInt8TypeInContext(self.context), 0);
            let p = LLVMBuildLoad2(
                self.builder,
                ptr_ty,
                src,
                CString::new("clone.own.load")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, LLVMConstPointerNull(ptr_ty), dst);

            let is_null = LLVMBuildIsNull(
                self.builder,
                p,
                CString::new("clone.own.isnull")?.as_ptr(),
            );
            let parent_fn = LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder));
            let copy_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("clone.own.copy")?.as_ptr(),
            );
            let done_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("clone.own.done")?.as_ptr(),
            );
            LLVMBuildCondBr(self.builder, is_null, done_bb, copy_bb);

            LLVMPositionBuilderAtEnd(self.builder, copy_bb);
            let inner_llvm = self.get_llvm_type(inner)?;
            let size_val = LLVMSizeOf(inner_llvm);
            let malloc_fn = self.ensure_malloc_fn()?;
            let malloc_ty = self.malloc_function_type();
            let mut args = vec![size_val];
            let new_box = LLVMBuildCall2(
                self.builder,
                malloc_ty,
                malloc_fn,
                args.as_mut_ptr(),
                args.len() as u32,
                CString::new("clone.own.alloc")?.as_ptr(),
            );
            self.codegen_clone_slot(new_box, p, inner)?;
            LLVMBuildStore(self.builder, new_box, dst);
            LLVMBuildBr(self.builder, done_bb);

            LLVMPositionBuilderAtEnd(self.builder, done_bb);
        }
        Ok(())
    }

    fn emit_clone_shared(
        &mut self,
        dst: LLVMValueRef,
        src: LLVMValueRef,
        inner: &Type,
    ) -> Result<()> {
        unsafe {
            let ptr_ty = LLVMPointerType(LLVMInt8TypeInContext(self.context), 0);
            let p = LLVMBuildLoad2(
                self.builder,
                ptr_ty,
                src,
                CString::new("clone.shared.load")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, p, dst);

            let is_null = LLVMBuildIsNull(
                self.builder,
                p,
                CString::new("clone.shared.isnull")?.as_ptr(),
            );
            let parent_fn = LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder));
            let bump_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("clone.shared.bump")?.as_ptr(),
            );
            let done_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("clone.shared.done")?.as_ptr(),
            );
            LLVMBuildCondBr(self.builder, is_null, done_bb, bump_bb);

            LLVMPositionBuilderAtEnd(self.builder, bump_bb);
            let usize_ty = LLVMInt64TypeInContext(self.context);
            let elem_llvm_ty = self.get_llvm_type(inner)?;
            let mut field_tys = vec![usize_ty, elem_llvm_ty];
            let rc_struct =
                LLVMStructTypeInContext(self.context, field_tys.as_mut_ptr(), 2, 0);
            let rc_ptr = LLVMBuildStructGEP2(
                self.builder,
                rc_struct,
                p,
                0,
                CString::new("clone.shared.rc.ptr")?.as_ptr(),
            );
            let old = LLVMBuildLoad2(
                self.builder,
                usize_ty,
                rc_ptr,
                CString::new("clone.shared.rc.old")?.as_ptr(),
            );
            let one = LLVMConstInt(usize_ty, 1, 0);
            let new = LLVMBuildAdd(
                self.builder,
                old,
                one,
                CString::new("clone.shared.rc.new")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, new, rc_ptr);
            LLVMBuildBr(self.builder, done_bb);

            LLVMPositionBuilderAtEnd(self.builder, done_bb);
        }
        Ok(())
    }

    fn emit_clone_enum(
        &mut self,
        dst: LLVMValueRef,
        src: LLVMValueRef,
        enum_name: &str,
    ) -> Result<()> {
        let layout = match self.enum_layouts.get(enum_name) {
            Some(layout) => layout.clone(),
            None => bail!("missing enum layout for clone of {}", enum_name),
        };
        let llvm_enum = self.get_enum_type(enum_name)?;

        unsafe {
            // Raw copy first: tag plus every payload slot, so non-droppable
            // variants are already correct.
            let whole = LLVMBuildLoad2(
                self.builder,
                llvm_enum,
                src,
                CString::new("clone.enum.raw")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, whole, dst);
        }

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

        unsafe {
            let tag_ptr = LLVMBuildStructGEP2(
                self.builder,
                llvm_enum,
                src,
                0,
                CString::new("clone.enum.tag.ptr")?.as_ptr(),
            );
            let tag_val = LLVMBuildLoad2(
                self.builder,
                LLVMInt32TypeInContext(self.context),
                tag_ptr,
                CString::new("clone.enum.tag")?.as_ptr(),
            );

            let parent_fn = LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder));
            let done_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("clone.enum.done")?.as_ptr(),
            );

            for (variant_index, payload_type) in &droppable_variants {
                let clone_bb = LLVMAppendBasicBlockInContext(
                    self.context,
                    parent_fn,
                    CString::new(format!("clone.enum.variant.{}", variant_index))?.as_ptr(),
                );
                let next_bb = LLVMAppendBasicBlockInContext(
                    self.context,
                    parent_fn,
                    CString::new(format!("clone.enum.next.{}", variant_index))?.as_ptr(),
                );
                let cmp = LLVMBuildICmp(
                    self.builder,
                    llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                    tag_val,
                    LLVMConstInt(
                        LLVMInt32TypeInContext(self.context),
                        *variant_index as u64,
                        0,
                    ),
                    CString::new(format!("clone.enum.is.{}", variant_index))?.as_ptr(),
                );
                LLVMBuildCondBr(self.builder, cmp, clone_bb, next_bb);

                LLVMPositionBuilderAtEnd(self.builder, clone_bb);
                let field_index = 1 + *variant_index;
                let src_payload = LLVMBuildStructGEP2(
                    self.builder,
                    llvm_enum,
                    src,
                    field_index,
                    CString::new("clone.enum.src.payload")?.as_ptr(),
                );
                let dst_payload = LLVMBuildStructGEP2(
                    self.builder,
                    llvm_enum,
                    dst,
                    field_index,
                    CString::new("clone.enum.dst.payload")?.as_ptr(),
                );
                self.codegen_clone_slot(dst_payload, src_payload, payload_type)?;
                LLVMBuildBr(self.builder, done_bb);

                LLVMPositionBuilderAtEnd(self.builder, next_bb);
            }
            LLVMBuildBr(self.builder, done_bb);
            LLVMPositionBuilderAtEnd(self.builder, done_bb);
        }
        Ok(())
    }

    fn emit_clone_named(
        &mut self,
        dst: LLVMValueRef,
        src: LLVMValueRef,
        name: &str,
    ) -> Result<()> {
        if self.enum_layouts.contains_key(name) {
            return self.emit_clone_enum(dst, src, name);
        }

        let layout = match self.struct_layouts.get(name) {
            Some(l) => l.clone(),
            None => bail!("missing struct layout for clone of {}", name),
        };

        // Monomorphized container structs by name, mirroring drop dispatch.
        if name.starts_with("Vec$") && layout.fields.len() == 3 {
            if let Type::RawPtr(ref elem_type) = layout.fields[0].1 {
                let elem = elem_type.as_ref().clone();
                return self.emit_clone_vec(dst, src, name, &elem);
            }
        }
        if name.starts_with("Map$") {
            let (key_type, value_type) = self.map_key_value_types_from_map(name)?;
            return self.emit_clone_map(dst, src, name, &key_type, &value_type);
        }

        // Generic struct: raw copy, then deep-clone droppable fields.
        let llvm_struct = self.get_struct_type(name)?;
        unsafe {
            let whole = LLVMBuildLoad2(
                self.builder,
                llvm_struct,
                src,
                CString::new("clone.struct.raw")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, whole, dst);
        }
        for (field_index, (_field_name, field_type)) in layout.fields.iter().enumerate() {
            if !Self::field_type_has_drop_glue(field_type) {
                continue;
            }
            unsafe {
                let src_field = LLVMBuildStructGEP2(
                    self.builder,
                    llvm_struct,
                    src,
                    field_index as u32,
                    CString::new(format!("clone.{}.src.f{}", name, field_index))?.as_ptr(),
                );
                let dst_field = LLVMBuildStructGEP2(
                    self.builder,
                    llvm_struct,
                    dst,
                    field_index as u32,
                    CString::new(format!("clone.{}.dst.f{}", name, field_index))?.as_ptr(),
                );
                self.codegen_clone_slot(dst_field, src_field, field_type)?;
            }
        }
        Ok(())
    }

    fn emit_clone_tuple(
        &mut self,
        dst: LLVMValueRef,
        src: LLVMValueRef,
        elem_types: &[Type],
    ) -> Result<()> {
        let llvm_ty = self.get_llvm_type(&Type::Tuple(elem_types.to_vec()))?;
        unsafe {
            let whole = LLVMBuildLoad2(
                self.builder,
                llvm_ty,
                src,
                CString::new("clone.tuple.raw")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, whole, dst);
        }
        for (idx, elem_ty) in elem_types.iter().enumerate() {
            if !Self::field_type_has_drop_glue(elem_ty) {
                continue;
            }
            unsafe {
                let src_field = LLVMBuildStructGEP2(
                    self.builder,
                    llvm_ty,
                    src,
                    idx as u32,
                    CString::new(format!("clone.tuple.src.{}", idx))?.as_ptr(),
                );
                let dst_field = LLVMBuildStructGEP2(
                    self.builder,
                    llvm_ty,
                    dst,
                    idx as u32,
                    CString::new(format!("clone.tuple.dst.{}", idx))?.as_ptr(),
                );
                self.codegen_clone_slot(dst_field, src_field, elem_ty)?;
            }
        }
        Ok(())
    }

    fn emit_clone_vec(
        &mut self,
        dst: LLVMValueRef,
        src: LLVMValueRef,
        struct_name: &str,
        elem_type: &Type,
    ) -> Result<()> {
        let (llvm_vec_ty, usize_ty) = self
            .get_vec_layout(struct_name)
            .ok_or_else(|| anyhow!("missing vec layout for clone of {}", struct_name))?;

        unsafe {
            let ptr_ty = LLVMPointerType(LLVMInt8TypeInContext(self.context), 0);

            let src_data_ptr = LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                src,
                0,
                CString::new("clone.vec.src.data")?.as_ptr(),
            );
            let src_len_ptr = LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                src,
                1,
                CString::new("clone.vec.src.len")?.as_ptr(),
            );
            let src_cap_ptr = LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                src,
                2,
                CString::new("clone.vec.src.cap")?.as_ptr(),
            );
            let dst_data_ptr = LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                dst,
                0,
                CString::new("clone.vec.dst.data")?.as_ptr(),
            );
            let dst_len_ptr = LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                dst,
                1,
                CString::new("clone.vec.dst.len")?.as_ptr(),
            );
            let dst_cap_ptr = LLVMBuildStructGEP2(
                self.builder,
                llvm_vec_ty,
                dst,
                2,
                CString::new("clone.vec.dst.cap")?.as_ptr(),
            );

            let data = LLVMBuildLoad2(
                self.builder,
                ptr_ty,
                src_data_ptr,
                CString::new("clone.vec.data")?.as_ptr(),
            );
            let len = LLVMBuildLoad2(
                self.builder,
                usize_ty,
                src_len_ptr,
                CString::new("clone.vec.len")?.as_ptr(),
            );
            let cap = LLVMBuildLoad2(
                self.builder,
                usize_ty,
                src_cap_ptr,
                CString::new("clone.vec.cap")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, len, dst_len_ptr);
            LLVMBuildStore(self.builder, cap, dst_cap_ptr);
            LLVMBuildStore(self.builder, LLVMConstPointerNull(ptr_ty), dst_data_ptr);

            let is_null = LLVMBuildIsNull(
                self.builder,
                data,
                CString::new("clone.vec.data.isnull")?.as_ptr(),
            );
            let zero = LLVMConstInt(usize_ty, 0, 0);
            let cap_zero = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                cap,
                zero,
                CString::new("clone.vec.cap.zero")?.as_ptr(),
            );
            let skip = LLVMBuildOr(
                self.builder,
                is_null,
                cap_zero,
                CString::new("clone.vec.skip")?.as_ptr(),
            );

            let parent_fn = LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder));
            let copy_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("clone.vec.copy")?.as_ptr(),
            );
            let done_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("clone.vec.done")?.as_ptr(),
            );
            LLVMBuildCondBr(self.builder, skip, done_bb, copy_bb);

            LLVMPositionBuilderAtEnd(self.builder, copy_bb);
            let elem_llvm_ty = self.get_llvm_type(elem_type)?;
            let elem_size = LLVMSizeOf(elem_llvm_ty);
            let byte_len = LLVMBuildMul(
                self.builder,
                cap,
                elem_size,
                CString::new("clone.vec.bytes")?.as_ptr(),
            );
            let malloc_fn = self.ensure_malloc_fn()?;
            let malloc_ty = self.malloc_function_type();
            let mut args = vec![byte_len];
            let new_buf = LLVMBuildCall2(
                self.builder,
                malloc_ty,
                malloc_fn,
                args.as_mut_ptr(),
                args.len() as u32,
                CString::new("clone.vec.alloc")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, new_buf, dst_data_ptr);

            // for i in 0..len { clone element i }
            let idx_slot = LLVMBuildAlloca(
                self.builder,
                usize_ty,
                CString::new("clone.vec.idx")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, zero, idx_slot);

            let check_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("clone.vec.check")?.as_ptr(),
            );
            let body_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("clone.vec.body")?.as_ptr(),
            );
            LLVMBuildBr(self.builder, check_bb);

            LLVMPositionBuilderAtEnd(self.builder, check_bb);
            let idx = LLVMBuildLoad2(
                self.builder,
                usize_ty,
                idx_slot,
                CString::new("clone.vec.idx.load")?.as_ptr(),
            );
            let in_range = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntULT,
                idx,
                len,
                CString::new("clone.vec.inrange")?.as_ptr(),
            );
            LLVMBuildCondBr(self.builder, in_range, body_bb, done_bb);

            LLVMPositionBuilderAtEnd(self.builder, body_bb);
            let mut src_idx = vec![idx];
            let src_elem = LLVMBuildGEP2(
                self.builder,
                elem_llvm_ty,
                data,
                src_idx.as_mut_ptr(),
                1,
                CString::new("clone.vec.src.elem")?.as_ptr(),
            );
            let mut dst_idx = vec![idx];
            let dst_elem = LLVMBuildGEP2(
                self.builder,
                elem_llvm_ty,
                new_buf,
                dst_idx.as_mut_ptr(),
                1,
                CString::new("clone.vec.dst.elem")?.as_ptr(),
            );
            self.codegen_clone_slot(dst_elem, src_elem, elem_type)?;
            let one = LLVMConstInt(usize_ty, 1, 0);
            let next = LLVMBuildAdd(
                self.builder,
                idx,
                one,
                CString::new("clone.vec.idx.next")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, next, idx_slot);
            LLVMBuildBr(self.builder, check_bb);

            LLVMPositionBuilderAtEnd(self.builder, done_bb);
        }
        Ok(())
    }

    fn emit_clone_map(
        &mut self,
        dst: LLVMValueRef,
        src: LLVMValueRef,
        map_name: &str,
        key_type: &Type,
        value_type: &Type,
    ) -> Result<()> {
        self.ensure_map_bucket_type(key_type, value_type)?;
        let (llvm_map_ty, usize_ty) = self
            .get_map_layout(map_name)
            .ok_or_else(|| anyhow!("missing map layout for clone of {}", map_name))?;
        let bucket_name = self.map_bucket_name(key_type, value_type);
        let llvm_bucket_ty = self.get_struct_type(&bucket_name)?;
        let bucket_layout = self
            .struct_layouts
            .get(&bucket_name)
            .ok_or_else(|| anyhow!("missing bucket layout for {}", bucket_name))?
            .clone();
        let key_field_ty = bucket_layout.fields[0].1.clone();
        let value_field_ty = bucket_layout.fields[1].1.clone();

        unsafe {
            let ptr_ty = LLVMPointerType(LLVMInt8TypeInContext(self.context), 0);

            let src_buckets_ptr = LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                src,
                0,
                CString::new("clone.map.src.buckets")?.as_ptr(),
            );
            let src_cap_ptr = LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                src,
                1,
                CString::new("clone.map.src.cap")?.as_ptr(),
            );
            let src_len_ptr = LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                src,
                2,
                CString::new("clone.map.src.len")?.as_ptr(),
            );
            let dst_buckets_ptr = LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                dst,
                0,
                CString::new("clone.map.dst.buckets")?.as_ptr(),
            );
            let dst_cap_ptr = LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                dst,
                1,
                CString::new("clone.map.dst.cap")?.as_ptr(),
            );
            let dst_len_ptr = LLVMBuildStructGEP2(
                self.builder,
                llvm_map_ty,
                dst,
                2,
                CString::new("clone.map.dst.len")?.as_ptr(),
            );

            let cap = LLVMBuildLoad2(
                self.builder,
                usize_ty,
                src_cap_ptr,
                CString::new("clone.map.cap")?.as_ptr(),
            );
            let len = LLVMBuildLoad2(
                self.builder,
                usize_ty,
                src_len_ptr,
                CString::new("clone.map.len")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, cap, dst_cap_ptr);
            LLVMBuildStore(self.builder, len, dst_len_ptr);
            LLVMBuildStore(self.builder, LLVMConstPointerNull(ptr_ty), dst_buckets_ptr);

            let zero = LLVMConstInt(usize_ty, 0, 0);
            let cap_zero = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                cap,
                zero,
                CString::new("clone.map.cap.zero")?.as_ptr(),
            );

            let parent_fn = LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder));
            let copy_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("clone.map.copy")?.as_ptr(),
            );
            let done_bb = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("clone.map.done")?.as_ptr(),
            );
            LLVMBuildCondBr(self.builder, cap_zero, done_bb, copy_bb);

            LLVMPositionBuilderAtEnd(self.builder, copy_bb);
            let src_array = LLVMBuildLoad2(
                self.builder,
                ptr_ty,
                src_buckets_ptr,
                CString::new("clone.map.src.array")?.as_ptr(),
            );
            let head_size = LLVMSizeOf(ptr_ty);
            let array_bytes = LLVMBuildMul(
                self.builder,
                cap,
                head_size,
                CString::new("clone.map.array.bytes")?.as_ptr(),
            );
            let malloc_fn = self.ensure_malloc_fn()?;
            let malloc_ty = self.malloc_function_type();
            let mut margs = vec![array_bytes];
            let new_array = LLVMBuildCall2(
                self.builder,
                malloc_ty,
                malloc_fn,
                margs.as_mut_ptr(),
                margs.len() as u32,
                CString::new("clone.map.array.alloc")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, new_array, dst_buckets_ptr);

            // outer loop over buckets
            let idx_slot = LLVMBuildAlloca(
                self.builder,
                usize_ty,
                CString::new("clone.map.idx")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, zero, idx_slot);
            // src chain cursor and dst tail cursor
            let cur_slot = LLVMBuildAlloca(
                self.builder,
                ptr_ty,
                CString::new("clone.map.cur")?.as_ptr(),
            );
            let tail_slot = LLVMBuildAlloca(
                self.builder,
                ptr_ty,
                CString::new("clone.map.tail")?.as_ptr(),
            );

            let outer_check = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("clone.map.outer.check")?.as_ptr(),
            );
            let outer_body = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("clone.map.outer.body")?.as_ptr(),
            );
            let inner_check = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("clone.map.inner.check")?.as_ptr(),
            );
            let inner_body = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("clone.map.inner.body")?.as_ptr(),
            );
            let inner_done = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("clone.map.inner.done")?.as_ptr(),
            );
            LLVMBuildBr(self.builder, outer_check);

            // outer: idx < cap ?
            LLVMPositionBuilderAtEnd(self.builder, outer_check);
            let idx = LLVMBuildLoad2(
                self.builder,
                usize_ty,
                idx_slot,
                CString::new("clone.map.idx.load")?.as_ptr(),
            );
            let idx_in_range = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntULT,
                idx,
                cap,
                CString::new("clone.map.idx.inrange")?.as_ptr(),
            );
            LLVMBuildCondBr(self.builder, idx_in_range, outer_body, done_bb);

            // outer body: init dst slot to null, cursor to src head, tail empty
            LLVMPositionBuilderAtEnd(self.builder, outer_body);
            let mut gep_idx = vec![idx];
            let src_head_ptr = LLVMBuildGEP2(
                self.builder,
                ptr_ty,
                src_array,
                gep_idx.as_mut_ptr(),
                1,
                CString::new("clone.map.src.head.ptr")?.as_ptr(),
            );
            let src_head = LLVMBuildLoad2(
                self.builder,
                ptr_ty,
                src_head_ptr,
                CString::new("clone.map.src.head")?.as_ptr(),
            );
            let mut gep_idx2 = vec![idx];
            let dst_head_ptr = LLVMBuildGEP2(
                self.builder,
                ptr_ty,
                new_array,
                gep_idx2.as_mut_ptr(),
                1,
                CString::new("clone.map.dst.head.ptr")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, LLVMConstPointerNull(ptr_ty), dst_head_ptr);
            LLVMBuildStore(self.builder, src_head, cur_slot);
            LLVMBuildStore(self.builder, LLVMConstPointerNull(ptr_ty), tail_slot);
            LLVMBuildBr(self.builder, inner_check);

            // inner: cur != null ?
            LLVMPositionBuilderAtEnd(self.builder, inner_check);
            let cur = LLVMBuildLoad2(
                self.builder,
                ptr_ty,
                cur_slot,
                CString::new("clone.map.cur.load")?.as_ptr(),
            );
            let cur_null = LLVMBuildIsNull(
                self.builder,
                cur,
                CString::new("clone.map.cur.null")?.as_ptr(),
            );
            LLVMBuildCondBr(self.builder, cur_null, inner_done, inner_body);

            // inner body: clone node
            LLVMPositionBuilderAtEnd(self.builder, inner_body);
            let bucket_size = LLVMSizeOf(llvm_bucket_ty);
            let mut nargs = vec![bucket_size];
            let new_node = LLVMBuildCall2(
                self.builder,
                malloc_ty,
                malloc_fn,
                nargs.as_mut_ptr(),
                nargs.len() as u32,
                CString::new("clone.map.node.alloc")?.as_ptr(),
            );
            // key
            let src_key = LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                cur,
                0,
                CString::new("clone.map.src.key")?.as_ptr(),
            );
            let dst_key = LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                new_node,
                0,
                CString::new("clone.map.dst.key")?.as_ptr(),
            );
            self.codegen_clone_slot(dst_key, src_key, &key_field_ty)?;
            // value
            let src_val = LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                cur,
                1,
                CString::new("clone.map.src.val")?.as_ptr(),
            );
            let dst_val = LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                new_node,
                1,
                CString::new("clone.map.dst.val")?.as_ptr(),
            );
            self.codegen_clone_slot(dst_val, src_val, &value_field_ty)?;
            // next = null
            let new_next = LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                new_node,
                2,
                CString::new("clone.map.new.next")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, LLVMConstPointerNull(ptr_ty), new_next);

            // append to dst chain: tail == null ? head = node : tail.next = node
            let tail = LLVMBuildLoad2(
                self.builder,
                ptr_ty,
                tail_slot,
                CString::new("clone.map.tail.load")?.as_ptr(),
            );
            let tail_null = LLVMBuildIsNull(
                self.builder,
                tail,
                CString::new("clone.map.tail.null")?.as_ptr(),
            );
            let set_head = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("clone.map.set.head")?.as_ptr(),
            );
            let set_next = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("clone.map.set.next")?.as_ptr(),
            );
            let appended = LLVMAppendBasicBlockInContext(
                self.context,
                parent_fn,
                CString::new("clone.map.appended")?.as_ptr(),
            );
            LLVMBuildCondBr(self.builder, tail_null, set_head, set_next);

            LLVMPositionBuilderAtEnd(self.builder, set_head);
            LLVMBuildStore(self.builder, new_node, dst_head_ptr);
            LLVMBuildBr(self.builder, appended);

            LLVMPositionBuilderAtEnd(self.builder, set_next);
            let tail_next = LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                tail,
                2,
                CString::new("clone.map.tail.next")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, new_node, tail_next);
            LLVMBuildBr(self.builder, appended);

            LLVMPositionBuilderAtEnd(self.builder, appended);
            LLVMBuildStore(self.builder, new_node, tail_slot);
            // advance cursor
            let cur_next_ptr = LLVMBuildStructGEP2(
                self.builder,
                llvm_bucket_ty,
                cur,
                2,
                CString::new("clone.map.cur.next.ptr")?.as_ptr(),
            );
            let cur_next = LLVMBuildLoad2(
                self.builder,
                ptr_ty,
                cur_next_ptr,
                CString::new("clone.map.cur.next")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, cur_next, cur_slot);
            LLVMBuildBr(self.builder, inner_check);

            // inner done: next bucket
            LLVMPositionBuilderAtEnd(self.builder, inner_done);
            let one = LLVMConstInt(usize_ty, 1, 0);
            let idx_next = LLVMBuildAdd(
                self.builder,
                idx,
                one,
                CString::new("clone.map.idx.next")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, idx_next, idx_slot);
            LLVMBuildBr(self.builder, outer_check);

            LLVMPositionBuilderAtEnd(self.builder, done_bb);
        }
        Ok(())
    }
}
