use glyph_core::ast::{ImportKind, Item, Module};
use glyph_core::diag::Diagnostic;
use glyph_core::types::Type;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use crate::stdlib::std_modules;

/// Dependency graph for tracking module dependencies
#[derive(Debug, Clone)]
pub struct DependencyGraph {
    /// Edges: module_id -> set of modules it depends on
    edges: HashMap<String, HashSet<String>>,
}

impl DependencyGraph {
    pub fn new() -> Self {
        Self {
            edges: HashMap::new(),
        }
    }

    /// Add a dependency edge: `from` depends on `to`
    pub fn add_dependency(&mut self, from: String, to: String) {
        self.edges
            .entry(from)
            .or_insert_with(HashSet::new)
            .insert(to);
    }

    /// Detect cycles using DFS with a recursion stack
    /// Returns the cycle path if found, None otherwise
    pub fn find_cycle(&self) -> Option<Vec<String>> {
        let mut visited = HashSet::new();
        let mut rec_stack = HashSet::new();

        // Get all nodes (including those with no outgoing edges)
        let all_nodes: HashSet<String> = self
            .edges
            .keys()
            .cloned()
            .chain(self.edges.values().flat_map(|deps| deps.iter().cloned()))
            .collect();

        for node in &all_nodes {
            if !visited.contains(node) {
                let mut path = Vec::new();
                if let Some(cycle) = self.dfs_cycle(node, &mut visited, &mut rec_stack, &mut path) {
                    return Some(cycle);
                }
            }
        }

        None
    }

    fn dfs_cycle(
        &self,
        node: &str,
        visited: &mut HashSet<String>,
        rec_stack: &mut HashSet<String>,
        path: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        visited.insert(node.to_string());
        rec_stack.insert(node.to_string());
        path.push(node.to_string());

        if let Some(neighbors) = self.edges.get(node) {
            for neighbor in neighbors {
                if !visited.contains(neighbor) {
                    if let Some(cycle) = self.dfs_cycle(neighbor, visited, rec_stack, path) {
                        return Some(cycle);
                    }
                } else if rec_stack.contains(neighbor) {
                    // Found a cycle - extract the cycle path
                    if let Some(cycle_start) = path.iter().position(|n| n == neighbor) {
                        let mut cycle = path[cycle_start..].to_vec();
                        cycle.push(neighbor.to_string());
                        return Some(cycle);
                    }
                }
            }
        }

        rec_stack.remove(node);
        path.pop();
        None
    }

    /// Perform topological sort using Kahn's algorithm
    /// Returns compilation order (dependencies first) or error with cycle
    pub fn topological_sort(&self) -> Result<Vec<String>, Vec<String>> {
        // Calculate in-degrees
        let all_nodes: HashSet<String> = self
            .edges
            .keys()
            .cloned()
            .chain(self.edges.values().flat_map(|deps| deps.iter().cloned()))
            .collect();

        let mut in_degree: HashMap<String, usize> =
            all_nodes.iter().map(|n| (n.clone(), 0)).collect();

        for deps in self.edges.values() {
            for dep in deps {
                *in_degree.entry(dep.clone()).or_insert(0) += 1;
            }
        }

        // Queue of nodes with in-degree 0
        let mut queue: VecDeque<String> = in_degree
            .iter()
            .filter(|&(_, &deg)| deg == 0)
            .map(|(node, _)| node.clone())
            .collect();

        let mut result = Vec::new();

        while let Some(node) = queue.pop_front() {
            result.push(node.clone());

            if let Some(neighbors) = self.edges.get(&node) {
                for neighbor in neighbors {
                    if let Some(deg) = in_degree.get_mut(neighbor) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push_back(neighbor.clone());
                        }
                    }
                }
            }
        }

        // If not all nodes are in result, there's a cycle
        if result.len() != all_nodes.len() {
            // Find the cycle
            if let Some(cycle) = self.find_cycle() {
                return Err(cycle);
            }
            return Err(vec!["unknown cycle".to_string()]);
        }

        // Reverse the result to get dependencies-first order
        // (Our edges are "from depends on to", so we processed dependents first)
        result.reverse();
        Ok(result)
    }
}

/// Build dependency graph from modules
/// Returns the graph or diagnostics if there are errors
pub fn build_dependency_graph(
    modules: &HashMap<String, Module>,
) -> Result<DependencyGraph, Vec<Diagnostic>> {
    build_dependency_graph_with_aliases(modules, &HashMap::new())
}

fn build_dependency_graph_with_aliases(
    modules: &HashMap<String, Module>,
    aliases: &HashMap<String, String>,
) -> Result<DependencyGraph, Vec<Diagnostic>> {
    let mut graph = DependencyGraph::new();
    let mut diagnostics = Vec::new();

    for (module_id, module) in modules {
        if aliases.contains_key(module_id) {
            continue;
        }
        for import in &module.imports {
            // Convert import path to module ID
            let dep_module_id = import.path.segments.join("/");

            // Validate dependency exists
            if !modules.contains_key(&dep_module_id) {
                diagnostics.push(
                    Diagnostic::error(
                        format!("unknown import: module '{}' not found", dep_module_id),
                        Some(import.span),
                    )
                    .with_module_id(module_id.clone()),
                );
                continue;
            }

            let dependency = aliases.get(&dep_module_id).unwrap_or(&dep_module_id);
            graph.add_dependency(module_id.clone(), dependency.clone());
        }

        // Ensure module is represented even if it has no imports
        graph
            .edges
            .entry(module_id.clone())
            .or_insert_with(HashSet::new);
    }

    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }

    Ok(graph)
}

/// Import scope tracks which symbols are accessible in a module
#[derive(Debug, Clone)]
pub struct ImportScope {
    /// Selective imports: local_name -> (source_module, original_name)
    /// Example: "import Point as P from geometry" -> "P" -> ("geometry", "Point")
    pub direct_symbols: HashMap<String, (String, String)>,

    /// Wildcard imports: module names that can be used for qualified access
    /// Example: "import math" -> "math" in this set
    pub wildcard_modules: HashSet<String>,
}

/// Multi-module context containing all parsed modules and their scopes
#[derive(Debug, Clone)]
pub struct MultiModuleContext {
    /// All parsed modules: module_id -> Module AST
    pub modules: HashMap<String, Module>,

    /// Import scopes: module_id -> ImportScope
    pub import_scopes: HashMap<String, ImportScope>,

    /// Module symbols: module_id -> ModuleSymbols
    pub module_symbols: HashMap<String, ModuleSymbols>,

    /// Alternate module IDs referring to the same library source.
    pub module_aliases: HashMap<String, String>,

    /// Collision-safe MIR names, keyed by canonical module ID and source name.
    pub function_symbols: HashMap<(String, String), String>,
}

impl MultiModuleContext {
    pub(crate) fn function_symbol(&self, module_id: &str, name: &str) -> String {
        let canonical = self
            .module_aliases
            .get(module_id)
            .map_or(module_id, String::as_str);
        self.function_symbols
            .get(&(canonical.to_string(), name.to_string()))
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }
}

/// Symbols exported by a module
#[derive(Debug, Clone)]
pub struct ModuleSymbols {
    pub structs: HashSet<String>,
    pub enums: HashSet<String>,
    pub interfaces: HashSet<String>,
    pub functions: HashSet<String>,
    pub consts: HashSet<String>,
}

impl ModuleSymbols {
    pub fn new() -> Self {
        Self {
            structs: HashSet::new(),
            enums: HashSet::new(),
            interfaces: HashSet::new(),
            functions: HashSet::new(),
            consts: HashSet::new(),
        }
    }
}

/// Collect all symbols (structs, interfaces, functions) from each module
fn collect_module_symbols(modules: &HashMap<String, Module>) -> HashMap<String, ModuleSymbols> {
    let mut symbols_map = HashMap::new();

    for (module_id, module) in modules {
        let mut symbols = ModuleSymbols::new();

        for item in &module.items {
            match item {
                Item::Struct(s) => {
                    symbols.structs.insert(s.name.0.clone());
                }
                Item::Enum(e) => {
                    symbols.enums.insert(e.name.0.clone());
                }
                Item::Interface(i) => {
                    symbols.interfaces.insert(i.name.0.clone());
                }
                Item::Function(f) => {
                    symbols.functions.insert(f.name.0.clone());
                }
                Item::ExternFunction(f) => {
                    symbols.functions.insert(f.name.0.clone());
                }
                Item::Const(c) => {
                    symbols.consts.insert(c.name.0.clone());
                }
                Item::Impl(_) => {
                    // Methods are handled separately in resolver
                }
            }
        }

        symbols_map.insert(module_id.clone(), symbols);
    }

    symbols_map
}

/// Build import scopes for each module and detect collisions
fn build_import_scopes(
    modules: &HashMap<String, Module>,
    module_symbols: &HashMap<String, ModuleSymbols>,
) -> Result<HashMap<String, ImportScope>, Vec<Diagnostic>> {
    let mut scopes = HashMap::new();
    let mut diagnostics = Vec::new();

    for (module_id, module) in modules {
        let mut scope = ImportScope {
            direct_symbols: HashMap::new(),
            wildcard_modules: HashSet::new(),
        };

        for import in &module.imports {
            let source_module = import.path.segments.join("/");

            match &import.kind {
                ImportKind::Wildcard => {
                    // Wildcard import: just track the module name for qualified access
                    scope.wildcard_modules.insert(source_module.clone());
                }
                ImportKind::Selective { items } => {
                    // Selective import: bring specific symbols into scope
                    for item in items {
                        let local_name = item.alias.as_ref().unwrap_or(&item.name).0.clone();
                        let original_name = item.name.0.clone();

                        // Check if symbol exists in source module
                        if let Some(symbols) = module_symbols.get(&source_module) {
                            let exists = symbols.structs.contains(&original_name)
                                || symbols.enums.contains(&original_name)
                                || symbols.interfaces.contains(&original_name)
                                || symbols.functions.contains(&original_name)
                                || symbols.consts.contains(&original_name);

                            let builtin_from_std =
                                source_module == "std" && Type::from_name(&original_name).is_some();
                            if !exists && !builtin_from_std {
                                diagnostics.push(
                                    Diagnostic::error(
                                        format!(
                                            "symbol '{}' not found in module '{}'",
                                            original_name, source_module
                                        ),
                                        Some(item.span),
                                    )
                                    .with_module_id(module_id.clone()),
                                );
                                continue;
                            }
                        }

                        // Check for collision with existing imports
                        if let Some((existing_module, _)) = scope.direct_symbols.get(&local_name) {
                            diagnostics.push(Diagnostic::error(
                                format!(
                                    "symbol '{}' conflicts: imported from both '{}' and '{}'. Use aliasing to resolve: 'import {} as {} from {}'",
                                    local_name,
                                    existing_module,
                                    source_module,
                                    local_name,
                                    format!("{}_{}", source_module.replace('/', "_"), local_name),
                                    source_module
                                ),
                                Some(item.span),
                            )
                            .with_module_id(module_id.clone()));
                        } else {
                            scope
                                .direct_symbols
                                .insert(local_name, (source_module.clone(), original_name));
                        }
                    }
                }
            }
        }

        scopes.insert(module_id.clone(), scope);
    }

    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }

    Ok(scopes)
}

/// Main entry point for multi-module resolution
/// Returns the multi-module context and compilation order
pub fn resolve_multi_module(
    modules: HashMap<String, Module>,
    entry_module: &str,
    root_dir: &Path,
) -> Result<(MultiModuleContext, Vec<String>), Vec<Diagnostic>> {
    resolve_multi_module_with_aliases(modules, entry_module, root_dir, HashMap::new())
}

/// Resolve modules with explicit aliases supplied by a source-aware loader.
/// Aliases retain their import spelling but compile a single canonical body.
pub fn resolve_multi_module_with_aliases(
    mut modules: HashMap<String, Module>,
    entry_module: &str,
    _root_dir: &Path, // Reserved for future use
    module_aliases: HashMap<String, String>,
) -> Result<(MultiModuleContext, Vec<String>), Vec<Diagnostic>> {
    // Inject built-in std modules if they are not already provided by the user
    for (id, module) in std_modules() {
        modules.entry(id).or_insert(module);
    }

    if !modules.contains_key(entry_module) {
        return Err(vec![Diagnostic::error(
            format!("entry module '{}' not found", entry_module),
            None,
        )]);
    }

    let mut aliases = HashMap::new();
    for alias in module_aliases.keys() {
        let mut current = alias.as_str();
        let mut visited = HashSet::new();
        while let Some(target) = module_aliases.get(current) {
            if !visited.insert(current) {
                return Err(vec![Diagnostic::error(
                    format!("circular module alias involving '{alias}'"),
                    None,
                )]);
            }
            current = target;
        }
        let Some(canonical) = modules.get(current) else {
            return Err(vec![Diagnostic::error(
                format!("module alias '{alias}' targets missing module '{current}'"),
                None,
            )]);
        };
        if modules.get(alias) != Some(canonical) {
            return Err(vec![Diagnostic::error(
                format!("module alias '{alias}' does not share the source of '{current}'"),
                None,
            )]);
        }
        aliases.insert(alias.clone(), current.to_string());
    }
    let entry_module = aliases
        .get(entry_module)
        .map_or(entry_module, String::as_str);

    // Step 1: Build dependency graph
    let dep_graph = build_dependency_graph_with_aliases(&modules, &aliases)?;

    // Step 2: Detect circular imports
    if let Some(cycle) = dep_graph.find_cycle() {
        return Err(vec![Diagnostic::error(
            format!("circular import detected: {}", cycle.join(" → ")),
            None,
        )]);
    }

    // Step 3: Topological sort for compilation order
    let compile_order = dep_graph.topological_sort().map_err(|cycle| {
        vec![Diagnostic::error(
            format!("circular import: {}", cycle.join(" → ")),
            None,
        )]
    })?;

    // Only compile modules reachable from the entry module. This prevents unused std modules
    // (and their generic definitions) from being lowered into MIR and sent to codegen.
    let mut reachable: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(entry_module.to_string());
    while let Some(node) = queue.pop_front() {
        if !reachable.insert(node.clone()) {
            continue;
        }
        if let Some(deps) = dep_graph.edges.get(&node) {
            for dep in deps {
                queue.push_back(dep.clone());
            }
        }
    }
    let compile_order: Vec<String> = compile_order
        .into_iter()
        .filter(|m| reachable.contains(m))
        .collect();

    // Step 4: Collect symbols from all modules
    let module_symbols = collect_module_symbols(&modules);

    // Step 5: Build import scopes (with collision detection)
    let import_scopes = build_import_scopes(&modules, &module_symbols)?;

    let function_symbols =
        crate::function_symbols::collect_function_symbols(&modules, &compile_order, entry_module);

    let context = MultiModuleContext {
        modules,
        import_scopes,
        module_symbols,
        module_aliases: aliases,
        function_symbols,
    };

    Ok((context, compile_order))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_simple_cycle() {
        let mut graph = DependencyGraph::new();
        graph.add_dependency("a".to_string(), "b".to_string());
        graph.add_dependency("b".to_string(), "a".to_string());

        let cycle = graph.find_cycle();
        assert!(cycle.is_some());
        let cycle = cycle.unwrap();
        assert_eq!(cycle.len(), 3); // a -> b -> a
        assert_eq!(cycle[0], cycle[2]); // First and last should be same
    }

    #[test]
    fn detects_complex_cycle() {
        let mut graph = DependencyGraph::new();
        graph.add_dependency("a".to_string(), "b".to_string());
        graph.add_dependency("b".to_string(), "c".to_string());
        graph.add_dependency("c".to_string(), "a".to_string());

        let cycle = graph.find_cycle();
        assert!(cycle.is_some());
        let cycle = cycle.unwrap();
        assert_eq!(cycle.len(), 4); // a -> b -> c -> a
    }

    #[test]
    fn no_cycle_in_dag() {
        let mut graph = DependencyGraph::new();
        graph.add_dependency("a".to_string(), "b".to_string());
        graph.add_dependency("b".to_string(), "c".to_string());
        graph.add_dependency("a".to_string(), "c".to_string());

        let cycle = graph.find_cycle();
        assert!(cycle.is_none());
    }

    #[test]
    fn topological_sort_linear() {
        let mut graph = DependencyGraph::new();
        graph.add_dependency("a".to_string(), "b".to_string());
        graph.add_dependency("b".to_string(), "c".to_string());

        let order = graph.topological_sort().unwrap();
        assert_eq!(order.len(), 3);

        // c should come before b, b should come before a
        let c_pos = order.iter().position(|n| n == "c").unwrap();
        let b_pos = order.iter().position(|n| n == "b").unwrap();
        let a_pos = order.iter().position(|n| n == "a").unwrap();

        assert!(c_pos < b_pos);
        assert!(b_pos < a_pos);
    }

    #[test]
    fn topological_sort_detects_cycle() {
        let mut graph = DependencyGraph::new();
        graph.add_dependency("a".to_string(), "b".to_string());
        graph.add_dependency("b".to_string(), "a".to_string());

        let result = graph.topological_sort();
        assert!(result.is_err());
    }

    #[test]
    fn build_graph_from_modules() {
        use glyph_core::ast::{Import, ImportKind, ImportPath, Module};
        use glyph_core::span::Span;

        let mut modules = HashMap::new();

        // main depends on utils
        modules.insert(
            "main".to_string(),
            Module {
                imports: vec![Import {
                    kind: ImportKind::Wildcard,
                    path: ImportPath {
                        segments: vec!["utils".to_string()],
                        span: Span::new(0, 0),
                    },
                    span: Span::new(0, 0),
                }],
                items: vec![],
            },
        );

        // utils has no dependencies
        modules.insert(
            "utils".to_string(),
            Module {
                imports: vec![],
                items: vec![],
            },
        );

        let graph = build_dependency_graph(&modules).unwrap();
        let order = graph.topological_sort().unwrap();

        // utils should come before main
        let utils_pos = order.iter().position(|n| n == "utils").unwrap();
        let main_pos = order.iter().position(|n| n == "main").unwrap();
        assert!(utils_pos < main_pos);
    }

    #[test]
    fn build_graph_detects_missing_module() {
        use glyph_core::ast::{Import, ImportKind, ImportPath, Module};
        use glyph_core::span::Span;

        let mut modules = HashMap::new();

        // main depends on nonexistent module
        modules.insert(
            "main".to_string(),
            Module {
                imports: vec![Import {
                    kind: ImportKind::Wildcard,
                    path: ImportPath {
                        segments: vec!["nonexistent".to_string()],
                        span: Span::new(0, 10),
                    },
                    span: Span::new(0, 10),
                }],
                items: vec![],
            },
        );

        let result = build_dependency_graph(&modules);
        assert!(result.is_err());

        let diagnostics = result.unwrap_err();
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("nonexistent"));
    }

    #[test]
    fn build_graph_detects_circular_imports() {
        use glyph_core::ast::{Import, ImportKind, ImportPath, Module};
        use glyph_core::span::Span;

        let mut modules = HashMap::new();

        // a depends on b
        modules.insert(
            "a".to_string(),
            Module {
                imports: vec![Import {
                    kind: ImportKind::Wildcard,
                    path: ImportPath {
                        segments: vec!["b".to_string()],
                        span: Span::new(0, 0),
                    },
                    span: Span::new(0, 0),
                }],
                items: vec![],
            },
        );

        // b depends on a (circular!)
        modules.insert(
            "b".to_string(),
            Module {
                imports: vec![Import {
                    kind: ImportKind::Wildcard,
                    path: ImportPath {
                        segments: vec!["a".to_string()],
                        span: Span::new(0, 0),
                    },
                    span: Span::new(0, 0),
                }],
                items: vec![],
            },
        );

        let graph = build_dependency_graph(&modules).unwrap();
        let cycle = graph.find_cycle();
        assert!(cycle.is_some());

        let cycle = cycle.unwrap();
        assert_eq!(cycle.len(), 3); // a -> b -> a
    }

    #[test]
    fn collect_symbols_from_module() {
        use glyph_core::ast::{Block, Function, Ident, StructDef};
        use glyph_core::span::Span;

        let mut modules = HashMap::new();
        modules.insert(
            "geometry".to_string(),
            Module {
                imports: vec![],
                items: vec![
                    Item::Struct(StructDef {
                        name: Ident("Point".into()),
                        generic_params: Vec::new(),
                        fields: vec![],
                        interfaces: vec![],
                        methods: vec![],
                        inline_impls: vec![],
                        span: Span::new(0, 0),
                    }),
                    Item::Function(Function {
                        name: Ident("distance".into()),
                        params: vec![],
                        ret_type: None,
                        body: Block {
                            stmts: vec![],
                            span: Span::new(0, 0),
                        },
                        span: Span::new(0, 0),
                    }),
                ],
            },
        );

        let symbols = collect_module_symbols(&modules);
        let geom_symbols = symbols.get("geometry").unwrap();

        assert!(geom_symbols.structs.contains("Point"));
        assert!(geom_symbols.functions.contains("distance"));
    }

    #[test]
    fn build_scopes_detects_collision() {
        use glyph_core::ast::{Ident, Import, ImportItem, ImportPath, StructDef};
        use glyph_core::span::Span;

        let mut modules = HashMap::new();

        // Module with two structs
        modules.insert(
            "geo".to_string(),
            Module {
                imports: vec![],
                items: vec![Item::Struct(StructDef {
                    name: Ident("Point".into()),
                    generic_params: Vec::new(),
                    fields: vec![],
                    interfaces: vec![],
                    methods: vec![],
                    inline_impls: vec![],
                    span: Span::new(0, 0),
                })],
            },
        );

        modules.insert(
            "graphics".to_string(),
            Module {
                imports: vec![],
                items: vec![Item::Struct(StructDef {
                    name: Ident("Point".into()),
                    generic_params: Vec::new(),
                    fields: vec![],
                    interfaces: vec![],
                    methods: vec![],
                    inline_impls: vec![],
                    span: Span::new(0, 0),
                })],
            },
        );

        // Module importing Point from both (collision!)
        modules.insert(
            "main".to_string(),
            Module {
                imports: vec![
                    Import {
                        kind: ImportKind::Selective {
                            items: vec![ImportItem {
                                name: Ident("Point".into()),
                                alias: None,
                                span: Span::new(0, 5),
                            }],
                        },
                        path: ImportPath {
                            segments: vec!["geo".to_string()],
                            span: Span::new(0, 0),
                        },
                        span: Span::new(0, 0),
                    },
                    Import {
                        kind: ImportKind::Selective {
                            items: vec![ImportItem {
                                name: Ident("Point".into()),
                                alias: None,
                                span: Span::new(10, 15),
                            }],
                        },
                        path: ImportPath {
                            segments: vec!["graphics".to_string()],
                            span: Span::new(0, 0),
                        },
                        span: Span::new(0, 0),
                    },
                ],
                items: vec![],
            },
        );

        let symbols = collect_module_symbols(&modules);
        let result = build_import_scopes(&modules, &symbols);

        assert!(result.is_err());
        let diagnostics = result.unwrap_err();
        assert!(diagnostics.iter().any(|d| d.message.contains("conflicts")));
    }

    #[test]
    fn resolve_multi_module_success() {
        use glyph_core::ast::{Ident, Import, ImportPath, StructDef};
        use glyph_core::span::Span;
        use std::path::Path;

        let mut modules = HashMap::new();

        modules.insert(
            "utils".to_string(),
            Module {
                imports: vec![],
                items: vec![Item::Struct(StructDef {
                    name: Ident("Point".into()),
                    generic_params: Vec::new(),
                    fields: vec![],
                    interfaces: vec![],
                    methods: vec![],
                    inline_impls: vec![],
                    span: Span::new(0, 0),
                })],
            },
        );

        modules.insert(
            "main".to_string(),
            Module {
                imports: vec![Import {
                    kind: ImportKind::Wildcard,
                    path: ImportPath {
                        segments: vec!["utils".to_string()],
                        span: Span::new(0, 0),
                    },
                    span: Span::new(0, 0),
                }],
                items: vec![],
            },
        );

        let result = resolve_multi_module(modules, "main", Path::new("."));
        assert!(result.is_ok());

        let (context, order) = result.unwrap();

        // Verify compilation order: utils before main
        let utils_pos = order.iter().position(|m| m == "utils").unwrap();
        let main_pos = order.iter().position(|m| m == "main").unwrap();
        assert!(utils_pos < main_pos);

        // Verify import scope
        let main_scope = context.import_scopes.get("main").unwrap();
        assert!(main_scope.wildcard_modules.contains("utils"));
    }

    #[test]
    fn selective_import_extern_function() {
        use glyph_core::ast::{ExternFunctionDecl, Ident, Import, ImportItem, ImportPath};
        use glyph_core::span::Span;

        let mut modules = HashMap::new();
        modules.insert(
            "ffi".to_string(),
            Module {
                imports: vec![],
                items: vec![Item::ExternFunction(ExternFunctionDecl {
                    abi: Some("C".into()),
                    name: Ident("puts".into()),
                    params: vec![],
                    ret_type: None,
                    link_name: None,
                    span: Span::new(0, 0),
                })],
            },
        );

        modules.insert(
            "main".to_string(),
            Module {
                imports: vec![Import {
                    kind: ImportKind::Selective {
                        items: vec![ImportItem {
                            name: Ident("puts".into()),
                            alias: None,
                            span: Span::new(0, 0),
                        }],
                    },
                    path: ImportPath {
                        segments: vec!["ffi".to_string()],
                        span: Span::new(0, 0),
                    },
                    span: Span::new(0, 0),
                }],
                items: vec![],
            },
        );

        let symbols = collect_module_symbols(&modules);
        let scopes = build_import_scopes(&modules, &symbols).expect("scopes");
        let main_scope = scopes.get("main").unwrap();
        assert!(main_scope.direct_symbols.contains_key("puts"));
    }

    #[test]
    fn extern_import_collision_detected() {
        use glyph_core::ast::{ExternFunctionDecl, Ident, Import, ImportItem, ImportPath};
        use glyph_core::span::Span;

        let mut modules = HashMap::new();
        modules.insert(
            "ffi1".to_string(),
            Module {
                imports: vec![],
                items: vec![Item::ExternFunction(ExternFunctionDecl {
                    abi: Some("C".into()),
                    name: Ident("foo".into()),
                    params: vec![],
                    ret_type: None,
                    link_name: None,
                    span: Span::new(0, 0),
                })],
            },
        );

        modules.insert(
            "ffi2".to_string(),
            Module {
                imports: vec![],
                items: vec![Item::ExternFunction(ExternFunctionDecl {
                    abi: Some("C".into()),
                    name: Ident("foo".into()),
                    params: vec![],
                    ret_type: None,
                    link_name: None,
                    span: Span::new(0, 0),
                })],
            },
        );

        modules.insert(
            "main".to_string(),
            Module {
                imports: vec![
                    Import {
                        kind: ImportKind::Selective {
                            items: vec![ImportItem {
                                name: Ident("foo".into()),
                                alias: None,
                                span: Span::new(0, 0),
                            }],
                        },
                        path: ImportPath {
                            segments: vec!["ffi1".to_string()],
                            span: Span::new(0, 0),
                        },
                        span: Span::new(0, 0),
                    },
                    Import {
                        kind: ImportKind::Selective {
                            items: vec![ImportItem {
                                name: Ident("foo".into()),
                                alias: None,
                                span: Span::new(0, 0),
                            }],
                        },
                        path: ImportPath {
                            segments: vec!["ffi2".to_string()],
                            span: Span::new(0, 0),
                        },
                        span: Span::new(0, 0),
                    },
                ],
                items: vec![],
            },
        );

        let symbols = collect_module_symbols(&modules);
        let result = build_import_scopes(&modules, &symbols);
        assert!(result.is_err(), "expected collision diagnostics");
    }

    #[test]
    fn extern_import_from_subdir_module() {
        use glyph_core::ast::{
            ExternFunctionDecl, Ident, Import, ImportItem, ImportKind, ImportPath,
        };
        use glyph_core::span::Span;

        let mut modules = HashMap::new();
        modules.insert(
            "ffi/sys".to_string(),
            Module {
                imports: vec![],
                items: vec![Item::ExternFunction(ExternFunctionDecl {
                    abi: Some("C".into()),
                    name: Ident("puts".into()),
                    params: vec![],
                    ret_type: None,
                    link_name: None,
                    span: Span::new(0, 0),
                })],
            },
        );

        modules.insert(
            "main".to_string(),
            Module {
                imports: vec![Import {
                    kind: ImportKind::Selective {
                        items: vec![ImportItem {
                            name: Ident("puts".into()),
                            alias: None,
                            span: Span::new(0, 0),
                        }],
                    },
                    path: ImportPath {
                        segments: vec!["ffi".into(), "sys".into()],
                        span: Span::new(0, 0),
                    },
                    span: Span::new(0, 0),
                }],
                items: vec![],
            },
        );

        let symbols = collect_module_symbols(&modules);
        let scopes = build_import_scopes(&modules, &symbols).expect("scopes");
        let main_scope = scopes.get("main").unwrap();
        assert!(main_scope.direct_symbols.contains_key("puts"));
    }

    #[test]
    fn std_modules_are_injected_and_importable() {
        use std::path::Path;

        let src = "import std; fn main() { ret }";
        let lexed = crate::lex(src);
        let parsed = crate::parse(&lexed.tokens, src);

        let mut modules = HashMap::new();
        modules.insert("main".to_string(), parsed.module);

        let (ctx, order) =
            resolve_multi_module(modules, "main", Path::new(".")).expect("resolve std");

        assert!(ctx.modules.contains_key("std"));
        assert!(ctx.modules.contains_key("std/io"));
        assert!(ctx.modules.contains_key("std/string"));
        assert!(
            order.contains(&"std/io".to_string()),
            "compile order should include std/io"
        );
        assert!(
            order.contains(&"std/string".to_string()),
            "compile order should include std/string"
        );

        let scope = ctx.import_scopes.get("main").unwrap();
        assert!(
            scope.wildcard_modules.contains("std"),
            "import std should register std wildcard"
        );

        let std_io_symbols = ctx.module_symbols.get("std/io").unwrap();
        assert!(std_io_symbols.functions.contains("puts"));
        assert!(std_io_symbols.functions.contains("fopen"));
        assert!(std_io_symbols.functions.contains("read"));
        assert!(std_io_symbols.functions.contains("write"));
        let std_string_symbols = ctx.module_symbols.get("std/string").unwrap();
        assert!(std_string_symbols.functions.contains("strdup"));
    }
}
