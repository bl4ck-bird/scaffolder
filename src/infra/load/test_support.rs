//! Shared arrange fixtures for the `infra/load` test modules: temporary template roots and the
//! control-file symlinks the loaders guard against. Kept `pub(crate)` so it never leaves infra.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

/// A fresh template root together with its canonical path — loaders compare read targets against
/// the canonical root, so tests need both the live path and its resolved form.
pub(crate) fn temp_root() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("create temp root");
    let canon = dir.path().canonicalize().expect("canonicalize temp root");
    (dir, canon)
}

/// A symlink whose target stays inside the template root (the loaders' allowed case).
pub(crate) fn link_internal(root: &Path, real_name: &str, link_name: &str, contents: &str) {
    let real = root.join(real_name);
    fs::write(&real, contents).expect("write internal symlink target");
    symlink(&real, root.join(link_name)).expect("create internal symlink");
}

/// Points `link_name` in `into` at a file in a fresh external tempdir — a symlink escaping the
/// template. The returned guard owns that tempdir; keep it bound for the target to stay valid.
pub(crate) fn link_external(into: &Path, link_name: &str, contents: &str) -> TempDir {
    let outside = TempDir::new().expect("create external dir");
    let target = outside.path().join(link_name);
    fs::write(&target, contents).expect("write external symlink target");
    symlink(&target, into.join(link_name)).expect("create external symlink");
    outside
}

/// Creates `base/name/` holding an empty `scaffold.toml` — the minimum for a store lookup to treat
/// the directory as a template.
pub(crate) fn write_template(base: &Path, name: &str) {
    let dir = base.join(name);
    fs::create_dir_all(&dir).expect("create template dir");
    fs::write(dir.join("scaffold.toml"), "").expect("write scaffold.toml");
}
