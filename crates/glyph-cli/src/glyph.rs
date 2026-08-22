use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command as ProcessCommand;

use clap::{Parser, Subcommand};
use serde::Deserialize;

mod build_version;

#[cfg(feature = "codegen")]
use glyph_backend::{
    codegen::CodegenContext,
    linker::{Linker, LinkerOptions},
};
use glyph_core::ast::Module;
use glyph_frontend::{FrontendOptions, compile_modules};

mod module_loader;
use module_loader::{discover_and_parse_modules, module_id_from_path};

mod diagnostics;
use diagnostics::{SourceInfo, format_diagnostic};

mod dependency;
use dependency::{load_dependency_modules, resolve_dependencies};

const EXIT_SUCCESS: i32 = 0;
const EXIT_USAGE: i32 = 1;
const EXIT_COMPILER: i32 = 2;
const EXIT_INTERNAL: i32 = 3;

#[derive(Parser, Debug)]
#[command(
    name = "glyph",
    version = crate::build_version::BUILD_VERSION,
    about = crate::build_version::WORKFLOW_ABOUT
)]
struct Cli {
    #[command(subcommand)]
    command: GlyphCommand,
}

#[derive(Subcommand, Debug)]
enum GlyphCommand {
    /// Initialize a new Glyph project
    Init {
        /// Project name (defaults to directory name)
        name: Option<String>,
    },
    /// Build binaries described in glyph.toml
    Build {
        /// Build the release profile
        #[arg(long)]
        release: bool,
        /// Build only this binary
        #[arg(long)]
        bin: Option<String>,
        /// Emit extra logging
        #[arg(short, long)]
        verbose: bool,
    },
    /// Build then run a binary with optional args
    Run {
        #[arg(long)]
        release: bool,
        #[arg(long)]
        bin: Option<String>,
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
        #[arg(short, long)]
        verbose: bool,
    },
}

#[derive(Debug)]
struct GlyphError {
    code: i32,
    message: String,
}

type GlyphResult<T> = Result<T, GlyphError>;

#[derive(Debug, Deserialize)]
struct PackageSection {
    name: String,
    version: String,
}

#[derive(Debug, Deserialize)]
struct BinTarget {
    name: String,
    path: String,
}

#[derive(Debug, Deserialize)]
struct LibTarget {
    #[serde(default = "default_lib_path")]
    path: String,
}

fn default_lib_path() -> String {
    "src/lib.glyph".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DependencySpec {
    Path { path: String },
}

/// `[link]` section: native libraries and search paths passed to the linker.
///
/// ```toml
/// [link]
/// libs = ["m", "portaudio"]
/// search_paths = ["vendor/lib"]
/// ```
///
/// Relative search paths are resolved against the project root.
#[derive(Debug, Deserialize, Default)]
struct LinkSection {
    #[serde(default)]
    libs: Vec<String>,
    #[serde(default)]
    search_paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    package: PackageSection,
    #[serde(default)]
    lib: Option<LibTarget>,
    #[serde(default)]
    bin: Vec<BinTarget>,
    #[serde(default)]
    dependencies: HashMap<String, DependencySpec>,
    #[serde(default)]
    link: LinkSection,
}

fn main() {
    let exit = match run() {
        Ok(()) => EXIT_SUCCESS,
        Err(err) => {
            eprintln!("{}", err.message);
            err.code
        }
    };
    std::process::exit(exit);
}

fn run() -> GlyphResult<()> {
    let cli = Cli::parse();
    match cli.command {
        GlyphCommand::Init { name } => init_cmd(name),
        GlyphCommand::Build {
            release,
            bin,
            verbose,
        } => {
            let _ = build_cmd(release, bin, verbose)?;
            Ok(())
        }
        GlyphCommand::Run {
            release,
            bin,
            args,
            verbose,
        } => run_cmd(release, bin, args, verbose),
    }
}

fn init_cmd(name: Option<String>) -> GlyphResult<()> {
    let cwd = std::env::current_dir().map_err(internal_err("failed to read current dir"))?;
    let manifest_path = cwd.join("glyph.toml");

    // Check if glyph.toml already exists
    if manifest_path.exists() {
        return Err(usage_err("glyph.toml already exists in current directory"));
    }

    // Get project name from argument or directory name
    let project_name = if let Some(name) = name {
        name
    } else {
        cwd.file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| usage_err("could not determine project name from directory"))?
            .to_string()
    };

    // Create glyph.toml
    let toml_content = format!(
        r#"[package]
name = "{}"
version = "0.1.0"

[dependencies]
# Add dependencies here

[[bin]]
name = "{}"
path = "src/main.glyph"
"#,
        project_name, project_name
    );

    fs::write(&manifest_path, toml_content).map_err(internal_err("failed to write glyph.toml"))?;

    println!("Created glyph.toml");

    // Create src directory
    let src_dir = cwd.join("src");
    fs::create_dir_all(&src_dir).map_err(internal_err("failed to create src/ directory"))?;

    // Create src/main.glyph
    let main_glyph_content = r#"from std import println

fn main() -> i32 {
  println("Hello, Glyph!")
  ret 0
}
"#;

    let main_path = src_dir.join("main.glyph");
    fs::write(&main_path, main_glyph_content)
        .map_err(internal_err("failed to write src/main.glyph"))?;

    println!("Created src/main.glyph");
    println!("\nProject '{}' initialized!", project_name);
    println!("\nNext steps:");
    println!("  glyph build");
    println!("  glyph run");

    Ok(())
}

fn build_cmd(release: bool, bin: Option<String>, verbose: bool) -> GlyphResult<Option<PathBuf>> {
    let cwd = std::env::current_dir().map_err(internal_err("failed to read current dir"))?;
    let manifest_path = locate_manifest(&cwd)?;
    let root = manifest_path
        .parent()
        .ok_or_else(|| usage_err("glyph.toml has no parent directory"))?;

    let manifest = load_manifest(&manifest_path, root)?;
    let profile = if release { "release" } else { "debug" };

    if manifest.bin.is_empty() {
        if bin.is_some() {
            return Err(usage_err(
                "no [[bin]] targets defined (this is a library-only case)",
            ));
        }
        check_lib_case(root, &manifest, verbose)?;
        println!(
            "Checked library case '{}' successfully",
            manifest.package.name
        );
        return Ok(None);
    }

    let target = select_bin(&manifest, bin.as_deref())?;
    let built_path = build_bin(root, &manifest, &target, profile, verbose)?;
    println!(
        "Built {} ({}) -> {}",
        target.name,
        profile,
        built_path.display()
    );
    Ok(Some(built_path))
}

fn run_cmd(
    release: bool,
    bin: Option<String>,
    args: Vec<String>,
    verbose: bool,
) -> GlyphResult<()> {
    let cwd = std::env::current_dir().map_err(internal_err("failed to read current dir"))?;
    let manifest_path = locate_manifest(&cwd)?;
    let root = manifest_path
        .parent()
        .ok_or_else(|| usage_err("glyph.toml has no parent directory"))?;
    let manifest = load_manifest(&manifest_path, root)?;
    let target = select_bin(&manifest, bin.as_deref())?;
    let profile = if release { "release" } else { "debug" };
    let exe = build_bin(root, &manifest, &target, profile, verbose)?;
    let status = ProcessCommand::new(&exe)
        .args(args)
        .status()
        .map_err(internal_err("failed to spawn binary"))?;
    if !status.success() {
        return Err(GlyphError {
            code: EXIT_COMPILER,
            message: format!("program exited with status {:?}", status.code()),
        });
    }
    Ok(())
}

fn locate_manifest(start: &Path) -> GlyphResult<PathBuf> {
    for ancestor in start.ancestors() {
        let candidate = ancestor.join("glyph.toml");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(GlyphError {
        code: EXIT_USAGE,
        message: "glyph.toml not found (searching current dir and parents)".to_string(),
    })
}

fn parse_manifest(path: &Path) -> GlyphResult<Manifest> {
    let contents = fs::read_to_string(path)
        .map_err(|e| usage_err(format!("failed to read {}: {}", path.display(), e)))?;
    let manifest: Manifest =
        toml::from_str(&contents).map_err(|e| usage_err(format!("invalid glyph.toml: {}", e)))?;
    Ok(manifest)
}

fn load_manifest(path: &Path, root: &Path) -> GlyphResult<Manifest> {
    let manifest = parse_manifest(path)?;
    validate_manifest(&manifest, root)?;
    Ok(manifest)
}

fn validate_manifest(manifest: &Manifest, root: &Path) -> GlyphResult<()> {
    if manifest.package.name.trim().is_empty() {
        return Err(usage_err("[package].name is required"));
    }
    if manifest.package.version.trim().is_empty() {
        return Err(usage_err("[package].version is required"));
    }
    if manifest.bin.is_empty() && manifest.lib.is_none() {
        return Err(usage_err(
            "at least one [[bin]] entry or a [lib] target is required",
        ));
    }

    if let Some(lib) = &manifest.lib {
        if lib.path.trim().is_empty() {
            return Err(usage_err("[lib].path is required"));
        }
        let _ = validate_relative_glyph_path("[lib].path", "lib path", &lib.path, root)?;
    }

    let mut seen = HashSet::new();
    for bin in &manifest.bin {
        if bin.name.trim().is_empty() {
            return Err(usage_err("[[bin]].name is required"));
        }
        if !seen.insert(&bin.name) {
            return Err(usage_err(format!("duplicate [[bin]] name '{}'", bin.name)));
        }
        if bin.path.trim().is_empty() {
            return Err(usage_err(format!(
                "[[bin]] '{}' must specify path",
                bin.name
            )));
        }
        let _ = validate_relative_glyph_path("[[bin]].path", "bin path", &bin.path, root)?;
    }

    for (name, spec) in &manifest.dependencies {
        if !is_valid_identifier(name) {
            return Err(usage_err(format!(
                "dependency name '{}' is not a valid identifier",
                name
            )));
        }
        if name == "std" {
            return Err(usage_err("dependency name 'std' is reserved"));
        }

        let path = match spec {
            DependencySpec::Path { path } => path,
        };
        if path.trim().is_empty() {
            return Err(usage_err(format!(
                "dependency '{}' must specify a path",
                name
            )));
        }
        let dep_root = resolve_dependency_root(root, path);
        if !dep_root.is_dir() {
            return Err(usage_err(format!(
                "dependency '{}' path does not exist: {}",
                name,
                dep_root.display()
            )));
        }
        let dep_manifest_path = dep_root.join("glyph.toml");
        if !dep_manifest_path.is_file() {
            return Err(usage_err(format!(
                "dependency '{}' missing glyph.toml at {}",
                name,
                dep_manifest_path.display()
            )));
        }
        let dep_manifest = parse_manifest(&dep_manifest_path)?;
        let Some(lib) = &dep_manifest.lib else {
            return Err(usage_err(format!(
                "dependency '{}' does not define a [lib] target",
                name
            )));
        };
        if lib.path.trim().is_empty() {
            return Err(usage_err(format!(
                "dependency '{}' has empty [lib].path",
                name
            )));
        }
        let _ = validate_relative_glyph_path(
            "[lib].path",
            "dependency lib path",
            &lib.path,
            &dep_root,
        )?;
    }

    Ok(())
}

fn select_bin<'a>(manifest: &'a Manifest, requested: Option<&str>) -> GlyphResult<&'a BinTarget> {
    if manifest.bin.is_empty() {
        return Err(usage_err(
            "no [[bin]] targets defined (this is a library-only case)",
        ));
    }
    if let Some(name) = requested {
        manifest.bin.iter().find(|b| b.name == name).ok_or_else(|| {
            let names: Vec<_> = manifest.bin.iter().map(|b| b.name.as_str()).collect();
            usage_err(format!(
                "bin '{}' not found. available: {}",
                name,
                names.join(", ")
            ))
        })
    } else if manifest.bin.len() == 1 {
        Ok(&manifest.bin[0])
    } else {
        let names: Vec<_> = manifest.bin.iter().map(|b| b.name.as_str()).collect();
        Err(usage_err(format!(
            "multiple bins defined; select one with --bin <name> (available: {})",
            names.join(", ")
        )))
    }
}

fn load_case_modules(
    root: &Path,
    module_root: &Path,
    manifest: &Manifest,
) -> GlyphResult<(HashMap<String, Module>, HashMap<String, SourceInfo>)> {
    let load = discover_and_parse_modules(module_root).map_err(|e| GlyphError {
        code: EXIT_COMPILER,
        message: format!("failed to load modules: {}", e),
    })?;
    if !load.diagnostics.is_empty() {
        for diag in &load.diagnostics {
            eprintln!("{}", format_diagnostic(diag, &load.sources));
        }
        return Err(GlyphError {
            code: EXIT_COMPILER,
            message: "build failed due to compiler diagnostics".to_string(),
        });
    }
    let mut modules = load.modules;
    let mut sources = load.sources;

    let mut visited = HashSet::new();
    let dependencies = resolve_dependencies(root, manifest, &mut visited)?;
    for dep in dependencies {
        let (dep_modules, dep_sources) = load_dependency_modules(&dep)?;
        merge_dependency_maps(&mut modules, &mut sources, dep_modules, dep_sources)?;
    }

    Ok((modules, sources))
}

fn merge_dependency_maps(
    modules: &mut HashMap<String, Module>,
    sources: &mut HashMap<String, SourceInfo>,
    dep_modules: HashMap<String, Module>,
    dep_sources: HashMap<String, SourceInfo>,
) -> GlyphResult<()> {
    for (module_id, module) in dep_modules {
        if modules.contains_key(&module_id) {
            return Err(GlyphError {
                code: EXIT_COMPILER,
                message: format!("module id collision for '{}'", module_id),
            });
        }
        modules.insert(module_id, module);
    }
    for (module_id, source) in dep_sources {
        if sources.contains_key(&module_id) {
            return Err(GlyphError {
                code: EXIT_COMPILER,
                message: format!("module source collision for '{}'", module_id),
            });
        }
        sources.insert(module_id, source);
    }
    Ok(())
}

fn check_lib_case(root: &Path, manifest: &Manifest, verbose: bool) -> GlyphResult<()> {
    let lib = manifest
        .lib
        .as_ref()
        .ok_or_else(|| usage_err("[lib] target is required for library-only builds"))?;
    let source_path = root.join(&lib.path);
    let module_root = source_path.parent().unwrap_or(root);

    let entry_module = module_id_from_path(module_root, &source_path).map_err(|e| GlyphError {
        code: EXIT_COMPILER,
        message: format!("failed to determine entry module: {}", e),
    })?;

    let (modules, sources) = load_case_modules(root, module_root, manifest)?;

    let frontend_output = compile_modules(
        modules,
        &entry_module,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );

    if !frontend_output.diagnostics.is_empty() {
        for diag in &frontend_output.diagnostics {
            eprintln!("{}", format_diagnostic(diag, &sources));
        }
        return Err(GlyphError {
            code: EXIT_COMPILER,
            message: "build failed due to compiler diagnostics".to_string(),
        });
    }

    let _ = verbose;
    Ok(())
}

fn build_bin(
    root: &Path,
    manifest: &Manifest,
    bin: &BinTarget,
    profile: &str,
    verbose: bool,
) -> GlyphResult<PathBuf> {
    let source_path = root.join(&bin.path);
    let module_root = source_path.parent().unwrap_or(root);
    let output_dir = root.join("target").join(profile);
    fs::create_dir_all(&output_dir).map_err(internal_err(format!(
        "failed to create {}",
        output_dir.display()
    )))?;

    let entry_module = module_id_from_path(module_root, &source_path).map_err(|e| GlyphError {
        code: EXIT_COMPILER,
        message: format!("failed to determine entry module: {}", e),
    })?;

    let (modules, sources) = load_case_modules(root, module_root, manifest)?;

    let frontend_output = compile_modules(
        modules,
        &entry_module,
        FrontendOptions {
            emit_mir: true,
            include_std: true,
        },
    );

    if !frontend_output.diagnostics.is_empty() {
        for diag in &frontend_output.diagnostics {
            eprintln!("{}", format_diagnostic(diag, &sources));
        }
        return Err(GlyphError {
            code: EXIT_COMPILER,
            message: "build failed due to compiler diagnostics".to_string(),
        });
    }

    #[cfg(not(feature = "codegen"))]
    {
        let _ = verbose;
        return Err(GlyphError {
            code: EXIT_INTERNAL,
            message: "building executables requires the 'codegen' feature".to_string(),
        });
    }

    #[cfg(feature = "codegen")]
    {
        let obj_path = output_dir.join(format!("{}.o", bin.name));
        let exe_path = output_dir.join(&bin.name);

        if verbose {
            eprintln!("[glyph] writing object: {}", obj_path.display());
        }

        let mut ctx = CodegenContext::new("glyph_module")
            .map_err(internal_err("failed to create codegen context"))?;
        ctx.codegen_module(&frontend_output.mir)
            .map_err(internal_err("codegen failed"))?;
        ctx.emit_object_file(&obj_path)
            .map_err(internal_err("failed to emit object file"))?;

        let linker = Linker::new();
        let runtime_lib = Linker::get_runtime_lib_path();
        let link_search_paths = manifest
            .link
            .search_paths
            .iter()
            .map(|p| root.join(p))
            .collect();
        let opts = LinkerOptions {
            output_path: exe_path.clone(),
            object_files: vec![obj_path.clone()],
            link_libs: manifest.link.libs.clone(),
            link_search_paths,
            runtime_lib_path: runtime_lib,
        };

        if verbose {
            eprintln!("[glyph] linking to {}", exe_path.display());
        }

        linker.link(&opts).map_err(internal_err("linking failed"))?;
        let _ = fs::remove_file(&obj_path);
        Ok(exe_path)
    }
}

fn validate_relative_glyph_path(
    field_label: &str,
    path_label: &str,
    path_str: &str,
    root: &Path,
) -> GlyphResult<PathBuf> {
    let path = Path::new(path_str);
    if path.is_absolute() {
        return Err(usage_err(format!(
            "{} must be relative: '{}'",
            field_label, path_str
        )));
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(usage_err(format!(
            "{} must not traverse parents ('{}')",
            field_label, path_str
        )));
    }
    if path.extension().and_then(|s| s.to_str()) != Some("glyph") {
        return Err(usage_err(format!(
            "{} must end with .glyph ('{}')",
            field_label, path_str
        )));
    }

    let full = root.join(path);
    if !full.is_file() {
        return Err(usage_err(format!(
            "{} does not exist: {}",
            path_label,
            full.display()
        )));
    }

    Ok(full)
}

fn resolve_dependency_root(root: &Path, path: &str) -> PathBuf {
    let dep_path = Path::new(path);
    if dep_path.is_absolute() {
        dep_path.to_path_buf()
    } else {
        root.join(dep_path)
    }
}

fn is_valid_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !is_ident_start(first) {
        return false;
    }
    chars.all(is_ident_continue)
}

fn is_ident_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_'
}

fn is_ident_continue(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn usage_err<T: Into<String>>(msg: T) -> GlyphError {
    GlyphError {
        code: EXIT_USAGE,
        message: msg.into(),
    }
}

fn internal_err<T: Into<String>, E: std::fmt::Display>(msg: T) -> impl FnOnce(E) -> GlyphError {
    let message = msg.into();
    move |e| GlyphError {
        code: EXIT_INTERNAL,
        message: format!("{}: {}", message, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn select_bin_requires_flag_when_multiple() {
        let manifest = Manifest {
            package: PackageSection {
                name: "foo".into(),
                version: "0.1.0".into(),
            },
            lib: None,
            bin: vec![
                BinTarget {
                    name: "a".into(),
                    path: "a.glyph".into(),
                },
                BinTarget {
                    name: "b".into(),
                    path: "b.glyph".into(),
                },
            ],
            dependencies: HashMap::new(),
        };
        let err = select_bin(&manifest, None).unwrap_err();
        assert_eq!(err.code, EXIT_USAGE);
    }

    #[test]
    fn validate_manifest_rejects_parent_dirs() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        let manifest = Manifest {
            package: PackageSection {
                name: "foo".into(),
                version: "0.1.0".into(),
            },
            lib: None,
            bin: vec![BinTarget {
                name: "a".into(),
                path: "../a.glyph".into(),
            }],
            dependencies: HashMap::new(),
        };
        // Need the file to exist to reach traversal check; create placeholder in root/.. not possible
        // Instead, expect traversal rejection before file existence is checked.
        let err = validate_manifest(&manifest, root).unwrap_err();
        assert_eq!(err.code, EXIT_USAGE);
    }
}
