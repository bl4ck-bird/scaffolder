//! 파일 쓰기(mode·umask·심링크 방어·containment) — `PayloadStore`.

use std::ffi::OsString;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::domain::place::{safe_rel_path, DestStatus, FileMode, PayloadEntry, PayloadStore, RelPath};

/// 파일시스템 기반 `PayloadStore`: payload 읽기 + target에 containment·심링크 방어 하에 쓰기.
pub struct FsPayloadStore;

impl PayloadStore for FsPayloadStore {
    fn list_entries(&self, source_root: &Path) -> Result<Vec<PayloadEntry>> {
        let mut entries = Vec::new();
        walk(source_root, source_root, &mut entries)?;
        entries.sort_by(|a, b| a.rel.as_path().cmp(b.rel.as_path()));
        Ok(entries)
    }

    fn read_content(&self, source_root: &Path, entry: &PayloadEntry) -> Result<Vec<u8>> {
        let path = source_root.join(entry.rel.as_path());
        fs::read(&path).with_context(|| format!("failed to read payload file {}", path.display()))
    }

    fn write_file(
        &self,
        target_root: &Path,
        rel: &RelPath,
        content: &[u8],
        _mode: FileMode,
    ) -> Result<()> {
        // mode(chmod) 적용은 M3로 연기: S1은 OS 기본 생성 권한(umask 적용)에 의존한다.
        let path = target_root.join(rel.as_path());

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create parent directory for {}", path.display()))?;
        }

        // 기존 dest가 심링크면 대상을 따라 덮어쓰지 않도록 먼저 unlink한다(§1.10) — 그렇지
        // 않으면 fs::write가 심링크를 따라가 target 밖의 심링크 대상을 오염시킬 수 있다.
        match path.symlink_metadata() {
            Ok(meta) if meta.file_type().is_symlink() => {
                fs::remove_file(&path)
                    .with_context(|| format!("failed to unlink existing symlink at {}", path.display()))?;
            }
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| format!("failed to stat {}", path.display()));
            }
        }

        fs::write(&path, content).with_context(|| format!("failed to write {}", path.display()))?;

        Ok(())
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

/// 존재하는 최상위 조상까지 canonicalize(심링크 해석)한 뒤 비존재 tail 컴포넌트를 그대로
/// 재결합한다. `path` 전체가 존재하면 최종 심링크까지 포함해 통째로 해석된다(§1.10).
fn resolve_final_path(path: &Path) -> Result<PathBuf> {
    let mut existing = path.to_path_buf();
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

fn walk(source_root: &Path, dir: &Path, out: &mut Vec<PayloadEntry>) -> Result<()> {
    let read_dir =
        fs::read_dir(dir).with_context(|| format!("failed to read directory {}", dir.display()))?;

    for entry in read_dir {
        let entry = entry.with_context(|| format!("failed to read entry in {}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to stat {}", path.display()))?;

        let rel_str = path
            .strip_prefix(source_root)
            .with_context(|| format!("failed to compute relative path for {}", path.display()))?
            .to_string_lossy()
            .into_owned();
        let rel = safe_rel_path(&rel_str)?;

        if file_type.is_dir() {
            out.push(PayloadEntry {
                rel: rel.clone(),
                is_dir: true,
            });
            walk(source_root, &path, out)?;
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
    fn list_entries_enumerates_files_and_dirs_recursively() {
        let source = tempfile::tempdir().expect("tempdir");
        fs::write(source.path().join("a.txt"), b"a").expect("write a.txt");
        fs::create_dir(source.path().join("sub")).expect("mkdir sub");
        fs::write(source.path().join("sub/b.txt"), b"b").expect("write b.txt");

        let entries = FsPayloadStore.list_entries(source.path()).expect("list_entries");

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
            .write_file(target.path(), &rel, b"payload", FileMode::base())
            .expect("write_file");

        let written = fs::read(target.path().join("sub/nested/file.txt")).expect("read back");
        assert_eq!(written, b"payload");
    }

    #[test]
    fn write_file_overwrites_existing_regular_file_content() {
        let target = tempfile::tempdir().expect("tempdir");
        let rel = safe_rel_path("file.txt").unwrap();
        fs::write(target.path().join("file.txt"), b"old content").expect("seed file");

        FsPayloadStore
            .write_file(target.path(), &rel, b"new content", FileMode::base())
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
            .write_file(target.path(), &rel, b"new payload", FileMode::base())
            .expect("write_file");

        // 심링크 대상(target 밖)은 오염되지 않아야 한다.
        let external_content = fs::read(&external_file).expect("read external file");
        assert_eq!(external_content, b"untouched");

        // dest 위치는 이제 일반 파일로 교체되어 있어야 한다.
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
        assert!(!status.inside_target);
    }
}
