use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=SELA_GIT_SHA");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");

    let sha = std::env::var("SELA_GIT_SHA")
        .ok()
        .or_else(git_short_sha)
        .filter(|s| !s.is_empty());
    if let Some(sha) = sha {
        println!("cargo:rustc-env=SELA_GIT_SHA={sha}");
    }
}

fn git_short_sha() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?.trim().to_owned())
}
