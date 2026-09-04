//! Tests for `scripts/install-binary.sh`, the Herdr plugin build step.
//!
//! The invariant under test is **artifact provenance**: the prebuilt release
//! asset is only trusted when the checkout *is* the repo that publishes it
//! (`elKei24/herdr-co-review`). A fork or otherwise-different checkout must be
//! built from source — downloading would silently install upstream's binary
//! and bypass the checked-out code (that's how a fork plugin install once ran
//! v1.8.0 code it never compiled).
//!
//! No network and no Rust toolchain: `curl` and `cargo` are stub executables
//! placed in `$HOME/.local/bin`, which the script prepends to PATH, and the
//! stub "binaries" print marker strings on `--version`.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const FORK: &str = "rudironsoni/herdr-co-review";
const UPSTREAM: &str = "elKei24/herdr-co-review";
const BUILT_MARK: &str = "STUB-BUILT-FROM-CHECKOUT";
const DOWNLOADED_MARK: &str = "STUB-DOWNLOADED-RELEASE-ASSET";

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/install-binary.sh")
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A fake plugin checkout: a git repo at `<root>/checkout` with `origin` set,
/// `bin/`, and a copy of the real `scripts/` (the build step resolves the
/// checkout root from its own path and chains into `link-on-path.sh`). With
/// `origin == None` the directory is not a git repo at all (standalone run).
fn make_checkout(root: &Path, origin: Option<&str>) -> PathBuf {
    let checkout = root.join("checkout");
    fs::create_dir_all(checkout.join("bin")).unwrap();
    let scripts_dir = checkout.join("scripts");
    fs::create_dir_all(&scripts_dir).unwrap();
    fs::copy(script(), scripts_dir.join("install-binary.sh")).unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/link-on-path.sh"),
        scripts_dir.join("link-on-path.sh"),
    )
    .unwrap();
    if let Some(url) = origin {
        git(&checkout, &["init", "-q"]);
        git(&checkout, &["remote", "add", "origin", url]);
    }
    checkout
}

/// `$HOME` with stub `curl` and `cargo` in `.local/bin` (the script prepends
/// it to PATH). `curl` "downloads" a tarball holding a binary that prints
/// [`DOWNLOADED_MARK`]; `cargo build --release` "builds" one that prints
/// [`BUILT_MARK`]. Each stub also drops a marker file proving it was invoked.
fn make_home(root: &Path) -> PathBuf {
    let home = root.join("home");
    let shim = home.join(".local/bin");
    fs::create_dir_all(&shim).unwrap();

    let asset_dir = root.join("asset");
    fs::create_dir_all(&asset_dir).unwrap();
    executable(
        &asset_dir.join("co-review"),
        &format!("#!/bin/sh\necho {DOWNLOADED_MARK}\n"),
    );
    executable(
        &shim.join("curl"),
        &format!(
            "#!/bin/sh\n\
             echo called > \"{root}/curl-called\"\n\
             for last; do :; done\n\
             tar -C \"{asset}\" -czf \"$last\" co-review\n",
            root = root.display(),
            asset = asset_dir.display()
        ),
    );
    executable(
        &shim.join("cargo"),
        &format!(
            "#!/bin/sh\n\
             echo called > \"{root}/cargo-called\"\n\
             mkdir -p target/release\n\
             printf '#!/bin/sh\\necho {BUILT_MARK}\\n' > target/release/co-review\n\
             chmod +x target/release/co-review\n",
            root = root.display()
        ),
    );
    home
}

/// Run the build step via the checkout's own copy of the script, exactly as
/// the plugin build command would. Returns (combined output, ok).
fn install(checkout: &Path, home: &Path) -> (String, bool) {
    let out = Command::new("bash")
        .arg("scripts/install-binary.sh")
        .current_dir(checkout)
        .env("HOME", home)
        .env("CO_REVIEW_NO_PATH_LINK", "1")
        .env_remove("CO_REVIEW_INSTALL_DIR")
        .output()
        .expect("spawn bash");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (text, out.status.success())
}

/// A fork checkout must be built from source: the prebuilt (upstream) release
/// asset must never be trusted for code the asset did not come from.
#[test]
fn fork_checkout_builds_instead_of_downloading() {
    let root = tempfile::tempdir().unwrap();
    let checkout = make_checkout(root.path(), Some(&format!("git@github.com:{FORK}.git")));
    let home = make_home(root.path());

    let (out, ok) = install(&checkout, &home);
    assert!(ok, "install failed: {out}");
    assert!(
        root.path().join("cargo-called").exists(),
        "a fork checkout must be built from source: {out}"
    );
    assert!(
        !root.path().join("curl-called").exists(),
        "the prebuilt asset must not be downloaded for a foreign checkout: {out}"
    );
    assert!(out.contains("building from source"), "{out}");
    assert!(out.contains(BUILT_MARK), "{out}");
    assert!(!out.contains(DOWNLOADED_MARK), "{out}");
}

/// Origin matching is URL-shape- and case-insensitive.
#[test]
fn fork_provenance_holds_across_url_shapes() {
    for url in [
        format!("https://github.com/{FORK}"),
        format!("https://github.com/{FORK}.git"),
        format!("ssh://git@github.com/{FORK}.git"),
        format!("git@github.com:{}/herdr-co-review.git", "RUDIRONSONI"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let checkout = make_checkout(root.path(), Some(&url));
        let home = make_home(root.path());
        let (out, ok) = install(&checkout, &home);
        assert!(ok, "install failed for {url}: {out}");
        assert!(
            !root.path().join("curl-called").exists(),
            "must not download for {url}: {out}"
        );
    }
}

/// The fast path stays fast: a checkout that *is* the upstream repo keeps
/// using the prebuilt release asset (no Rust toolchain required).
#[test]
fn upstream_checkout_still_downloads_the_release_asset() {
    let root = tempfile::tempdir().unwrap();
    let checkout = make_checkout(
        root.path(),
        Some(&format!("https://github.com/{UPSTREAM}.git")),
    );
    let home = make_home(root.path());

    let (out, ok) = install(&checkout, &home);
    assert!(ok, "install failed: {out}");
    assert!(out.contains(DOWNLOADED_MARK), "{out}");
    assert!(!out.contains(BUILT_MARK), "{out}");
    assert!(
        !root.path().join("cargo-called").exists(),
        "must not fall back to cargo for the upstream checkout: {out}"
    );
}

/// A non-git directory (standalone run, e.g. from a tarball) cannot prove
/// foreign provenance, so it keeps the historical download behavior.
#[test]
fn a_checkout_without_origin_keeps_the_download_path() {
    let root = tempfile::tempdir().unwrap();
    let checkout = make_checkout(root.path(), None);
    let home = make_home(root.path());

    let (out, ok) = install(&checkout, &home);
    assert!(ok, "install failed: {out}");
    assert!(out.contains(DOWNLOADED_MARK), "{out}");
}
