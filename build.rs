//! Capture the cake build's git identity at compile time.
//!
//! The build script writes empty values when git is unavailable so a source
//! tarball or a non-git build still compiles and omits the metadata.

use std::process::Command;

fn main() {
    let commit = git(&["rev-parse", "HEAD"]).unwrap_or_default();
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
        .map(|status| (!status.is_empty()).to_string())
        .unwrap_or_default();

    println!("cargo:rustc-env=CAKE_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=CAKE_BUILD_DIRTY={dirty}");
}

fn git(args: &[&str]) -> Option<String> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let output = Command::new("git")
        .arg("-C")
        .arg(manifest)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}
