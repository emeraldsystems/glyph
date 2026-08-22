//! Compiler-specialized bounded single-producer/single-consumer ring.
//!
//! Each `T` gets a concrete LLVM layout, but no generic source function is
//! required. One allocation contains this header and a trailing `[T; capacity]`:
//!
//! ```text
//! { capacity, head, tail, sender_open, receiver_open, endpoint_refs, [0 x T] }
//! ```
//!
//! The producer owns `tail` and publishes it with Release after initializing a
//! slot. The consumer observes `tail` with Acquire before reading that slot.
//! Conversely, the consumer publishes `head` with Release after clearing a
//! slot and the producer observes it with Acquire before reusing storage.

use super::*;
use glyph_core::atomic::{AtomicOrdering, AtomicRmwOp, AtomicScalar};
use glyph_core::types::Mutability;

const CAPACITY_FIELD: u32 = 0;
const HEAD_FIELD: u32 = 1;
const TAIL_FIELD: u32 = 2;
const SENDER_OPEN_FIELD: u32 = 3;
const RECEIVER_OPEN_FIELD: u32 = 4;
const ENDPOINT_REFS_FIELD: u32 = 5;
const DATA_FIELD: u32 = 6;

impl CodegenContext {
    pub(super) fn spsc_state_type(&self, elem_type: &Type) -> Result<LLVMTypeRef> {
        let usize_ty = self.atomic_storage_type(AtomicScalar::Usize)?;
        let bool_ty = self.atomic_storage_type(AtomicScalar::Bool)?;
        let data_ty = unsafe { LLVMArrayType2(self.get_llvm_type(elem_type)?, 0) };
        let mut fields = [
            usize_ty, usize_ty, usize_ty, bool_ty, bool_ty, usize_ty, data_ty,
        ];
        Ok(unsafe {
            LLVMStructTypeInContext(self.context, fields.as_mut_ptr(), fields.len() as u32, 0)
        })
    }

    pub(super) fn spsc_pointer_type(&self, elem_type: &Type) -> Result<LLVMTypeRef> {
        Ok(unsafe { LLVMPointerType(self.spsc_state_type(elem_type)?, 0) })
    }

    fn spsc_field_ptr(
        &self,
        state: LLVMValueRef,
        elem_type: &Type,
        field: u32,
        name: &str,
    ) -> Result<LLVMValueRef> {
        Ok(unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                self.spsc_state_type(elem_type)?,
                state,
                field,
                CString::new(name)?.as_ptr(),
            )
        })
    }

    fn spsc_data_slot(
        &self,
        state: LLVMValueRef,
        elem_type: &Type,
        index: LLVMValueRef,
        name: &str,
    ) -> Result<LLVMValueRef> {
        let data = self.spsc_field_ptr(state, elem_type, DATA_FIELD, "spsc.data")?;
        let elem_ty = self.get_llvm_type(elem_type)?;
        Ok(unsafe {
            LLVMBuildInBoundsGEP2(
                self.builder,
                elem_ty,
                data,
                [index].as_mut_ptr(),
                1,
                CString::new(name)?.as_ptr(),
            )
        })
    }

    fn spsc_abort(&mut self) -> Result<()> {
        let name = CString::new("abort")?;
        let abort_ty = unsafe {
            LLVMFunctionType(
                LLVMVoidTypeInContext(self.context),
                std::ptr::null_mut(),
                0,
                0,
            )
        };
        let abort = unsafe {
            let existing = LLVMGetNamedFunction(self.module, name.as_ptr());
            if existing.is_null() {
                LLVMAddFunction(self.module, name.as_ptr(), abort_ty)
            } else {
                existing
            }
        };
        self.build_call2(abort_ty, abort, &mut [], "")?;
        unsafe { LLVMBuildUnreachable(self.builder) };
        Ok(())
    }

    fn spsc_validate_elem_type(&self, elem_type: &Type) -> Result<()> {
        if Self::is_unit_type(elem_type) {
            bail!("SPSC<()> is unsupported because unit has no addressable ring slot");
        }
        let llvm_ty = self.get_llvm_type(elem_type)?;
        let target_data = self
            .target_data
            .ok_or_else(|| anyhow!("missing target data for SPSC layout"))?;
        if unsafe { LLVMABISizeOfType(target_data, llvm_ty) } == 0 {
            bail!("SPSC element type has zero-sized target layout");
        }
        Ok(())
    }

    fn spsc_output_slot(
        &self,
        local: LocalId,
        expected: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        label: &str,
    ) -> Result<LLVMValueRef> {
        let actual = func
            .locals
            .get(local.0 as usize)
            .and_then(|entry| entry.ty.as_ref())
            .ok_or_else(|| anyhow!("missing type for SPSC {label} local {local:?}"))?;
        let storage = local_map
            .get(&local)
            .copied()
            .ok_or_else(|| anyhow!("missing storage for SPSC {label} local {local:?}"))?;
        if actual == expected {
            return Ok(storage);
        }
        if matches!(actual, Type::Ref(inner, Mutability::Mutable) if inner.as_ref() == expected) {
            let expected_ptr = unsafe { LLVMPointerType(self.get_llvm_type(expected)?, 0) };
            return Ok(unsafe {
                LLVMBuildLoad2(
                    self.builder,
                    expected_ptr,
                    storage,
                    CString::new(format!("spsc.{label}.out"))?.as_ptr(),
                )
            });
        }
        bail!(
            "SPSC {label} expects writable {:?} storage, found {:?}",
            expected,
            actual
        )
    }

    fn spsc_endpoint_ptr(
        &self,
        local: LocalId,
        expected: &Type,
        elem_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
        label: &str,
    ) -> Result<LLVMValueRef> {
        let actual = func
            .locals
            .get(local.0 as usize)
            .and_then(|entry| entry.ty.as_ref());
        if actual != Some(expected) {
            bail!(
                "SPSC {label} expects endpoint {:?}, found {:?}",
                expected,
                actual
            );
        }
        let slot = local_map
            .get(&local)
            .copied()
            .ok_or_else(|| anyhow!("missing SPSC {label} local {local:?}"))?;
        Ok(unsafe {
            LLVMBuildLoad2(
                self.builder,
                self.spsc_pointer_type(elem_type)?,
                slot,
                CString::new(format!("spsc.{label}.state"))?.as_ptr(),
            )
        })
    }

    pub(super) fn codegen_spsc_channel_new(
        &mut self,
        capacity: &MirValue,
        out_receiver: LocalId,
        elem_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        self.spsc_validate_elem_type(elem_type)?;
        let receiver_ty = Type::spsc_receiver(elem_type.clone());
        let receiver_slot =
            self.spsc_output_slot(out_receiver, &receiver_ty, func, local_map, "receiver")?;
        let usize_ty = self.atomic_storage_type(AtomicScalar::Usize)?;
        let capacity_value = self.codegen_value(capacity, func, local_map)?;
        let capacity = self.ensure_usize(capacity_value, usize_ty)?;
        let state_ty = self.spsc_state_type(elem_type)?;
        let elem_ty = self.get_llvm_type(elem_type)?;
        let header_size = unsafe { LLVMSizeOf(state_ty) };
        let elem_size = unsafe { LLVMSizeOf(elem_ty) };
        let max = unsafe { LLVMConstAllOnes(usize_ty) };
        let room = unsafe { LLVMConstSub(max, header_size) };
        let max_capacity = unsafe {
            LLVMBuildUDiv(
                self.builder,
                room,
                elem_size,
                CString::new("spsc.max.capacity")?.as_ptr(),
            )
        };
        let zero = unsafe { LLVMConstInt(usize_ty, 0, 0) };
        let invalid_zero = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                capacity,
                zero,
                CString::new("spsc.capacity.zero")?.as_ptr(),
            )
        };
        let invalid_large = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntUGT,
                capacity,
                max_capacity,
                CString::new("spsc.capacity.overflow")?.as_ptr(),
            )
        };
        let invalid = unsafe {
            LLVMBuildOr(
                self.builder,
                invalid_zero,
                invalid_large,
                CString::new("spsc.capacity.invalid")?.as_ptr(),
            )
        };
        let parent = unsafe { LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder)) };
        let fatal = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("spsc.new.invalid")?.as_ptr(),
            )
        };
        let allocate = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("spsc.new.allocate")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, invalid, fatal, allocate) };
        unsafe { LLVMPositionBuilderAtEnd(self.builder, fatal) };
        self.spsc_abort()?;

        unsafe { LLVMPositionBuilderAtEnd(self.builder, allocate) };
        let payload_size = unsafe {
            LLVMBuildMul(
                self.builder,
                capacity,
                elem_size,
                CString::new("spsc.payload.bytes")?.as_ptr(),
            )
        };
        let total_size = unsafe {
            LLVMBuildAdd(
                self.builder,
                header_size,
                payload_size,
                CString::new("spsc.total.bytes")?.as_ptr(),
            )
        };
        let malloc = self.ensure_malloc_fn()?;
        let raw = self.build_call2(
            self.malloc_function_type(),
            malloc,
            &mut [total_size],
            "spsc.alloc",
        )?;
        let state = unsafe {
            LLVMBuildBitCast(
                self.builder,
                raw,
                self.spsc_pointer_type(elem_type)?,
                CString::new("spsc.state")?.as_ptr(),
            )
        };
        let is_null = unsafe {
            LLVMBuildIsNull(
                self.builder,
                state,
                CString::new("spsc.alloc.null")?.as_ptr(),
            )
        };
        let alloc_fatal = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("spsc.new.oom")?.as_ptr(),
            )
        };
        let init = unsafe {
            LLVMAppendBasicBlockInContext(
                self.context,
                parent,
                CString::new("spsc.new.init")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, is_null, alloc_fatal, init) };
        unsafe { LLVMPositionBuilderAtEnd(self.builder, alloc_fatal) };
        self.spsc_abort()?;

        unsafe { LLVMPositionBuilderAtEnd(self.builder, init) };
        unsafe {
            LLVMBuildStore(
                self.builder,
                capacity,
                self.spsc_field_ptr(state, elem_type, CAPACITY_FIELD, "spsc.capacity")?,
            );
            LLVMBuildStore(
                self.builder,
                zero,
                self.spsc_field_ptr(state, elem_type, HEAD_FIELD, "spsc.head")?,
            );
            LLVMBuildStore(
                self.builder,
                zero,
                self.spsc_field_ptr(state, elem_type, TAIL_FIELD, "spsc.tail")?,
            );
            let bool_ty = self.atomic_storage_type(AtomicScalar::Bool)?;
            let open = LLVMConstInt(bool_ty, 1, 0);
            LLVMBuildStore(
                self.builder,
                open,
                self.spsc_field_ptr(state, elem_type, SENDER_OPEN_FIELD, "spsc.sender.open")?,
            );
            LLVMBuildStore(
                self.builder,
                open,
                self.spsc_field_ptr(state, elem_type, RECEIVER_OPEN_FIELD, "spsc.receiver.open")?,
            );
            LLVMBuildStore(
                self.builder,
                LLVMConstInt(usize_ty, 2, 0),
                self.spsc_field_ptr(state, elem_type, ENDPOINT_REFS_FIELD, "spsc.endpoint.refs")?,
            );
            LLVMBuildStore(self.builder, state, receiver_slot);
        }
        Ok(state)
    }

    pub(super) fn codegen_spsc_try_send(
        &mut self,
        sender: LocalId,
        value: LocalId,
        out_unsent: LocalId,
        elem_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        self.spsc_validate_elem_type(elem_type)?;
        let sender_ty = Type::spsc_sender(elem_type.clone());
        let state =
            self.spsc_endpoint_ptr(sender, &sender_ty, elem_type, func, local_map, "send")?;
        let value_actual = func
            .locals
            .get(value.0 as usize)
            .and_then(|entry| entry.ty.as_ref());
        if value_actual != Some(elem_type) {
            bail!(
                "SPSC send value expects {:?}, found {:?}",
                elem_type,
                value_actual
            );
        }
        let value_slot = local_map
            .get(&value)
            .copied()
            .ok_or_else(|| anyhow!("missing SPSC send value local {value:?}"))?;
        let out_slot = self.spsc_output_slot(out_unsent, elem_type, func, local_map, "unsent")?;
        let elem_ty = self.get_llvm_type(elem_type)?;
        let owned = unsafe {
            let loaded = LLVMBuildLoad2(
                self.builder,
                elem_ty,
                value_slot,
                CString::new("spsc.send.value")?.as_ptr(),
            );
            LLVMBuildStore(self.builder, LLVMConstNull(elem_ty), value_slot);
            LLVMBuildStore(self.builder, LLVMConstNull(elem_ty), out_slot);
            loaded
        };
        let parent = unsafe { LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder)) };
        let live = self.append_spsc_block(parent, "spsc.send.live")?;
        let disconnected = self.append_spsc_block(parent, "spsc.send.disconnected")?;
        let check_room = self.append_spsc_block(parent, "spsc.send.check_room")?;
        let full = self.append_spsc_block(parent, "spsc.send.full")?;
        let publish = self.append_spsc_block(parent, "spsc.send.publish")?;
        let done = self.append_spsc_block(parent, "spsc.send.done")?;
        let is_null = unsafe {
            LLVMBuildIsNull(
                self.builder,
                state,
                CString::new("spsc.send.null")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, is_null, disconnected, live) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, live) };
        let receiver_open = self.build_atomic_load_value(
            self.spsc_field_ptr(
                state,
                elem_type,
                RECEIVER_OPEN_FIELD,
                "spsc.send.receiver_open",
            )?,
            AtomicScalar::Bool,
            AtomicOrdering::Acquire,
        )?;
        unsafe { LLVMBuildCondBr(self.builder, receiver_open, check_room, disconnected) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, check_room) };
        let usize_ty = self.atomic_storage_type(AtomicScalar::Usize)?;
        let head = self.build_atomic_load_value(
            self.spsc_field_ptr(state, elem_type, HEAD_FIELD, "spsc.send.head")?,
            AtomicScalar::Usize,
            AtomicOrdering::Acquire,
        )?;
        let tail = self.build_atomic_load_value(
            self.spsc_field_ptr(state, elem_type, TAIL_FIELD, "spsc.send.tail")?,
            AtomicScalar::Usize,
            AtomicOrdering::Relaxed,
        )?;
        let capacity = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                self.spsc_field_ptr(state, elem_type, CAPACITY_FIELD, "spsc.send.capacity")?,
                CString::new("spsc.send.capacity.load")?.as_ptr(),
            )
        };
        let used = unsafe {
            LLVMBuildSub(
                self.builder,
                tail,
                head,
                CString::new("spsc.send.used")?.as_ptr(),
            )
        };
        let is_full = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntUGE,
                used,
                capacity,
                CString::new("spsc.send.full")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, is_full, full, publish) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, publish) };
        let index = unsafe {
            LLVMBuildURem(
                self.builder,
                tail,
                capacity,
                CString::new("spsc.send.index")?.as_ptr(),
            )
        };
        unsafe {
            LLVMBuildStore(
                self.builder,
                owned,
                self.spsc_data_slot(state, elem_type, index, "spsc.send.slot")?,
            );
        }
        let next = unsafe {
            LLVMBuildAdd(
                self.builder,
                tail,
                LLVMConstInt(usize_ty, 1, 0),
                CString::new("spsc.send.next")?.as_ptr(),
            )
        };
        self.build_atomic_store_value(
            self.spsc_field_ptr(state, elem_type, TAIL_FIELD, "spsc.send.publish.tail")?,
            next,
            AtomicScalar::Usize,
            AtomicOrdering::Release,
        )?;
        unsafe { LLVMBuildBr(self.builder, done) };
        let publish_end = unsafe { LLVMGetInsertBlock(self.builder) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, full) };
        unsafe { LLVMBuildStore(self.builder, owned, out_slot) };
        unsafe { LLVMBuildBr(self.builder, done) };
        let full_end = unsafe { LLVMGetInsertBlock(self.builder) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, disconnected) };
        unsafe { LLVMBuildStore(self.builder, owned, out_slot) };
        unsafe { LLVMBuildBr(self.builder, done) };
        let disconnected_end = unsafe { LLVMGetInsertBlock(self.builder) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, done) };
        let status_ty = unsafe { LLVMInt32TypeInContext(self.context) };
        let phi = unsafe {
            LLVMBuildPhi(
                self.builder,
                status_ty,
                CString::new("spsc.send.status")?.as_ptr(),
            )
        };
        let mut values = unsafe {
            [
                LLVMConstInt(status_ty, 0, 0),
                LLVMConstInt(status_ty, 1, 0),
                LLVMConstInt(status_ty, 2, 0),
            ]
        };
        let mut blocks = [publish_end, full_end, disconnected_end];
        unsafe { LLVMAddIncoming(phi, values.as_mut_ptr(), blocks.as_mut_ptr(), 3) };
        Ok(phi)
    }

    pub(super) fn codegen_spsc_try_recv(
        &mut self,
        receiver: LocalId,
        out_value: LocalId,
        elem_type: &Type,
        func: &MirFunction,
        local_map: &HashMap<LocalId, LLVMValueRef>,
    ) -> Result<LLVMValueRef> {
        self.spsc_validate_elem_type(elem_type)?;
        let receiver_ty = Type::spsc_receiver(elem_type.clone());
        let state =
            self.spsc_endpoint_ptr(receiver, &receiver_ty, elem_type, func, local_map, "recv")?;
        let out_slot = self.spsc_output_slot(out_value, elem_type, func, local_map, "received")?;
        let elem_ty = self.get_llvm_type(elem_type)?;
        unsafe { LLVMBuildStore(self.builder, LLVMConstNull(elem_ty), out_slot) };
        let parent = unsafe { LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder)) };
        let live = self.append_spsc_block(parent, "spsc.recv.live")?;
        let value_block = self.append_spsc_block(parent, "spsc.recv.value")?;
        let empty_block = self.append_spsc_block(parent, "spsc.recv.empty")?;
        let empty = self.append_spsc_block(parent, "spsc.recv.still_open")?;
        let disconnected = self.append_spsc_block(parent, "spsc.recv.disconnected")?;
        let done = self.append_spsc_block(parent, "spsc.recv.done")?;
        let is_null = unsafe {
            LLVMBuildIsNull(
                self.builder,
                state,
                CString::new("spsc.recv.null")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, is_null, disconnected, live) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, live) };
        let head = self.build_atomic_load_value(
            self.spsc_field_ptr(state, elem_type, HEAD_FIELD, "spsc.recv.head")?,
            AtomicScalar::Usize,
            AtomicOrdering::Relaxed,
        )?;
        let tail = self.build_atomic_load_value(
            self.spsc_field_ptr(state, elem_type, TAIL_FIELD, "spsc.recv.tail")?,
            AtomicScalar::Usize,
            AtomicOrdering::Acquire,
        )?;
        let is_empty = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                head,
                tail,
                CString::new("spsc.recv.is_empty")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, is_empty, empty_block, value_block) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, value_block) };
        let usize_ty = self.atomic_storage_type(AtomicScalar::Usize)?;
        let capacity = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                self.spsc_field_ptr(state, elem_type, CAPACITY_FIELD, "spsc.recv.capacity")?,
                CString::new("spsc.recv.capacity.load")?.as_ptr(),
            )
        };
        let index = unsafe {
            LLVMBuildURem(
                self.builder,
                head,
                capacity,
                CString::new("spsc.recv.index")?.as_ptr(),
            )
        };
        let ring_slot = self.spsc_data_slot(state, elem_type, index, "spsc.recv.slot")?;
        let value = unsafe {
            LLVMBuildLoad2(
                self.builder,
                elem_ty,
                ring_slot,
                CString::new("spsc.recv.value.load")?.as_ptr(),
            )
        };
        unsafe {
            LLVMBuildStore(self.builder, LLVMConstNull(elem_ty), ring_slot);
            LLVMBuildStore(self.builder, value, out_slot);
        }
        let next = unsafe {
            LLVMBuildAdd(
                self.builder,
                head,
                LLVMConstInt(usize_ty, 1, 0),
                CString::new("spsc.recv.next")?.as_ptr(),
            )
        };
        self.build_atomic_store_value(
            self.spsc_field_ptr(state, elem_type, HEAD_FIELD, "spsc.recv.publish.head")?,
            next,
            AtomicScalar::Usize,
            AtomicOrdering::Release,
        )?;
        unsafe { LLVMBuildBr(self.builder, done) };
        let value_end = unsafe { LLVMGetInsertBlock(self.builder) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, empty_block) };
        let sender_open = self.build_atomic_load_value(
            self.spsc_field_ptr(state, elem_type, SENDER_OPEN_FIELD, "spsc.recv.sender_open")?,
            AtomicScalar::Bool,
            AtomicOrdering::Acquire,
        )?;
        unsafe { LLVMBuildCondBr(self.builder, sender_open, empty, disconnected) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, empty) };
        unsafe { LLVMBuildBr(self.builder, done) };
        let empty_end = unsafe { LLVMGetInsertBlock(self.builder) };
        unsafe { LLVMPositionBuilderAtEnd(self.builder, disconnected) };
        unsafe { LLVMBuildBr(self.builder, done) };
        let disconnected_end = unsafe { LLVMGetInsertBlock(self.builder) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, done) };
        let status_ty = unsafe { LLVMInt32TypeInContext(self.context) };
        let phi = unsafe {
            LLVMBuildPhi(
                self.builder,
                status_ty,
                CString::new("spsc.recv.status")?.as_ptr(),
            )
        };
        let mut values = unsafe {
            [
                LLVMConstInt(status_ty, 0, 0),
                LLVMConstInt(status_ty, 1, 0),
                LLVMConstInt(status_ty, 2, 0),
            ]
        };
        let mut blocks = [value_end, empty_end, disconnected_end];
        unsafe { LLVMAddIncoming(phi, values.as_mut_ptr(), blocks.as_mut_ptr(), 3) };
        Ok(phi)
    }

    fn append_spsc_block(&self, parent: LLVMValueRef, name: &str) -> Result<LLVMBasicBlockRef> {
        Ok(unsafe {
            LLVMAppendBasicBlockInContext(self.context, parent, CString::new(name)?.as_ptr())
        })
    }

    pub(super) fn codegen_drop_spsc_endpoint_slot(
        &mut self,
        slot: LLVMValueRef,
        elem_type: &Type,
        sender: bool,
    ) -> Result<()> {
        self.spsc_validate_elem_type(elem_type)?;
        let ptr_ty = self.spsc_pointer_type(elem_type)?;
        let state = unsafe {
            LLVMBuildLoad2(
                self.builder,
                ptr_ty,
                slot,
                CString::new("spsc.drop.state")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildStore(self.builder, LLVMConstPointerNull(ptr_ty), slot) };
        let parent = unsafe { LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder)) };
        let release = self.append_spsc_block(parent, "spsc.drop.release")?;
        let underflow = self.append_spsc_block(parent, "spsc.drop.underflow")?;
        let check_last = self.append_spsc_block(parent, "spsc.drop.check_last")?;
        let finalize = self.append_spsc_block(parent, "spsc.drop.finalize")?;
        let loop_check = self.append_spsc_block(parent, "spsc.drop.drain.check")?;
        let loop_body = self.append_spsc_block(parent, "spsc.drop.drain.value")?;
        let free = self.append_spsc_block(parent, "spsc.drop.free")?;
        let done = self.append_spsc_block(parent, "spsc.drop.done")?;
        let is_null = unsafe {
            LLVMBuildIsNull(
                self.builder,
                state,
                CString::new("spsc.drop.null")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, is_null, done, release) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, release) };
        let bool_ty = self.atomic_storage_type(AtomicScalar::Bool)?;
        self.build_atomic_store_value(
            self.spsc_field_ptr(
                state,
                elem_type,
                if sender {
                    SENDER_OPEN_FIELD
                } else {
                    RECEIVER_OPEN_FIELD
                },
                "spsc.drop.open",
            )?,
            unsafe { LLVMConstInt(bool_ty, 0, 0) },
            AtomicScalar::Bool,
            AtomicOrdering::Release,
        )?;
        let usize_ty = self.atomic_storage_type(AtomicScalar::Usize)?;
        let one = unsafe { LLVMConstInt(usize_ty, 1, 0) };
        let old = self.build_atomic_rmw_value(
            self.spsc_field_ptr(state, elem_type, ENDPOINT_REFS_FIELD, "spsc.drop.refs")?,
            one,
            AtomicScalar::Usize,
            AtomicRmwOp::Sub,
            AtomicOrdering::Release,
        )?;
        let was_zero = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                old,
                LLVMConstInt(usize_ty, 0, 0),
                CString::new("spsc.drop.was_zero")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, was_zero, underflow, check_last) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, underflow) };
        self.spsc_abort()?;

        unsafe { LLVMPositionBuilderAtEnd(self.builder, check_last) };
        let was_last = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                old,
                one,
                CString::new("spsc.drop.was_last")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, was_last, finalize, done) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, finalize) };
        self.build_atomic_fence_value(AtomicOrdering::Acquire)?;
        let head = self.build_atomic_load_value(
            self.spsc_field_ptr(state, elem_type, HEAD_FIELD, "spsc.drop.head")?,
            AtomicScalar::Usize,
            AtomicOrdering::Acquire,
        )?;
        let tail = self.build_atomic_load_value(
            self.spsc_field_ptr(state, elem_type, TAIL_FIELD, "spsc.drop.tail")?,
            AtomicScalar::Usize,
            AtomicOrdering::Acquire,
        )?;
        let capacity = unsafe {
            LLVMBuildLoad2(
                self.builder,
                usize_ty,
                self.spsc_field_ptr(state, elem_type, CAPACITY_FIELD, "spsc.drop.capacity")?,
                CString::new("spsc.drop.capacity.load")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildBr(self.builder, loop_check) };
        let initial = unsafe { LLVMGetInsertBlock(self.builder) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, loop_check) };
        let cursor = unsafe {
            LLVMBuildPhi(
                self.builder,
                usize_ty,
                CString::new("spsc.drop.cursor")?.as_ptr(),
            )
        };
        let mut initial_value = [head];
        let mut initial_block = [initial];
        unsafe {
            LLVMAddIncoming(
                cursor,
                initial_value.as_mut_ptr(),
                initial_block.as_mut_ptr(),
                1,
            )
        };
        let finished = unsafe {
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                cursor,
                tail,
                CString::new("spsc.drop.drained")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildCondBr(self.builder, finished, free, loop_body) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, loop_body) };
        let index = unsafe {
            LLVMBuildURem(
                self.builder,
                cursor,
                capacity,
                CString::new("spsc.drop.index")?.as_ptr(),
            )
        };
        let value_slot = self.spsc_data_slot(state, elem_type, index, "spsc.drop.value")?;
        self.codegen_drop_elem_slot(value_slot, elem_type)?;
        let next = unsafe {
            LLVMBuildAdd(
                self.builder,
                cursor,
                one,
                CString::new("spsc.drop.next")?.as_ptr(),
            )
        };
        unsafe { LLVMBuildBr(self.builder, loop_check) };
        let body_end = unsafe { LLVMGetInsertBlock(self.builder) };
        let mut next_value = [next];
        let mut body_block = [body_end];
        unsafe { LLVMAddIncoming(cursor, next_value.as_mut_ptr(), body_block.as_mut_ptr(), 1) };

        unsafe { LLVMPositionBuilderAtEnd(self.builder, free) };
        self.codegen_free(state)?;
        unsafe { LLVMBuildBr(self.builder, done) };
        unsafe { LLVMPositionBuilderAtEnd(self.builder, done) };
        Ok(())
    }
}
