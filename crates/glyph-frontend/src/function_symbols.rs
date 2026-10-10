//! Preserve source-level module identity when functions share a spelling.

use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use glyph_core::ast::{Item, Module};

pub(crate) fn collect_function_symbols(
    modules: &HashMap<String, Module>,
    compile_order: &[String],
    entry_module: &str,
) -> HashMap<(String, String), String> {
    let mut owners: HashMap<&str, HashSet<&str>> = HashMap::new();
    for module_id in compile_order {
        for item in &modules[module_id].items {
            let name = match item {
                Item::Function(function) => &function.name.0,
                Item::ExternFunction(function) => &function.name.0,
                _ => continue,
            };
            owners.entry(name).or_default().insert(module_id);
        }
    }

    let mut symbols = HashMap::new();
    for module_id in compile_order {
        for item in &modules[module_id].items {
            let Item::Function(function) = item else {
                continue;
            };
            let name = &function.name.0;
            // Only the entry module may define the ABI entry point. Otherwise
            // keep existing exported names when they are unique in the graph.
            if (name == "main" && module_id != entry_module)
                || (owners[name.as_str()].len() > 1
                    && !(name == "main" && module_id == entry_module))
            {
                // `$` is unavailable in source identifiers. Hex encoding is
                // injective even for nested paths and desugared method names,
                // and survives string-global/closure name generation intact.
                let encode = |value: &str| {
                    let mut encoded = String::with_capacity(value.len() * 2);
                    for byte in value.bytes() {
                        write!(&mut encoded, "{byte:02x}").expect("write to string");
                    }
                    encoded
                };
                symbols.insert(
                    (module_id.clone(), name.clone()),
                    format!("__glyph_module${}${}", encode(module_id), encode(name)),
                );
            }
        }
    }
    symbols
}
