use super::*;

impl CodegenContext {
    pub(super) fn coerce_int_binop(
        &mut self,
        lhs: LLVMValueRef,
        rhs: LLVMValueRef,
    ) -> (LLVMValueRef, LLVMValueRef) {
        unsafe {
            let lhs_ty = LLVMTypeOf(lhs);
            let rhs_ty = LLVMTypeOf(rhs);

            if LLVMGetTypeKind(lhs_ty) != llvm_sys::LLVMTypeKind::LLVMIntegerTypeKind {
                return (lhs, rhs);
            }
            if LLVMGetTypeKind(rhs_ty) != llvm_sys::LLVMTypeKind::LLVMIntegerTypeKind {
                return (lhs, rhs);
            }

            let lw = LLVMGetIntTypeWidth(lhs_ty);
            let rw = LLVMGetIntTypeWidth(rhs_ty);
            if lw == rw {
                return (lhs, rhs);
            }

            let name = CString::new("int.coerce").unwrap();

            if lw > rw {
                // Zero-extend when widening from i1 (bool) to avoid
                // sign-extending true (i1 1) into i32 -1.
                let rhs2 = if rw == 1 {
                    LLVMBuildZExt(self.builder, rhs, lhs_ty, name.as_ptr())
                } else {
                    LLVMBuildSExt(self.builder, rhs, lhs_ty, name.as_ptr())
                };
                (lhs, rhs2)
            } else {
                let lhs2 = if lw == 1 {
                    LLVMBuildZExt(self.builder, lhs, rhs_ty, name.as_ptr())
                } else {
                    LLVMBuildSExt(self.builder, lhs, rhs_ty, name.as_ptr())
                };
                (lhs2, rhs)
            }
        }
    }

    fn is_float_type_kind(kind: llvm_sys::LLVMTypeKind) -> bool {
        matches!(
            kind,
            llvm_sys::LLVMTypeKind::LLVMFloatTypeKind | llvm_sys::LLVMTypeKind::LLVMDoubleTypeKind
        )
    }

    /// Unify operands for a float binary op: int operands are promoted to the
    /// float side's type (UIToFP for unsigned Glyph types, SIToFP otherwise),
    /// and f32/f64 mixes are widened to f64.
    pub(super) fn coerce_float_binop(
        &mut self,
        lhs: LLVMValueRef,
        rhs: LLVMValueRef,
        lhs_unsigned: bool,
        rhs_unsigned: bool,
    ) -> (LLVMValueRef, LLVMValueRef) {
        unsafe {
            let name = CString::new("float.coerce").unwrap();
            let lhs_ty = LLVMTypeOf(lhs);
            let rhs_ty = LLVMTypeOf(rhs);
            let lhs_kind = LLVMGetTypeKind(lhs_ty);
            let rhs_kind = LLVMGetTypeKind(rhs_ty);
            let lhs_is_float = Self::is_float_type_kind(lhs_kind);
            let rhs_is_float = Self::is_float_type_kind(rhs_kind);

            let build_int_to_fp = |builder, val, target_ty, unsigned: bool| {
                if unsigned {
                    LLVMBuildUIToFP(builder, val, target_ty, name.as_ptr())
                } else {
                    LLVMBuildSIToFP(builder, val, target_ty, name.as_ptr())
                }
            };

            match (lhs_is_float, rhs_is_float) {
                (true, false) => {
                    if rhs_kind != llvm_sys::LLVMTypeKind::LLVMIntegerTypeKind {
                        return (lhs, rhs);
                    }
                    let rhs2 = build_int_to_fp(self.builder, rhs, lhs_ty, rhs_unsigned);
                    (lhs, rhs2)
                }
                (false, true) => {
                    if lhs_kind != llvm_sys::LLVMTypeKind::LLVMIntegerTypeKind {
                        return (lhs, rhs);
                    }
                    let lhs2 = build_int_to_fp(self.builder, lhs, rhs_ty, lhs_unsigned);
                    (lhs2, rhs)
                }
                (true, true) => {
                    if lhs_kind == rhs_kind {
                        return (lhs, rhs);
                    }
                    let double_ty = LLVMDoubleTypeInContext(self.context);
                    if lhs_kind == llvm_sys::LLVMTypeKind::LLVMFloatTypeKind {
                        let lhs2 = LLVMBuildFPExt(self.builder, lhs, double_ty, name.as_ptr());
                        (lhs2, rhs)
                    } else {
                        let rhs2 = LLVMBuildFPExt(self.builder, rhs, double_ty, name.as_ptr());
                        (lhs, rhs2)
                    }
                }
                (false, false) => (lhs, rhs),
            }
        }
    }

    pub(super) fn coerce_int_value(
        &mut self,
        val: LLVMValueRef,
        target_ty: LLVMTypeRef,
        signed: bool,
    ) -> LLVMValueRef {
        unsafe {
            let val_ty = LLVMTypeOf(val);
            if val_ty == target_ty {
                return val;
            }
            if LLVMGetTypeKind(val_ty) != llvm_sys::LLVMTypeKind::LLVMIntegerTypeKind {
                return val;
            }
            if LLVMGetTypeKind(target_ty) != llvm_sys::LLVMTypeKind::LLVMIntegerTypeKind {
                return val;
            }

            let val_bits = LLVMGetIntTypeWidth(val_ty);
            let target_bits = LLVMGetIntTypeWidth(target_ty);
            let name = CString::new("int.cast").unwrap();
            if target_bits > val_bits {
                if signed {
                    LLVMBuildSExt(self.builder, val, target_ty, name.as_ptr())
                } else {
                    LLVMBuildZExt(self.builder, val, target_ty, name.as_ptr())
                }
            } else {
                LLVMBuildTrunc(self.builder, val, target_ty, name.as_ptr())
            }
        }
    }

    /// Emit `expr as T` numeric conversions. Sign/zero extension follows the
    /// SOURCE type's signedness (Rust semantics); float->int truncates toward
    /// zero with the DEST type's signedness.
    pub(super) fn codegen_numeric_cast(
        &mut self,
        val: LLVMValueRef,
        from: &Type,
        to: &Type,
    ) -> Result<LLVMValueRef> {
        let src_unsigned = matches!(
            from,
            Type::U8 | Type::U32 | Type::U64 | Type::Usize | Type::Char | Type::Bool
        );
        let dst_signed = matches!(to, Type::I8 | Type::I32 | Type::I64);
        let from_is_float = from.is_float();
        let to_is_float = to.is_float();

        unsafe {
            let dst_llvm = self.get_llvm_type(to)?;
            let src_llvm = LLVMTypeOf(val);
            if src_llvm == dst_llvm {
                return Ok(val);
            }
            let name = CString::new("cast")?;

            let result = match (from_is_float, to_is_float) {
                (true, true) => {
                    if matches!(to, Type::F64) {
                        LLVMBuildFPExt(self.builder, val, dst_llvm, name.as_ptr())
                    } else {
                        LLVMBuildFPTrunc(self.builder, val, dst_llvm, name.as_ptr())
                    }
                }
                (true, false) => {
                    if dst_signed {
                        LLVMBuildFPToSI(self.builder, val, dst_llvm, name.as_ptr())
                    } else {
                        LLVMBuildFPToUI(self.builder, val, dst_llvm, name.as_ptr())
                    }
                }
                (false, true) => {
                    if src_unsigned {
                        LLVMBuildUIToFP(self.builder, val, dst_llvm, name.as_ptr())
                    } else {
                        LLVMBuildSIToFP(self.builder, val, dst_llvm, name.as_ptr())
                    }
                }
                (false, false) => {
                    let src_bits = LLVMGetIntTypeWidth(src_llvm);
                    let dst_bits = LLVMGetIntTypeWidth(dst_llvm);
                    if dst_bits > src_bits {
                        if src_unsigned {
                            LLVMBuildZExt(self.builder, val, dst_llvm, name.as_ptr())
                        } else {
                            LLVMBuildSExt(self.builder, val, dst_llvm, name.as_ptr())
                        }
                    } else {
                        LLVMBuildTrunc(self.builder, val, dst_llvm, name.as_ptr())
                    }
                }
            };
            Ok(result)
        }
    }

    pub(super) fn rvalue_tag(&self, rvalue: &Rvalue) -> &'static str {
        match rvalue {
            Rvalue::ConstInt(_) => "ConstInt",
            Rvalue::ConstFloat(_) => "ConstFloat",
            Rvalue::ConstBool(_) => "ConstBool",
            Rvalue::Cast { .. } => "Cast",
            Rvalue::StringLit { .. } => "StringLit",
            Rvalue::Move(_) => "Move",
            Rvalue::Binary { .. } => "Binary",
            Rvalue::StructLit { .. } => "StructLit",
            Rvalue::FieldAccess { .. } => "FieldAccess",
            Rvalue::FieldRef { .. } => "FieldRef",
            Rvalue::EnumConstruct { .. } => "EnumConstruct",
            Rvalue::EnumTag { .. } => "EnumTag",
            Rvalue::EnumPayload { .. } => "EnumPayload",
            Rvalue::FileOpen { .. } => "FileOpen",
            Rvalue::FileReadToString { .. } => "FileReadToString",
            Rvalue::FileWriteString { .. } => "FileWriteString",
            Rvalue::FileClose { .. } => "FileClose",
            Rvalue::StringLen { .. } => "StringLen",
            Rvalue::StringConcat { .. } => "StringConcat",
            Rvalue::StringSlice { .. } => "StringSlice",
            Rvalue::StringTrim { .. } => "StringTrim",
            Rvalue::StringSplit { .. } => "StringSplit",
            Rvalue::StringStartsWith { .. } => "StringStartsWith",
            Rvalue::StringEndsWith { .. } => "StringEndsWith",
            Rvalue::StringClone { .. } => "StringClone",
            Rvalue::Call { .. } => "Call",
            Rvalue::Ref { .. } => "Ref",
            Rvalue::ArrayLit { .. } => "ArrayLit",
            Rvalue::ArrayIndex { .. } => "ArrayIndex",
            Rvalue::ArrayLen { .. } => "ArrayLen",
            Rvalue::VecNew { .. } => "VecNew",
            Rvalue::VecWithCapacity { .. } => "VecWithCapacity",
            Rvalue::VecLen { .. } => "VecLen",
            Rvalue::VecIndex { .. } => "VecIndex",
            Rvalue::VecIndexRef { .. } => "VecIndexRef",
            Rvalue::VecPush { .. } => "VecPush",
            Rvalue::VecPop { .. } => "VecPop",
            Rvalue::MapNew { .. } => "MapNew",
            Rvalue::MapWithCapacity { .. } => "MapWithCapacity",
            Rvalue::MapAdd { .. } => "MapAdd",
            Rvalue::MapUpdate { .. } => "MapUpdate",
            Rvalue::MapDel { .. } => "MapDel",
            Rvalue::MapGet { .. } => "MapGet",
            Rvalue::MapHas { .. } => "MapHas",
            Rvalue::MapKeys { .. } => "MapKeys",
            Rvalue::MapVals { .. } => "MapVals",
            Rvalue::OwnNew { .. } => "OwnNew",
            Rvalue::OwnIntoRaw { .. } => "OwnIntoRaw",
            Rvalue::OwnFromRaw { .. } => "OwnFromRaw",
            Rvalue::RawPtrNull { .. } => "RawPtrNull",
            Rvalue::SharedNew { .. } => "SharedNew",
            Rvalue::SharedClone { .. } => "SharedClone",
        }
    }

    pub(super) fn mir_value_type(&self, value: &MirValue, func: &MirFunction) -> Option<Type> {
        match value {
            MirValue::Unit => Some(Type::Void),
            MirValue::Int(_) => Some(Type::I32),
            MirValue::Float(_) => Some(Type::F64),
            MirValue::Bool(_) => Some(Type::Bool),
            MirValue::Local(id) => func
                .locals
                .get(id.0 as usize)
                .and_then(|local| local.ty.clone()),
        }
    }

    pub(super) fn call_param_types(
        &self,
        name: &str,
        mir_module: &MirModule,
    ) -> (Vec<Option<Type>>, bool) {
        if let Some(func) = mir_module.functions.iter().find(|f| f.name == name) {
            let params = func
                .params
                .iter()
                .map(|id| func.locals.get(id.0 as usize).and_then(|l| l.ty.clone()))
                .collect();
            return (params, false);
        }

        if let Some(ext) = mir_module.extern_functions.iter().find(|f| f.name == name) {
            let params = ext.params.iter().cloned().map(Some).collect();
            return (params, true);
        }

        (Vec::new(), false)
    }

    pub(super) fn codegen_rvalue_for_local(
        &mut self,
        rvalue: &Rvalue,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        functions: &HashMap<String, LLVMValueRef>,
        mir_module: &MirModule,
        target_local: Option<LocalId>,
    ) -> Result<LLVMValueRef> {
        // For ConstInt assigned to a typed local, use that local's type
        if let Rvalue::ConstInt(i) = rvalue {
            if let Some(lid) = target_local {
                if let Some(local) = func.locals.get(lid.0 as usize) {
                    if let Some(ty) = local.ty.as_ref() {
                        if ty.is_int() {
                            unsafe {
                                let llvm_ty = self.get_llvm_type(ty)?;
                                let signed = matches!(ty, Type::I8 | Type::I32 | Type::I64);
                                return Ok(LLVMConstInt(
                                    llvm_ty,
                                    *i as u64,
                                    if signed { 1 } else { 0 },
                                ));
                            }
                        }
                    }
                }
            }
        }
        // Same for ConstFloat: an f32-typed local gets a float constant, not double
        if let Rvalue::ConstFloat(fl) = rvalue {
            if let Some(lid) = target_local {
                if let Some(local) = func.locals.get(lid.0 as usize) {
                    if let Some(ty) = local.ty.as_ref() {
                        if ty.is_float() {
                            unsafe {
                                let llvm_ty = self.get_llvm_type(ty)?;
                                return Ok(LLVMConstReal(llvm_ty, *fl));
                            }
                        }
                    }
                }
            }
        }
        self.codegen_rvalue(rvalue, func, local_map, functions, mir_module)
    }

    pub(super) fn codegen_rvalue(
        &mut self,
        rvalue: &Rvalue,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        functions: &HashMap<String, LLVMValueRef>,
        mir_module: &MirModule,
    ) -> Result<LLVMValueRef> {
        unsafe {
            match rvalue {
                Rvalue::ConstInt(i) => {
                    let ty = LLVMInt32TypeInContext(self.context);
                    Ok(LLVMConstInt(ty, *i as u64, 1))
                }
                Rvalue::ConstFloat(fl) => {
                    let ty = LLVMDoubleTypeInContext(self.context);
                    Ok(LLVMConstReal(ty, *fl))
                }
                Rvalue::Cast { value, from, to } => {
                    let val = self.codegen_value(value, func, local_map)?;
                    self.codegen_numeric_cast(val, from, to)
                }
                Rvalue::ConstBool(b) => {
                    let ty = LLVMInt1TypeInContext(self.context);
                    Ok(LLVMConstInt(ty, if *b { 1 } else { 0 }, 0))
                }
                Rvalue::StringLit {
                    content,
                    global_name,
                } => self.codegen_string_literal(content, global_name),
                Rvalue::Move(local_id) => {
                    let local_ptr = local_map
                        .get(local_id)
                        .ok_or_else(|| anyhow!("undefined local {:?}", local_id))?;

                    if LLVMGetTypeKind(LLVMTypeOf(*local_ptr))
                        == llvm_sys::LLVMTypeKind::LLVMPointerTypeKind
                    {
                        let load_name = CString::new("load")?;
                        let llvm_ty = self.local_llvm_type(func, *local_id)?;
                        Ok(LLVMBuildLoad2(
                            self.builder,
                            llvm_ty,
                            *local_ptr,
                            load_name.as_ptr(),
                        ))
                    } else {
                        Ok(*local_ptr)
                    }
                }
                Rvalue::Binary { op, lhs, rhs } => {
                    use glyph_core::ast::BinaryOp;
                    let name = CString::new("binop")?;

                    let lhs_ty = self.mir_value_type(lhs, func);
                    let rhs_ty = self.mir_value_type(rhs, func);
                    let is_string_like = |ty: Option<&Type>| {
                        matches!(ty, Some(Type::Str | Type::String))
                            || matches!(
                                ty,
                                Some(Type::Ref(inner, _))
                                    if matches!(inner.as_ref(), Type::Str | Type::String)
                            )
                    };

                    if matches!(op, BinaryOp::Eq | BinaryOp::Ne)
                        && is_string_like(lhs_ty.as_ref())
                        && is_string_like(rhs_ty.as_ref())
                    {
                        let lhs_val = match lhs {
                            MirValue::Local(id) => {
                                self.codegen_string_base_value(*id, func, local_map)?
                            }
                            _ => self.codegen_value(lhs, func, local_map)?,
                        };
                        let rhs_val = match rhs {
                            MirValue::Local(id) => {
                                self.codegen_string_base_value(*id, func, local_map)?
                            }
                            _ => self.codegen_value(rhs, func, local_map)?,
                        };
                        let eq_val = self.codegen_string_eq(lhs_val, rhs_val, functions)?;
                        if matches!(op, BinaryOp::Ne) {
                            let one = LLVMConstInt(LLVMInt1TypeInContext(self.context), 1, 0);
                            return Ok(LLVMBuildXor(
                                self.builder,
                                eq_val,
                                one,
                                CString::new("str.ne")?.as_ptr(),
                            ));
                        }
                        return Ok(eq_val);
                    }

                    let lhs_val0 = self.codegen_value(lhs, func, local_map)?;
                    let rhs_val0 = self.codegen_value(rhs, func, local_map)?;

                    // Float operands take the FP instruction path; mixed int/float
                    // operands promote the int side to the float type.
                    let is_float_op = Self::is_float_type_kind(LLVMGetTypeKind(LLVMTypeOf(
                        lhs_val0,
                    ))) || Self::is_float_type_kind(
                        LLVMGetTypeKind(LLVMTypeOf(rhs_val0)),
                    );
                    if is_float_op {
                        let is_unsigned = |ty: Option<&Type>| {
                            matches!(ty, Some(Type::U8 | Type::U32 | Type::U64 | Type::Usize))
                        };
                        let (lhs_val, rhs_val) = self.coerce_float_binop(
                            lhs_val0,
                            rhs_val0,
                            is_unsigned(lhs_ty.as_ref()),
                            is_unsigned(rhs_ty.as_ref()),
                        );
                        use llvm_sys::LLVMRealPredicate::*;
                        let result = match op {
                            BinaryOp::Add => {
                                LLVMBuildFAdd(self.builder, lhs_val, rhs_val, name.as_ptr())
                            }
                            BinaryOp::Sub => {
                                LLVMBuildFSub(self.builder, lhs_val, rhs_val, name.as_ptr())
                            }
                            BinaryOp::Mul => {
                                LLVMBuildFMul(self.builder, lhs_val, rhs_val, name.as_ptr())
                            }
                            BinaryOp::Div => {
                                LLVMBuildFDiv(self.builder, lhs_val, rhs_val, name.as_ptr())
                            }
                            BinaryOp::Mod => {
                                LLVMBuildFRem(self.builder, lhs_val, rhs_val, name.as_ptr())
                            }
                            BinaryOp::Eq => LLVMBuildFCmp(
                                self.builder,
                                LLVMRealOEQ,
                                lhs_val,
                                rhs_val,
                                name.as_ptr(),
                            ),
                            BinaryOp::Ne => LLVMBuildFCmp(
                                self.builder,
                                LLVMRealUNE,
                                lhs_val,
                                rhs_val,
                                name.as_ptr(),
                            ),
                            BinaryOp::Lt => LLVMBuildFCmp(
                                self.builder,
                                LLVMRealOLT,
                                lhs_val,
                                rhs_val,
                                name.as_ptr(),
                            ),
                            BinaryOp::Le => LLVMBuildFCmp(
                                self.builder,
                                LLVMRealOLE,
                                lhs_val,
                                rhs_val,
                                name.as_ptr(),
                            ),
                            BinaryOp::Gt => LLVMBuildFCmp(
                                self.builder,
                                LLVMRealOGT,
                                lhs_val,
                                rhs_val,
                                name.as_ptr(),
                            ),
                            BinaryOp::Ge => LLVMBuildFCmp(
                                self.builder,
                                LLVMRealOGE,
                                lhs_val,
                                rhs_val,
                                name.as_ptr(),
                            ),
                            BinaryOp::And | BinaryOp::Or => {
                                return Err(anyhow!("logical ops should be lowered in MIR"));
                            }
                        };
                        return Ok(result);
                    }

                    // Integer literals in MIR are currently untyped and codegen as i32.
                    // Coerce integer widths so operations like `usize + 1` work.
                    let (lhs_val, rhs_val) = self.coerce_int_binop(lhs_val0, rhs_val0);

                    let result = match op {
                        BinaryOp::Add => {
                            LLVMBuildAdd(self.builder, lhs_val, rhs_val, name.as_ptr())
                        }
                        BinaryOp::Sub => {
                            LLVMBuildSub(self.builder, lhs_val, rhs_val, name.as_ptr())
                        }
                        BinaryOp::Mul => {
                            LLVMBuildMul(self.builder, lhs_val, rhs_val, name.as_ptr())
                        }
                        BinaryOp::Div => {
                            LLVMBuildSDiv(self.builder, lhs_val, rhs_val, name.as_ptr())
                        }
                        BinaryOp::Mod => {
                            LLVMBuildSRem(self.builder, lhs_val, rhs_val, name.as_ptr())
                        }
                        BinaryOp::Eq => LLVMBuildICmp(
                            self.builder,
                            llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                            lhs_val,
                            rhs_val,
                            name.as_ptr(),
                        ),
                        BinaryOp::Ne => LLVMBuildICmp(
                            self.builder,
                            llvm_sys::LLVMIntPredicate::LLVMIntNE,
                            lhs_val,
                            rhs_val,
                            name.as_ptr(),
                        ),
                        BinaryOp::Lt => LLVMBuildICmp(
                            self.builder,
                            llvm_sys::LLVMIntPredicate::LLVMIntSLT,
                            lhs_val,
                            rhs_val,
                            name.as_ptr(),
                        ),
                        BinaryOp::Le => LLVMBuildICmp(
                            self.builder,
                            llvm_sys::LLVMIntPredicate::LLVMIntSLE,
                            lhs_val,
                            rhs_val,
                            name.as_ptr(),
                        ),
                        BinaryOp::Gt => LLVMBuildICmp(
                            self.builder,
                            llvm_sys::LLVMIntPredicate::LLVMIntSGT,
                            lhs_val,
                            rhs_val,
                            name.as_ptr(),
                        ),
                        BinaryOp::Ge => LLVMBuildICmp(
                            self.builder,
                            llvm_sys::LLVMIntPredicate::LLVMIntSGE,
                            lhs_val,
                            rhs_val,
                            name.as_ptr(),
                        ),
                        BinaryOp::And | BinaryOp::Or => {
                            return Err(anyhow!("logical ops should be lowered in MIR"));
                        }
                    };
                    Ok(result)
                }
                Rvalue::StructLit {
                    struct_name,
                    field_values,
                } => self.codegen_struct_literal(struct_name, field_values, func, local_map),
                Rvalue::FieldAccess {
                    base,
                    field_name: _,
                    field_index,
                } => self.codegen_field_access(*base, *field_index as usize, func, local_map),
                Rvalue::FieldRef {
                    base,
                    field_name: _,
                    field_index,
                    mutability: _,
                } => self.codegen_field_ref(*base, *field_index as usize, func, local_map),
                Rvalue::EnumConstruct {
                    enum_name,
                    variant_index,
                    payload,
                } => {
                    let payload_ty = self
                        .enum_layouts
                        .get(enum_name)
                        .and_then(|l| l.variants.get(*variant_index as usize))
                        .and_then(|v| v.payload.clone());
                    let payload_val = match payload {
                        Some(val) => Some(match payload_ty.as_ref() {
                            Some(ty) => self.codegen_value_owned(val, ty, func, local_map)?,
                            None => self.codegen_value(val, func, local_map)?,
                        }),
                        None => None,
                    };
                    self.codegen_enum_value(enum_name, *variant_index, payload_val)
                }
                Rvalue::EnumTag { base } => {
                    let (enum_name, enum_ptr) =
                        self.enum_pointer_for_local(*base, func, local_map)?;
                    let llvm_enum = self.get_enum_type(&enum_name)?;
                    let tag_ptr = LLVMBuildStructGEP2(
                        self.builder,
                        llvm_enum,
                        enum_ptr,
                        0,
                        CString::new("enum.tag.ptr")?.as_ptr(),
                    );
                    let tag_ty = LLVMInt32TypeInContext(self.context);
                    Ok(LLVMBuildLoad2(
                        self.builder,
                        tag_ty,
                        tag_ptr,
                        CString::new("enum.tag.load")?.as_ptr(),
                    ))
                }
                Rvalue::EnumPayload {
                    base,
                    variant_index,
                    payload_type,
                } => {
                    let (enum_name, enum_ptr) =
                        self.enum_pointer_for_local(*base, func, local_map)?;
                    let llvm_enum = self.get_enum_type(&enum_name)?;
                    let payload_ptr = LLVMBuildStructGEP2(
                        self.builder,
                        llvm_enum,
                        enum_ptr,
                        1 + *variant_index,
                        CString::new("enum.payload.ptr")?.as_ptr(),
                    );
                    let llvm_payload_ty = match payload_type {
                        ty if Self::is_unit_type(ty) => LLVMInt8TypeInContext(self.context),
                        _ => self.get_llvm_type(payload_type)?,
                    };
                    Ok(LLVMBuildLoad2(
                        self.builder,
                        llvm_payload_ty,
                        payload_ptr,
                        CString::new("enum.payload")?.as_ptr(),
                    ))
                }
                Rvalue::FileOpen { path, create } => {
                    self.codegen_file_open(path, *create, func, local_map, functions, mir_module)
                }
                Rvalue::FileReadToString { file } => {
                    self.codegen_file_read_to_string(*file, func, local_map, functions, mir_module)
                }
                Rvalue::FileWriteString { file, contents } => self.codegen_file_write_string(
                    *file, contents, func, local_map, functions, mir_module,
                ),
                Rvalue::FileClose { file } => {
                    self.codegen_file_close(*file, func, local_map, functions, mir_module)
                }
                Rvalue::StringLen { base } => {
                    self.codegen_string_len(*base, func, local_map, functions)
                }
                Rvalue::StringConcat { base, value } => {
                    self.codegen_string_concat(*base, value, func, local_map, functions)
                }
                Rvalue::StringSlice { base, start, len } => {
                    self.codegen_string_slice(*base, start, len, func, local_map, functions)
                }
                Rvalue::StringTrim { base } => {
                    self.codegen_string_trim(*base, func, local_map, functions)
                }
                Rvalue::StringSplit { base, sep } => {
                    self.codegen_string_split(*base, sep, func, local_map, functions)
                }
                Rvalue::StringStartsWith { base, needle } => {
                    self.codegen_string_starts_with(*base, needle, func, local_map, functions)
                }
                Rvalue::StringEndsWith { base, needle } => {
                    self.codegen_string_ends_with(*base, needle, func, local_map, functions)
                }
                Rvalue::StringClone { base } => {
                    self.codegen_string_clone(*base, func, local_map, functions)
                }
                Rvalue::Call { name, args } => {
                    if name == "argv" || name == "std::sys::argv" {
                        return self.codegen_sys_argv_value();
                    }

                    // Some low-level libc helpers may be referenced by stdlib lowering even
                    // when they aren't explicitly declared in the MIR module.
                    let callee = if let Some(callee) = functions.get(name).copied() {
                        callee
                    } else {
                        match name.as_str() {
                            "malloc" => self.ensure_malloc_fn()?,
                            "free" => self.ensure_free_fn()?,
                            "strdup" => self.ensure_strdup_fn()?,
                            "strlen" | "memcpy" | "memcmp" | "strstr" | "isspace" => {
                                self.get_or_declare_extern_function(name)?
                            }
                            _ => return Err(anyhow!("unknown function {}", name)),
                        }
                    };

                    // Find the MIR function to get its type signature
                    let fn_ty = if let Some(target_func) =
                        mir_module.functions.iter().find(|f| f.name == *name)
                    {
                        self.llvm_function_type(target_func)?.0
                    } else if let Some(extern_func) =
                        mir_module.extern_functions.iter().find(|f| f.name == *name)
                    {
                        self.llvm_extern_function_type(extern_func)?.0
                    } else {
                        match name.as_str() {
                            "malloc" => self.malloc_function_type(),
                            "free" => self.free_function_type(),
                            "strdup" => self.strdup_function_type(),
                            "strlen" | "memcpy" | "memcmp" | "strstr" | "isspace" => {
                                self.function_type_for(name)?
                            }
                            _ => bail!("function {} not found in MIR module", name),
                        }
                    };

                    let (mut param_types, mut is_extern) = self.call_param_types(name, mir_module);
                    if param_types.is_empty() {
                        match name.as_str() {
                            "malloc" => {
                                // usize -> ptr
                                param_types = vec![Some(Type::Usize)];
                                is_extern = true;
                            }
                            "free" => {
                                // ptr -> void
                                param_types = vec![Some(Type::RawPtr(Box::new(Type::I8)))];
                                is_extern = true;
                            }
                            "strdup" => {
                                // str -> String
                                param_types = vec![Some(Type::Str)];
                                is_extern = true;
                            }
                            _ => {}
                        }
                    }

                    // Codegen arguments
                    let sret_type = self.sret_functions.get(name).cloned();
                    let mut sret_slot: Option<LLVMValueRef> = None;
                    let mut llvm_args: Vec<LLVMValueRef> = Vec::new();
                    if let Some(ret_ty) = sret_type.as_ref() {
                        let llvm_ret_ty = self.get_llvm_type(ret_ty)?;
                        let slot = LLVMBuildAlloca(
                            self.builder,
                            llvm_ret_ty,
                            CString::new("sret.tmp")?.as_ptr(),
                        );
                        llvm_args.push(slot);
                        sret_slot = Some(slot);
                    }
                    for (idx, arg) in args.iter().enumerate() {
                        let mut arg_val = self.codegen_value(arg, func, local_map)?;

                        if let Some(Some(param_ty)) = param_types.get(idx) {
                            let arg_ty = self.mir_value_type(arg, func);

                            // A non-owning view (skip_drop) passed by value
                            // transfers ownership to the callee, which will
                            // drop it; deep-clone so the container's original
                            // is not freed twice. By-reference params borrow
                            // and extern "C" callees don't run drop glue, so
                            // neither needs a clone.
                            if !is_extern && !matches!(param_ty, Type::Ref(_, _)) {
                                if let MirValue::Local(arg_local) = arg {
                                    let is_view = func
                                        .locals
                                        .get(arg_local.0 as usize)
                                        .map_or(false, |l| l.skip_drop);
                                    if is_view && Self::type_needs_clone(param_ty) {
                                        let clone_ty =
                                            arg_ty.clone().unwrap_or_else(|| param_ty.clone());
                                        arg_val =
                                            self.codegen_deep_clone_value(&clone_ty, arg_val)?;
                                    }
                                }
                            }

                            if let Some(Type::Ref(inner, _)) = arg_ty.as_ref() {
                                if inner.as_ref() == param_ty {
                                    let inner_llvm_ty = self.get_llvm_type(param_ty)?;
                                    arg_val = LLVMBuildLoad2(
                                        self.builder,
                                        inner_llvm_ty,
                                        arg_val,
                                        CString::new("arg.deref")?.as_ptr(),
                                    );
                                }
                            }

                            if let Type::Ref(inner, _) = param_ty {
                                if arg_ty.as_ref() == Some(inner.as_ref()) {
                                    let inner_llvm_ty = self.get_llvm_type(inner)?;
                                    let slot = LLVMBuildAlloca(
                                        self.builder,
                                        inner_llvm_ty,
                                        CString::new("arg.addr")?.as_ptr(),
                                    );
                                    LLVMBuildStore(self.builder, arg_val, slot);
                                    arg_val = slot;
                                }
                            }

                            if is_extern {
                                if let Some(arg_ty) = arg_ty.as_ref() {
                                    if (matches!(arg_ty, Type::Str)
                                        && (matches!(param_ty, Type::RawPtr(_))
                                            || matches!(param_ty, Type::Str)))
                                        || (matches!(arg_ty, Type::String)
                                            && (matches!(param_ty, Type::RawPtr(_))
                                                || matches!(param_ty, Type::Str)
                                                || matches!(param_ty, Type::String)))
                                    {
                                        let expected_ty = self.get_llvm_type(param_ty)?;
                                        if LLVMTypeOf(arg_val) != expected_ty {
                                            let cast_name = CString::new("str.as_ptr")?;
                                            arg_val = LLVMBuildBitCast(
                                                self.builder,
                                                arg_val,
                                                expected_ty,
                                                cast_name.as_ptr(),
                                            );
                                        }
                                    }
                                }
                            }

                            let expected_ty = self.get_llvm_type(param_ty)?;
                            let arg_ty = LLVMTypeOf(arg_val);
                            let expected_kind = LLVMGetTypeKind(expected_ty);
                            let arg_kind = LLVMGetTypeKind(arg_ty);
                            if expected_kind == llvm_sys::LLVMTypeKind::LLVMIntegerTypeKind
                                && arg_kind == llvm_sys::LLVMTypeKind::LLVMIntegerTypeKind
                            {
                                let signed = matches!(param_ty, Type::I8 | Type::I32 | Type::I64);
                                arg_val = self.coerce_int_value(arg_val, expected_ty, signed);
                            } else if Self::is_float_type_kind(expected_kind)
                                && Self::is_float_type_kind(arg_kind)
                                && arg_ty != expected_ty
                            {
                                let cast_name = CString::new("arg.fp.cast")?;
                                arg_val = if matches!(param_ty, Type::F64) {
                                    LLVMBuildFPExt(
                                        self.builder,
                                        arg_val,
                                        expected_ty,
                                        cast_name.as_ptr(),
                                    )
                                } else {
                                    LLVMBuildFPTrunc(
                                        self.builder,
                                        arg_val,
                                        expected_ty,
                                        cast_name.as_ptr(),
                                    )
                                };
                            } else if Self::is_float_type_kind(expected_kind)
                                && arg_kind == llvm_sys::LLVMTypeKind::LLVMIntegerTypeKind
                            {
                                let arg_glyph_ty = self.mir_value_type(arg, func);
                                let unsigned = matches!(
                                    arg_glyph_ty,
                                    Some(Type::U8 | Type::U32 | Type::U64 | Type::Usize)
                                );
                                let cast_name = CString::new("arg.int.to.fp")?;
                                arg_val = if unsigned {
                                    LLVMBuildUIToFP(
                                        self.builder,
                                        arg_val,
                                        expected_ty,
                                        cast_name.as_ptr(),
                                    )
                                } else {
                                    LLVMBuildSIToFP(
                                        self.builder,
                                        arg_val,
                                        expected_ty,
                                        cast_name.as_ptr(),
                                    )
                                };
                            } else if expected_kind == llvm_sys::LLVMTypeKind::LLVMPointerTypeKind
                                && arg_kind == llvm_sys::LLVMTypeKind::LLVMPointerTypeKind
                                && arg_ty != expected_ty
                            {
                                let cast_name = CString::new("arg.ptr.cast")?;
                                arg_val = LLVMBuildBitCast(
                                    self.builder,
                                    arg_val,
                                    expected_ty,
                                    cast_name.as_ptr(),
                                );
                            }
                        }

                        llvm_args.push(arg_val);
                    }

                    let call_val = self.build_call2(fn_ty, callee, &mut llvm_args, "call")?;
                    if let (Some(ret_ty), Some(slot)) = (sret_type, sret_slot) {
                        let llvm_ret_ty = self.get_llvm_type(&ret_ty)?;
                        self.add_sret_call_attribute(call_val, llvm_ret_ty);
                        let load_name = CString::new("sret.load")?;
                        Ok(LLVMBuildLoad2(
                            self.builder,
                            llvm_ret_ty,
                            slot,
                            load_name.as_ptr(),
                        ))
                    } else {
                        Ok(call_val)
                    }
                }
                Rvalue::Ref { base, .. } => {
                    let base_ptr = local_map
                        .get(base)
                        .ok_or_else(|| anyhow!("undefined local {:?}", base))?;
                    Ok(*base_ptr)
                }
                Rvalue::ArrayLit {
                    elem_type,
                    elements,
                } => self.codegen_array_literal(elem_type, elements, func, local_map),
                Rvalue::ArrayIndex {
                    base,
                    index,
                    bounds_check,
                } => self.codegen_array_index(*base, index, *bounds_check, func, local_map),
                Rvalue::ArrayLen { base } => self.codegen_array_len(*base, func),
                Rvalue::VecNew { elem_type } | Rvalue::VecWithCapacity { elem_type, .. } => {
                    // Allocate Vec struct and zero/init fields; capacity handled below.
                    let vec_struct =
                        self.codegen_vec_struct_init(func, local_map, elem_type, rvalue)?;
                    Ok(vec_struct)
                }
                Rvalue::VecLen { vec } => self.codegen_vec_len(*vec, func, local_map),
                Rvalue::VecIndex {
                    vec,
                    elem_type,
                    index,
                    bounds_check,
                } => self.codegen_vec_index(*vec, elem_type, index, *bounds_check, func, local_map),
                Rvalue::VecIndexRef {
                    vec,
                    elem_type,
                    index,
                    bounds_check,
                    ..
                } => self.codegen_vec_index_ref(
                    *vec,
                    elem_type,
                    index,
                    *bounds_check,
                    func,
                    local_map,
                ),
                Rvalue::VecPush {
                    vec,
                    elem_type,
                    value,
                } => self.codegen_vec_push(*vec, elem_type, value, func, local_map),
                Rvalue::VecPop { vec, elem_type } => {
                    self.codegen_vec_pop(*vec, elem_type, func, local_map, mir_module)
                }
                Rvalue::MapNew {
                    key_type,
                    value_type,
                }
                | Rvalue::MapWithCapacity {
                    key_type,
                    value_type,
                    ..
                } => self.codegen_map_init(key_type, value_type, rvalue, func, local_map),
                Rvalue::MapAdd {
                    map,
                    key_type,
                    key,
                    value_type,
                    value,
                } => self.codegen_map_add(
                    *map, key_type, key, value_type, value, func, local_map, functions, mir_module,
                ),
                Rvalue::MapUpdate {
                    map,
                    key_type,
                    key,
                    value_type,
                    value,
                } => self.codegen_map_update(
                    *map, key_type, key, value_type, value, func, local_map, functions, mir_module,
                ),
                Rvalue::MapDel {
                    map,
                    key_type,
                    value_type,
                    key,
                } => self.codegen_map_del(
                    *map, key_type, value_type, key, func, local_map, functions, mir_module,
                ),
                Rvalue::MapGet {
                    map,
                    key_type,
                    value_type,
                    key,
                } => self.codegen_map_get(
                    *map, key_type, value_type, key, func, local_map, functions, mir_module,
                ),
                Rvalue::MapHas { map, key_type, key } => self
                    .codegen_map_has(*map, key_type, key, func, local_map, functions, mir_module),
                Rvalue::MapKeys {
                    map,
                    key_type,
                    value_type,
                } => self.codegen_map_keys(
                    *map, key_type, value_type, func, local_map, functions, mir_module,
                ),
                Rvalue::MapVals {
                    map,
                    key_type,
                    value_type,
                } => self.codegen_map_vals(
                    *map, key_type, value_type, func, local_map, functions, mir_module,
                ),
                Rvalue::OwnNew { elem_type, value } => {
                    self.codegen_own_new(elem_type, value, func, local_map)
                }
                Rvalue::OwnIntoRaw { base, elem_type } => {
                    self.codegen_own_into_raw(*base, elem_type, local_map)
                }
                Rvalue::OwnFromRaw { ptr, elem_type } => {
                    self.codegen_own_from_raw(ptr, elem_type, func, local_map)
                }
                Rvalue::RawPtrNull { elem_type } => self.codegen_raw_ptr_null(elem_type),
                Rvalue::SharedNew { elem_type, value } => {
                    self.codegen_shared_new(elem_type, value, func, local_map)
                }
                Rvalue::SharedClone { base, elem_type } => {
                    self.codegen_shared_clone(*base, elem_type, local_map)
                }
            }
        }
    }

    pub(super) fn codegen_value(
        &mut self,
        value: &MirValue,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        unsafe {
            match value {
                MirValue::Unit => {
                    let ty = LLVMInt32TypeInContext(self.context);
                    Ok(LLVMConstInt(ty, 0, 0))
                }
                MirValue::Int(i) => {
                    let ty = LLVMInt32TypeInContext(self.context);
                    Ok(LLVMConstInt(ty, *i as u64, 1))
                }
                MirValue::Float(fl) => {
                    let ty = LLVMDoubleTypeInContext(self.context);
                    Ok(LLVMConstReal(ty, *fl))
                }
                MirValue::Bool(b) => {
                    let ty = LLVMInt1TypeInContext(self.context);
                    Ok(LLVMConstInt(ty, if *b { 1 } else { 0 }, 0))
                }
                MirValue::Local(local_id) => {
                    let local_ptr = local_map
                        .get(local_id)
                        .ok_or_else(|| anyhow!("undefined local {:?}", local_id))?;

                    if LLVMGetTypeKind(LLVMTypeOf(*local_ptr))
                        == llvm_sys::LLVMTypeKind::LLVMPointerTypeKind
                    {
                        let load_name = CString::new("load")?;
                        let llvm_ty = self.local_llvm_type(func, *local_id)?;
                        Ok(LLVMBuildLoad2(
                            self.builder,
                            llvm_ty,
                            *local_ptr,
                            load_name.as_ptr(),
                        ))
                    } else {
                        Ok(*local_ptr)
                    }
                }
            }
        }
    }

    pub(super) fn local_llvm_type(
        &self,
        func: &MirFunction,
        local_id: LocalId,
    ) -> Result<LLVMTypeRef> {
        let local = func
            .locals
            .get(local_id.0 as usize)
            .ok_or_else(|| anyhow!("missing local {:?}", local_id))?;
        if let Some(ty) = local.ty.as_ref() {
            if Self::is_unit_type(ty) {
                return Ok(unsafe { LLVMInt8TypeInContext(self.context) });
            }
            self.get_llvm_type(ty)
        } else {
            Ok(unsafe { LLVMInt32TypeInContext(self.context) })
        }
    }
}
