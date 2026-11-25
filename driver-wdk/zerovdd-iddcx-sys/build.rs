use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=c/iddcx_trampoline.h");
    println!("cargo:rerun-if-changed=c/iddcx_trampoline.c");
    println!("cargo:rerun-if-env-changed=WDKContentRoot");
    println!("cargo:rerun-if-env-changed=INCLUDE");
    println!("cargo:rerun-if-env-changed=LIB");
    println!("cargo:rerun-if-env-changed=CC");

    let config = wdk_build::Config::from_env_auto()?;
    config.configure_library_build()?;

    let mut iddcx_lib_dir: Option<PathBuf> = None;
    for lib_dir in config.library_paths()? {
        let candidate = lib_dir.join("iddcx").join("1.10");
        if candidate.join("iddcxstub.lib").is_file() || candidate.join("IddCxStub.lib").is_file() {
            iddcx_lib_dir = Some(candidate);
            break;
        }
    }

    let iddcx_lib_dir = iddcx_lib_dir
        .ok_or("Could not locate iddcx\\1.10\\iddcxstub.lib inside WDK library paths")?;
    println!("cargo:rustc-link-search=native={}", iddcx_lib_dir.display());
    println!("cargo:rustc-link-lib=iddcxstub");
    println!("cargo:rustc-link-lib=d3d11");
    println!("cargo:rustc-link-lib=dxgi");

    let mut build = cc::Build::new();
    // WHY: Fall back to clang-cl when no global Visual Studio cl.exe exists on PATH.
    if env::var_os("CC").is_none() && build.try_get_compiler().is_err() {
        build.compiler("clang-cl");
    }

    build
        .cpp(false)
        .warnings(false)
        .file("c/iddcx_trampoline.c")
        .include("c")
        .flag("/TP")
        .flag("/EHs-c-")
        .flag("/GR-")
        .flag_if_supported("-Wno-builtin-macro-redefined")
        .flag_if_supported("-Wno-invalid-token-paste")
        .flag_if_supported("-Wno-ignored-attributes")
        .flag_if_supported("-Wno-unknown-pragmas")
        .flag_if_supported("-Wno-microsoft-enum-forward-reference")
        .flag_if_supported("-Wno-switch")
        .flag_if_supported("-Wno-nonportable-include-path")
        .flag_if_supported("-Wno-ignored-pragma-intrinsic")
        .flag_if_supported("-Wno-pragma-pack")
        .flag_if_supported("-Wno-unused-parameter");

    for include_path in config.include_paths()? {
        build.include(include_path);
    }
    if let Some(extra_includes) = env::var_os("INCLUDE") {
        for path in env::split_paths(&extra_includes) {
            if !path.as_os_str().is_empty() {
                build.include(path);
            }
        }
    }

    build.define("UMDF_USING_NTSTATUS", None);
    for (key, value) in config.preprocessor_definitions() {
        build.define(&key, value.as_deref());
    }

    build.compile("zerovdd_iddcx_trampoline");
    Ok(())
}
