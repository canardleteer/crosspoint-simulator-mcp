use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let proto_root = manifest.join("../../protos");
    println!("cargo:rerun-if-changed={}", proto_root.display());
    println!(
        "cargo:rerun-if-changed={}",
        manifest.join("buf.gen.yaml").display()
    );

    let buf = buf_tools::buf_bin_path();
    let status = Command::new(buf)
        .arg("generate")
        .current_dir(&manifest)
        .status()
        .expect("failed to spawn buf generate");
    if !status.success() {
        panic!("buf generate failed: {status}");
    }
}
