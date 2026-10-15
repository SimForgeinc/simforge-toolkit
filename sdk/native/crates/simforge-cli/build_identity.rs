// Build provenance shared by the build scripts of `simforge-bindings-common`
// (the engine every binding links) and `simforge-cli` (`include!`d by both,
// so the rule is one). Emits, for the crate being built:
//
// - `SIMFORGE_BUILD_SOURCE_REVISION`: the commit (`<sha>`, `<sha>-dirty`, or
//   `unknown`); `SIMFORGE_SOURCE_REVISION` set by a build wins over git;
// - `SIMFORGE_BUILD_RELEASE`: `1` when the build is a release
//   (`SIMFORGE_RELEASE_BUILD=1`, set by the release process), else `0`;
// - `SIMFORGE_BUILD_PEP440_LOCAL` / `SIMFORGE_BUILD_SEMVER_META`: the local
//   version label of a non-release build (`g<sha12>[.dirty]` for PEP 440,
//   `<sha12>[.dirty]` for semver build metadata; `unknown` without a
//   commit), empty for a release, which keeps its clean version.
//
// Provenance only: no cache key, digest or compatibility check reads these.

use std::path::Path;
use std::process::Command;

fn run(cmd: &str, args: &[&str], dir: &Path) -> Option<String> {
    let out = Command::new(cmd)
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn rerun_on_git_path(dir: &Path, path: &str) {
    if let Some(p) = run("git", &["rev-parse", "--git-path", path], dir) {
        let p = Path::new(&p);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            dir.join(p)
        };
        println!("cargo:rerun-if-changed={}", abs.display());
    }
}

/// The commit `dir` is built from; `sdk_root`: the tree whose uncommitted changes make it dirty.
fn source_revision(dir: &Path, sdk_root: &str) -> String {
    println!("cargo:rerun-if-env-changed=SIMFORGE_SOURCE_REVISION");
    std::env::var("SIMFORGE_SOURCE_REVISION")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| {
            let head = run("git", &["rev-parse", "HEAD"], dir)?;
            // Re-run when HEAD moves (branch switch or new commit).
            rerun_on_git_path(dir, "HEAD");
            rerun_on_git_path(dir, "index");
            if let Some(reference) = run("git", &["symbolic-ref", "-q", "HEAD"], dir) {
                rerun_on_git_path(dir, &reference);
            }
            let dirty = run(
                "git",
                &[
                    "status",
                    "--porcelain",
                    "--untracked-files=no",
                    "--",
                    sdk_root,
                ],
                dir,
            )
            .map(|s| !s.is_empty())
            .unwrap_or(false);
            Some(if dirty { format!("{head}-dirty") } else { head })
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Emit the build provenance of the crate at `CARGO_MANIFEST_DIR` (module docs).
fn emit_build_identity(sdk_root: &str) {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let dir = Path::new(&manifest);
    let revision = source_revision(dir, sdk_root);
    println!("cargo:rerun-if-env-changed=SIMFORGE_RELEASE_BUILD");
    let release = std::env::var("SIMFORGE_RELEASE_BUILD").is_ok_and(|v| v == "1");
    let (pep440, semver) = if release {
        (String::new(), String::new())
    } else {
        let (sha, dirty) = match revision.strip_suffix("-dirty") {
            Some(sha) => (sha, true),
            None => (revision.as_str(), false),
        };
        if sha == "unknown" {
            ("unknown".to_owned(), "unknown".to_owned())
        } else {
            let short: String = sha.chars().take(12).collect();
            let dot_dirty = if dirty { ".dirty" } else { "" };
            (
                format!("g{short}{dot_dirty}"),
                format!("{short}{dot_dirty}"),
            )
        }
    };
    println!("cargo:rustc-env=SIMFORGE_BUILD_SOURCE_REVISION={revision}");
    println!(
        "cargo:rustc-env=SIMFORGE_BUILD_RELEASE={}",
        u8::from(release)
    );
    println!("cargo:rustc-env=SIMFORGE_BUILD_PEP440_LOCAL={pep440}");
    println!("cargo:rustc-env=SIMFORGE_BUILD_SEMVER_META={semver}");
}
