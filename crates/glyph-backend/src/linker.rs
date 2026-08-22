use anyhow::{Context, Result, anyhow};
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Options for linking object files into an executable
#[derive(Debug, Clone)]
pub struct LinkerOptions {
    /// Path where the output executable should be written
    pub output_path: PathBuf,
    /// Object files to link together
    pub object_files: Vec<PathBuf>,
    /// Additional libraries to link (e.g., "c", "m")
    pub link_libs: Vec<String>,
    /// Additional library search paths
    pub link_search_paths: Vec<PathBuf>,
    /// Path to the Glyph runtime library (libglyph_runtime.a)
    pub runtime_lib_path: Option<PathBuf>,
}

/// Platform-specific linker that invokes system linkers (clang/ld)
pub struct Linker {
    platform: Platform,
}

#[derive(Debug, Clone, Copy)]
enum Platform {
    MacOS,
    Linux,
    Other,
}

impl Linker {
    /// Create a new linker for the current platform
    pub fn new() -> Self {
        let platform = if cfg!(target_os = "macos") {
            Platform::MacOS
        } else if cfg!(target_os = "linux") {
            Platform::Linux
        } else {
            Platform::Other
        };

        Self { platform }
    }

    /// Link object files into an executable
    pub fn link(&self, opts: &LinkerOptions) -> Result<()> {
        match self.platform {
            Platform::MacOS => self.link_macos(opts),
            Platform::Linux => self.link_linux(opts),
            Platform::Other => Err(anyhow!("Unsupported platform for linking")),
        }
    }

    /// Link on macOS using clang
    fn link_macos(&self, opts: &LinkerOptions) -> Result<()> {
        let mut cmd = Command::new("clang");

        // Add all object files
        for obj in &opts.object_files {
            cmd.arg(obj);
        }

        // Specify output executable path
        cmd.arg("-o").arg(&opts.output_path);

        // Link against the Glyph runtime library if available
        if let Some(runtime_path) = &opts.runtime_lib_path {
            cmd.arg(runtime_path);
        }

        // Add library search paths
        for path in &opts.link_search_paths {
            cmd.arg(format!("-L{}", path.display()));
        }

        // Link against requested libraries
        for lib in &opts.link_libs {
            cmd.arg(format!("-l{}", lib));
        }

        // macOS-specific: Link against system library
        cmd.arg("-lSystem");

        // Execute the linker
        let output = cmd
            .output()
            .context("Failed to execute clang linker. Is clang installed?")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("Linker failed:\n{}", stderr));
        }

        // Make the output executable (chmod +x)
        #[cfg(unix)]
        make_executable(&opts.output_path)?;

        Ok(())
    }

    /// Link on Linux using clang
    fn link_linux(&self, opts: &LinkerOptions) -> Result<()> {
        let mut cmd = Command::new("clang");

        // Add all object files
        for obj in &opts.object_files {
            cmd.arg(obj);
        }

        // Specify output executable path
        cmd.arg("-o").arg(&opts.output_path);

        // Link against the Glyph runtime library if available
        if let Some(runtime_path) = &opts.runtime_lib_path {
            cmd.arg(runtime_path);
        }

        // Add library search paths
        for path in &opts.link_search_paths {
            cmd.arg(format!("-L{}", path.display()));
        }

        // Link against requested libraries
        for lib in &opts.link_libs {
            cmd.arg(format!("-l{}", lib));
        }

        // Linux-specific: Link against libc, and libm so std/math externs
        // (sin, pow, ...) resolve without a [link] section. (On macOS libm
        // is part of libSystem.)
        cmd.arg("-lc");
        cmd.arg("-lm");

        // Execute the linker
        let output = cmd
            .output()
            .context("Failed to execute clang linker. Is clang installed?")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("Linker failed:\n{}", stderr));
        }

        // Make the output executable (chmod +x)
        #[cfg(unix)]
        make_executable(&opts.output_path)?;

        Ok(())
    }

    /// Get the path to the built runtime library from cargo build output
    ///
    /// The build.rs script compiles the runtime and places it in OUT_DIR.
    /// This function retrieves that path embedded at compile time.
    pub fn get_runtime_lib_path() -> Option<PathBuf> {
        // OUT_DIR is only available during compilation, so we embed it at compile time
        // using the env! macro which is evaluated during rustc compilation
        #[cfg(feature = "codegen")]
        {
            let out_dir = env!("OUT_DIR");
            let lib_path = PathBuf::from(out_dir).join("libglyph_runtime.a");

            if lib_path.exists() {
                Some(lib_path)
            } else {
                None
            }
        }
        #[cfg(not(feature = "codegen"))]
        {
            None
        }
    }
}

impl Default for Linker {
    fn default() -> Self {
        Self::new()
    }
}

/// Make a file executable on Unix systems (chmod +x)
#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut perms = std::fs::metadata(path)
        .context("Failed to read file metadata")?
        .permissions();

    // Set executable bit for owner, group, and others (0o755 = rwxr-xr-x)
    perms.set_mode(0o755);

    std::fs::set_permissions(path, perms).context("Failed to set executable permissions")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linker_detects_platform() {
        let linker = Linker::new();
        // Just ensure it doesn't panic
        assert!(matches!(
            linker.platform,
            Platform::MacOS | Platform::Linux | Platform::Other
        ));
    }

    #[test]
    fn runtime_lib_path_returns_pathbuf() {
        // This test just ensures the function doesn't panic
        // The actual path may or may not exist depending on build state
        let _ = Linker::get_runtime_lib_path();
    }
}
