//! Records a fingerprint of the sources this binary is built from (`SOURCE_HASH`), so `thomas doctor` can tell whether the running binary matches the code on disk.
include!("../core/src/source_hash.rs");

fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let root = root.canonicalize().unwrap_or(root);
    println!(
        "cargo:rustc-env=SOURCE_HASH={}",
        source_hash(&root, &BUILD_DIRS, &BUILD_EXTENSIONS)
    );
    for d in BUILD_DIRS {
        println!("cargo:rerun-if-changed=../../{d}");
    }
}
