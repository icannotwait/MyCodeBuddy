fn main() {
    configure_test_shard();
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
/// to an existing file at build.rs time. The real `codeg-mcp` sidecar is
/// produced by `pnpm tauri:prepare-sidecars` (invoked from
/// `beforeBuildCommand` / `beforeDevCommand` and the CI release matrix) —
/// but plain `cargo check --features tauri-runtime` doesn't go through that
/// path, so without a backstop every contributor would hit
/// `resource path ... doesn't exist` on first compile.
///
/// `codeg-mcp` is the only Tauri externalBin sidecar. Codex ACP launches from
/// the official npm package (`@agentclientprotocol/codex-acp`), not as a sidecar.
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
    let sidecar = "codeg-mcp";
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

/// Opt-in unit-test compilation shards. Unset means the original full suite.
/// The wrapper audits all shard listings against the source inventory.
fn configure_test_shard() {
    println!("cargo:rerun-if-env-changed=CODEG_TEST_SHARD");
    println!("cargo:rerun-if-changed=test-shard-count.txt");
    let count: usize = include_str!("test-shard-count.txt")
        .trim()
        .parse()
        .expect("test-shard-count.txt must contain a shard count");
    assert!((1..=16).contains(&count), "test shard count must be 1..16");
    let values = (0..count)
        .map(|shard| format!("\"{shard}\""))
        .collect::<Vec<_>>()
        .join(", ");
    println!("cargo:rustc-check-cfg=cfg(codeg_test_shard, values(none(), {values}))");
    if let Some(value) = std::env::var_os("CODEG_TEST_SHARD") {
        let value = value.to_str().expect("CODEG_TEST_SHARD must be UTF-8");
        let shard: usize = value
            .parse()
            .expect("CODEG_TEST_SHARD must be a shard number");
        assert!(
            shard < count && value == shard.to_string(),
            "CODEG_TEST_SHARD must identify a configured shard; use scripts/rust-test-shards.py for the complete suite"
        );
        println!("cargo:warning=Compiling ONLY library test shard {shard} of {count}; this is not the complete suite");
        println!("cargo:rustc-cfg=codeg_test_shard");
        println!("cargo:rustc-cfg=codeg_test_shard=\"{shard}\"");
    }
}
