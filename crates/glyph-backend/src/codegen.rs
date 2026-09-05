use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use glyph_core::mir::{
    BlockId, LocalId, MirBlock, MirExternFunction, MirFunction, MirInst, MirModule, MirValue,
    Rvalue,
};
use glyph_core::types::{BorrowedCallableKind, EnumType, Mutability, StructType, Type};
use llvm_sys::analysis::*;
use llvm_sys::core::*;
use llvm_sys::prelude::*;
use llvm_sys::target::*;
use llvm_sys::target_machine::*;
use llvm_sys::{LLVMBuilder, LLVMContext, LLVMLinkage, LLVMModule};

pub struct CodegenContext {
    context: *mut LLVMContext,
    module: *mut LLVMModule,
    builder: *mut LLVMBuilder,
    struct_types: HashMap<String, LLVMTypeRef>,
    struct_layouts: HashMap<String, StructType>,
    enum_types: HashMap<String, LLVMTypeRef>,
    enum_layouts: HashMap<String, EnumType>,
    malloc_fn: Option<LLVMValueRef>,
    free_fn: Option<LLVMValueRef>,
    strdup_fn: Option<LLVMValueRef>,
    string_globals: HashMap<String, LLVMValueRef>,
    function_types: HashMap<String, LLVMTypeRef>,
    function_ref_thunks: HashMap<String, LLVMValueRef>,
    closure_artifacts: HashMap<String, ClosureArtifacts>,
    borrowed_closure_artifacts: HashMap<String, BorrowedClosureArtifacts>,
    sret_functions: HashMap<String, Type>,
    target_data: Option<LLVMTargetDataRef>,
    requested_target_triple: Option<String>,
    argv_global: Option<LLVMValueRef>,
    argc_global: Option<LLVMValueRef>,
    argv_vec_global: Option<LLVMValueRef>,
    drop_in_progress: std::collections::HashSet<String>,
    clone_fns: HashMap<String, LLVMValueRef>,
}

#[derive(Clone, Copy)]
struct ClosureArtifacts {
    env_type: LLVMTypeRef,
    invoke: LLVMValueRef,
    drop: LLVMValueRef,
}

#[derive(Clone, Copy)]
struct BorrowedClosureArtifacts {
    env_type: LLVMTypeRef,
    invoke: LLVMValueRef,
}

mod aggregate;
mod arc;
mod array;
mod atomic;
mod callable;
mod clone;
mod closure;
mod context;
mod emit;
mod entry;
mod externs;
mod file;
mod functions;
mod map;
mod map_debug;
mod map_hash;
mod map_layout;
mod mutex;
mod ownership;
mod rvalue;
mod spsc;
mod string;
mod string_ops;
mod thread;
mod types;
mod vec;

#[cfg(test)]
mod tests;

impl Drop for CodegenContext {
    fn drop(&mut self) {
        unsafe {
            if let Some(target_data) = self.target_data {
                LLVMDisposeTargetData(target_data);
            }
            LLVMDisposeBuilder(self.builder);
            LLVMDisposeModule(self.module);
            LLVMContextDispose(self.context);
        }
    }
}

fn tuple_struct_name_codegen(elem_types: &[Type]) -> String {
    if elem_types.is_empty() {
        return "unit".to_string();
    }

    let type_names: Vec<String> = elem_types.iter().map(type_key_simple_codegen).collect();
    format!("__Tuple{}_{}", elem_types.len(), type_names.join("_"))
}

fn type_key_simple_codegen(ty: &Type) -> String {
    match ty {
        Type::I8 => "i8".into(),
        Type::I16 => "i16".into(),
        Type::I32 => "i32".into(),
        Type::I64 => "i64".into(),
        Type::U8 => "u8".into(),
        Type::U16 => "u16".into(),
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
        Type::Named(n) => n.replace("::", "_"),
        Type::Enum(n) => format!("enum_{}", n.replace("::", "_")),
        Type::Param(p) => format!("P_{}", p),
        Type::Ref(inner, _) => format!("ref_{}", type_key_simple_codegen(inner)),
        Type::Array(inner, size) => format!("arr{}_{}", size, type_key_simple_codegen(inner)),
        Type::Own(inner) => format!("own_{}", type_key_simple_codegen(inner)),
        Type::RawPtr(inner) => format!("rawptr_{}", type_key_simple_codegen(inner)),
        Type::Shared(inner) => format!("shared_{}", type_key_simple_codegen(inner)),
        Type::Atomic(scalar) => format!("atomic_{}", scalar.type_name()),
        Type::Function { params, ret } => {
            let params: Vec<String> = params.iter().map(type_key_simple_codegen).collect();
            format!(
                "fn_{}_to_{}",
                if params.is_empty() {
                    "unit".to_string()
                } else {
                    params.join("__")
                },
                type_key_simple_codegen(ret)
            )
        }
        Type::BorrowedFunction { kind, params, ret } => {
            let capability = match kind {
                BorrowedCallableKind::Fn => "fn_ref",
                BorrowedCallableKind::FnMut => "fn_mut",
            };
            let params: Vec<String> = params.iter().map(type_key_simple_codegen).collect();
            format!(
                "{}_{}_to_{}",
                capability,
                if params.is_empty() {
                    "unit".to_string()
                } else {
                    params.join("__")
                },
                type_key_simple_codegen(ret)
            )
        }
        Type::App { base, args } => {
            let args: Vec<String> = args.iter().map(type_key_simple_codegen).collect();
            format!("app_{}_{}", base.replace("::", "_"), args.join("__"))
        }
        Type::Tuple(elem_types) => {
            if elem_types.is_empty() {
                "unit".into()
            } else {
                let type_names: Vec<String> =
                    elem_types.iter().map(type_key_simple_codegen).collect();
                format!("__Tuple{}_{}", elem_types.len(), type_names.join("_"))
            }
        }
    }
}
