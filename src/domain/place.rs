//! `RelPath`/`safe_rel_path`, `FileMode`, and the `PayloadStore` port (payload reads +
//! target writes).

use std::path::{Component, Path, PathBuf};

use anyhow::{Result, bail};

/// A normalized relative path, constructible only after literal `..` and absolute paths are rejected.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelPath(PathBuf);

impl RelPath {
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl std::fmt::Display for RelPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.display())
    }
}

/// Normalizes component-by-component and rejects literal `..` escapes and absolute paths.
pub fn safe_rel_path(input: &str) -> Result<RelPath> {
    let mut normalized = PathBuf::new();

    for component in Path::new(input).components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                bail!("path {input:?} escapes root via literal '..'")
            }
            Component::RootDir | Component::Prefix(_) => {
                bail!("absolute path {input:?} is not allowed")
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        bail!("path {input:?} is empty after normalization");
    }

    Ok(RelPath(normalized))
}

/// Lexically normalizes an absolute target path (resolving `.`/`..`, but not symlinks).
/// Settling the effective path stops a `..` input from (a) creating an unexpected empty
/// sibling directory or (b) misleading the new/existing decision. `..` never climbs above root.
pub fn normalize_target(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Unix permission bits: `0o666` base → executable `|0o111` → private `&^0o77` →
/// readonly `&^0o222`. No special bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMode(u32);

impl FileMode {
    pub fn base() -> Self {
        FileMode(0o666)
    }

    /// Applies mode prefixes to `base` (`0o666`). Prefix order in the file name is irrelevant;
    /// application uses a fixed order (executable → private → readonly) because `|0o111` and
    /// `&^0o77` do not commute, so order would change the result. Stackable.
    pub fn from_modes(modes: &[crate::domain::name::Mode]) -> Self {
        use crate::domain::name::Mode;
        let mut mode = FileMode::base();
        if modes.contains(&Mode::Executable) {
            mode = mode.with_executable();
        }
        if modes.contains(&Mode::Private) {
            mode = mode.with_private();
        }
        if modes.contains(&Mode::Readonly) {
            mode = mode.with_readonly();
        }
        mode
    }

    pub fn with_executable(self) -> Self {
        FileMode(self.0 | 0o111)
    }

    pub fn with_private(self) -> Self {
        FileMode(self.0 & !0o77)
    }

    pub fn with_readonly(self) -> Self {
        FileMode(self.0 & !0o222)
    }

    pub fn bits(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadEntry {
    pub rel: RelPath,
    pub is_dir: bool,
}

/// Result of a dest-status check (input to the write-stage confirm decision); it does not confirm itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestStatus {
    /// The actual write location after resolving symlinks.
    pub final_path: PathBuf,
    /// Whether per-component containment passed (false → external-write confirm).
    pub inside_target: bool,
    /// Whether the dest already exists (including the symlink itself).
    pub exists: bool,
    /// Whether the dest is a symlink (the link itself, not its target).
    pub is_symlink: bool,
}

/// Result of `ensure_target`: whether we created the target (cleanup candidate on failure)
/// or it pre-existed (preserved). Decided by exclusively creating the final component rather
/// than `exists()`, so there is no `..`/create-race misjudgment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetPreparation {
    /// This run created the final component → cleanup candidate on failure.
    Created,
    /// The final component already existed → never cleaned up on failure (user data).
    Existing,
}

/// Port for enumerating/reading payload and writing to the target; implemented by infra.
pub trait PayloadStore {
    fn list_entries(&self, source_root: &Path) -> Result<Vec<PayloadEntry>>;
    fn read_content(&self, source_root: &Path, entry: &PayloadEntry) -> Result<Vec<u8>>;
    /// Prepares the target directory (called after plan, just before write). Lexically
    /// normalizes the path, creates the effective parent, and exclusively creates the final
    /// component: `Created` if new, `Existing` if already present. Errors if the final
    /// location holds a non-directory (file or non-directory symlink), since we did not create it.
    fn ensure_target(&self, target_root: &Path) -> Result<TargetPreparation>;
    /// Recursively removes the exact target path the prepare step created (failure cleanup).
    /// Called only on the prepared target root, never on a rendered path.
    fn cleanup_target(&self, target_root: &Path) -> Result<()>;
    /// Atomically writes one payload file. With `overwrite` false the dest must be newly
    /// created — if a race creates it first, this fails rather than clobbering (no-clobber).
    /// With `overwrite` true it atomically replaces the existing dest.
    fn write_file(
        &self,
        target_root: &Path,
        rel: &RelPath,
        content: &[u8],
        mode: FileMode,
        overwrite: bool,
    ) -> Result<()>;
    fn dest_status(&self, target_root: &Path, rel: &RelPath) -> Result<DestStatus>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_relative_path() {
        let rel = safe_rel_path("a/b.txt").unwrap();
        assert_eq!(rel.as_path(), std::path::Path::new("a/b.txt"));
    }

    #[test]
    fn rejects_literal_parent_dir_escape() {
        assert!(safe_rel_path("../x").is_err());
    }

    #[test]
    fn rejects_absolute_path() {
        assert!(safe_rel_path("/etc/x").is_err());
    }

    #[test]
    fn rejects_literal_parent_dir_even_when_it_resolves_inside() {
        assert!(safe_rel_path("a/../b").is_err());
    }

    #[test]
    fn file_mode_computes_executable_private_readonly() {
        let base = FileMode::base();
        assert_eq!(base.bits(), 0o666);
        assert_eq!(base.with_executable().bits(), 0o777);
        assert_eq!(base.with_private().bits(), 0o600);
        assert_eq!(base.with_readonly().bits(), 0o444);
        assert_eq!(base.with_executable().with_private().bits(), 0o700);
    }

    #[test]
    fn from_modes_is_prefix_order_independent() {
        use crate::domain::name::Mode;
        // Different prefix order in the name, same result via the fixed computation order.
        let a = FileMode::from_modes(&[Mode::Executable, Mode::Private]);
        let b = FileMode::from_modes(&[Mode::Private, Mode::Executable]);
        assert_eq!(a.bits(), b.bits());
        assert_eq!(a.bits(), 0o700);
    }

    #[test]
    fn from_modes_empty_is_base() {
        assert_eq!(FileMode::from_modes(&[]).bits(), 0o666);
    }

    #[test]
    fn from_modes_all_three_stacked() {
        use crate::domain::name::Mode;
        // exec(|0o111) → private(&^0o77) → readonly(&^0o222): 0o777→0o700→0o500 (keeps execute, drops write).
        let m = FileMode::from_modes(&[Mode::Readonly, Mode::Executable, Mode::Private]);
        assert_eq!(m.bits(), 0o500);
    }

    #[test]
    fn from_modes_two_way_combos() {
        use crate::domain::name::Mode;
        // Locks the full contract (the 2-way combos among the 8 pre-umask results).
        // exec+readonly: 0o777 &^0o222 = 0o555.
        assert_eq!(
            FileMode::from_modes(&[Mode::Executable, Mode::Readonly]).bits(),
            0o555
        );
        // private+readonly: 0o600 &^0o222 = 0o400.
        assert_eq!(
            FileMode::from_modes(&[Mode::Private, Mode::Readonly]).bits(),
            0o400
        );
        // exec+private: 0o777 &^0o77 = 0o700.
        assert_eq!(
            FileMode::from_modes(&[Mode::Executable, Mode::Private]).bits(),
            0o700
        );
    }

    #[test]
    fn normalize_target_resolves_parent_and_cur_dir() {
        assert_eq!(
            normalize_target(Path::new("/tmp/new/../existing")),
            PathBuf::from("/tmp/existing")
        );
        assert_eq!(
            normalize_target(Path::new("/a/b/./c")),
            PathBuf::from("/a/b/c")
        );
        assert_eq!(normalize_target(Path::new("/a/b/..")), PathBuf::from("/a"));
        assert_eq!(normalize_target(Path::new("/a/b")), PathBuf::from("/a/b"));
    }

    #[test]
    fn normalize_target_does_not_climb_above_root() {
        assert_eq!(normalize_target(Path::new("/a/../..")), PathBuf::from("/"));
    }
}
