//! 스토어 조회(XDG·`--template-dir` 우선순위) — `TemplateStore`.

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::domain::store::TemplateStore;

/// `--template-dir` > `$SCAFFOLDER_HOME` > `$XDG_CONFIG_HOME/scaffolder` > `~/.scaffolder`
/// 순으로 스토어를 조회하는 `TemplateStore`.
pub struct FsTemplateStore {
    template_dir: Option<PathBuf>,
}

impl FsTemplateStore {
    pub fn new(template_dir: Option<PathBuf>) -> Self {
        Self { template_dir }
    }

    fn store_bases(&self) -> Vec<PathBuf> {
        let mut bases = Vec::new();
        bases.extend(self.template_dir.clone());
        if let Some(home) = env::var_os("SCAFFOLDER_HOME").filter(|v| !v.is_empty()) {
            bases.push(PathBuf::from(home));
        }
        if let Some(xdg) = env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            bases.push(PathBuf::from(xdg).join("scaffolder"));
        }
        if let Some(home) = dirs::home_dir() {
            bases.push(home.join(".scaffolder"));
        }
        bases
    }
}

impl TemplateStore for FsTemplateStore {
    fn resolve(&self, name_or_path: &str) -> Result<PathBuf> {
        let as_path = Path::new(name_or_path);
        // "."/".."는 존재하는 디렉토리라도 스토어 이름 검증 경로로 보내 base 밖 참조를 막는다.
        if name_or_path != "." && name_or_path != ".." && as_path.is_dir() {
            return Ok(as_path.to_path_buf());
        }

        let name = validate_store_name(name_or_path)?;

        let bases = self.store_bases();
        for base in &bases {
            let candidate = base.join(name);
            if candidate.join("scaffold.toml").is_file() {
                return Ok(candidate);
            }
        }

        let searched = bases
            .iter()
            .map(|base| base.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        bail!("template {name_or_path:?} not found; searched: [{searched}]");
    }
}

/// 스토어 이름은 base 하위 단일 경로 컴포넌트여야 한다(구분자·`.`/`..` 금지).
fn validate_store_name(name_or_path: &str) -> Result<&str> {
    if name_or_path.is_empty()
        || name_or_path.contains('/')
        || name_or_path == "."
        || name_or_path == ".."
    {
        bail!("template name {name_or_path:?} must be a single path component");
    }
    Ok(name_or_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use tempfile::tempdir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// `SCAFFOLDER_HOME`/`XDG_CONFIG_HOME`는 프로세스 전역이라 동시 테스트 실행 시 서로
    /// 오염시킨다 — 뮤텍스로 직렬화하고 이전 값을 저장·복원한다.
    fn with_env_vars<T>(vars: &[(&str, Option<&str>)], f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous: Vec<(&str, Option<String>)> = vars
            .iter()
            .map(|(k, _)| (*k, env::var(k).ok()))
            .collect();

        for (k, v) in vars {
            match v {
                Some(val) => unsafe { env::set_var(k, val) },
                None => unsafe { env::remove_var(k) },
            }
        }

        let result = f();

        for (k, v) in previous {
            match v {
                Some(val) => unsafe { env::set_var(k, val) },
                None => unsafe { env::remove_var(k) },
            }
        }

        result
    }

    #[test]
    fn resolves_existing_local_directory_as_is() {
        let dir = tempdir().expect("tempdir");
        let store = FsTemplateStore::new(None);

        let resolved = store
            .resolve(dir.path().to_str().expect("utf8 path"))
            .expect("local directory should resolve");

        assert_eq!(resolved, dir.path());
    }

    #[test]
    fn resolves_store_name_from_template_dir_override() {
        let template_dir = tempdir().expect("tempdir");
        let template_path = template_dir.path().join("myapp");
        std::fs::create_dir_all(&template_path).expect("create template dir");
        std::fs::write(template_path.join("scaffold.toml"), "").expect("write manifest");

        let store = FsTemplateStore::new(Some(template_dir.path().to_path_buf()));

        // resolve reads SCAFFOLDER_HOME/XDG_CONFIG_HOME even on the template-dir hit path,
        // so it must serialize against other tests that mutate those vars concurrently.
        let resolved = with_env_vars(&[], || store.resolve("myapp"))
            .expect("store name should resolve");

        assert_eq!(resolved, template_path);
    }

    #[test]
    fn template_dir_takes_priority_over_scaffolder_home() {
        let template_dir = tempdir().expect("tempdir");
        let scaffolder_home = tempdir().expect("tempdir");

        let winner = template_dir.path().join("shared");
        std::fs::create_dir_all(&winner).expect("create winner dir");
        std::fs::write(winner.join("scaffold.toml"), "").expect("write manifest");

        let loser = scaffolder_home.path().join("shared");
        std::fs::create_dir_all(&loser).expect("create loser dir");
        std::fs::write(loser.join("scaffold.toml"), "").expect("write manifest");

        let store = FsTemplateStore::new(Some(template_dir.path().to_path_buf()));

        let resolved = with_env_vars(
            &[(
                "SCAFFOLDER_HOME",
                Some(scaffolder_home.path().to_str().expect("utf8 path")),
            )],
            || store.resolve("shared"),
        )
        .expect("store name should resolve");

        assert_eq!(resolved, winner);
    }

    #[test]
    fn scaffolder_home_is_used_when_no_template_dir_override() {
        let scaffolder_home = tempdir().expect("tempdir");
        let template_path = scaffolder_home.path().join("fromhome");
        std::fs::create_dir_all(&template_path).expect("create template dir");
        std::fs::write(template_path.join("scaffold.toml"), "").expect("write manifest");

        let store = FsTemplateStore::new(None);

        let resolved = with_env_vars(
            &[
                (
                    "SCAFFOLDER_HOME",
                    Some(scaffolder_home.path().to_str().expect("utf8 path")),
                ),
                ("XDG_CONFIG_HOME", None),
            ],
            || store.resolve("fromhome"),
        )
        .expect("store name should resolve");

        assert_eq!(resolved, template_path);
    }

    #[test]
    fn missing_template_reports_searched_locations() {
        let template_dir = tempdir().expect("tempdir");
        let store = FsTemplateStore::new(Some(template_dir.path().to_path_buf()));

        let err = with_env_vars(
            &[("SCAFFOLDER_HOME", None), ("XDG_CONFIG_HOME", None)],
            || store.resolve("does-not-exist"),
        )
        .expect_err("missing template should error");

        let message = err.to_string();
        assert!(message.contains("does-not-exist"));
        assert!(message.contains(template_dir.path().to_str().expect("utf8 path")));
    }

    #[test]
    fn rejects_name_with_path_separator() {
        let store = FsTemplateStore::new(None);

        assert!(store.resolve("a/b").is_err());
    }

    #[test]
    fn rejects_parent_dir_component() {
        let store = FsTemplateStore::new(None);

        assert!(store.resolve("..").is_err());
    }

    #[test]
    fn rejects_current_dir_component() {
        let store = FsTemplateStore::new(None);

        assert!(store.resolve(".").is_err());
    }
}
