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

/// GLYPH-83: makes the *current process* (the main executable plus every
/// dylib already loaded into it) searchable by `LLVMSearchForAddressOfSymbol`.
/// Passing a null filename per the LLVM docs means "the calling program
/// itself" rather than an on-disk library. This is what lets the JIT
/// resolve runtime/*.c functions (glyph_fmt_write_str, glyph_json_*,
/// glyph_net_*, glyph_audio_*, ...) that `glyph-cli`'s build.rs
/// force-loads into the binary but that no Rust code references directly —
/// without it, those symbols exist in the binary but MCJIT's default
/// resolver (and our own search below) would still come up empty.
fn ensure_process_symbols_loaded() {
    static INIT: Once = Once::new();
    INIT.call_once(|| unsafe {
        llvm_sys::support::LLVMLoadLibraryPermanently(std::ptr::null());
    });
}

/// Resolve one extern function name to an address: the caller's explicit
/// override map first (this is how tests and thread_runtime.rs pin down a
/// specific native symbol), then a process-wide symbol search covering
/// everything actually linked into the running binary (the force-loaded
/// runtime archive, libc, libLLVM, ...). Returns `None` if neither source
/// has it, which the caller turns into a named error instead of letting
/// MCJIT patch the call site with a null address.
fn resolve_extern_symbol(name: &str, symbols: &HashMap<String, u64>) -> Option<u64> {
    if let Some(&addr) = symbols.get(name) {
        if addr != 0 {
            return Some(addr);
        }
    }
    ensure_process_symbols_loaded();
    let name_c = CString::new(name).ok()?;
    let addr = unsafe { llvm_sys::support::LLVMSearchForAddressOfSymbol(name_c.as_ptr()) };
    if addr.is_null() {
        None
    } else {
        Some(addr as usize as u64)
    }
}

impl CodegenContext {
    /// Resolve one extern function name the same way
    /// `jit_execute_i32_with_symbols`'s preflight check does: `symbols`
    /// first, then a process-wide symbol search. Exposed publicly
    /// (GLYPH-83) so callers — tests in particular — can check ahead of
    /// time whether a given runtime symbol is actually linked into the
    /// current process, without duplicating the resolution logic or
    /// standing up a full MIR module just to ask.
    pub fn jit_resolve_symbol(name: &str, symbols: &HashMap<String, u64>) -> Option<u64> {
        resolve_extern_symbol(name, symbols)
    }

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

            // GLYPH-83: resolve every extern the module actually declares
            // *before* creating the execution engine or touching the entry
            // point. An extern that resolves to nothing is not a
            // hypothetical: it previously meant MCJIT quietly patched the
            // call site with a null address and the whole process
            // segfaulted (SIGSEGV, no Rust panic, no diagnostic) the
            // instant that code ran. Failing here instead turns that into
            // a normal, named `Result::Err`.
            let mut resolved_externs: Vec<(String, u64)> = Vec::new();
            let mut unresolved_externs: Vec<String> = Vec::new();
            {
                let mut f = LLVMGetFirstFunction(module_clone);
                while !f.is_null() {
                    let next = LLVMGetNextFunction(f);
                    // A declaration with no intrinsic ID is a genuine
                    // external dependency (runtime C function, libc, ...).
                    // Functions this module defines itself always get a
                    // body during codegen, so they never show up here;
                    // real LLVM intrinsics (llvm.memcpy.*, ...) are
                    // resolved by LLVM's own lowering, not by name lookup.
                    //
                    // Some stdlib externs (e.g. "println", "argv") exist
                    // only so import/type resolution has a signature to
                    // check against — every real call site is intercepted
                    // during MIR lowering and rewritten into direct calls
                    // to the actual runtime primitives (see
                    // mir_lower::call::lower_print_builtin and
                    // codegen_sys_argv_value), so the declaration is never
                    // referenced by any instruction in the module. Skip
                    // anything with zero uses: it cannot be reached at
                    // runtime, so an unresolved address for it is harmless
                    // and flagging it would be a false positive.
                    if LLVMIsDeclaration(f) != 0
                        && LLVMGetIntrinsicID(f) == 0
                        && !LLVMGetFirstUse(f).is_null()
                    {
                        let name_ptr = LLVMGetValueName(f);
                        if !name_ptr.is_null() {
                            let name = CStr::from_ptr(name_ptr).to_string_lossy().into_owned();
                            if !name.is_empty() {
                                match resolve_extern_symbol(&name, symbols) {
                                    Some(addr) => resolved_externs.push((name, addr)),
                                    None => unresolved_externs.push(name),
                                }
                            }
                        }
                    }
                    f = next;
                }
            }
            if !unresolved_externs.is_empty() {
                unresolved_externs.sort();
                unresolved_externs.dedup();
                // The engine never took ownership of module_clone, so it's
                // ours to free before bailing.
                LLVMDisposeModule(module_clone);
                return Err(anyhow!(
                    "JIT execution aborted: {} unresolved external symbol(s): {}",
                    unresolved_externs.len(),
                    unresolved_externs.join(", ")
                ));
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

            // Map every declared extern to the address `resolve_extern_symbol`
            // found for it during preflight above (the caller's explicit
            // `symbols` override, or a process-wide symbol search) so
            // execution uses exactly what was validated — not whatever
            // MCJIT's own default resolver might independently come up
            // with.
            for (sym_name, addr) in &resolved_externs {
                let sym_name_c = CString::new(sym_name.as_str())?;
                let func = LLVMGetNamedFunction(module_clone, sym_name_c.as_ptr());
                if !func.is_null() {
                    LLVMAddGlobalMapping(ee, func, *addr as *mut std::ffi::c_void);
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
                output_path_c.as_ptr(),
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
