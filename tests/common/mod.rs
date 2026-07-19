//! Shared fixtures for the `apply` integration tests: a `Command` builder that seeds the
//! standard `apply <template> <workdir>/demo` invocation, a store-env-isolated `Command`, and a
//! declarative payload-tree builder.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use tempfile::TempDir;

/// A seeded `apply` invocation: a fresh working directory, its `demo` target, and a `Command`
/// already set to `apply <template> <workdir>/demo` with `cwd = workdir`. Tests add flags to
/// `cmd`, run it, then inspect `target`.
pub struct Apply {
    _workdir: TempDir,
    pub target: PathBuf,
    pub cmd: Command,
}

impl Apply {
    /// The working directory used as `cwd` and target parent — for tests that arrange extra files
    /// (answers files, siblings) alongside the target.
    pub fn workdir(&self) -> &Path {
        self._workdir.path()
    }
}

/// Builds an `apply <template> <workdir>/demo` command with `cwd = workdir`.
pub fn apply(template: &Path) -> Apply {
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template)
        .arg(&target);

    Apply {
        _workdir: workdir,
        target,
        cmd,
    }
}

/// A `scaffolder` command with the store lookup env isolated from the developer's real machine:
/// `SCAFFOLDER_HOME`/`XDG_CONFIG_HOME` emptied and `HOME` pointed at a throwaway directory, so an
/// ambient `~/.scaffolder` can't make a "missing template" case unexpectedly resolve.
pub fn scaffolder(fake_home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.env("SCAFFOLDER_HOME", "")
        .env("XDG_CONFIG_HOME", "")
        .env("HOME", fake_home);
    cmd
}

/// Materializes a set of `(relative path, contents)` entries under `root`, creating parent
/// directories as needed.
pub fn build_tree(root: &Path, entries: &[(&str, &str)]) {
    for (rel, contents) in entries {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|e| panic!("mkdir parent of {}: {e}", path.display()));
        }
        std::fs::write(&path, contents).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }
}
