fn main() {
    println!("cargo:rerun-if-changed=native");
    if std::env::var_os("CARGO_FEATURE_BACKEND").is_none() {
        return;
    }
    assert_eq!(std::env::var("CARGO_CFG_TARGET_OS").unwrap(), "linux");
    assert_eq!(std::env::var("CARGO_CFG_TARGET_ARCH").unwrap(), "x86_64");
    let sources = "aesctr comkey constraints cpucycles dachshund data fips202 gaussian jlproj labradoodle labrador_core labrador_tail labrador lnp ntt pack polx poly polz proofsystem randombytes rejection labrados_python timing bridge wire";
    let mut build = cc::Build::new();
    build
        .include("native/scalar512")
        .include("native")
        .define("LOGQ", "38")
        .define("NDEBUG", None)
        .flag("-std=gnu2x")
        .flag("-fwrapv")
        .flag("-ffast-math")
        .flag("-ffp-contract=off")
        .flag("-mavx2")
        .flag("-Wno-psabi")
        .warnings(false)
        .opt_level(3);
    for source in sources.split_whitespace() {
        build.file(format!("native/{source}.c"));
    }
    build.compile("dgx_labrador_packed11_v2");
    println!("cargo:rustc-link-lib=mvec");
    println!("cargo:rustc-link-lib=m");
}
