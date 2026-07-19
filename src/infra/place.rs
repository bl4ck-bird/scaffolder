//! 파일 쓰기(mode·umask·심링크 방어·containment) — `PayloadStore`.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{anyhow, bail, Context, Result};

use crate::domain::place::{
    normalize_target, safe_rel_path, DestStatus, FileMode, PayloadEntry, PayloadStore, RelPath,
    TargetPreparation,
};

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
        walk(source_root, source_root, &canonical_root, &mut visited, &mut entries, 0)?;
        entries.sort_by(|a, b| a.rel.as_path().cmp(b.rel.as_path()));
        Ok(entries)
    }

    fn read_content(&self, source_root: &Path, entry: &PayloadEntry) -> Result<Vec<u8>> {
        let path = source_root.join(entry.rel.as_path());
        // 열거(walk)와 읽기 사이에 심링크가 외부로 교체됐을 수 있으므로 읽기 직전 containment를
        // 재검증하고, 심링크를 재추종하지 않도록 canonical 경로에서 읽는다(source-side 갭 축소).
        let canonical_root = source_root.canonicalize().with_context(|| {
            format!("payload source root {} does not exist", source_root.display())
        })?;
        let canonical = path
            .canonicalize()
            .with_context(|| format!("failed to resolve payload file {}", path.display()))?;
        if !canonical.starts_with(&canonical_root) {
            bail!("payload file {} resolves outside the source root", path.display());
        }
        fs::read(&canonical)
            .with_context(|| format!("failed to read payload file {}", path.display()))
    }

    fn ensure_target(&self, target_root: &Path) -> Result<TargetPreparation> {
        // 경로를 lexical 정규화해 실효 target을 확정한 뒤, 그 부모만 준비하고 최종 컴포넌트를
        // 배타적으로 생성한다. `exists()` 후 `create_dir_all` 방식은 `..` 경로와 create-race를
        // 신규로 오판정해 사전 존재 사용자 데이터를 삭제할 수 있다(배타적 create_dir로 원천 차단).
        let effective = normalize_target(target_root);
        if let Some(parent) = effective.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create parent of target {}", effective.display())
            })?;
        }
        match fs::create_dir(&effective) {
            Ok(()) => Ok(TargetPreparation::Created),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                // 이미 있는 것이 실제 디렉토리(또는 디렉토리를 가리키는 symlink)여야 target으로 쓸 수
                // 있다. `metadata`는 symlink를 follow하므로 broken/비-디렉토리 symlink는 오류가 된다.
                // 어느 경우든 우리가 만들지 않았으므로 정리 대상이 아니다.
                let meta = fs::metadata(&effective).with_context(|| {
                    format!("target {} exists but could not be inspected", effective.display())
                })?;
                if meta.is_dir() {
                    Ok(TargetPreparation::Existing)
                } else {
                    bail!("target {} exists and is not a directory", effective.display())
                }
            }
            Err(e) => Err(anyhow!(e))
                .with_context(|| format!("failed to create target directory {}", effective.display())),
        }
    }

    fn cleanup_target(&self, target_root: &Path) -> Result<()> {
        // ensure_target과 동일하게 정규화한 실효 경로만 삭제한다 — 렌더 경로가 아니라 준비된 그
        // target root에만 호출된다(호출부 pipeline이 Created일 때만 부른다).
        let effective = normalize_target(target_root);
        fs::remove_dir_all(&effective)
            .with_context(|| format!("failed to clean up target directory {}", effective.display()))
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
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create parent directory for {}", path.display()))?;
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

/// temp 파일 고유 접미사용 프로세스-로컬 카운터.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
const MAX_TEMP_ATTEMPTS: u32 = 32;
/// payload 트리 재귀 depth 상한(심링크 alias·병적 깊이 backstop). 실제 템플릿은 훨씬 얕다.
const MAX_WALK_DEPTH: u32 = 64;

/// 같은 부모 디렉토리에 temp를 만들어 내용을 쓰고 `rename`으로 dest에 원자 교체한다.
///
/// - temp는 dest와 동일 디렉토리(=동일 파일시스템)라 `rename`이 원자적이다.
/// - `create_new`(O_EXCL)는 심링크를 따라가지 않고 새 파일만 만든다.
/// - 모드는 생성 시 `mode`로 지정해 OS가 umask를 적용한다. private 파일이 최종 위치에
///   처음부터 올바른 권한으로 나타나므로 잘못된 권한 노출 창이 없다.
/// - `rename`은 dest가 기존 심링크여도 심링크 자체를 원자 교체한다(대상 미추종) → target 밖 오염 없음.
/// - 부분출력 없음: 실패 시 temp를 정리하고 dest는 이전 상태를 유지한다.
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

        let placed = if overwrite {
            // 기존 dest를 원자 교체(심링크면 링크 자체를 대체).
            fs::rename(&temp_path, dest)
        } else {
            // dest가 새로 생겨야 한다. `hard_link`는 dest가 이미 있으면 EEXIST로 실패하므로, plan
            // 이후 경쟁으로 생긴 파일을 조용히 덮지 않는다. 성공·실패 무관하게 temp 링크를 제거한다.
            let result = fs::hard_link(&temp_path, dest);
            let _ = fs::remove_file(&temp_path);
            result
        };
        if let Err(e) = placed {
            // overwrite(rename) 실패 시 temp가 남으므로 정리한다. non-overwrite는 위에서 이미 제거됨.
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
    // 비-Unix는 BLUEPRINT non-goal. best-effort(모드/원자성 보장 없음).
    fs::write(dest, content).with_context(|| format!("failed to write {}", dest.display()))
}

/// 최종 기록 위치를 해석한다. 최종 컴포넌트는 `atomic_write`가 제자리에서 원자 교체하므로
/// **dereference하지 않고**, 부모(중간 컴포넌트)만 심링크를 따라 해석한 뒤 최종 basename을 그대로
/// 붙인다. 이렇게 하면 최종 컴포넌트가 외부를 가리키는 기존 심링크여도 containment는 target 안으로
/// 판정되어 overwrite(제자리 교체)로 처리된다 — 중간 컴포넌트 심링크만 외부쓰기 대상이다.
fn resolve_final_path(path: &Path) -> Result<PathBuf> {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(file_name)) => {
            let resolved_parent = resolve_existing_ancestor(parent)?;
            Ok(resolved_parent.join(file_name))
        }
        _ => Ok(path.to_path_buf()),
    }
}

/// 존재하는 최상위 조상까지 canonicalize(심링크 해석)한 뒤 비존재 tail 컴포넌트를 재결합한다.
/// 디렉토리 경로 해석에만 쓴다(중간 컴포넌트 심링크는 따라간다).
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

/// payload를 열거한다. target 안(=source root 안)을 가리키는 심링크는 dereference한다:
/// 디렉토리 심링크는 재귀, 파일 심링크는 target 내용을 읽는다(`read_content`의 `fs::read`가 추종).
/// source root 밖을 가리키는 심링크는 외부 내용 유입이므로 거부한다(fail-loud). 디렉토리 심링크로
/// 생기는 cycle은 canonical 경로 추적으로 탐지해 에러낸다.
fn walk(
    source_root: &Path,
    dir: &Path,
    canonical_root: &Path,
    visited: &mut HashSet<PathBuf>,
    out: &mut Vec<PayloadEntry>,
    depth: u32,
) -> Result<()> {
    // 병적으로 깊은(또는 심링크 alias로 부풀려진) 트리에 대한 backstop.
    if depth > MAX_WALK_DEPTH {
        bail!("payload tree exceeds max depth {MAX_WALK_DEPTH} at {}", dir.display());
    }
    let read_dir =
        fs::read_dir(dir).with_context(|| format!("failed to read directory {}", dir.display()))?;

    for entry in read_dir {
        let entry = entry.with_context(|| format!("failed to read entry in {}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to stat {}", path.display()))?;

        // 파일명은 렌더·출력 경로에 UTF-8로 쓰이므로 비-UTF8 경로는 lossy 변환(다른 이름과 충돌)
        // 대신 거부한다(fail-loud).
        let rel_str = path
            .strip_prefix(source_root)
            .with_context(|| format!("failed to compute relative path for {}", path.display()))?
            .to_str()
            .ok_or_else(|| anyhow!("payload path {} is not valid UTF-8", path.display()))?
            .to_string();
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
        // base/missing/../preexisting에서 preexisting만 존재 → 실효 target=base/preexisting=Existing,
        // sibling base/missing은 생성되지 않아야 한다.
        let base = tempfile::tempdir().expect("tempdir");
        fs::create_dir(base.path().join("preexisting")).expect("mkdir preexisting");
        let target = base.path().join("missing").join("..").join("preexisting");
        let prep = FsPayloadStore.ensure_target(&target).expect("ensure");
        assert_eq!(prep, TargetPreparation::Existing);
        assert!(!base.path().join("missing").exists(), "sibling must not be created");
    }

    #[test]
    fn ensure_target_errors_when_final_component_is_a_file_and_keeps_it() {
        let base = tempfile::tempdir().expect("tempdir");
        let target = base.path().join("afile");
        fs::write(&target, b"data").expect("write file");
        let result = FsPayloadStore.ensure_target(&target);
        assert!(result.is_err(), "file at target must be an error");
        assert!(target.is_file(), "the file must not be deleted");
        assert_eq!(fs::read(&target).expect("file survives"), b"data", "file contents intact");
    }

    #[test]
    fn ensure_target_errors_on_file_symlink_and_keeps_it() {
        // 최종 컴포넌트가 파일을 가리키는 symlink → create_dir AlreadyExists, metadata(follow)가
        // 파일이라 오류. symlink와 대상 파일 모두 보존(우리가 만들지 않음).
        let base = tempfile::tempdir().expect("tempdir");
        let real = base.path().join("real_file");
        fs::write(&real, b"data").expect("write real file");
        let target = base.path().join("link");
        symlink(&real, &target).expect("file symlink");
        let result = FsPayloadStore.ensure_target(&target);
        assert!(result.is_err(), "symlink to a file must be a prepare error");
        assert!(target.symlink_metadata().is_ok(), "the symlink must not be deleted");
        assert_eq!(fs::read(&real).expect("target file survives"), b"data");
    }

    #[test]
    fn ensure_target_errors_on_broken_symlink_and_keeps_it() {
        // broken symlink → create_dir AlreadyExists, metadata(follow)가 ENOENT → 오류. 보존.
        let base = tempfile::tempdir().expect("tempdir");
        let target = base.path().join("broken");
        symlink(base.path().join("nonexistent"), &target).expect("broken symlink");
        let result = FsPayloadStore.ensure_target(&target);
        assert!(result.is_err(), "broken symlink must be a prepare error");
        assert!(target.symlink_metadata().is_ok(), "the broken symlink must not be deleted");
    }

    #[test]
    fn ensure_target_reports_existing_for_directory_symlink_and_preserves_contents() {
        // 디렉토리를 가리키는 symlink → AlreadyExists, metadata(follow) is_dir → Existing.
        // 정리 대상이 아니므로 symlink 대상 디렉토리의 내용은 보존된다.
        let base = tempfile::tempdir().expect("tempdir");
        let real = base.path().join("real_dir");
        fs::create_dir(&real).expect("mkdir real dir");
        fs::write(real.join("sentinel"), b"keep").expect("sentinel");
        let target = base.path().join("dirlink");
        symlink(&real, &target).expect("dir symlink");
        let prep = FsPayloadStore.ensure_target(&target).expect("dir symlink resolves to Existing");
        assert_eq!(prep, TargetPreparation::Existing);
        assert_eq!(fs::read(real.join("sentinel")).expect("contents survive"), b"keep");
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
        // 정리는 exact prepared root에만 국한 — parent 파일과 sibling 디렉토리의 sentinel은 불변.
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
        assert_eq!(fs::read(sibling.join("s")).expect("sibling survives"), b"sib");
    }

    #[test]
    fn cleanup_target_does_not_follow_symlink_to_external_dir() {
        // target 안에 외부 디렉토리를 가리키는 symlink가 있어도, remove_dir_all은 symlink를 따라가지
        // 않고 unlink만 하므로 외부 대상은 보존된다.
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
    fn list_entries_allows_noncyclic_diamond_symlinks() {
        // 두 심링크가 같은 real dir을 가리키지만 순환은 아니다 — cycle로 오탐하면 안 된다.
        let source = tempfile::tempdir().expect("tempdir");
        fs::create_dir(source.path().join("real")).expect("mkdir real");
        fs::write(source.path().join("real/x.txt"), b"x").expect("write x");
        symlink(source.path().join("real"), source.path().join("link_a")).expect("link_a");
        symlink(source.path().join("real"), source.path().join("link_b")).expect("link_b");

        let entries = FsPayloadStore.list_entries(source.path()).expect("list_entries");
        let rels: Vec<String> = entries.iter().map(|e| e.rel.to_string()).collect();
        assert!(rels.contains(&"link_a/x.txt".to_string()));
        assert!(rels.contains(&"link_b/x.txt".to_string()));
    }

    #[test]
    fn list_entries_errors_on_broken_payload_symlink() {
        let source = tempfile::tempdir().expect("tempdir");
        symlink(source.path().join("missing"), source.path().join("dangling")).expect("symlink");
        let result = FsPayloadStore.list_entries(source.path());
        assert!(result.is_err(), "broken payload symlink must fail loud");
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

        // overwrite=false인데 dest가 이미 있으면(plan 이후 경쟁 생성 등) 조용히 덮지 않고 실패한다.
        let result =
            FsPayloadStore.write_file(target.path(), &rel, b"new", FileMode::base(), false);
        assert!(result.is_err(), "no-clobber create must fail when dest exists");

        // 기존 내용 보존.
        assert_eq!(
            fs::read(target.path().join("file.txt")).unwrap(),
            b"pre-existing"
        );
        // temp 파일 잔여 없음.
        let leftover = fs::read_dir(target.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().ends_with(".tmp"));
        assert!(!leftover, "no temp file should remain after a failed no-clobber write");
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
        // 최종 컴포넌트 심링크는 제자리 교체되므로 target 안(overwrite)으로 판정된다 — 외부를
        // 가리켜도 rename은 링크 자체를 대체하고 대상을 건드리지 않는다.
        assert!(status.inside_target);
        assert_eq!(
            status.final_path,
            target.path().canonicalize().unwrap().join("link.txt")
        );
    }
}
