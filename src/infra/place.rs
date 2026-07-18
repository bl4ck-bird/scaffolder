//! 파일 쓰기(mode·umask·심링크 방어·containment) — `PayloadStore`.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{anyhow, bail, Context, Result};

use crate::domain::place::{safe_rel_path, DestStatus, FileMode, PayloadEntry, PayloadStore, RelPath};

/// 파일시스템 기반 `PayloadStore`: payload 읽기 + target에 containment·심링크 방어 하에 쓰기.
pub struct FsPayloadStore;

impl PayloadStore for FsPayloadStore {
    fn list_entries(&self, source_root: &Path) -> Result<Vec<PayloadEntry>> {
        let mut entries = Vec::new();
        let canonical_root = source_root
            .canonicalize()
            .with_context(|| format!("payload source root {} does not exist", source_root.display()))?;
        let mut visited = HashSet::new();
        visited.insert(canonical_root.clone());
        walk(source_root, source_root, &canonical_root, &mut visited, &mut entries)?;
        entries.sort_by(|a, b| a.rel.as_path().cmp(b.rel.as_path()));
        Ok(entries)
    }

    fn read_content(&self, source_root: &Path, entry: &PayloadEntry) -> Result<Vec<u8>> {
        let path = source_root.join(entry.rel.as_path());
        fs::read(&path).with_context(|| format!("failed to read payload file {}", path.display()))
    }

    fn ensure_target(&self, target_root: &Path) -> Result<()> {
        fs::create_dir_all(target_root)
            .with_context(|| format!("failed to create target directory {}", target_root.display()))
    }

    fn write_file(
        &self,
        target_root: &Path,
        rel: &RelPath,
        content: &[u8],
        mode: FileMode,
    ) -> Result<()> {
        let path = target_root.join(rel.as_path());

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create parent directory for {}", path.display()))?;
        }

        atomic_write(&path, content, mode)
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

/// temp 파일 고유 접미사용 프로세스-로컬 카운터.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
const MAX_TEMP_ATTEMPTS: u32 = 32;

/// 같은 부모 디렉토리에 temp를 만들어 내용을 쓰고 `rename`으로 dest에 원자 교체한다.
///
/// - temp는 dest와 동일 디렉토리(=동일 파일시스템)라 `rename`이 원자적이다.
/// - `create_new`(O_EXCL)는 심링크를 따라가지 않고 새 파일만 만든다.
/// - 모드는 생성 시 `mode`로 지정해 OS가 umask를 적용한다(§1.3). private 파일이 최종 위치에
///   처음부터 올바른 권한으로 나타나므로 잘못된 권한 노출 창이 없다.
/// - `rename`은 dest가 기존 심링크여도 심링크 자체를 원자 교체한다(대상 미추종) → target 밖 오염 없음.
/// - 부분출력 없음: 실패 시 temp를 정리하고 dest는 이전 상태를 유지한다.
#[cfg(unix)]
fn atomic_write(dest: &Path, content: &[u8], mode: FileMode) -> Result<()> {
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
        let temp_path = parent.join(format!(
            ".{file_name}.{}.{counter}.tmp",
            std::process::id()
        ));

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
            return Err(e).with_context(|| format!("failed to write temp file for {}", dest.display()));
        }
        drop(file);

        if let Err(e) = fs::rename(&temp_path, dest) {
            let _ = fs::remove_file(&temp_path);
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
fn atomic_write(dest: &Path, content: &[u8], _mode: FileMode) -> Result<()> {
    // 비-Unix는 BLUEPRINT non-goal. best-effort(모드/원자성 보장 없음).
    fs::write(dest, content).with_context(|| format!("failed to write {}", dest.display()))
}

/// 존재하는 최상위 조상까지 canonicalize(심링크 해석)한 뒤 비존재 tail 컴포넌트를 그대로
/// 재결합한다. `path` 전체가 존재하면 최종 심링크까지 포함해 통째로 해석된다.
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

/// payload를 열거한다. target 안(=source root 안)을 가리키는 심링크는 dereference한다(§1.10):
/// 디렉토리 심링크는 재귀, 파일 심링크는 target 내용을 읽는다(`read_content`의 `fs::read`가 추종).
/// source root 밖을 가리키는 심링크는 외부 내용 유입이므로 거부한다(fail-loud). 디렉토리 심링크로
/// 생기는 cycle은 canonical 경로 추적으로 탐지해 에러낸다.
fn walk(
    source_root: &Path,
    dir: &Path,
    canonical_root: &Path,
    visited: &mut HashSet<PathBuf>,
    out: &mut Vec<PayloadEntry>,
) -> Result<()> {
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

        if file_type.is_symlink() {
            // 심링크는 최종 위치를 canonical로 해석해 source root 안인지 판정한다.
            let canonical_target = path.canonicalize().with_context(|| {
                format!("failed to resolve payload symlink {}", path.display())
            })?;
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
                walk(source_root, &path, canonical_root, visited, out)?;
                visited.remove(&canonical_target);
            } else {
                out.push(PayloadEntry { rel, is_dir: false });
            }
        } else if file_type.is_dir() {
            out.push(PayloadEntry {
                rel: rel.clone(),
                is_dir: true,
            });
            walk(source_root, &path, canonical_root, visited, out)?;
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
    fn list_entries_dereferences_inside_directory_symlink() {
        let source = tempfile::tempdir().expect("tempdir");
        fs::create_dir(source.path().join("real")).expect("mkdir real");
        fs::write(source.path().join("real/x.txt"), b"x").expect("write x");
        symlink(source.path().join("real"), source.path().join("link")).expect("symlink dir");

        let entries = FsPayloadStore.list_entries(source.path()).expect("list_entries");
        let rels: Vec<String> = entries.iter().map(|e| e.rel.to_string()).collect();

        // 심링크 디렉토리가 dereference되어 하위가 열거된다.
        assert!(rels.contains(&"link".to_string()));
        assert!(rels.contains(&"link/x.txt".to_string()));
        let link_entry = entries.iter().find(|e| e.rel.to_string() == "link").unwrap();
        assert!(link_entry.is_dir);
    }

    #[test]
    fn list_entries_reads_inside_file_symlink_content() {
        let source = tempfile::tempdir().expect("tempdir");
        fs::write(source.path().join("real.txt"), b"real-content").expect("write real");
        symlink(source.path().join("real.txt"), source.path().join("link.txt")).expect("symlink");

        let entries = FsPayloadStore.list_entries(source.path()).expect("list_entries");
        let link = entries.iter().find(|e| e.rel.to_string() == "link.txt").unwrap();
        assert!(!link.is_dir);
        let content = FsPayloadStore.read_content(source.path(), link).expect("read");
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
    fn list_entries_detects_directory_symlink_cycle() {
        let source = tempfile::tempdir().expect("tempdir");
        fs::create_dir(source.path().join("sub")).expect("mkdir sub");
        // sub/loop → source root(조상)을 가리켜 cycle을 만든다.
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
