//! Release optimization runs on an emission clone so LLVM cannot invalidate the
//! context's builder, cached values, or subsequent O0 emission and IR inspection.

use super::*;
use llvm_sys::error::{LLVMDisposeErrorMessage, LLVMGetErrorMessage};
use llvm_sys::transforms::pass_builder::*;

struct ObjectModule(LLVMModuleRef);

impl Drop for ObjectModule {
    fn drop(&mut self) {
        unsafe { LLVMDisposeModule(self.0) };
    }
}

struct TargetMachine(LLVMTargetMachineRef);

impl Drop for TargetMachine {
    fn drop(&mut self) {
        unsafe { LLVMDisposeTargetMachine(self.0) };
    }
}

struct PassOptions(LLVMPassBuilderOptionsRef);

impl Drop for PassOptions {
    fn drop(&mut self) {
        unsafe { LLVMDisposePassBuilderOptions(self.0) };
    }
}

fn verify_module(module: LLVMModuleRef) -> Result<()> {
    unsafe {
        let mut error = std::ptr::null_mut();
        let invalid = LLVMVerifyModule(
            module,
            LLVMVerifierFailureAction::LLVMReturnStatusAction,
            &mut error,
        );
        let message = take_message(error);
        if invalid != 0 {
            bail!("LLVM module verification failed: {}", message);
        }
        Ok(())
    }
}

/// LLVM's message allocator differs from LLVMErrorRef's allocator.
unsafe fn take_message(message: *mut std::os::raw::c_char) -> String {
    if message.is_null() {
        return "unknown error".to_owned();
    }
    let result = unsafe { CStr::from_ptr(message).to_string_lossy().into_owned() };
    unsafe { LLVMDisposeMessage(message) };
    result
}

fn run_passes(module: LLVMModuleRef, machine: LLVMTargetMachineRef, passes: &CStr) -> Result<()> {
    unsafe {
        let options = PassOptions(LLVMCreatePassBuilderOptions());
        // Diagnose a bad transformation at its source rather than handing
        // malformed IR to machine-code emission.
        LLVMPassBuilderOptionsSetVerifyEach(options.0, 1);
        let error = LLVMRunPasses(module, passes.as_ptr(), machine, options.0);
        if !error.is_null() {
            // LLVMGetErrorMessage consumes the LLVMErrorRef; its returned
            // string must use LLVMDisposeErrorMessage, not LLVMDisposeMessage.
            let message = LLVMGetErrorMessage(error);
            let description = if message.is_null() {
                "unknown error".to_owned()
            } else {
                let description = CStr::from_ptr(message).to_string_lossy().into_owned();
                LLVMDisposeErrorMessage(message);
                description
            };
            bail!("LLVM optimization failed: {}", description);
        }
        Ok(())
    }
}

impl CodegenContext {
    /// Emit an unoptimized object file, preserving the existing API's O0 default.
    pub fn emit_object_file(&self, output_path: &Path) -> Result<()> {
        self.emit_object_file_with_optimization(output_path, OptimizationLevel::None)
    }

    /// Emit O0 or LLVM's ordinary O2 without enabling fast-math, reassociation,
    /// or fused arithmetic. The original module remains available unchanged.
    pub fn emit_object_file_with_optimization(
        &self,
        output_path: &Path,
        optimization: OptimizationLevel,
    ) -> Result<()> {
        // Reject bad paths before allocating LLVM-owned emission resources.
        let output_path = CString::new(
            output_path
                .to_str()
                .ok_or_else(|| anyhow!("Invalid output path"))?,
        )?;
        let (module, machine) = self.object_module(optimization)?;
        unsafe {
            let mut error = std::ptr::null_mut();
            if LLVMTargetMachineEmitToFile(
                machine.0,
                module.0,
                output_path.as_ptr(),
                LLVMCodeGenFileType::LLVMObjectFile,
                &mut error,
            ) != 0
            {
                bail!("Failed to emit object file: {}", take_message(error));
            }
        }
        Ok(())
    }

    fn object_module(
        &self,
        optimization: OptimizationLevel,
    ) -> Result<(ObjectModule, TargetMachine)> {
        verify_module(self.module)?;
        super::context::init_targets();
        let triple = self.effective_target_triple()?;
        let module = ObjectModule(unsafe { LLVMCloneModule(self.module) });
        unsafe {
            LLVMSetTarget(module.0, triple.as_ptr());
            let mut target = std::ptr::null_mut();
            let mut error = std::ptr::null_mut();
            if LLVMGetTargetFromTriple(triple.as_ptr(), &mut target, &mut error) != 0 {
                bail!("Failed to get target: {}", take_message(error));
            }

            // Preserve the existing portable CPU/feature policy in both modes.
            let level = match optimization {
                OptimizationLevel::None => LLVMCodeGenOptLevel::LLVMCodeGenLevelNone,
                OptimizationLevel::Default => LLVMCodeGenOptLevel::LLVMCodeGenLevelDefault,
            };
            let machine = LLVMCreateTargetMachine(
                target,
                triple.as_ptr(),
                c"generic".as_ptr(),
                c"".as_ptr(),
                level,
                LLVMRelocMode::LLVMRelocPIC,
                LLVMCodeModel::LLVMCodeModelDefault,
            );
            if machine.is_null() {
                bail!("Failed to create target machine");
            }
            let machine = TargetMachine(machine);
            let layout = LLVMCreateTargetDataLayout(machine.0);
            LLVMSetModuleDataLayout(module.0, layout);
            LLVMDisposeTargetData(layout);

            if optimization == OptimizationLevel::Default {
                // The pipeline honors the ordinary FMul/FAdd instructions
                // already emitted: no fast-math flags or unsafe FP attributes.
                run_passes(module.0, machine.0, c"default<O2>")?;
                verify_module(module.0)?;
            }
            Ok((module, machine))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glyph_core::mir::Local;

    fn arithmetic_context() -> CodegenContext {
        let mut context = CodegenContext::new("optimization").unwrap();
        let mir = MirModule {
            struct_types: HashMap::new(),
            enum_types: HashMap::new(),
            extern_functions: vec![],
            functions: vec![MirFunction {
                name: "main".into(),
                ret_type: Some(Type::I32),
                params: vec![],
                locals: vec![Local {
                    name: Some("answer".into()),
                    ty: Some(Type::I32),
                    mutable: false,
                    skip_drop: false,
                }],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(0),
                            value: Rvalue::ConstInt(42),
                        },
                        MirInst::Return(Some(MirValue::Local(LocalId(0)))),
                    ],
                }],
            }],
        };
        context.codegen_module(&mir).unwrap();
        context
    }

    fn ir(module: LLVMModuleRef) -> String {
        unsafe { take_message(LLVMPrintModuleToString(module)) }
    }

    #[test]
    fn o2_promotes_locals_without_changing_source_module() {
        let context = arithmetic_context();
        let before = context.dump_ir();
        assert!(before.contains("alloca i32"), "{before}");
        let (optimized, _machine) = context.object_module(OptimizationLevel::Default).unwrap();
        let optimized_ir = ir(optimized.0);
        assert!(!optimized_ir.contains("alloca i32"), "{optimized_ir}");
        assert!(optimized_ir.contains("ret i32 42"), "{optimized_ir}");
        assert!(optimized_ir.contains("target triple"));
        assert!(optimized_ir.contains("target datalayout"));
        assert_eq!(context.dump_ir(), before);

        let (debug, _machine) = context.object_module(OptimizationLevel::None).unwrap();
        assert_eq!(ir(debug.0), before);
    }

    #[test]
    fn invalid_pipeline_is_a_reported_error_and_keeps_source_usable() {
        let context = arithmetic_context();
        let (module, machine) = context.object_module(OptimizationLevel::None).unwrap();
        let error = run_passes(module.0, machine.0, c"not-a-glyph-pass").unwrap_err();
        assert!(error.to_string().contains("unknown pass"), "{error}");
        assert_eq!(context.jit_execute_i32("main").unwrap(), 42);
    }
}
