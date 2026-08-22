use super::*;

impl CodegenContext {
    pub(super) fn is_unit_type(ty: &Type) -> bool {
        matches!(ty, Type::Void) || matches!(ty, Type::Tuple(elem_types) if elem_types.is_empty())
    }

    pub(super) fn create_named_types(&mut self, mir_module: &MirModule) -> Result<()> {
        for (name, layout) in &mir_module.struct_types {
            let name_c = CString::new(name.as_str())?;
            let llvm_ty = unsafe { LLVMStructCreateNamed(self.context, name_c.as_ptr()) };
            self.struct_types.insert(name.clone(), llvm_ty);
            self.struct_layouts.insert(name.clone(), layout.clone());
        }

        for (name, layout) in &mir_module.enum_types {
            let name_c = CString::new(name.as_str())?;
            let llvm_ty = unsafe { LLVMStructCreateNamed(self.context, name_c.as_ptr()) };
            self.enum_types.insert(name.clone(), llvm_ty);
            self.enum_layouts.insert(name.clone(), layout.clone());
        }

        Ok(())
    }

    pub(super) fn register_struct_types(&mut self, mir_module: &MirModule) -> Result<()> {
        for (name, layout) in &mir_module.struct_types {
            let llvm_ty = *self
                .struct_types
                .get(name)
                .ok_or_else(|| anyhow!("missing llvm type for struct {}", name))?;

            let mut field_tys: Vec<LLVMTypeRef> = Vec::new();
            for (field_name, field_ty) in &layout.fields {
                field_tys.push(
                    self.get_llvm_type(field_ty)
                        .with_context(|| format!("in struct {} field {}", name, field_name))?,
                );
            }

            unsafe {
                LLVMStructSetBody(llvm_ty, field_tys.as_mut_ptr(), field_tys.len() as u32, 0);
            }
        }

        Ok(())
    }

    pub(super) fn register_enum_types(&mut self, mir_module: &MirModule) -> Result<()> {
        for (name, layout) in &mir_module.enum_types {
            let llvm_ty = *self
                .enum_types
                .get(name)
                .ok_or_else(|| anyhow!("missing llvm type for enum {}", name))?;

            let mut field_tys: Vec<LLVMTypeRef> = Vec::new();
            // tag
            field_tys.push(unsafe { LLVMInt32TypeInContext(self.context) });
            for variant in &layout.variants {
                let ty = if let Some(payload) = &variant.payload {
                    if Self::is_unit_type(payload) {
                        unsafe { LLVMInt8TypeInContext(self.context) }
                    } else {
                        self.get_llvm_type(payload)
                            .with_context(|| format!("in enum {} variant {}", name, variant.name))?
                    }
                } else {
                    unsafe { LLVMInt8TypeInContext(self.context) }
                };
                field_tys.push(ty);
            }

            unsafe {
                LLVMStructSetBody(llvm_ty, field_tys.as_mut_ptr(), field_tys.len() as u32, 0);
            }
        }

        Ok(())
    }

    pub(super) fn get_vec_layout(&self, struct_name: &str) -> Option<(LLVMTypeRef, LLVMTypeRef)> {
        // Vec$T layout expected: [ptr, len, cap] where len/cap are usize (i64 backend)
        let llvm_ty = *self.struct_types.get(struct_name)?;
        let layout = self.struct_layouts.get(struct_name)?;
        let len_ty = unsafe { LLVMInt64TypeInContext(self.context) };
        if layout.fields.len() == 3 {
            Some((llvm_ty, len_ty))
        } else {
            None
        }
    }

    pub(super) fn get_map_layout(&self, struct_name: &str) -> Option<(LLVMTypeRef, LLVMTypeRef)> {
        // Map$K__V layout: [buckets ptr, cap, len]
        let llvm_ty = *self.struct_types.get(struct_name)?;
        let layout = self.struct_layouts.get(struct_name)?;
        let len_ty = unsafe { LLVMInt64TypeInContext(self.context) };
        if layout.fields.len() == 3 {
            Some((llvm_ty, len_ty))
        } else {
            None
        }
    }

    pub(super) fn sanitize(&self, name: &str) -> String {
        name.chars()
            .map(|c| if c.is_alphanumeric() { c } else { '_' })
            .collect()
    }

    pub(super) fn type_key(&self, ty: &Type) -> String {
        match ty {
            Type::I8 => "i8".into(),
            Type::I32 => "i32".into(),
            Type::I64 => "i64".into(),
            Type::U8 => "u8".into(),
            Type::U32 => "u32".into(),
            Type::U64 => "u64".into(),
            Type::Usize => "usize".into(),
            Type::F32 => "f32".into(),
            Type::F64 => "f64".into(),
            Type::Bool => "bool".into(),
            Type::Char => "char".into(),
            Type::Str => "str".into(),
            Type::String => "String".into(),
            Type::Void => "void".into(),
            Type::Named(n) => self.sanitize(n),
            Type::Enum(n) => format!("enum_{}", self.sanitize(n)),
            Type::Param(p) => format!("P_{}", self.sanitize(p)),
            Type::Ref(inner, m) => {
                let m = if matches!(m, Mutability::Mutable) {
                    "mut"
                } else {
                    "ref"
                };
                format!("{}_{}", m, self.type_key(inner))
            }
            Type::Array(inner, size) => format!("arr{}_{}", size, self.type_key(inner)),
            Type::Own(inner) => format!("own_{}", self.type_key(inner)),
            Type::RawPtr(inner) => format!("rawptr_{}", self.type_key(inner)),
            Type::Shared(inner) => format!("shared_{}", self.type_key(inner)),
            Type::Function { params, ret } => {
                let params: Vec<String> = params.iter().map(|p| self.type_key(p)).collect();
                format!(
                    "fn_{}_to_{}",
                    if params.is_empty() {
                        "unit".to_string()
                    } else {
                        params.join("__")
                    },
                    self.type_key(ret)
                )
            }
            Type::App { base, args } => {
                let args: Vec<String> = args.iter().map(|a| self.type_key(a)).collect();
                format!("app_{}_{}", self.sanitize(base), args.join("__"))
            }
            Type::Tuple(elem_types) => {
                if elem_types.is_empty() {
                    "unit".into()
                } else {
                    let type_names: Vec<String> =
                        elem_types.iter().map(|t| self.type_key(t)).collect();
                    format!("__Tuple{}_{}", elem_types.len(), type_names.join("_"))
                }
            }
        }
    }

    pub(super) fn ensure_usize(
        &self,
        val: LLVMValueRef,
        usize_ty: LLVMTypeRef,
    ) -> Result<LLVMValueRef> {
        unsafe {
            if LLVMTypeOf(val) == usize_ty {
                Ok(val)
            } else {
                let cast_name = CString::new("usize.cast")?;
                Ok(LLVMBuildSExt(
                    self.builder,
                    val,
                    usize_ty,
                    cast_name.as_ptr(),
                ))
            }
        }
    }

    pub(super) fn get_struct_type(&self, name: &str) -> Result<LLVMTypeRef> {
        self.struct_types
            .get(name)
            .copied()
            .ok_or_else(|| anyhow!("unknown struct type {}", name))
    }

    pub(super) fn ret_uses_sret(&self, ret_ty: &Type) -> Result<bool> {
        if Self::is_unit_type(ret_ty) {
            return Ok(false);
        }

        match ret_ty {
            Type::Named(_)
            | Type::Tuple(_)
            | Type::Enum(_)
            | Type::Array(_, _)
            | Type::App { .. }
            | Type::Function { .. } => {}
            _ => return Ok(false),
        }

        let target_data = self
            .target_data
            .ok_or_else(|| anyhow!("missing target data for ABI sizing"))?;
        let llvm_ty = self.get_llvm_type(ret_ty)?;
        let size = unsafe { llvm_sys::target::LLVMABISizeOfType(target_data, llvm_ty) };
        Ok(size > 16)
    }

    pub(super) fn get_enum_type(&self, name: &str) -> Result<LLVMTypeRef> {
        self.enum_types
            .get(name)
            .copied()
            .ok_or_else(|| anyhow!("unknown enum type {}", name))
    }

    pub(super) fn get_llvm_type(&self, ty: &Type) -> Result<LLVMTypeRef> {
        let ty = ty.clone();
        unsafe {
            Ok(match ty {
                Type::I8 => LLVMInt8TypeInContext(self.context),
                Type::I32 => LLVMInt32TypeInContext(self.context),
                Type::I64 => LLVMInt64TypeInContext(self.context),
                Type::U8 => LLVMInt8TypeInContext(self.context),
                Type::U32 => LLVMInt32TypeInContext(self.context),
                Type::U64 => LLVMInt64TypeInContext(self.context),
                Type::Usize => LLVMInt64TypeInContext(self.context),
                Type::F32 => LLVMFloatTypeInContext(self.context),
                Type::F64 => LLVMDoubleTypeInContext(self.context),
                Type::Bool => LLVMInt1TypeInContext(self.context),
                Type::Char => LLVMInt32TypeInContext(self.context),
                Type::Str => LLVMPointerType(LLVMInt8TypeInContext(self.context), 0),
                Type::String => LLVMPointerType(LLVMInt8TypeInContext(self.context), 0),
                Type::Void => LLVMVoidTypeInContext(self.context),
                Type::Named(name) => {
                    // Check enum_types first, then struct_types
                    self.enum_types
                        .get(&name)
                        .copied()
                        .or_else(|| self.struct_types.get(&name).copied())
                        .ok_or_else(|| {
                            anyhow!("unknown type {} (not found in enum or struct types)", name)
                        })?
                }
                Type::Enum(name) => self.get_enum_type(&name)?,
                Type::Ref(inner, _) => {
                    let inner_ty = self.get_llvm_type(&inner)?;
                    LLVMPointerType(inner_ty, 0)
                }
                Type::Array(elem_ty, size) => {
                    let elem_llvm = self.get_llvm_type(&elem_ty)?;
                    LLVMArrayType2(elem_llvm, size as u64)
                }
                Type::Own(inner) | Type::RawPtr(inner) | Type::Shared(inner) => {
                    let elem_ty = self.get_llvm_type(&inner)?;
                    LLVMPointerType(elem_ty, 0)
                }
                Type::Function { .. } => {
                    let ptr_ty = LLVMPointerType(LLVMInt8TypeInContext(self.context), 0);
                    let mut fields = [ptr_ty, ptr_ty, ptr_ty];
                    LLVMStructTypeInContext(
                        self.context,
                        fields.as_mut_ptr(),
                        fields.len() as u32,
                        0,
                    )
                }
                Type::Tuple(elem_types) => {
                    if elem_types.is_empty() {
                        // Empty tuple is void/unit type
                        LLVMVoidTypeInContext(self.context)
                    } else {
                        // Generate tuple struct name and look it up
                        let struct_name = tuple_struct_name_codegen(&elem_types);
                        self.struct_types
                            .get(&struct_name)
                            .copied()
                            .ok_or_else(|| {
                                anyhow!("tuple type {} not found in struct_types", struct_name)
                            })?
                    }
                }
                Type::Param(p) => {
                    anyhow::bail!(
                        "generic types must be monomorphized before codegen (param: {})",
                        p
                    )
                }
                Type::App { base, args } => {
                    anyhow::bail!(
                        "generic types must be monomorphized before codegen (app: {}<{:?}>)",
                        base,
                        args
                    )
                }
            })
        }
    }
}
