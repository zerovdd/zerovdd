fn main() -> Result<(), wdk_build::ConfigError> {
    println!("cargo:rustc-link-lib=avrt");
    println!("cargo:rustc-link-lib=advapi32");
    wdk_build::configure_wdk_binary_build()
}
