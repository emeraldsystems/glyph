use super::*;
use std::sync::{Mutex, Once};

fn jit_lock() -> &'static Mutex<()> {
    static JIT_LOCK: Mutex<()> = Mutex::new(());
    &JIT_LOCK
}

fn init_native_jit() {
    static INIT: Once = Once::new();
    INIT.call_once(|| unsafe {
        llvm_sys::target::LLVM_InitializeNativeTarget();
        llvm_sys::target::LLVM_InitializeNativeAsmPrinter();
        llvm_sys::execution_engine::LLVMLinkInMCJIT();
    });
}

impl CodegenContext {
    /// Execute a function via JIT for testing purposes
    /// Note: This creates a clone of the module for JIT execution
    pub fn jit_execute_i32(&self, fn_name: &str) -> Result<i32> {
        self.jit_execute_i32_with_symbols(fn_name, &HashMap::new())
    }

    /// Execute a function via JIT with custom symbol resolution
    /// The symbols map provides address resolution for extern functions
    /// Note: This creates a clone of the module for JIT execution
    pub fn jit_execute_i32_with_symbols(
        &self,
        fn_name: &str,
        symbols: &HashMap<String, u64>,
    ) -> Result<i32> {
        use llvm_sys::execution_engine::*;
        use llvm_sys::target::*;

        unsafe {
            let _guard = jit_lock().lock().unwrap_or_else(|e| e.into_inner());
            init_native_jit();

            // Clone the module so the execution engine can take ownership
            let module_clone = LLVMCloneModule(self.module);

            // Verify the cloned module before JIT execution.
            let mut verify_err = std::ptr::null_mut();
            if LLVMVerifyModule(
                module_clone,
                LLVMVerifierFailureAction::LLVMReturnStatusAction,
                &mut verify_err,
            ) != 0
            {
                let msg = if verify_err.is_null() {
                    "unknown verification error".to_string()
                } else {
                    let s = CStr::from_ptr(verify_err).to_string_lossy().into_owned();
                    LLVMDisposeMessage(verify_err);
                    s
                };
                return Err(anyhow!("LLVM module verification failed: {}", msg));
            }

            // Set explicit target triple to avoid ambiguities (e.g., aarch64 variants)
            let target_triple = LLVMGetDefaultTargetTriple();
            LLVMSetTarget(module_clone, target_triple);

            let mut target = std::ptr::null_mut();
            let mut error = std::ptr::null_mut();
            if LLVMGetTargetFromTriple(target_triple, &mut target, &mut error) != 0 {
                let err_msg = if error.is_null() {
                    "unknown error".to_string()
                } else {
                    let msg = CStr::from_ptr(error).to_string_lossy().into_owned();
                    LLVMDisposeMessage(error);
                    msg
                };
                LLVMDisposeMessage(target_triple);
                return Err(anyhow!("Failed to get target: {}", err_msg));
            }

            let cpu = CString::new("generic")?;
            let features = CString::new("")?;
            let target_machine = LLVMCreateTargetMachine(
                target,
                target_triple,
                cpu.as_ptr(),
                features.as_ptr(),
                LLVMCodeGenOptLevel::LLVMCodeGenLevelNone,
                LLVMRelocMode::LLVMRelocPIC,
                LLVMCodeModel::LLVMCodeModelDefault,
            );

            if target_machine.is_null() {
                LLVMDisposeMessage(target_triple);
                return Err(anyhow!("Failed to create target machine"));
            }

            let data_layout = LLVMCreateTargetDataLayout(target_machine);
            LLVMSetModuleDataLayout(module_clone, data_layout);
            LLVMDisposeTargetData(data_layout);
            LLVMDisposeTargetMachine(target_machine);
            LLVMDisposeMessage(target_triple);

            // Create MCJIT execution engine
            let mut ee = std::ptr::null_mut();
            let mut error = std::ptr::null_mut();

            if LLVMCreateMCJITCompilerForModule(
                &mut ee,
                module_clone,
                std::ptr::null_mut(),
                0,
                &mut error,
            ) != 0
            {
                let err_msg = if error.is_null() {
                    "unknown error".to_string()
                } else {
                    let msg = CStr::from_ptr(error).to_string_lossy().into_owned();
                    LLVMDisposeMessage(error);
                    msg
                };
                return Err(anyhow!("Failed to create execution engine: {}", err_msg));
            }

            // Register custom symbols with the execution engine
            for (sym_name, &addr) in symbols {
                let sym_name_c = CString::new(sym_name.as_str())?;
                let func = LLVMGetNamedFunction(module_clone, sym_name_c.as_ptr());
                if !func.is_null() {
                    LLVMAddGlobalMapping(ee, func, addr as *mut std::ffi::c_void);
                }
            }

            // Find the function
            let fn_name_c = CString::new(fn_name)?;
            let func_addr = LLVMGetFunctionAddress(ee, fn_name_c.as_ptr());

            if func_addr == 0 {
                LLVMDisposeExecutionEngine(ee);
                return Err(anyhow!("Function {} not found", fn_name));
            }

            // Cast to function pointer and call it
            let func: extern "C" fn() -> i32 = std::mem::transmute(func_addr);
            let result = func();

            // Clean up execution engine (which also disposes the cloned module)
            LLVMDisposeExecutionEngine(ee);

            Ok(result)
        }
    }

    /// Emit an object file (.o) to disk using LLVM's target machine API
    pub fn emit_object_file(&self, output_path: &Path) -> Result<()> {
        unsafe {
            // Verify module before attempting emission.
            let mut verify_err = std::ptr::null_mut();
            if LLVMVerifyModule(
                self.module,
                LLVMVerifierFailureAction::LLVMReturnStatusAction,
                &mut verify_err,
            ) != 0
            {
                let msg = if verify_err.is_null() {
                    "unknown verification error".to_string()
                } else {
                    let s = CStr::from_ptr(verify_err).to_string_lossy().into_owned();
                    LLVMDisposeMessage(verify_err);
                    s
                };
                return Err(anyhow!("LLVM module verification failed: {}", msg));
            }

            // Initialize all LLVM targets (x86, ARM, etc.)
            LLVM_InitializeAllTargetInfos();
            LLVM_InitializeAllTargets();
            LLVM_InitializeAllTargetMCs();
            LLVM_InitializeAllAsmPrinters();

            let target_triple = self.effective_target_triple()?;
            LLVMSetTarget(self.module, target_triple.as_ptr());

            // Get target from triple
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

            // Create target machine with native CPU features
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
            LLVMDisposeTargetData(data_layout);

            // Convert output path to C string
            let output_path_str = output_path
                .to_str()
                .ok_or_else(|| anyhow!("Invalid output path"))?;
            let output_path_c = CString::new(output_path_str)?;

            // Emit object file to disk
            let mut error = std::ptr::null_mut();
            if LLVMTargetMachineEmitToFile(
                target_machine,
                self.module,
                output_path_c.as_ptr() as *mut i8,
                LLVMCodeGenFileType::LLVMObjectFile,
                &mut error,
            ) != 0
            {
                let err_msg = if error.is_null() {
                    "unknown error".to_string()
                } else {
                    let msg = CStr::from_ptr(error).to_string_lossy().into_owned();
                    LLVMDisposeMessage(error);
                    msg
                };
                LLVMDisposeTargetMachine(target_machine);
                return Err(anyhow!("Failed to emit object file: {}", err_msg));
            }

            // Clean up
            LLVMDisposeTargetMachine(target_machine);

            Ok(())
        }
    }
}
