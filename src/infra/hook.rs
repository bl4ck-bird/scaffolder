//! 프로세스 실행(`sh -c`·폴더 스크립트) — `HookSource`·`HookRunner`.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{anyhow, bail, Context, Result};

use crate::domain::hook::{HookPhase, HookRunner, HookScript, HookSource};
use crate::infra::load::trust::ensure_within_root;

/// std 프로세스 실행 기반 `HookRunner`.
pub struct StdHookRunner;

impl HookRunner for StdHookRunner {
    fn run_inline(&self, command: &str, cwd: &Path, env: &BTreeMap<String, String>) -> Result<()> {
        let status = Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .current_dir(cwd)
            .envs(env)
            .status()
            .with_context(|| format!("failed to spawn inline hook command: {command}"))?;

        if !status.success() {
            bail!("inline hook command `{command}` exited with {status}");
        }
        Ok(())
    }

    fn run_script_file(&self, path: &Path, cwd: &Path, env: &BTreeMap<String, String>) -> Result<()> {
        // `path`가 상대(상대 template root에서 유래)면 `Command::current_dir(cwd)`가 exec 전
        // child cwd로 chdir하는 순서는 플랫폼 의존적이라, 상대 program 경로가 `cwd`(target) 기준
        // ENOENT로 해석될 수 있다. 절대화는 이 프로세스의 실제 cwd(상대 template 인자의 기준)로
        // 고정해, `cwd` 인자와 무관하게 항상 올바른 스크립트를 찾는다.
        let program = std::path::absolute(path)
            .with_context(|| format!("failed to resolve hook script path {}", path.display()))?;

        let status = Command::new(&program)
            .current_dir(cwd)
            .envs(env)
            .status()
            .with_context(|| {
                format!(
                    "failed to execute hook script {} (hook script is not executable; add a shebang and chmod +x)",
                    path.display()
                )
            })?;

        if !status.success() {
            bail!("hook script {} exited with {status}", path.display());
        }
        Ok(())
    }

    fn run_rendered(
        &self,
        name: &str,
        content: &[u8],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<()> {
        let temp = TempScript::create(name, content)?;

        let status = Command::new(temp.path())
            .current_dir(cwd)
            .envs(env)
            .status()
            .with_context(|| {
                format!(
                    "failed to execute rendered hook script {} (hook script is not executable; add a shebang and chmod +x)",
                    temp.path().display()
                )
            })?;

        if !status.success() {
            bail!("rendered hook script {name} exited with {status}");
        }
        Ok(())
    }
}

/// temp 파일 고유 접미사용 프로세스-로컬 카운터.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// secure temp hook script 가드. exec 성공·실패·조기 반환 어느 경로든 Drop에서 파일을
/// 제거한다(Rust에 `defer`가 없어 RAII로 정리를 보장).
struct TempScript(PathBuf);

impl TempScript {
    /// `env::temp_dir()`에 `create_new`(O_EXCL, 심링크 미추종)·`mode(0o700)`(소유자 전용)로 유니크
    /// 파일을 만들어 `content`를 쓴다. `name`은 `sanitize_name`으로 정규화한다.
    fn create(name: &str, content: &[u8]) -> Result<Self> {
        use std::io::ErrorKind;
        use std::os::unix::fs::OpenOptionsExt;

        let sanitized = sanitize_name(name);
        let pid = std::process::id();

        // PID 재사용 + 이전 프로세스가 SIGKILL되어 남은 동일 이름 temp 파일과 충돌하면(Drop이
        // 실행되지 못해 정리되지 않은 leftover), counter를 계속 증가시켜 재시도한다. 다른 종류의
        // 에러(권한 등)는 재시도해도 해결되지 않으므로 즉시 fatal로 처리한다.
        const MAX_ATTEMPTS: u32 = 1000;
        let mut last_err = None;
        for _ in 0..MAX_ATTEMPTS {
            let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("scaffolder-hook-{pid}-{counter}-{sanitized}"));

            let open_result = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o700)
                .open(&path);

            match open_result {
                Ok(file) => return Self::finish(path, file, content),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    last_err = Some((path, e));
                    continue;
                }
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!("failed to create temp hook script {}", path.display())
                    });
                }
            }
        }

        let (path, e) = last_err.expect("loop always sets last_err before exhausting attempts");
        Err(e).with_context(|| {
            format!(
                "failed to create temp hook script {} after {MAX_ATTEMPTS} attempts (persistent name collisions)",
                path.display()
            )
        })
    }

    fn finish(path: PathBuf, mut file: fs::File, content: &[u8]) -> Result<Self> {
        let guard = TempScript(path.clone());

        file.write_all(content)
            .with_context(|| format!("failed to write temp hook script {}", path.display()))?;
        drop(file);

        Ok(guard)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempScript {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// alphanumeric·`.`·`-`·`_`가 아닌 문자를 전부 `_`로 치환한다. traversal이 막히는 건 이 치환
/// 자체가 아니라(`.`은 허용 문자라 `..`는 그대로 남는다), 호출부(`TempScript::create`)가 결과
/// 앞에 항상 `scaffolder-hook-<pid>-<counter>-` 접두사를 붙여 최종 경로를 단일 파일명
/// 컴포넌트로 고정하고, `name`이 `FsHookSource::scripts`의 `read_dir` file_name(마찬가지로
/// 단일 컴포넌트)에서만 온다는 불변식 덕분이다.
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 파일시스템 기반 `HookSource`: `hooks/<before|after>/`를 파일명 바이트 lexical 순서로 열거한다.
/// 심링크는 follow해 대상을 검사한다(내부 symlink→file은 실행 가능해야 하므로 skip하지
/// 않는다), 각 스크립트 경로는 외부 심링크면 `trust` 없이 거부한다.
pub struct FsHookSource {
    pub root_canon: PathBuf,
    pub trust: bool,
}

impl HookSource for FsHookSource {
    fn scripts(&self, template_root: &Path, phase: HookPhase) -> Result<Vec<HookScript>> {
        let phase_dir = match phase {
            HookPhase::Before => "before",
            HookPhase::After => "after",
        };
        let dir = template_root.join("hooks").join(phase_dir);

        if !dir.exists() {
            return Ok(Vec::new());
        }
        // leaf 가드(아래)는 `read_dir`가 이미 심링크를 follow해 열거를 마친 뒤에야 실행되므로,
        // 파일 없는 외부 심링크 phase dir는 leaf 가드를 한 번도 못 거치고 통과해 버린다 —
        // `read_dir` 전에 dir 자체를 가드해 내용과 무관하게 default-reject를 보장한다.
        ensure_within_root(&dir, &self.root_canon, self.trust)?;

        let mut names: Vec<String> = Vec::new();
        for entry in fs::read_dir(&dir)
            .with_context(|| format!("failed to read hook directory {}", dir.display()))?
        {
            let entry =
                entry.with_context(|| format!("failed to read entry in {}", dir.display()))?;
            let path = entry.path();
            // follow해 판정한다: no-follow(`file_type()`)면 내부 symlink→file 훅이 skip돼 버린다.
            // broken 심링크는 fail-loud(조용히 skip하면 의도한 훅이 실행되지 않고 사라진다).
            match fs::metadata(&path) {
                Ok(meta) if meta.is_file() => {}
                Ok(_) => continue,
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("failed to stat {} (broken symlink?)", path.display()));
                }
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|raw| anyhow!("hook file name {raw:?} is not valid UTF-8"))?;
            names.push(name);
        }
        names.sort();

        names
            .into_iter()
            .map(|name| {
                let path = dir.join(&name);
                ensure_within_root(&path, &self.root_canon, self.trust)?;
                if let Some(stripped) = name.strip_suffix(".jinja") {
                    let raw = fs::read_to_string(&path).with_context(|| {
                        format!("hook template {} is not valid UTF-8", path.display())
                    })?;
                    Ok(HookScript::Template {
                        name: stripped.to_string(),
                        raw,
                    })
                } else {
                    Ok(HookScript::Executable { name, path })
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    /// 여러 테스트가 공유 OS `env::temp_dir()`에서 `scaffolder-hook-` 파일 생성·잔존을 검사한다.
    /// 병렬 테스트 스레드가 서로의 임시 파일을 오탐지하지 않도록 이 그룹을 직렬화한다.
    static TEMP_DIR_SCAN_LOCK: Mutex<()> = Mutex::new(());

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn run_inline_executes_with_env_and_cwd() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env = env(&[("SCAFFODER_X", "unused"), ("SCAFFOLDER_X", "v")]);

        StdHookRunner
            .run_inline("echo $SCAFFOLDER_X > out.txt", tmp.path(), &env)
            .expect("run_inline");

        let written = fs::read_to_string(tmp.path().join("out.txt")).expect("read out.txt");
        assert_eq!(written.trim(), "v");
    }

    #[test]
    fn run_inline_errors_on_nonzero_exit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = StdHookRunner.run_inline("exit 1", tmp.path(), &BTreeMap::new());
        assert!(result.is_err(), "nonzero exit must be an error");
    }

    #[test]
    fn run_rendered_executes_script_and_leaves_no_temp_file() {
        let _guard = TEMP_DIR_SCAN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let script = b"#!/bin/sh\necho hi > o.txt\n";

        StdHookRunner
            .run_rendered("s.sh", script, tmp.path(), &BTreeMap::new())
            .expect("run_rendered");

        let written = fs::read_to_string(tmp.path().join("o.txt")).expect("read o.txt");
        assert_eq!(written.trim(), "hi");

        let leftover = fs::read_dir(std::env::temp_dir())
            .expect("read temp_dir")
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains("scaffolder-hook-"));
        assert!(!leftover, "no scaffolder-hook temp file should remain after success");
    }

    #[test]
    fn run_rendered_cleans_up_temp_file_on_failure() {
        let _guard = TEMP_DIR_SCAN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        let script = b"#!/bin/sh\nexit 1\n";

        let result = StdHookRunner.run_rendered("f.sh", script, tmp.path(), &BTreeMap::new());
        assert!(result.is_err(), "nonzero exit must be an error");

        let leftover = fs::read_dir(std::env::temp_dir())
            .expect("read temp_dir")
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains("scaffolder-hook-"));
        assert!(!leftover, "no scaffolder-hook temp file should remain after failure");
    }

    #[test]
    fn run_rendered_creates_temp_file_with_owner_only_mode() {
        // TempScript::create가 실행 도중 잠깐 존재하는 파일의 모드를 검증하기 위해, 정리 전에
        // 파일 자체를 직접 만들어 모드를 확인한다(exec은 이 테스트의 관심사가 아니다). temp
        // 파일이 살아있는 동안 다른 temp-dir 스캔 테스트와 겹치지 않도록 같은 lock을 공유한다.
        let _guard = TEMP_DIR_SCAN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp = super::TempScript::create("mode-check.sh", b"#!/bin/sh\ntrue\n").expect("create");
        let meta = fs::metadata(temp.path()).expect("metadata");
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
    }

    /// `base`에서 `target`으로의 순수 lexical 상대 경로(fs 접근 없음, symlink 무관) — 테스트가
    /// 프로세스 cwd 밖에 있는 스크립트를 상대 경로로 가리키기 위한 헬퍼.
    fn relative_from(base: &Path, target: &Path) -> PathBuf {
        let base_comps: Vec<_> = base.components().collect();
        let target_comps: Vec<_> = target.components().collect();
        let mut common = 0;
        while common < base_comps.len()
            && common < target_comps.len()
            && base_comps[common] == target_comps[common]
        {
            common += 1;
        }
        let mut result = PathBuf::new();
        for _ in common..base_comps.len() {
            result.push("..");
        }
        for c in &target_comps[common..] {
            result.push(c.as_os_str());
        }
        result
    }

    #[test]
    fn run_script_file_resolves_relative_path_against_process_cwd_not_target_cwd() {
        let process_cwd = std::env::current_dir().expect("current_dir");
        let script_dir = tempfile::tempdir().expect("script tempdir");
        let script_path = script_dir.path().join("relhook.sh");
        fs::write(&script_path, b"#!/bin/sh\necho hi > r.txt\n").expect("write script");
        let mut perms = fs::metadata(&script_path).expect("metadata").permissions();
        perms.set_mode(0o700);
        fs::set_permissions(&script_path, perms).expect("chmod");

        let relative = relative_from(&process_cwd, &script_path);
        assert!(
            relative.is_relative(),
            "test setup must produce a relative path, got {relative:?}"
        );

        // `target`(hook의 `cwd` 인자)를 process cwd보다 훨씬 깊게 만들어, `relative`의 `..`
        // 개수가 root에서 clamp되어 우연히 올바른 절대 경로로 수렴하는 경우를 배제한다 — 그래야
        // "process cwd 기준으로 절대화" 대 "`current_dir`로 넘긴 target 기준 exec 해석"이 실제로
        // 다른 결과를 낸다.
        let target_base = tempfile::tempdir().expect("target base tempdir");
        let mut target_path = target_base.path().to_path_buf();
        for i in 0..40 {
            target_path.push(format!("d{i}"));
        }
        fs::create_dir_all(&target_path).expect("mkdir nested target");

        StdHookRunner
            .run_script_file(&relative, &target_path, &BTreeMap::new())
            .expect("run_script_file must resolve relative path against process cwd");

        let written = fs::read_to_string(target_path.join("r.txt")).expect("read r.txt");
        assert_eq!(written.trim(), "hi");
    }

    #[test]
    fn create_retries_on_leftover_name_collision_instead_of_failing() {
        // PID 재사용 + 이전 프로세스가 SIGKILL되어 남은 temp 파일 시나리오를 재현: counter가
        // 만들 다음 경로를 미리 점유해 둔다. lock을 test 전체에서 유지해, 이 사이 다른 스레드가
        // TEMP_COUNTER를 건드리지 못하게 한다(이 파일의 TempScript::create 호출부는 전부 이 락을
        // 공유한다).
        let _guard = TEMP_DIR_SCAN_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let counter_before = TEMP_COUNTER.load(Ordering::Relaxed);
        let sanitized = sanitize_name("collide.sh");
        let colliding_path = std::env::temp_dir().join(format!(
            "scaffolder-hook-{}-{counter_before}-{sanitized}",
            std::process::id()
        ));
        fs::write(&colliding_path, b"leftover from a killed process").expect("seed leftover file");

        let result = TempScript::create("collide.sh", b"#!/bin/sh\ntrue\n");

        let cleanup = || {
            let _ = fs::remove_file(&colliding_path);
        };

        let created = match result {
            Ok(created) => created,
            Err(e) => {
                cleanup();
                panic!("create must retry past a leftover collision, got error: {e:#}");
            }
        };

        assert_ne!(
            created.path(),
            colliding_path,
            "create must not reuse the colliding path"
        );
        let content = fs::read(created.path()).expect("read created script");
        assert_eq!(content, b"#!/bin/sh\ntrue\n");

        cleanup();
    }

    #[test]
    fn run_script_file_executes_in_place() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let script_path = tmp.path().join("run.sh");
        fs::write(&script_path, b"#!/bin/sh\necho hi > r.txt\n").expect("write script");
        let mut perms = fs::metadata(&script_path).expect("metadata").permissions();
        perms.set_mode(0o700);
        fs::set_permissions(&script_path, perms).expect("chmod");

        StdHookRunner
            .run_script_file(&script_path, tmp.path(), &BTreeMap::new())
            .expect("run_script_file");

        let written = fs::read_to_string(tmp.path().join("r.txt")).expect("read r.txt");
        assert_eq!(written.trim(), "hi");
    }

    #[test]
    fn run_script_file_errors_when_not_executable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let script_path = tmp.path().join("noexec.sh");
        fs::write(&script_path, b"#!/bin/sh\necho hi\n").expect("write script");
        let mut perms = fs::metadata(&script_path).expect("metadata").permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&script_path, perms).expect("chmod");

        let result = StdHookRunner.run_script_file(&script_path, tmp.path(), &BTreeMap::new());
        assert!(result.is_err(), "non-executable script must error");
        let message = format!("{:#}", result.unwrap_err());
        assert!(
            message.contains("not executable"),
            "error should hint at chmod +x, got: {message}"
        );
    }

    fn hook_source(root: &Path) -> FsHookSource {
        FsHookSource {
            root_canon: root.canonicalize().expect("canonicalize root"),
            trust: false,
        }
    }

    #[test]
    fn scripts_returns_lexical_order_and_classifies_jinja_as_template() {
        let root = tempfile::tempdir().expect("tempdir");
        let before = root.path().join("hooks/before");
        fs::create_dir_all(&before).expect("mkdir hooks/before");
        fs::write(before.join("10-a.sh"), b"#!/bin/sh\ntrue\n").expect("write 10-a.sh");
        fs::write(before.join("20-b.sh.jinja"), b"#!/bin/sh\n# {{ name }}\n").expect("write 20-b");
        fs::write(before.join("05-c.sh"), b"#!/bin/sh\ntrue\n").expect("write 05-c.sh");

        let scripts = hook_source(root.path())
            .scripts(root.path(), HookPhase::Before)
            .expect("scripts");

        assert_eq!(scripts.len(), 3);
        match &scripts[0] {
            HookScript::Executable { name, .. } => assert_eq!(name, "05-c.sh"),
            other => panic!("expected Executable, got {other:?}"),
        }
        match &scripts[1] {
            HookScript::Executable { name, .. } => assert_eq!(name, "10-a.sh"),
            other => panic!("expected Executable, got {other:?}"),
        }
        match &scripts[2] {
            HookScript::Template { name, raw } => {
                assert_eq!(name, "20-b.sh");
                assert!(raw.contains("{{ name }}"));
            }
            other => panic!("expected Template, got {other:?}"),
        }
    }

    #[test]
    fn scripts_returns_empty_when_phase_folder_missing() {
        let root = tempfile::tempdir().expect("tempdir");
        let scripts = hook_source(root.path())
            .scripts(root.path(), HookPhase::After)
            .expect("scripts");
        assert!(scripts.is_empty());
    }

    #[test]
    fn scripts_follows_internal_symlink_to_file_and_includes_it() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = tempfile::tempdir().expect("tempdir");
        let before = root.path().join("hooks/before");
        fs::create_dir_all(&before).expect("mkdir hooks/before");
        let real = root.path().join("real-hook.sh");
        fs::write(&real, b"#!/bin/sh\ntrue\n").expect("write real hook");
        let mut perms = fs::metadata(&real).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&real, perms).expect("chmod");
        symlink(&real, before.join("01-link.sh")).expect("symlink hook");

        let scripts = hook_source(root.path())
            .scripts(root.path(), HookPhase::Before)
            .expect("scripts");

        assert_eq!(scripts.len(), 1);
        match &scripts[0] {
            HookScript::Executable { name, .. } => assert_eq!(name, "01-link.sh"),
            other => panic!("expected Executable, got {other:?}"),
        }
    }

    #[test]
    fn scripts_errors_on_broken_symlink() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("tempdir");
        let before = root.path().join("hooks/before");
        fs::create_dir_all(&before).expect("mkdir hooks/before");
        symlink(root.path().join("nowhere"), before.join("01-broken.sh")).expect("symlink hook");

        let result = hook_source(root.path()).scripts(root.path(), HookPhase::Before);
        assert!(result.is_err(), "broken symlink hook must be a fail-loud error");
    }

    #[test]
    fn scripts_rejects_external_symlink_without_trust() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = tempfile::tempdir().expect("tempdir");
        let before = root.path().join("hooks/before");
        fs::create_dir_all(&before).expect("mkdir hooks/before");
        let outside = tempfile::tempdir().expect("outside tempdir");
        let external = outside.path().join("evil.sh");
        fs::write(&external, b"#!/bin/sh\ntrue\n").expect("write external hook");
        let mut perms = fs::metadata(&external).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&external, perms).expect("chmod");
        symlink(&external, before.join("01-external.sh")).expect("symlink hook");

        let result = hook_source(root.path()).scripts(root.path(), HookPhase::Before);
        assert!(result.is_err());
    }

    #[test]
    fn scripts_allows_external_symlink_with_trust() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = tempfile::tempdir().expect("tempdir");
        let before = root.path().join("hooks/before");
        fs::create_dir_all(&before).expect("mkdir hooks/before");
        let outside = tempfile::tempdir().expect("outside tempdir");
        let external = outside.path().join("evil.sh");
        fs::write(&external, b"#!/bin/sh\ntrue\n").expect("write external hook");
        let mut perms = fs::metadata(&external).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&external, perms).expect("chmod");
        symlink(&external, before.join("01-external.sh")).expect("symlink hook");

        let trusted = FsHookSource {
            root_canon: root.path().canonicalize().expect("canonicalize root"),
            trust: true,
        };
        let scripts = trusted
            .scripts(root.path(), HookPhase::Before)
            .expect("scripts");
        assert_eq!(scripts.len(), 1);
    }

    #[test]
    fn scripts_rejects_external_symlink_phase_dir_without_trust() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(root.path().join("hooks")).expect("mkdir hooks");
        let outside = tempfile::tempdir().expect("outside tempdir");
        let external_before = outside.path().join("before");
        fs::create_dir_all(&external_before).expect("mkdir external before");
        symlink(&external_before, root.path().join("hooks/before")).expect("symlink phase dir");

        let result = hook_source(root.path()).scripts(root.path(), HookPhase::Before);
        assert!(
            result.is_err(),
            "external symlinked phase dir must be rejected without --trust even if empty"
        );
    }

    #[test]
    fn scripts_allows_external_symlink_phase_dir_with_trust() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(root.path().join("hooks")).expect("mkdir hooks");
        let outside = tempfile::tempdir().expect("outside tempdir");
        let external_before = outside.path().join("before");
        fs::create_dir_all(&external_before).expect("mkdir external before");
        fs::write(external_before.join("01-a.sh"), b"#!/bin/sh\ntrue\n").expect("write hook");
        let mut perms = fs::metadata(external_before.join("01-a.sh"))
            .expect("metadata")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(external_before.join("01-a.sh"), perms).expect("chmod");
        symlink(&external_before, root.path().join("hooks/before")).expect("symlink phase dir");

        let trusted = FsHookSource {
            root_canon: root.path().canonicalize().expect("canonicalize root"),
            trust: true,
        };
        let scripts = trusted
            .scripts(root.path(), HookPhase::Before)
            .expect("scripts");
        assert_eq!(scripts.len(), 1);
    }
}
