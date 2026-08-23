use super::*;

impl CodegenContext {
    pub(super) fn debug_map_log(
        &mut self,
        label: &str,
        cap: LLVMValueRef,
        len: LLVMValueRef,
        head: LLVMValueRef,
    ) -> Result<()> {
        if std::env::var("GLYPH_DEBUG_MAP").is_err() {
            return Ok(());
        }

        let printf_fn = self.ensure_printf_fn()?;
        let printf_ty = self.printf_function_type();
        let fmt_name = format!(".str.map.debug.{}", label);
        let fmt = format!("[map:{}] cap=%llu len=%llu head=%p\n", label);
        let fmt_ptr = self.codegen_string_literal(&fmt, &fmt_name)?;

        let i8_ty = unsafe { LLVMInt8TypeInContext(self.context) };
        let i8_ptr_ty = unsafe { LLVMPointerType(i8_ty, 0) };
        let head_ptr = unsafe {
            LLVMBuildBitCast(
                self.builder,
                head,
                i8_ptr_ty,
                CString::new("map.head.cast")?.as_ptr(),
            )
        };

        let u64_ty = unsafe { LLVMInt64TypeInContext(self.context) };
        let cap_val = self.ensure_usize(cap, u64_ty)?;
        let len_val = self.ensure_usize(len, u64_ty)?;
        let mut args = vec![fmt_ptr, cap_val, len_val, head_ptr];
        unsafe {
            LLVMBuildCall2(
                self.builder,
                printf_ty,
                printf_fn,
                args.as_mut_ptr(),
                args.len() as u32,
                CString::new("map.debug")?.as_ptr(),
            );
        }
        Ok(())
    }

    pub(super) fn debug_map_log_index(
        &mut self,
        label: &str,
        cap: LLVMValueRef,
        len: LLVMValueRef,
        buckets: LLVMValueRef,
        head: LLVMValueRef,
        idx: LLVMValueRef,
        hash: LLVMValueRef,
        key_len: LLVMValueRef,
    ) -> Result<()> {
        if std::env::var("GLYPH_DEBUG_MAP").is_err() {
            return Ok(());
        }

        let printf_fn = self.ensure_printf_fn()?;
        let printf_ty = self.printf_function_type();
        let fmt_name = format!(".str.map.debug.idx.{}", label);
        let fmt = format!(
            "[map:{}] cap=%llu len=%llu buckets=%p head=%p idx=%llu hash=%llu key_len=%llu\n",
            label
        );
        let fmt_ptr = self.codegen_string_literal(&fmt, &fmt_name)?;

        let i8_ty = unsafe { LLVMInt8TypeInContext(self.context) };
        let i8_ptr_ty = unsafe { LLVMPointerType(i8_ty, 0) };
        let buckets_ptr = unsafe {
            LLVMBuildBitCast(
                self.builder,
                buckets,
                i8_ptr_ty,
                CString::new("map.buckets.cast")?.as_ptr(),
            )
        };
        let head_ptr = unsafe {
            LLVMBuildBitCast(
                self.builder,
                head,
                i8_ptr_ty,
                CString::new("map.head.cast")?.as_ptr(),
            )
        };

        let u64_ty = unsafe { LLVMInt64TypeInContext(self.context) };
        let cap_val = self.ensure_usize(cap, u64_ty)?;
        let len_val = self.ensure_usize(len, u64_ty)?;
        let idx_val = self.ensure_usize(idx, u64_ty)?;
        let hash_val = self.ensure_usize(hash, u64_ty)?;
        let key_len_val = self.ensure_usize(key_len, u64_ty)?;
        let mut args = vec![
            fmt_ptr,
            cap_val,
            len_val,
            buckets_ptr,
            head_ptr,
            idx_val,
            hash_val,
            key_len_val,
        ];
        unsafe {
            LLVMBuildCall2(
                self.builder,
                printf_ty,
                printf_fn,
                args.as_mut_ptr(),
                args.len() as u32,
                CString::new("map.debug.idx")?.as_ptr(),
            );
        }
        Ok(())
    }

    pub(super) fn debug_map_key(
        &mut self,
        label: &str,
        key_ptr: LLVMValueRef,
        key_len: LLVMValueRef,
    ) -> Result<()> {
        if std::env::var("GLYPH_DEBUG_MAP").is_err() {
            return Ok(());
        }

        let printf_fn = self.ensure_printf_fn()?;
        let printf_ty = self.printf_function_type();
        let fmt_name = format!(".str.map.debug.key.{}", label);
        let fmt = format!("[map:{}] key=%p len=%llu first=%llu\n", label);
        let fmt_ptr = self.codegen_string_literal(&fmt, &fmt_name)?;

        let i8_ty = unsafe { LLVMInt8TypeInContext(self.context) };
        let i8_ptr_ty = unsafe { LLVMPointerType(i8_ty, 0) };
        let key_ptr_cast = unsafe {
            LLVMBuildBitCast(
                self.builder,
                key_ptr,
                i8_ptr_ty,
                CString::new("map.key.cast")?.as_ptr(),
            )
        };
        let first_ptr = unsafe {
            LLVMBuildGEP2(
                self.builder,
                i8_ty,
                key_ptr_cast,
                vec![LLVMConstInt(LLVMInt64TypeInContext(self.context), 0, 0)].as_mut_ptr(),
                1,
                CString::new("map.key.first")?.as_ptr(),
            )
        };
        let first_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                i8_ty,
                first_ptr,
                CString::new("map.key.first.load")?.as_ptr(),
            )
        };
        let first_u64 = unsafe {
            LLVMBuildZExt(
                self.builder,
                first_val,
                LLVMInt64TypeInContext(self.context),
                CString::new("map.key.first.u64")?.as_ptr(),
            )
        };
        let len_ty = unsafe { LLVMInt64TypeInContext(self.context) };
        let len_val = self.ensure_usize(key_len, len_ty)?;
        let mut args = vec![fmt_ptr, key_ptr_cast, len_val, first_u64];
        unsafe {
            LLVMBuildCall2(
                self.builder,
                printf_ty,
                printf_fn,
                args.as_mut_ptr(),
                args.len() as u32,
                CString::new("map.debug.key")?.as_ptr(),
            );
        }
        Ok(())
    }

    pub(super) fn debug_map_key_compare(
        &mut self,
        label: &str,
        input_ptr: LLVMValueRef,
        bucket_ptr: LLVMValueRef,
        input_len: LLVMValueRef,
        bucket_len: LLVMValueRef,
        eq_val: LLVMValueRef,
    ) -> Result<()> {
        if std::env::var("GLYPH_DEBUG_MAP").is_err() {
            return Ok(());
        }

        let printf_fn = self.ensure_printf_fn()?;
        let printf_ty = self.printf_function_type();
        let fmt_name = format!(".str.map.debug.cmp.{}", label);
        let fmt = format!(
            "[map:{}] eq=%llu in.len=%llu bucket.len=%llu in.first=%llu bucket.first=%llu\n",
            label
        );
        let fmt_ptr = self.codegen_string_literal(&fmt, &fmt_name)?;

        let i8_ty = unsafe { LLVMInt8TypeInContext(self.context) };
        let i8_ptr_ty = unsafe { LLVMPointerType(i8_ty, 0) };
        let input_ptr = unsafe {
            LLVMBuildBitCast(
                self.builder,
                input_ptr,
                i8_ptr_ty,
                CString::new("map.cmp.in.cast")?.as_ptr(),
            )
        };
        let bucket_ptr = unsafe {
            LLVMBuildBitCast(
                self.builder,
                bucket_ptr,
                i8_ptr_ty,
                CString::new("map.cmp.bucket.cast")?.as_ptr(),
            )
        };
        let zero_idx = unsafe { LLVMConstInt(LLVMInt64TypeInContext(self.context), 0, 0) };
        let input_first_ptr = unsafe {
            LLVMBuildGEP2(
                self.builder,
                i8_ty,
                input_ptr,
                vec![zero_idx].as_mut_ptr(),
                1,
                CString::new("map.cmp.in.first")?.as_ptr(),
            )
        };
        let bucket_first_ptr = unsafe {
            LLVMBuildGEP2(
                self.builder,
                i8_ty,
                bucket_ptr,
                vec![zero_idx].as_mut_ptr(),
                1,
                CString::new("map.cmp.bucket.first")?.as_ptr(),
            )
        };
        let input_first = unsafe {
            LLVMBuildLoad2(
                self.builder,
                i8_ty,
                input_first_ptr,
                CString::new("map.cmp.in.first.load")?.as_ptr(),
            )
        };
        let bucket_first = unsafe {
            LLVMBuildLoad2(
                self.builder,
                i8_ty,
                bucket_first_ptr,
                CString::new("map.cmp.bucket.first.load")?.as_ptr(),
            )
        };
        let u64_ty = unsafe { LLVMInt64TypeInContext(self.context) };
        let input_first_u64 = unsafe {
            LLVMBuildZExt(
                self.builder,
                input_first,
                u64_ty,
                CString::new("map.cmp.in.first.u64")?.as_ptr(),
            )
        };
        let bucket_first_u64 = unsafe {
            LLVMBuildZExt(
                self.builder,
                bucket_first,
                u64_ty,
                CString::new("map.cmp.bucket.first.u64")?.as_ptr(),
            )
        };
        let input_len_u64 = self.ensure_usize(input_len, u64_ty)?;
        let bucket_len_u64 = self.ensure_usize(bucket_len, u64_ty)?;
        let eq_u64 = unsafe {
            LLVMBuildZExt(
                self.builder,
                eq_val,
                u64_ty,
                CString::new("map.cmp.eq.u64")?.as_ptr(),
            )
        };
        let mut args = vec![
            fmt_ptr,
            eq_u64,
            input_len_u64,
            bucket_len_u64,
            input_first_u64,
            bucket_first_u64,
        ];
        unsafe {
            LLVMBuildCall2(
                self.builder,
                printf_ty,
                printf_fn,
                args.as_mut_ptr(),
                args.len() as u32,
                CString::new("map.debug.cmp")?.as_ptr(),
            );
        }
        Ok(())
    }

    pub(super) fn debug_map_hash_compare(
        &mut self,
        label: &str,
        input_hash: LLVMValueRef,
        bucket_hash: LLVMValueRef,
    ) -> Result<()> {
        if std::env::var("GLYPH_DEBUG_MAP").is_err() {
            return Ok(());
        }

        let printf_fn = self.ensure_printf_fn()?;
        let printf_ty = self.printf_function_type();
        let fmt_name = format!(".str.map.debug.hash.{}", label);
        let fmt = format!("[map:{}] hash.in=%llu hash.bucket=%llu\n", label);
        let fmt_ptr = self.codegen_string_literal(&fmt, &fmt_name)?;
        let u64_ty = unsafe { LLVMInt64TypeInContext(self.context) };
        let in_val = self.ensure_usize(input_hash, u64_ty)?;
        let bucket_val = self.ensure_usize(bucket_hash, u64_ty)?;
        let mut args = vec![fmt_ptr, in_val, bucket_val];
        unsafe {
            LLVMBuildCall2(
                self.builder,
                printf_ty,
                printf_fn,
                args.as_mut_ptr(),
                args.len() as u32,
                CString::new("map.debug.hash")?.as_ptr(),
            );
        }
        Ok(())
    }

    pub(super) fn debug_map_tag(&mut self, label: &str, tag: LLVMValueRef) -> Result<()> {
        if std::env::var("GLYPH_DEBUG_MAP").is_err() {
            return Ok(());
        }

        let printf_fn = self.ensure_printf_fn()?;
        let printf_ty = self.printf_function_type();
        let fmt_name = format!(".str.map.debug.tag.{}", label);
        let fmt = format!("[map:{}] tag=%llu\n", label);
        let fmt_ptr = self.codegen_string_literal(&fmt, &fmt_name)?;
        let u64_ty = unsafe { LLVMInt64TypeInContext(self.context) };
        let tag_u64 = unsafe {
            LLVMBuildZExt(
                self.builder,
                tag,
                u64_ty,
                CString::new("map.tag.u64")?.as_ptr(),
            )
        };
        let mut args = vec![fmt_ptr, tag_u64];
        unsafe {
            LLVMBuildCall2(
                self.builder,
                printf_ty,
                printf_fn,
                args.as_mut_ptr(),
                args.len() as u32,
                CString::new("map.debug.tag")?.as_ptr(),
            );
        }
        Ok(())
    }

    pub(super) fn debug_map_json_tag(&mut self, label: &str, json_val: LLVMValueRef) -> Result<()> {
        if std::env::var("GLYPH_DEBUG_MAP").is_err() {
            return Ok(());
        }

        let json_ty = self.get_enum_type("JsonValue")?;
        let tmp =
            unsafe { self.build_entry_alloca(self.builder, json_ty, CString::new("json.tmp")?.as_ptr()) };
        unsafe { LLVMBuildStore(self.builder, json_val, tmp) };
        let tag_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                json_ty,
                tmp,
                0,
                CString::new("json.tag")?.as_ptr(),
            )
        };
        let tag_val = unsafe {
            LLVMBuildLoad2(
                self.builder,
                LLVMInt32TypeInContext(self.context),
                tag_ptr,
                CString::new("json.tag.load")?.as_ptr(),
            )
        };
        self.debug_map_tag(label, tag_val)
    }
}
