//! Compiles moomoo's published OpenD protocol definitions (proto/, Apache-2.0, from
//! github.com/MoomooOpen/py-moomoo-api) into Rust types, and embeds the git commit of the build
//! (`engine::ENGINE_COMMIT`) so every archived backtest and paper record says which code produced it.
use std::process::Command;

/// Build script: records the git commit (`git describe`) as `BOT_GIT_COMMIT` so every run and archive row names the code that made it.
fn main() -> std::io::Result<()> {
    let commit = Command::new("git")
        .args(["describe", "--always", "--dirty", "--abbrev=12"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=BOT_GIT_COMMIT={commit}");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");
    println!("cargo:rerun-if-changed=build.rs");
    let protos: Vec<_> = std::fs::read_dir("proto")?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "proto"))
        .collect();
    println!("cargo:rerun-if-changed=proto");
    prost_build::Config::new()
        .include_file("pb.rs")
        .compile_protos(&protos, &["proto"])
}
