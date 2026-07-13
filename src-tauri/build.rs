fn main() {
    configure_common_controls_v6_manifest();

    #[cfg(feature = "tauri-runtime")]
    {
        ensure_sidecar_placeholder();
        // The package-wide linker manifest below also covers lib/bin unit-test
        // harnesses. Keep Tauri's icon/version resource, but omit its duplicate
        // ID=1 manifest so normal binaries still link successfully.
        let attributes = tauri_build::Attributes::new()
            .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
        tauri_build::try_build(attributes).expect("failed to run Tauri build script");
    }
}

fn configure_common_controls_v6_manifest() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("windows-msvc") {
        // `rustc-link-arg-tests` reaches integration tests but not the unit-test
        // harnesses generated from lib.rs and bin targets. Emit package-wide
        // link args so every Windows executable gets the same activation
        // context; library artifacts themselves do not invoke the linker.
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        // Do not wrap the dependency string in extra quotes: rustc/Command
        // already quotes space-containing link args for link.exe. Nested
        // quotes here produce LNK1181 (linker treats name='...' as a .lib).
        println!(
            "cargo:rustc-link-arg=/MANIFESTDEPENDENCY:\
             type='win32' name='Microsoft.Windows.Common-Controls' \
             version='6.0.0.0' processorArchitecture='*' \
             publicKeyToken='6595b64144ccf1df' language='*'"
        );
    }
}

/// Tauri's bundler validates that every `bundle.externalBin` path resolves
/// to an existing file at build.rs time. The real `codeg-mcp` and
/// `codex-acp` sidecars are produced by `pnpm tauri:prepare-sidecars` (invoked from
/// `beforeBuildCommand` / `beforeDevCommand` and the CI release matrix) —
/// but plain `cargo check --features tauri-runtime` doesn't go through that
/// path, so without a backstop every contributor would hit
/// `resource path ... doesn't exist` on first compile.
///
/// We write a zero-byte placeholder when the sidecar is missing so
/// `cargo check` / clippy / rust-analyzer succeed. Production paths
/// overwrite the placeholder with the real binary before Tauri bundles it:
///   * `pnpm tauri build`  → `beforeBuildCommand` → `prepare-sidecars.mjs`
///   * release.yml         → explicit sidecar staging step
///   * `pnpm tauri dev`    → `beforeDevCommand` → `prepare-sidecars.mjs`
///
/// If you ever bypass those wrappers (e.g. invoking the Tauri CLI directly
/// without beforeBuildCommand) you'd ship the placeholder, so emit a
/// cargo:warning that surfaces in any compile log to make that loud.
#[cfg(feature = "tauri-runtime")]
fn ensure_sidecar_placeholder() {
    use std::fs;
    use std::path::PathBuf;

    let triple = std::env::var("TARGET").unwrap_or_default();
    if triple.is_empty() {
        return;
    }
    let ext = if triple.contains("windows") {
        ".exe"
    } else {
        ""
    };
    let dir = PathBuf::from("binaries");
    for sidecar in ["codeg-mcp", "codex-acp"] {
        let path = dir.join(format!("{sidecar}-{triple}{ext}"));

        println!("cargo:rerun-if-changed={}", path.display());

        let needs_placeholder = match fs::metadata(&path) {
            Ok(meta) => meta.len() == 0,
            Err(_) => true,
        };

        if needs_placeholder {
            if let Err(e) = fs::create_dir_all(&dir) {
                panic!("failed to create {}: {e}", dir.display());
            }
            if let Err(e) = fs::write(&path, b"") {
                panic!(
                    "failed to write sidecar placeholder {}: {e}",
                    path.display()
                );
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o755));
            }
            println!(
                "cargo:warning={sidecar} sidecar missing at {}; wrote 0-byte placeholder. \
                 Run `pnpm tauri:prepare-sidecars` before `tauri build` to ship a working binary.",
                path.display()
            );
        }
    }
}
