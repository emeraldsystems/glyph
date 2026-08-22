use std::collections::HashMap;
use std::ffi::c_void;

#[repr(C)]
struct GlyphThread {
    _private: [u8; 0],
}

#[repr(C)]
struct GlyphMutex {
    _private: [u8; 0],
}

type GlyphThreadEntry = unsafe extern "C" fn(*mut c_void);
type GlyphThreadDropUnstarted = unsafe extern "C" fn(*mut c_void);
type GlyphThreadResultEntry = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void);
type GlyphThreadDropResult = unsafe extern "C" fn(*mut c_void);

unsafe extern "C" {
    fn glyph_thread_spawn(
        out: *mut *mut GlyphThread,
        entry: Option<GlyphThreadEntry>,
        env: *mut c_void,
        drop_unstarted: Option<GlyphThreadDropUnstarted>,
    ) -> i32;
    fn glyph_thread_spawn_result(
        out: *mut *mut GlyphThread,
        entry: Option<GlyphThreadResultEntry>,
        invoke: *mut c_void,
        env: *mut c_void,
        drop_unstarted: Option<GlyphThreadDropUnstarted>,
        result_size: usize,
        drop_result: Option<GlyphThreadDropResult>,
    ) -> i32;
    fn glyph_thread_join(handle: *mut *mut GlyphThread) -> i32;
    fn glyph_thread_join_result(handle: *mut *mut GlyphThread, out_result: *mut c_void) -> i32;
    fn glyph_thread_detach(handle: *mut *mut GlyphThread) -> i32;
    fn glyph_mutex_create(out: *mut *mut GlyphMutex) -> i32;
    fn glyph_mutex_lock(mutex: *mut GlyphMutex) -> i32;
    fn glyph_mutex_try_lock(mutex: *mut GlyphMutex) -> i32;
    fn glyph_mutex_unlock(mutex: *mut GlyphMutex) -> i32;
    fn glyph_mutex_destroy(mutex: *mut *mut GlyphMutex) -> i32;
}

/// Register the native runtime functions needed by JIT-lowered std/thread.
/// Taking each address also ensures the corresponding object is retained from
/// the static runtime archive when the CLI itself is linked.
pub(super) fn register_symbols(symbols: &mut HashMap<String, u64>) {
    symbols.insert(
        "glyph_thread_spawn".to_string(),
        glyph_thread_spawn as *const () as usize as u64,
    );
    symbols.insert(
        "glyph_thread_join".to_string(),
        glyph_thread_join as *const () as usize as u64,
    );
    symbols.insert(
        "glyph_thread_spawn_result".to_string(),
        glyph_thread_spawn_result as *const () as usize as u64,
    );
    symbols.insert(
        "glyph_thread_join_result".to_string(),
        glyph_thread_join_result as *const () as usize as u64,
    );
    symbols.insert(
        "glyph_thread_detach".to_string(),
        glyph_thread_detach as *const () as usize as u64,
    );
    symbols.insert(
        "glyph_mutex_create".to_string(),
        glyph_mutex_create as *const () as usize as u64,
    );
    symbols.insert(
        "glyph_mutex_lock".to_string(),
        glyph_mutex_lock as *const () as usize as u64,
    );
    symbols.insert(
        "glyph_mutex_try_lock".to_string(),
        glyph_mutex_try_lock as *const () as usize as u64,
    );
    symbols.insert(
        "glyph_mutex_unlock".to_string(),
        glyph_mutex_unlock as *const () as usize as u64,
    );
    symbols.insert(
        "glyph_mutex_destroy".to_string(),
        glyph_mutex_destroy as *const () as usize as u64,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use glyph_backend::codegen::CodegenContext;
    use glyph_core::mir::{
        Local, LocalId, MirBlock, MirExternFunction, MirFunction, MirInst, MirModule, MirValue,
        Rvalue,
    };
    use glyph_core::types::Type;

    #[test]
    fn registers_complete_public_thread_runtime_surface() {
        let mut symbols = HashMap::new();
        register_symbols(&mut symbols);
        assert_eq!(symbols.len(), 10);
        for name in [
            "glyph_thread_spawn",
            "glyph_thread_join",
            "glyph_thread_spawn_result",
            "glyph_thread_join_result",
            "glyph_thread_detach",
            "glyph_mutex_create",
            "glyph_mutex_lock",
            "glyph_mutex_try_lock",
            "glyph_mutex_unlock",
            "glyph_mutex_destroy",
        ] {
            assert!(symbols.get(name).is_some_and(|address| *address != 0));
        }
    }

    #[test]
    fn jit_resolves_and_calls_native_thread_runtime_symbol() {
        let handle_ptr = Type::RawPtr(Box::new(Type::RawPtr(Box::new(Type::U8))));
        let mut ctx = CodegenContext::new("thread_runtime_symbols").unwrap();
        let mir = MirModule {
            struct_types: HashMap::new(),
            enum_types: HashMap::new(),
            extern_functions: vec![MirExternFunction {
                name: "glyph_thread_join".into(),
                ret_type: Some(Type::I32),
                params: vec![handle_ptr.clone()],
                abi: Some("C".into()),
                link_name: None,
            }],
            functions: vec![MirFunction {
                name: "main".into(),
                ret_type: Some(Type::I32),
                params: vec![],
                locals: vec![
                    Local {
                        name: None,
                        ty: Some(handle_ptr),
                        mutable: false,
                        skip_drop: true,
                    },
                    Local {
                        name: None,
                        ty: Some(Type::I32),
                        mutable: false,
                        skip_drop: true,
                    },
                ],
                blocks: vec![MirBlock {
                    insts: vec![
                        MirInst::Assign {
                            local: LocalId(0),
                            value: Rvalue::RawPtrNull {
                                elem_type: Type::RawPtr(Box::new(Type::U8)),
                            },
                        },
                        MirInst::Assign {
                            local: LocalId(1),
                            value: Rvalue::Call {
                                name: "glyph_thread_join".into(),
                                args: vec![MirValue::Local(LocalId(0))],
                            },
                        },
                        MirInst::Return(Some(MirValue::Local(LocalId(1)))),
                    ],
                }],
            }],
        };
        ctx.codegen_module(&mir).unwrap();

        let mut symbols = HashMap::new();
        register_symbols(&mut symbols);
        let result = ctx.jit_execute_i32_with_symbols("main", &symbols).unwrap();
        assert_eq!(result, -libc::EINVAL);
    }
}
