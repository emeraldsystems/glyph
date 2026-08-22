use super::*;
use std::sync::Once;

static LLVM_TARGET_INIT: Once = Once::new();

impl CodegenContext {
    pub fn new(module_name: &str) -> Result<Self> {
        unsafe {
            let context = LLVMContextCreate();
            let module_name_c = CString::new(module_name)?;
            let module = LLVMModuleCreateWithNameInContext(module_name_c.as_ptr(), context);
            let builder = LLVMCreateBuilderInContext(context);

            Ok(Self {
                context,
                module,
                builder,
                struct_types: HashMap::new(),
                struct_layouts: HashMap::new(),
                enum_types: HashMap::new(),
                enum_layouts: HashMap::new(),
                malloc_fn: None,
                free_fn: None,
                strdup_fn: None,
                string_globals: HashMap::new(),
                function_types: HashMap::new(),
                function_ref_thunks: HashMap::new(),
                closure_artifacts: HashMap::new(),
                borrowed_closure_artifacts: HashMap::new(),
                sret_functions: HashMap::new(),
                target_data: None,
                requested_target_triple: None,
                argv_global: None,
                argc_global: None,
                argv_vec_global: None,
                drop_in_progress: std::collections::HashSet::new(),
                clone_fns: HashMap::new(),
            })
        }
    }

    /// Create a module for an explicit LLVM target triple.
    ///
    /// Target selection happens before any LLVM types are materialized so
    /// target-dependent guarantees (notably lock-free atomic widths and
    /// alignment) are validated against the requested target rather than the
    /// build host.
    pub fn new_for_target(module_name: &str, target_triple: &str) -> Result<Self> {
        if target_triple.is_empty() {
            bail!("target triple must not be empty");
        }
        CString::new(target_triple)?;
        let mut context = Self::new(module_name)?;
        context.requested_target_triple = Some(target_triple.to_owned());
        Ok(context)
    }

    pub(super) fn effective_target_triple(&self) -> Result<CString> {
        if let Some(target_triple) = self.requested_target_triple.as_ref() {
            return Ok(CString::new(target_triple.as_str())?);
        }
        unsafe {
            let default_triple = LLVMGetDefaultTargetTriple();
            if default_triple.is_null() {
                bail!("LLVM did not provide a default target triple");
            }
            let target_triple = CStr::from_ptr(default_triple)
                .to_string_lossy()
                .into_owned();
            LLVMDisposeMessage(default_triple);
            Ok(CString::new(target_triple)?)
        }
    }

    pub(super) fn init_target_data(&mut self) -> Result<()> {
        unsafe {
            if self.target_data.is_some() {
                return Ok(());
            }

            LLVM_TARGET_INIT.call_once(|| {
                LLVM_InitializeAllTargetInfos();
                LLVM_InitializeAllTargets();
                LLVM_InitializeAllTargetMCs();
                LLVM_InitializeAllAsmPrinters();
            });

            let target_triple = self.effective_target_triple()?;
            LLVMSetTarget(self.module, target_triple.as_ptr());

            let mut target = std::ptr::null_mut();
            let mut error = std::ptr::null_mut();
            if LLVMGetTargetFromTriple(target_triple.as_ptr(), &mut target, &mut error) != 0 {
                let err_msg = if error.is_null() {
                    "unknown error".to_string()
                } else {
                    let msg = CStr::from_ptr(error).to_string_lossy().into_owned();
                    LLVMDisposeMessage(error);
                    msg
                };
                return Err(anyhow!("Failed to get target: {}", err_msg));
            }

            let cpu = CString::new("generic")?;
            let features = CString::new("")?;
            let target_machine = LLVMCreateTargetMachine(
                target,
                target_triple.as_ptr(),
                cpu.as_ptr(),
                features.as_ptr(),
                LLVMCodeGenOptLevel::LLVMCodeGenLevelNone,
                LLVMRelocMode::LLVMRelocPIC,
                LLVMCodeModel::LLVMCodeModelDefault,
            );

            if target_machine.is_null() {
                return Err(anyhow!("Failed to create target machine"));
            }

            let data_layout = LLVMCreateTargetDataLayout(target_machine);
            LLVMSetModuleDataLayout(self.module, data_layout);
            self.target_data = Some(data_layout);

            LLVMDisposeTargetMachine(target_machine);
        }

        Ok(())
    }

    pub(super) fn add_sret_attribute(&self, func: LLVMValueRef, ret_ty: LLVMTypeRef) {
        unsafe {
            let kind = LLVMGetEnumAttributeKindForName(b"sret".as_ptr().cast(), 4);
            if kind == 0 {
                return;
            }
            let attr = LLVMCreateTypeAttribute(self.context, kind, ret_ty);
            LLVMAddAttributeAtIndex(func, 1, attr);
        }
    }

    pub(super) fn add_sret_call_attribute(&self, call: LLVMValueRef, ret_ty: LLVMTypeRef) {
        unsafe {
            let kind = LLVMGetEnumAttributeKindForName(b"sret".as_ptr().cast(), 4);
            if kind == 0 {
                return;
            }
            let attr = LLVMCreateTypeAttribute(self.context, kind, ret_ty);
            LLVMAddCallSiteAttribute(call, 1, attr);
        }
    }

    pub fn get_module(&self) -> *mut LLVMModule {
        self.module
    }

    pub fn dump_ir(&self) -> String {
        unsafe {
            let ir_ptr = LLVMPrintModuleToString(self.module);
            let ir_cstr = CStr::from_ptr(ir_ptr);
            let ir_string = ir_cstr.to_string_lossy().into_owned();
            LLVMDisposeMessage(ir_ptr);
            ir_string
        }
    }

    pub(super) fn build_call2(
        &mut self,
        fn_ty: LLVMTypeRef,
        callee: LLVMValueRef,
        args: &mut [LLVMValueRef],
        name: &str,
    ) -> Result<LLVMValueRef> {
        unsafe {
            let ret_ty = LLVMGetReturnType(fn_ty);
            let name = if LLVMGetTypeKind(ret_ty) == llvm_sys::LLVMTypeKind::LLVMVoidTypeKind {
                ""
            } else {
                name
            };
            let name_c = CString::new(name)?;
            let args_ptr = if args.is_empty() {
                std::ptr::null_mut()
            } else {
                args.as_mut_ptr()
            };
            Ok(LLVMBuildCall2(
                self.builder,
                fn_ty,
                callee,
                args_ptr,
                args.len() as u32,
                name_c.as_ptr(),
            ))
        }
    }

    pub(super) fn debug_log(&self, msg: &str) {
        if std::env::var("GLYPH_DEBUG_CODEGEN").is_ok() {
            eprintln!("[codegen] {}", msg);
        }
    }
}
