use super::*;
use glyph_core::atomic::{
    AtomicOrdering, AtomicRmwOp, AtomicScalar, AtomicTargetCapabilities,
    guaranteed_native_atomic_width, validate_compare_exchange_orderings,
};
use llvm_sys::{LLVMAtomicOrdering, LLVMAtomicRMWBinOp};

impl CodegenContext {
    fn target_pointer_width_bits(&self) -> Result<u32> {
        let target_data = self
            .target_data
            .ok_or_else(|| anyhow!("missing target data while validating atomics"))?;
        Ok(unsafe { LLVMPointerSize(target_data) * 8 })
    }

    fn target_native_atomic_width_bits(&self) -> Result<u32> {
        let triple_ptr = unsafe { LLVMGetTarget(self.module) };
        if triple_ptr.is_null() {
            bail!("missing target triple while validating atomics");
        }
        let triple = unsafe { CStr::from_ptr(triple_ptr) }.to_string_lossy();
        let architecture = triple.split('-').next().unwrap_or_default();
        guaranteed_native_atomic_width(architecture).ok_or_else(|| {
            anyhow!(
                "target `{triple}` has no Glyph native lock-free atomic guarantee; atomics are unsupported"
            )
        })
    }

    fn atomic_width_bits(&self, scalar: AtomicScalar) -> Result<u32> {
        let pointer_width_bits = self.target_pointer_width_bits()?;
        // Glyph's safe v1 guarantee is deliberately conservative: wider-than-
        // pointer operations are rejected instead of becoming hidden locks.
        // AtomicUsize follows Glyph's current fixed-i64 `usize` ABI, so it is
        // likewise rejected on a 32-bit target.
        AtomicTargetCapabilities {
            pointer_width_bits,
            max_atomic_width_bits: self.target_native_atomic_width_bits()?,
        }
        .validate(scalar)
        .map_err(anyhow::Error::msg)
    }

    pub(super) fn atomic_storage_type(&self, scalar: AtomicScalar) -> Result<LLVMTypeRef> {
        let bits = self.atomic_width_bits(scalar)?;
        let llvm_ty = unsafe { LLVMIntTypeInContext(self.context, bits) };
        let target_data = self
            .target_data
            .ok_or_else(|| anyhow!("missing target data while validating atomics"))?;
        let size = unsafe { LLVMABISizeOfType(target_data, llvm_ty) };
        let alignment = unsafe { LLVMABIAlignmentOfType(target_data, llvm_ty) };
        let required = u64::from(bits / 8);
        if size != required || u64::from(alignment) < required {
            bail!(
                "{} requires {}-byte naturally aligned atomic storage, but target layout provides size {} alignment {}",
                scalar.type_name(),
                required,
                size,
                alignment
            );
        }
        Ok(llvm_ty)
    }

    pub(super) fn atomic_alignment(&self, scalar: AtomicScalar) -> Result<u32> {
        let llvm_ty = self.atomic_storage_type(scalar)?;
        let target_data = self
            .target_data
            .ok_or_else(|| anyhow!("missing target data while validating atomics"))?;
        Ok(unsafe { LLVMABIAlignmentOfType(target_data, llvm_ty) })
    }

    fn llvm_atomic_ordering(ordering: AtomicOrdering) -> LLVMAtomicOrdering {
        match ordering {
            AtomicOrdering::Relaxed => LLVMAtomicOrdering::LLVMAtomicOrderingMonotonic,
            AtomicOrdering::Acquire => LLVMAtomicOrdering::LLVMAtomicOrderingAcquire,
            AtomicOrdering::Release => LLVMAtomicOrdering::LLVMAtomicOrderingRelease,
            AtomicOrdering::AcqRel => LLVMAtomicOrdering::LLVMAtomicOrderingAcquireRelease,
            AtomicOrdering::SeqCst => LLVMAtomicOrdering::LLVMAtomicOrderingSequentiallyConsistent,
        }
    }

    fn llvm_atomic_rmw_op(op: AtomicRmwOp) -> LLVMAtomicRMWBinOp {
        match op {
            AtomicRmwOp::Swap => LLVMAtomicRMWBinOp::LLVMAtomicRMWBinOpXchg,
            AtomicRmwOp::Add => LLVMAtomicRMWBinOp::LLVMAtomicRMWBinOpAdd,
            AtomicRmwOp::Sub => LLVMAtomicRMWBinOp::LLVMAtomicRMWBinOpSub,
            AtomicRmwOp::And => LLVMAtomicRMWBinOp::LLVMAtomicRMWBinOpAnd,
            AtomicRmwOp::Or => LLVMAtomicRMWBinOp::LLVMAtomicRMWBinOpOr,
            AtomicRmwOp::Xor => LLVMAtomicRMWBinOp::LLVMAtomicRMWBinOpXor,
        }
    }

    fn scalar_is_signed(scalar: AtomicScalar) -> bool {
        matches!(scalar, AtomicScalar::I32 | AtomicScalar::I64)
    }

    fn atomic_operand(
        &mut self,
        value: &MirValue,
        scalar: AtomicScalar,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        // Extension follows the VALUE's own source signedness, not the
        // atomic's declared scalar type (GLYPH-73): e.g. storing a `u8`
        // into an `AtomicI32` must zero-extend the `u8`.
        let src_ty = self.mir_value_type(value, func);
        let llvm_value = self.codegen_value(value, func, local_map)?;
        let storage_ty = self.atomic_storage_type(scalar)?;
        let signed = src_ty.as_ref().map_or_else(
            || Self::scalar_is_signed(scalar),
            |ty| Self::int_ext_is_signed(ty),
        );
        Ok(self.coerce_int_value(llvm_value, storage_ty, signed))
    }

    fn atomic_result_value(&mut self, value: LLVMValueRef, scalar: AtomicScalar) -> LLVMValueRef {
        if scalar != AtomicScalar::Bool {
            return value;
        }
        unsafe {
            LLVMBuildTrunc(
                self.builder,
                value,
                LLVMInt1TypeInContext(self.context),
                CString::new("atomic.bool").unwrap().as_ptr(),
            )
        }
    }

    fn atomic_pointer(
        &mut self,
        atomic: LocalId,
        scalar: AtomicScalar,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let actual = func
            .locals
            .get(atomic.0 as usize)
            .and_then(|local| local.ty.as_ref());
        let slot = local_map
            .get(&atomic)
            .copied()
            .ok_or_else(|| anyhow!("undefined atomic local {:?}", atomic))?;

        match actual {
            Some(Type::Atomic(actual_scalar)) if *actual_scalar == scalar => Ok(slot),
            Some(Type::Ref(inner, _)) if matches!(inner.as_ref(), Type::Atomic(actual_scalar) if *actual_scalar == scalar) =>
            {
                let pointer_ty = self.get_llvm_type(actual.expect("matched reference type"))?;
                Ok(unsafe {
                    LLVMBuildLoad2(
                        self.builder,
                        pointer_ty,
                        slot,
                        CString::new("atomic.ref")?.as_ptr(),
                    )
                })
            }
            _ => bail!(
                "atomic MIR references local {:?} as {}, but its addressable type is {:?}",
                atomic,
                scalar.type_name(),
                actual
            ),
        }
    }

    pub(super) fn codegen_atomic_new(
        &mut self,
        value: &MirValue,
        scalar: AtomicScalar,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        self.atomic_operand(value, scalar, func, local_map)
    }

    pub(super) fn codegen_atomic_load(
        &mut self,
        atomic: LocalId,
        scalar: AtomicScalar,
        ordering: AtomicOrdering,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let ptr = self.atomic_pointer(atomic, scalar, func, local_map)?;
        self.build_atomic_load_value(ptr, scalar, ordering)
    }

    /// Shared load primitive for compiler-owned synchronization structures.
    pub(super) fn build_atomic_load_value(
        &mut self,
        ptr: LLVMValueRef,
        scalar: AtomicScalar,
        ordering: AtomicOrdering,
    ) -> Result<LLVMValueRef> {
        if !ordering.valid_for_load() {
            bail!("invalid {:?} ordering for atomic load", ordering);
        }
        let storage_ty = self.atomic_storage_type(scalar)?;
        let load = unsafe {
            LLVMBuildLoad2(
                self.builder,
                storage_ty,
                ptr,
                CString::new("atomic.load")?.as_ptr(),
            )
        };
        unsafe {
            LLVMSetOrdering(load, Self::llvm_atomic_ordering(ordering));
            LLVMSetAlignment(load, self.atomic_alignment(scalar)?);
        }
        Ok(self.atomic_result_value(load, scalar))
    }

    pub(super) fn codegen_atomic_store(
        &mut self,
        atomic: LocalId,
        value: &MirValue,
        scalar: AtomicScalar,
        ordering: AtomicOrdering,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let ptr = self.atomic_pointer(atomic, scalar, func, local_map)?;
        let value = self.atomic_operand(value, scalar, func, local_map)?;
        self.build_atomic_store_value(ptr, value, scalar, ordering)?;
        Ok(unsafe { LLVMConstInt(LLVMInt32TypeInContext(self.context), 0, 0) })
    }

    /// Shared store primitive for compiler-owned synchronization structures.
    pub(super) fn build_atomic_store_value(
        &mut self,
        ptr: LLVMValueRef,
        value: LLVMValueRef,
        scalar: AtomicScalar,
        ordering: AtomicOrdering,
    ) -> Result<()> {
        if !ordering.valid_for_store() {
            bail!("invalid {:?} ordering for atomic store", ordering);
        }
        let storage_ty = self.atomic_storage_type(scalar)?;
        let value = self.coerce_int_value(value, storage_ty, Self::scalar_is_signed(scalar));
        let store = unsafe { LLVMBuildStore(self.builder, value, ptr) };
        unsafe {
            LLVMSetOrdering(store, Self::llvm_atomic_ordering(ordering));
            LLVMSetAlignment(store, self.atomic_alignment(scalar)?);
        }
        Ok(())
    }

    pub(super) fn codegen_atomic_rmw(
        &mut self,
        atomic: LocalId,
        value: &MirValue,
        scalar: AtomicScalar,
        op: AtomicRmwOp,
        ordering: AtomicOrdering,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        let ptr = self.atomic_pointer(atomic, scalar, func, local_map)?;
        let value = self.atomic_operand(value, scalar, func, local_map)?;
        self.build_atomic_rmw_value(ptr, value, scalar, op, ordering)
    }

    /// Shared primitive for compiler-owned synchronization structures.
    ///
    /// Public atomic operations, `Arc<T>`, and future lock/channel glue must
    /// all pass through this target validation and alignment path. Keeping the
    /// primitive here prevents a compiler-owned refcount from silently using
    /// weaker target guarantees than source-level atomics.
    pub(super) fn build_atomic_rmw_value(
        &mut self,
        ptr: LLVMValueRef,
        value: LLVMValueRef,
        scalar: AtomicScalar,
        op: AtomicRmwOp,
        ordering: AtomicOrdering,
    ) -> Result<LLVMValueRef> {
        if matches!(scalar, AtomicScalar::Bool) && !matches!(op, AtomicRmwOp::Swap) {
            bail!("AtomicBool only supports atomic swap RMW");
        }
        // Validate the target and coerce compiler-generated constants to the
        // exact storage width before constructing the instruction.
        let storage_ty = self.atomic_storage_type(scalar)?;
        let value = self.coerce_int_value(value, storage_ty, Self::scalar_is_signed(scalar));
        let result = unsafe {
            LLVMBuildAtomicRMW(
                self.builder,
                Self::llvm_atomic_rmw_op(op),
                ptr,
                value,
                Self::llvm_atomic_ordering(ordering),
                0,
            )
        };
        unsafe { LLVMSetAlignment(result, self.atomic_alignment(scalar)?) };
        Ok(self.atomic_result_value(result, scalar))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn codegen_atomic_compare_exchange(
        &mut self,
        atomic: LocalId,
        expected: &MirValue,
        desired: &MirValue,
        scalar: AtomicScalar,
        success: AtomicOrdering,
        failure: AtomicOrdering,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        validate_compare_exchange_orderings(success, failure).map_err(anyhow::Error::msg)?;
        let ptr = self.atomic_pointer(atomic, scalar, func, local_map)?;
        let expected = self.atomic_operand(expected, scalar, func, local_map)?;
        let desired = self.atomic_operand(desired, scalar, func, local_map)?;
        let pair = unsafe {
            LLVMBuildAtomicCmpXchg(
                self.builder,
                ptr,
                expected,
                desired,
                Self::llvm_atomic_ordering(success),
                Self::llvm_atomic_ordering(failure),
                0,
            )
        };
        unsafe { LLVMSetAlignment(pair, self.atomic_alignment(scalar)?) };
        let observed = unsafe {
            LLVMBuildExtractValue(
                self.builder,
                pair,
                0,
                CString::new("atomic.observed")?.as_ptr(),
            )
        };
        Ok(self.atomic_result_value(observed, scalar))
    }

    pub(super) fn codegen_atomic_fence(
        &mut self,
        ordering: AtomicOrdering,
    ) -> Result<LLVMValueRef> {
        self.build_atomic_fence_value(ordering)?;
        Ok(unsafe { LLVMConstInt(LLVMInt32TypeInContext(self.context), 0, 0) })
    }

    pub(super) fn build_atomic_fence_value(&mut self, ordering: AtomicOrdering) -> Result<()> {
        if matches!(ordering, AtomicOrdering::Relaxed) {
            bail!("an atomic fence cannot use Relaxed ordering");
        }
        unsafe {
            LLVMBuildFence(
                self.builder,
                Self::llvm_atomic_ordering(ordering),
                0,
                // Fence instructions have void type and therefore cannot
                // carry an SSA result name.
                CString::new("")?.as_ptr(),
            );
        }
        Ok(())
    }

    pub(super) fn codegen_atomic_is_lock_free(&self, scalar: AtomicScalar) -> Result<LLVMValueRef> {
        self.atomic_storage_type(scalar)?;
        Ok(unsafe { LLVMConstInt(LLVMInt1TypeInContext(self.context), 1, 0) })
    }
}
