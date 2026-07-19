//! File writes (mode, umask, symlink defense, containment) — `PayloadStore`.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow, bail};

use crate::domain::place::{
    DestStatus, FileMode, PayloadEntry, PayloadStore, RelPath, TargetPreparation, normalize_target,
    safe_rel_path,
};

/// Filesystem `PayloadStore`: reads payload and writes to the target under containment and symlink defense.
pub struct FsPayloadStore;

impl PayloadStore for FsPayloadStore {
    fn list_entries(&self, source_root: &Path) -> Result<Vec<PayloadEntry>> {
        let mut entries = Vec::new();
        let canonical_root = source_root.canonicalize().with_context(|| {
            format!(
                "payload source root {} does not exist",
                source_root.display()
            )
        })?;
        let mut visited = HashSet::new();
        visited.insert(canonical_root.clone());
        walk(
            source_root,
            source_root,
            &canonical_root,
            &mut visited,
            &mut entries,
            0,
        )?;
        entries.sort_by(|a, b| a.rel.as_path().cmp(b.rel.as_path()));
        Ok(entries)
    }

    fn read_content(&self, source_root: &Path, entry: &PayloadEntry) -> Result<Vec<u8>> {
        let path = source_root.join(entry.rel.as_path());
        // A symlink may have been swapped to point outside between the walk and this read, so
        // re-verify containment just before reading and read from the canonical path to avoid
        // re-following symlinks (narrows the source-side gap).
        let canonical_root = source_root.canonicalize().with_context(|| {
            format!(
                "payload source root {} does not exist",
                source_root.display()
            )
        })?;
        let canonical = path
            .canonicalize()
            .with_context(|| format!("failed to resolve payload file {}", path.display()))?;
        if !canonical.starts_with(&canonical_root) {
            bail!(
                "payload file {} resolves outside the source root",
                path.display()
            );
        }
        fs::read(&canonical)
            .with_context(|| format!("failed to read payload file {}", path.display()))
    }

    fn ensure_target(&self, target_root: &Path) -> Result<TargetPreparation> {
        // Lexically normalize to settle the effective target, then prepare only its parent and
        // create the final component exclusively. An exists()+create_dir_all approach misjudges
        // `..` paths and create-races as new and could delete pre-existing user data (exclusive
        // create_dir prevents that).
        let effective = normalize_target(target_root);
        if let Some(parent) = effective.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create parent of target {}", effective.display())
            })?;
        }
        match fs::create_dir(&effective) {
            Ok(()) => Ok(TargetPreparation::Created),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                // What already exists must be a real directory (or a symlink to one) to be usable
                // as the target. `metadata` follows symlinks, so a broken/non-directory symlink
                // errors. Either way we did not create it, so it is not a cleanup candidate.
                let meta = fs::metadata(&effective).with_context(|| {
                    format!(
                        "target {} exists but could not be inspected",
                        effective.display()
                    )
                })?;
                if meta.is_dir() {
                    Ok(TargetPreparation::Existing)
                } else {
                    bail!(
                        "target {} exists and is not a directory",
                        effective.display()
                    )
                }
            }
            Err(e) => Err(anyhow!(e)).with_context(|| {
                format!("failed to create target directory {}", effective.display())
            }),
        }
    }

    fn cleanup_target(&self, target_root: &Path) -> Result<()> {
        // Delete only the effective path, normalized the same as ensure_target — called on the
        // prepared target root, not a rendered path (the pipeline caller only invokes it when Created).
        let effective = normalize_target(target_root);
        fs::remove_dir_all(&effective).with_context(|| {
            format!(
                "failed to clean up target directory {}",
                effective.display()
            )
        })
    }

    fn write_file(
        &self,
        target_root: &Path,
        rel: &RelPath,
        content: &[u8],
        mode: FileMode,
        overwrite: bool,
    ) -> Result<()> {
        let path = target_root.join(rel.as_path());

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create parent directory for {}", path.display())
            })?;
        }

        atomic_write(&path, content, mode, overwrite)
    }

    fn dest_status(&self, target_root: &Path, rel: &RelPath) -> Result<DestStatus> {
        let final_intended = target_root.join(rel.as_path());
        let canonical_target = target_root
            .canonicalize()
            .with_context(|| format!("target root {} does not exist", target_root.display()))?;

        let final_path = resolve_final_path(&final_intended)?;
        let inside_target = final_path.starts_with(&canonical_target);

        let (exists, is_symlink) = match final_intended.symlink_metadata() {
            Ok(meta) => (true, meta.file_type().is_symlink()),
            Err(e) if e.kind() == ErrorKind::NotFound => (false, false),
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("failed to stat {}", final_intended.display()));
            }
        };

        Ok(DestStatus {
            final_path,
            inside_target,
            exists,
            is_symlink,
        })
    }
}

/// Process-local counter for the unique temp-file suffix.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
const MAX_TEMP_ATTEMPTS: u32 = 32;
/// Recursion depth cap for the payload tree (a backstop against symlink aliases / pathological depth). Real templates are much shallower.
const MAX_WALK_DEPTH: u32 = 64;

/// Writes content to a temp file in the same parent directory, then atomically replaces dest via `rename`.
///
/// - The temp is in dest's directory (same filesystem), so `rename` is atomic.
/// - `create_new` (O_EXCL) does not follow symlinks and only creates a new file.
/// - The mode is set at creation via `mode`, with the OS applying umask, so a private file appears
///   at its final location with correct permissions from the start — no wrong-permission window.
/// - `rename` atomically replaces even an existing dest symlink (the link itself, not its target) → no out-of-target contamination.
/// - No partial output: on failure the temp is cleaned up and dest keeps its prior state.
#[cfg(unix)]
fn atomic_write(dest: &Path, content: &[u8], mode: FileMode, overwrite: bool) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let parent = dest
        .parent()
        .ok_or_else(|| anyhow!("destination {} has no parent directory", dest.display()))?;
    let file_name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("destination {} has no valid file name", dest.display()))?;

    let mut last_exists_err = None;
    for _ in 0..MAX_TEMP_ATTEMPTS {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_path = parent.join(format!(".{file_name}.{}.{counter}.tmp", std::process::id()));

        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode.bits())
            .open(&temp_path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                last_exists_err = Some(e);
                continue;
            }
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("failed to create temp file for {}", dest.display()));
            }
        };

        if let Err(e) = file.write_all(content).and_then(|()| file.sync_all()) {
            let _ = fs::remove_file(&temp_path);
            return Err(e)
                .with_context(|| format!("failed to write temp file for {}", dest.display()));
        }
        drop(file);

        let placed = if overwrite {
            // Atomically replace the existing dest (if a symlink, replace the link itself).
            fs::rename(&temp_path, dest)
        } else {
            // The dest must be newly created. `hard_link` fails with EEXIST if it already exists,
            // so it won't silently clobber a file a race created after plan. Remove the temp link either way.
            let result = fs::hard_link(&temp_path, dest);
            let _ = fs::remove_file(&temp_path);
            result
        };
        if let Err(e) = placed {
            // On overwrite(rename) failure the temp remains, so clean it up. Non-overwrite is already removed above.
            if overwrite {
                let _ = fs::remove_file(&temp_path);
            }
            return Err(e).with_context(|| format!("failed to place {}", dest.display()));
        }
        return Ok(());
    }

    Err(anyhow!(
        "failed to create a unique temp file for {} after {MAX_TEMP_ATTEMPTS} attempts: {:?}",
        dest.display(),
        last_exists_err
    ))
}

#[cfg(not(unix))]
fn atomic_write(dest: &Path, content: &[u8], _mode: FileMode, _overwrite: bool) -> Result<()> {
    // Non-Unix is a BLUEPRINT non-goal. Best-effort (no mode/atomicity guarantee).
    fs::write(dest, content).with_context(|| format!("failed to write {}", dest.display()))
}

/// Resolves the final write location. The final component is **not dereferenced** (atomic_write
/// replaces it in place); only the parent (intermediate components) is symlink-resolved, then the
/// final basename is appended. So even a final-component symlink pointing outside is judged inside
/// the target and treated as an overwrite (in-place replace) — only intermediate-component symlinks
/// count as external writes.
fn resolve_final_path(path: &Path) -> Result<PathBuf> {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(file_name)) => {
            let resolved_parent = resolve_existing_ancestor(parent)?;
            Ok(resolved_parent.join(file_name))
        }
        _ => Ok(path.to_path_buf()),
    }
}

/// Canonicalizes (symlink-resolves) up to the nearest existing ancestor, then reattaches the
/// non-existent tail components. For directory-path resolution only (intermediate-component symlinks are followed).
fn resolve_existing_ancestor(dir: &Path) -> Result<PathBuf> {
    let mut existing = dir.to_path_buf();
    let mut tail: Vec<OsString> = Vec::new();

    while !existing.exists() {
        match existing.file_name() {
            Some(name) => {
                tail.push(name.to_os_string());
                existing.pop();
            }
            None => break,
        }
    }

    let mut resolved = existing
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", existing.display()))?;

    for name in tail.into_iter().rev() {
        resolved.push(name);
    }

    Ok(resolved)
}

/// Enumerates the payload. Symlinks pointing inside the target (= inside the source root) are
/// dereferenced: directory symlinks recurse, file symlinks read the target's content
/// (`read_content`'s `fs::read` follows). Symlinks pointing outside the source root are rejected as
/// external content ingress (fail-loud). Cycles from directory symlinks are detected via canonical
/// path tracking and error out.
fn walk(
    source_root: &Path,
    dir: &Path,
    canonical_root: &Path,
    visited: &mut HashSet<PathBuf>,
    out: &mut Vec<PayloadEntry>,
    depth: u32,
) -> Result<()> {
    // Backstop against a pathologically deep (or symlink-alias-inflated) tree.
    if depth > MAX_WALK_DEPTH {
        bail!(
            "payload tree exceeds max depth {MAX_WALK_DEPTH} at {}",
            dir.display()
        );
    }
    let read_dir =
        fs::read_dir(dir).with_context(|| format!("failed to read directory {}", dir.display()))?;

    for entry in read_dir {
        let entry = entry.with_context(|| format!("failed to read entry in {}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to stat {}", path.display()))?;

        // File names are used as UTF-8 in render/output paths, so a non-UTF8 path is rejected
        // (fail-loud) rather than lossily converted (which could collide with another name).
        let rel_str = path
            .strip_prefix(source_root)
            .with_context(|| format!("failed to compute relative path for {}", path.display()))?
            .to_str()
            .ok_or_else(|| anyhow!("payload path {} is not valid UTF-8", path.display()))?
            .to_string();
        let rel = safe_rel_path(&rel_str)?;

        if file_type.is_symlink() {
            // Resolve a symlink's final location canonically to decide if it is inside the source root.
            let canonical_target = path
                .canonicalize()
                .with_context(|| format!("failed to resolve payload symlink {}", path.display()))?;
            if !canonical_target.starts_with(canonical_root) {
                bail!(
                    "payload symlink {} points outside the source root",
                    path.display()
                );
            }
            let target_meta = fs::metadata(&path)
                .with_context(|| format!("failed to stat symlink target for {}", path.display()))?;
            if target_meta.is_dir() {
                if !visited.insert(canonical_target.clone()) {
                    bail!("payload symlink cycle detected at {}", path.display());
                }
                out.push(PayloadEntry {
                    rel: rel.clone(),
                    is_dir: true,
                });
                walk(source_root, &path, canonical_root, visited, out, depth + 1)?;
                visited.remove(&canonical_target);
            } else {
                out.push(PayloadEntry { rel, is_dir: false });
            }
        } else if file_type.is_dir() {
            out.push(PayloadEntry {
                rel: rel.clone(),
                is_dir: true,
            });
            walk(source_root, &path, canonical_root, visited, out, depth + 1)?;
        } else {
            out.push(PayloadEntry { rel, is_dir: false });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn ensure_target_reports_created_for_new_dir() {
        let base = tempfile::tempdir().expect("tempdir");
        let target = base.path().join("newproj");
        let prep = FsPayloadStore.ensure_target(&target).expect("ensure");
        assert_eq!(prep, TargetPreparation::Created);
        assert!(target.is_dir());
    }

    #[test]
    fn ensure_target_reports_existing_and_preserves_contents() {
        let base = tempfile::tempdir().expect("tempdir");
        let target = base.path().join("existing");
        fs::create_dir(&target).expect("mkdir");
        fs::write(target.join("sentinel"), b"keep").expect("sentinel");
        let prep = FsPayloadStore.ensure_target(&target).expect("ensure");
        assert_eq!(prep, TargetPreparation::Existing);
        assert_eq!(fs::read(target.join("sentinel")).unwrap(), b"keep");
    }

    #[test]
    fn ensure_target_normalizes_dots_and_does_not_create_sibling() {
        // In base/missing/../preexisting only preexisting exists → effective target=base/preexisting=Existing;
        // the sibling base/missing must not be created.
        let base = tempfile::tempdir().expect("tempdir");
        fs::create_dir(base.path().join("preexisting")).expect("mkdir preexisting");
        let target = base.path().join("missing").join("..").join("preexisting");
        let prep = FsPayloadStore.ensure_target(&target).expect("ensure");
        assert_eq!(prep, TargetPreparation::Existing);
        assert!(
            !base.path().join("missing").exists(),
            "sibling must not be created"
        );
    }

    #[test]
    fn ensure_target_errors_when_final_component_is_a_file_and_keeps_it() {
        let base = tempfile::tempdir().expect("tempdir");
        let target = base.path().join("afile");
        fs::write(&target, b"data").expect("write file");
        let result = FsPayloadStore.ensure_target(&target);
        assert!(result.is_err(), "file at target must be an error");
        assert!(target.is_file(), "the file must not be deleted");
        assert_eq!(
            fs::read(&target).expect("file survives"),
            b"data",
            "file contents intact"
        );
    }

    #[test]
    fn ensure_target_errors_on_file_symlink_and_keeps_it() {
        // Final component is a symlink to a file → create_dir AlreadyExists, metadata(follow) is a
        // file → error. Both the symlink and its target file are preserved (we did not create them).
        let base = tempfile::tempdir().expect("tempdir");
        let real = base.path().join("real_file");
        fs::write(&real, b"data").expect("write real file");
        let target = base.path().join("link");
        symlink(&real, &target).expect("file symlink");
        let result = FsPayloadStore.ensure_target(&target);
        assert!(result.is_err(), "symlink to a file must be a prepare error");
        assert!(
            target.symlink_metadata().is_ok(),
            "the symlink must not be deleted"
        );
        assert_eq!(fs::read(&real).expect("target file survives"), b"data");
    }

    #[test]
    fn ensure_target_errors_on_broken_symlink_and_keeps_it() {
        // broken symlink → create_dir AlreadyExists, metadata(follow) is ENOENT → error. Preserved.
        let base = tempfile::tempdir().expect("tempdir");
        let target = base.path().join("broken");
        symlink(base.path().join("nonexistent"), &target).expect("broken symlink");
        let result = FsPayloadStore.ensure_target(&target);
        assert!(result.is_err(), "broken symlink must be a prepare error");
        assert!(
            target.symlink_metadata().is_ok(),
            "the broken symlink must not be deleted"
        );
    }

    #[test]
    fn ensure_target_reports_existing_for_directory_symlink_and_preserves_contents() {
        // Symlink to a directory → AlreadyExists, metadata(follow) is_dir → Existing.
        // Not a cleanup candidate, so the symlink target directory's contents are preserved.
        let base = tempfile::tempdir().expect("tempdir");
        let real = base.path().join("real_dir");
        fs::create_dir(&real).expect("mkdir real dir");
        fs::write(real.join("sentinel"), b"keep").expect("sentinel");
        let target = base.path().join("dirlink");
        symlink(&real, &target).expect("dir symlink");
        let prep = FsPayloadStore
            .ensure_target(&target)
            .expect("dir symlink resolves to Existing");
        assert_eq!(prep, TargetPreparation::Existing);
        assert_eq!(
            fs::read(real.join("sentinel")).expect("contents survive"),
            b"keep"
        );
    }

    #[test]
    fn cleanup_target_removes_exact_prepared_path() {
        let base = tempfile::tempdir().expect("tempdir");
        let target = base.path().join("proj");
        fs::create_dir(&target).expect("mkdir");
        fs::write(target.join("f"), b"x").expect("write");
        FsPayloadStore.cleanup_target(&target).expect("cleanup");
        assert!(!target.exists());
    }

    #[test]
    fn cleanup_target_leaves_parent_and_sibling_untouched() {
        // Cleanup is confined to the exact prepared root — the parent file and sibling directory sentinel are untouched.
        let base = tempfile::tempdir().expect("tempdir");
        let target = base.path().join("proj");
        fs::create_dir(&target).expect("mkdir target");
        fs::write(target.join("f"), b"x").expect("write in target");
        fs::write(base.path().join("parent_sentinel"), b"parent").expect("parent sentinel");
        let sibling = base.path().join("sibling");
        fs::create_dir(&sibling).expect("mkdir sibling");
        fs::write(sibling.join("s"), b"sib").expect("sibling sentinel");

        FsPayloadStore.cleanup_target(&target).expect("cleanup");

        assert!(!target.exists(), "target removed");
        assert_eq!(
            fs::read(base.path().join("parent_sentinel")).expect("parent survives"),
            b"parent"
        );
        assert_eq!(
            fs::read(sibling.join("s")).expect("sibling survives"),
            b"sib"
        );
    }

    #[test]
    fn cleanup_target_does_not_follow_symlink_to_external_dir() {
        // Even with a symlink to an external directory inside the target, remove_dir_all does not
        // follow it (only unlinks), so the external target is preserved.
        let base = tempfile::tempdir().expect("tempdir");
        let external = base.path().join("external");
        fs::create_dir(&external).expect("mkdir external");
        fs::write(external.join("keep"), b"external-data").expect("external sentinel");
        let target = base.path().join("proj");
        fs::create_dir(&target).expect("mkdir target");
        symlink(&external, target.join("link")).expect("symlink to external dir");

        FsPayloadStore.cleanup_target(&target).expect("cleanup");

        assert!(!target.exists(), "target removed");
        assert_eq!(
            fs::read(external.join("keep")).expect("external survives"),
            b"external-data",
            "cleanup must not follow the symlink into the external directory"
        );
    }

    #[test]
    fn list_entries_enumerates_files_and_dirs_recursively() {
        let source = tempfile::tempdir().expect("tempdir");
        fs::write(source.path().join("a.txt"), b"a").expect("write a.txt");
        fs::create_dir(source.path().join("sub")).expect("mkdir sub");
        fs::write(source.path().join("sub/b.txt"), b"b").expect("write b.txt");

        let entries = FsPayloadStore
            .list_entries(source.path())
            .expect("list_entries");

        let rels: Vec<String> = entries.iter().map(|e| e.rel.to_string()).collect();
        assert!(rels.contains(&"a.txt".to_string()));
        assert!(rels.contains(&"sub".to_string()));
        assert!(rels.contains(&"sub/b.txt".to_string()));

        let sub_entry = entries.iter().find(|e| e.rel.to_string() == "sub").unwrap();
        assert!(sub_entry.is_dir);
        let b_entry = entries
            .iter()
            .find(|e| e.rel.to_string() == "sub/b.txt")
            .unwrap();
        assert!(!b_entry.is_dir);
    }

    #[test]
    fn list_entries_dereferences_inside_directory_symlink() {
        let source = tempfile::tempdir().expect("tempdir");
        fs::create_dir(source.path().join("real")).expect("mkdir real");
        fs::write(source.path().join("real/x.txt"), b"x").expect("write x");
        symlink(source.path().join("real"), source.path().join("link")).expect("symlink dir");

        let entries = FsPayloadStore
            .list_entries(source.path())
            .expect("list_entries");
        let rels: Vec<String> = entries.iter().map(|e| e.rel.to_string()).collect();

        // The symlink directory is dereferenced and its contents are enumerated.
        assert!(rels.contains(&"link".to_string()));
        assert!(rels.contains(&"link/x.txt".to_string()));
        let link_entry = entries
            .iter()
            .find(|e| e.rel.to_string() == "link")
            .unwrap();
        assert!(link_entry.is_dir);
    }

    #[test]
    fn list_entries_reads_inside_file_symlink_content() {
        let source = tempfile::tempdir().expect("tempdir");
        fs::write(source.path().join("real.txt"), b"real-content").expect("write real");
        symlink(
            source.path().join("real.txt"),
            source.path().join("link.txt"),
        )
        .expect("symlink");

        let entries = FsPayloadStore
            .list_entries(source.path())
            .expect("list_entries");
        let link = entries
            .iter()
            .find(|e| e.rel.to_string() == "link.txt")
            .unwrap();
        assert!(!link.is_dir);
        let content = FsPayloadStore
            .read_content(source.path(), link)
            .expect("read");
        assert_eq!(content, b"real-content");
    }

    #[test]
    fn list_entries_rejects_symlink_pointing_outside_source_root() {
        let source = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("outside");
        fs::write(outside.path().join("secret"), b"s").expect("write outside");
        symlink(outside.path().join("secret"), source.path().join("leak")).expect("symlink out");

        let result = FsPayloadStore.list_entries(source.path());
        assert!(result.is_err(), "external payload symlink must be rejected");
    }

    #[test]
    fn list_entries_allows_noncyclic_diamond_symlinks() {
        // Two symlinks point to the same real dir but it is not a cycle — must not be misflagged as one.
        let source = tempfile::tempdir().expect("tempdir");
        fs::create_dir(source.path().join("real")).expect("mkdir real");
        fs::write(source.path().join("real/x.txt"), b"x").expect("write x");
        symlink(source.path().join("real"), source.path().join("link_a")).expect("link_a");
        symlink(source.path().join("real"), source.path().join("link_b")).expect("link_b");

        let entries = FsPayloadStore
            .list_entries(source.path())
            .expect("list_entries");
        let rels: Vec<String> = entries.iter().map(|e| e.rel.to_string()).collect();
        assert!(rels.contains(&"link_a/x.txt".to_string()));
        assert!(rels.contains(&"link_b/x.txt".to_string()));
    }

    #[test]
    fn list_entries_errors_on_broken_payload_symlink() {
        let source = tempfile::tempdir().expect("tempdir");
        symlink(
            source.path().join("missing"),
            source.path().join("dangling"),
        )
        .expect("symlink");
        let result = FsPayloadStore.list_entries(source.path());
        assert!(result.is_err(), "broken payload symlink must fail loud");
    }

    #[test]
    fn list_entries_detects_directory_symlink_cycle() {
        let source = tempfile::tempdir().expect("tempdir");
        fs::create_dir(source.path().join("sub")).expect("mkdir sub");
        // sub/loop → points at the source root (an ancestor), forming a cycle.
        symlink(source.path(), source.path().join("sub/loop")).expect("symlink cycle");

        let result = FsPayloadStore.list_entries(source.path());
        assert!(result.is_err(), "directory symlink cycle must be detected");
    }

    #[test]
    fn read_content_returns_verbatim_bytes() {
        let source = tempfile::tempdir().expect("tempdir");
        fs::write(source.path().join("a.txt"), b"hello world").expect("write a.txt");
        let entry = PayloadEntry {
            rel: safe_rel_path("a.txt").unwrap(),
            is_dir: false,
        };

        let content = FsPayloadStore
            .read_content(source.path(), &entry)
            .expect("read_content");

        assert_eq!(content, b"hello world");
    }

    #[test]
    fn write_file_writes_verbatim_content_and_creates_parent_dirs() {
        let target = tempfile::tempdir().expect("tempdir");
        let rel = safe_rel_path("sub/nested/file.txt").unwrap();

        FsPayloadStore
            .write_file(target.path(), &rel, b"payload", FileMode::base(), false)
            .expect("write_file");

        let written = fs::read(target.path().join("sub/nested/file.txt")).expect("read back");
        assert_eq!(written, b"payload");
    }

    #[test]
    fn write_file_no_overwrite_fails_when_dest_exists() {
        let target = tempfile::tempdir().expect("tempdir");
        let rel = safe_rel_path("file.txt").unwrap();
        fs::write(target.path().join("file.txt"), b"pre-existing").expect("seed");

        // With overwrite=false and dest already present (e.g. a race after plan), fail rather than silently clobber.
        let result =
            FsPayloadStore.write_file(target.path(), &rel, b"new", FileMode::base(), false);
        assert!(
            result.is_err(),
            "no-clobber create must fail when dest exists"
        );

        // Existing content is preserved.
        assert_eq!(
            fs::read(target.path().join("file.txt")).unwrap(),
            b"pre-existing"
        );
        // No temp file remains.
        let leftover = fs::read_dir(target.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().ends_with(".tmp"));
        assert!(
            !leftover,
            "no temp file should remain after a failed no-clobber write"
        );
    }

    #[test]
    fn write_file_overwrites_existing_regular_file_content() {
        let target = tempfile::tempdir().expect("tempdir");
        let rel = safe_rel_path("file.txt").unwrap();
        fs::write(target.path().join("file.txt"), b"old content").expect("seed file");

        FsPayloadStore
            .write_file(target.path(), &rel, b"new content", FileMode::base(), true)
            .expect("write_file");

        let written = fs::read(target.path().join("file.txt")).expect("read back");
        assert_eq!(written, b"new content");
    }

    #[test]
    fn write_file_unlinks_existing_symlink_instead_of_following_it() {
        let target = tempfile::tempdir().expect("target tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        let external_file = outside.path().join("secret.txt");
        fs::write(&external_file, b"untouched").expect("seed external file");

        let link_path = target.path().join("link.txt");
        symlink(&external_file, &link_path).expect("create symlink");

        let rel = safe_rel_path("link.txt").unwrap();
        FsPayloadStore
            .write_file(target.path(), &rel, b"new payload", FileMode::base(), true)
            .expect("write_file");

        // The symlink target (outside target) must not be contaminated.
        let external_content = fs::read(&external_file).expect("read external file");
        assert_eq!(external_content, b"untouched");

        // The dest location must now be replaced by a regular file.
        let dest_meta = fs::symlink_metadata(&link_path).expect("stat dest");
        assert!(!dest_meta.file_type().is_symlink());
        let dest_content = fs::read(&link_path).expect("read dest");
        assert_eq!(dest_content, b"new payload");
    }

    #[test]
    fn dest_status_reports_inside_target_for_plain_rel_path() {
        let target = tempfile::tempdir().expect("tempdir");
        let rel = safe_rel_path("a/b.txt").unwrap();

        let status = FsPayloadStore
            .dest_status(target.path(), &rel)
            .expect("dest_status");

        assert!(status.inside_target);
        assert!(!status.exists);
        assert!(!status.is_symlink);
    }

    #[test]
    fn dest_status_reports_outside_target_through_symlinked_directory() {
        let target = tempfile::tempdir().expect("target tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");

        let link_dir = target.path().join("escape");
        symlink(outside.path(), &link_dir).expect("create dir symlink");

        let rel = safe_rel_path("escape/child.txt").unwrap();
        let status = FsPayloadStore
            .dest_status(target.path(), &rel)
            .expect("dest_status");

        assert!(!status.inside_target);
        let expected_outside = outside.path().canonicalize().unwrap().join("child.txt");
        assert_eq!(status.final_path, expected_outside);
    }

    #[test]
    fn dest_status_reports_existing_symlink_dest() {
        let target = tempfile::tempdir().expect("target tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        let external_file = outside.path().join("secret.txt");
        fs::write(&external_file, b"x").expect("seed external file");

        let link_path = target.path().join("link.txt");
        symlink(&external_file, &link_path).expect("create symlink");

        let rel = safe_rel_path("link.txt").unwrap();
        let status = FsPayloadStore
            .dest_status(target.path(), &rel)
            .expect("dest_status");

        assert!(status.exists);
        assert!(status.is_symlink);
        // A final-component symlink is replaced in place, so it is judged inside the target
        // (overwrite) — even pointing outside, rename replaces the link itself, not its target.
        assert!(status.inside_target);
        assert_eq!(
            status.final_path,
            target.path().canonicalize().unwrap().join("link.txt")
        );
    }
}
