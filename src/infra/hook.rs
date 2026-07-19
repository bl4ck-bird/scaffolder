//! Process execution (`sh -c`, folder scripts) — `HookSource`, `HookRunner`.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow, bail};

use crate::domain::hook::{HookPhase, HookRunner, HookScript, HookSource};
use crate::infra::load::trust::ensure_within_root;

/// `HookRunner` backed by std process execution.
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

    fn run_script_file(
        &self,
        path: &Path,
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<()> {
        // If `path` is relative (from a relative template root), whether `Command::current_dir(cwd)`
        // chdirs the child before exec is platform-dependent, so a relative program path could
        // resolve ENOENT against `cwd` (target). Making it absolute pins it to this process's real
        // cwd (the base of the relative template argument), finding the right script regardless of `cwd`.
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

/// Process-local counter for the unique temp-file suffix.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Secure temp hook-script guard. Removes the file in `Drop` on any path — exec success,
/// failure, or early return (Rust has no `defer`, so RAII guarantees cleanup).
struct TempScript(PathBuf);

impl TempScript {
    /// Creates a unique file in `env::temp_dir()` with `create_new` (O_EXCL, no symlink follow)
    /// and `mode(0o700)` (owner-only) and writes `content`. `name` is normalized by `sanitize_name`.
    fn create(name: &str, content: &[u8]) -> Result<Self> {
        use std::io::ErrorKind;
        use std::os::unix::fs::OpenOptionsExt;

        let sanitized = sanitize_name(name);
        let pid = std::process::id();

        // On a collision with a leftover same-name temp file (PID reuse + a prior process
        // SIGKILLed before Drop could clean up), keep bumping the counter and retry. Other errors
        // (permissions, etc.) will not resolve on retry, so treat them as immediately fatal.
        const MAX_ATTEMPTS: u32 = 1000;
        let mut last_err = None;
        for _ in 0..MAX_ATTEMPTS {
            let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("scaffolder-hook-{pid}-{counter}-{sanitized}"));

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

/// Replaces every character that is not alphanumeric or `.`/`-`/`_` with `_`. Traversal is
/// blocked not by this replacement (`.` is allowed, so `..` survives) but by the invariant that
/// the caller (`TempScript::create`) always prefixes the result with
/// `scaffolder-hook-<pid>-<counter>-`, pinning the final path to a single file-name component,
/// and that `name` only ever comes from `FsHookSource::scripts`'s `read_dir` file_name (likewise
/// a single component).
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

/// Filesystem `HookSource`: enumerates `hooks/<before|after>/` in byte-lexical file-name order.
/// Symlinks are followed to inspect the target (an internal symlink→file must stay runnable, so
/// it is not skipped); each script path is rejected without `trust` if it is an external symlink.
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
        // The leaf guard (below) only runs after read_dir has already followed the symlink and
        // enumerated, so a fileless external-symlink phase dir would slip past it entirely — guard
        // the dir itself before read_dir to guarantee default-reject regardless of contents.
        ensure_within_root(&dir, &self.root_canon, self.trust)?;

        let mut names: Vec<String> = Vec::new();
        for entry in fs::read_dir(&dir)
            .with_context(|| format!("failed to read hook directory {}", dir.display()))?
        {
            let entry =
                entry.with_context(|| format!("failed to read entry in {}", dir.display()))?;
            let path = entry.path();
            // Decide by following: no-follow (`file_type()`) would skip an internal symlink→file
            // hook. A broken symlink is fail-loud (silently skipping would drop an intended hook).
            match fs::metadata(&path) {
                Ok(meta) if meta.is_file() => {}
                Ok(_) => continue,
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!("failed to stat {} (broken symlink?)", path.display())
                    });
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

    /// Several tests check creation/leftover of `scaffolder-hook-` files in the shared OS
    /// `env::temp_dir()`. Serialize this group so parallel test threads don't mistake each other's temp files.
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
        assert!(
            !leftover,
            "no scaffolder-hook temp file should remain after success"
        );
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
        assert!(
            !leftover,
            "no scaffolder-hook temp file should remain after failure"
        );
    }

    #[test]
    fn run_rendered_creates_temp_file_with_owner_only_mode() {
        // To check the mode of the file TempScript::create briefly holds during execution, create
        // the file directly and inspect its mode before cleanup (exec is not this test's concern).
        // Share the same lock so it doesn't overlap other temp-dir scan tests while the file is alive.
        let _guard = TEMP_DIR_SCAN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp =
            super::TempScript::create("mode-check.sh", b"#!/bin/sh\ntrue\n").expect("create");
        let meta = fs::metadata(temp.path()).expect("metadata");
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);
    }

    /// A purely lexical relative path from `base` to `target` (no fs access, symlink-agnostic) —
    /// a helper for tests to point at a script outside the process cwd by relative path.
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

        // Make `target` (the hook's `cwd` argument) far deeper than the process cwd so that
        // `relative`'s `..` count clamping at root cannot accidentally converge on the right
        // absolute path — only then do "absolutize against process cwd" and "exec resolution
        // against the `current_dir` target" actually differ.
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
        // Reproduces the PID-reuse + SIGKILLed-leftover scenario: pre-occupy the next path the
        // counter will produce. Hold the lock across the whole test so no other thread touches
        // TEMP_COUNTER meanwhile (all TempScript::create callers in this file share this lock).
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
        use std::os::unix::fs::{PermissionsExt, symlink};

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
        assert!(
            result.is_err(),
            "broken symlink hook must be a fail-loud error"
        );
    }

    #[test]
    fn scripts_rejects_external_symlink_without_trust() {
        use std::os::unix::fs::{PermissionsExt, symlink};

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
        use std::os::unix::fs::{PermissionsExt, symlink};

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
        use std::os::unix::fs::{PermissionsExt, symlink};

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
