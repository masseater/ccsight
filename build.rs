//! Build script whose only job is to point `core.hooksPath` at the repo's
//! tracked hooks on the first `cargo build` from a source checkout — the same
//! thing `scripts/install-hooks.sh` does, minus the "remember to run it" step.
//! Without this the hook path stays at the default `.git/hooks` (which holds
//! nothing), so the pre-commit / pre-push gates silently never fire.
//!
//! Safe in every other context: it no-ops outside a git work tree (e.g. a
//! crate unpacked by `cargo install` from the registry has no `.git`), never
//! fails the build, and skips when `CCSIGHT_NO_HOOK_INSTALL` is set so a
//! contributor running their own hook setup isn't fought on every build.

use std::process::Command;

fn main() {
    // The hook path is a one-time, idempotent setup; re-running on unrelated
    // source edits just wastes a few git calls. Pin reruns to this file.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CCSIGHT_NO_HOOK_INSTALL");

    if std::env::var_os("CCSIGHT_NO_HOOK_INSTALL").is_some() {
        return;
    }

    // No tracked hooks here (packaged registry crate) → nothing to install.
    if !std::path::Path::new("scripts/hooks").is_dir() {
        return;
    }

    // Must be a git work tree. `cargo install ccsight` builds an unpacked
    // tarball with no `.git`, where this command fails or prints "false".
    let in_work_tree = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "true");
    if !in_work_tree {
        return;
    }

    // Already pointed at the tracked hooks → nothing to do (and don't re-warn).
    let current = Command::new("git")
        .args(["config", "--get", "core.hooksPath"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if current == "scripts/hooks" {
        return;
    }

    // Set it for any other value — unset, the default `.git/hooks`, or stale.
    // Mirrors install-hooks.sh's unconditional intent: this repo's hooks live
    // in scripts/hooks. Silent on purpose — `cargo:warning` would replay on
    // every build until this script reruns, nagging long after the one-time
    // setup. Best-effort; a git failure here must never break the build.
    // Named binding (not bare `_`) keeps clippy's let_underscore_must_use quiet.
    let _set = Command::new("git")
        .args(["config", "core.hooksPath", "scripts/hooks"])
        .output();
}
