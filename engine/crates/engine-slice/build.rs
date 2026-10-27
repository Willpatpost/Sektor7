//! Build-time provenance for the S6-C3 soak runner: the exact git
//! commit and clean/dirty status the binary was BUILT FROM (best
//! effort — `unknown` when git is unavailable, e.g. an exported
//! source tree). This is the "built-from" record the soak summary and
//! evidence manifest carry; the hardware runbooks verify the RUNTIME
//! checkout independently (`git rev-parse HEAD` + `git status
//! --porcelain` before the run), because a cached build can outlive a
//! checkout change.

use std::path::Path;
use std::process::Command;

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn main() {
    let commit = git_output(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git_output(&["status", "--porcelain"]).is_some_and(|status| !status.is_empty());
    println!("cargo:rustc-env=S7_GIT_COMMIT={commit}");
    println!("cargo:rustc-env=S7_GIT_DIRTY={dirty}");
    // Re-run when the checkout moves so the recorded commit stays
    // honest (the path is the workspace repository's, above this
    // crate; when it does not exist — an export — the captured values
    // simply stay as they were).
    if Path::new("../../../.git/HEAD").exists() {
        println!("cargo:rerun-if-changed=../../../.git/HEAD");
    }
}
