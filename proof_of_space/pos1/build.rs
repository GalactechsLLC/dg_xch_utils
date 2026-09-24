use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=src/gigahorse_gpu/kernel.inc");
    if env::var_os("CARGO_FEATURE_VULKAN").is_none() {
        return;
    }
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let source = output.join("gigahorse.comp");
    fs::write(
        &source,
        format!(
            "#version 450\n{}",
            include_str!("src/gigahorse_gpu/kernel.inc")
        ),
    )
    .unwrap();
    let result = Command::new("glslangValidator")
        .args(["-V", "--target-env", "vulkan1.2", "-S", "comp", "-o"])
        .arg(output.join("gigahorse.spv"))
        .arg(source)
        .output()
        .expect("building the GigaHorse Vulkan backend requires glslangValidator");
    assert!(
        result.status.success(),
        "GigaHorse shader compilation failed: {}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
