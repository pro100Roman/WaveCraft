mod build_version;

use build_version::{classify_build_version, BuildVersionState};
use std::env;
use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=GITHUB_ACTIONS");
    println!("cargo:rerun-if-env-changed=GITHUB_REF_TYPE");
    println!("cargo:rerun-if-env-changed=GITHUB_REF_NAME");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
    println!("cargo:rerun-if-changed=../../.git/packed-refs");
    println!("cargo:rerun-if-changed=../../.git/refs/heads/main");
    println!("cargo:rerun-if-changed=../../.git/refs/remotes/origin/main");
    for path in [
        "../../.cargo",
        "../../.github",
        "../../Cargo.toml",
        "../../Cargo.lock",
        "../../README.md",
        "../../apps",
        "../../crates",
        "../../docs",
        "../../python",
        "../../resources",
        "../../scripts",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }

    let manifest_directory = env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default();
    let repository = Path::new(&manifest_directory).join("../..");
    let version = repository_version(&repository);
    println!("cargo:rustc-env=WAVECRAFT_BUILD_VERSION={version}");
}

fn ci_tag() -> Option<String> {
    let is_github_ci = env::var("GITHUB_ACTIONS").ok().as_deref() == Some("true");
    let is_tag = env::var("GITHUB_REF_TYPE").ok().as_deref() == Some("tag");
    (is_github_ci && is_tag)
        .then(|| env::var("GITHUB_REF_NAME").ok())
        .flatten()
        .filter(|tag| !tag.trim().is_empty())
}

fn repository_version(repository: &Path) -> String {
    let github_main = env::var("GITHUB_ACTIONS").ok().as_deref() == Some("true")
        && env::var("GITHUB_REF_TYPE").ok().as_deref() == Some("branch")
        && env::var("GITHUB_REF_NAME").ok().as_deref() == Some("main");
    classify_build_version(&BuildVersionState {
        ci_tag: ci_tag(),
        head: git(repository, &["rev-parse", "HEAD"]),
        short_hash: git(repository, &["rev-parse", "--short=8", "HEAD"]),
        branch: git(repository, &["symbolic-ref", "--quiet", "--short", "HEAD"]),
        origin_main: git(
            repository,
            &["rev-parse", "--verify", "refs/remotes/origin/main"],
        ),
        clean: git(
            repository,
            &["status", "--porcelain", "--untracked-files=normal"],
        )
        .is_some_and(|status| status.is_empty()),
        github_main,
    })
}

fn git(repository: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
