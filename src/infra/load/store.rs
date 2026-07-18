//! 스토어 조회(XDG·`--template-dir` 우선순위) — `TemplateStore`.

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::domain::store::{TemplateCatalog, TemplateListing, TemplateStore};

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
        // "."/".."는 디렉토리로 존재해도 항상 거부한다 — CWD/상위를 암묵적 템플릿으로 못 쓰게
        // 막는 가드를 path-like 분기보다 앞세워 유지한다(base 밖 참조 방지).
        if name_or_path == "." || name_or_path == ".." {
            bail!("template name {name_or_path:?} must be a single path component");
        }

        // 구분자 포함 = 명시적 경로 지정으로 간주해 스토어 체인 없이 로컬로 즉시 판정한다.
        if name_or_path.contains('/') {
            let as_path = Path::new(name_or_path);
            return if as_path.is_dir() {
                Ok(as_path.to_path_buf())
            } else {
                bail!("local template path {name_or_path:?} not found or is not a directory");
            };
        }

        // bare 단일 컴포넌트 = 스토어명 후보 — 우선순위 체인을 먼저 순회해 --template-dir 등이
        // CWD의 동명 디렉토리에 조용히 밀리지 않게 한다(로컬은 스토어 미스 시에만 fallback).
        let name = validate_store_name(name_or_path)?;

        let bases = self.store_bases();
        for base in &bases {
            let candidate = base.join(name);
            if candidate.join("scaffold.toml").is_file() {
                return Ok(candidate);
            }
        }

        let as_path = Path::new(name_or_path);
        if as_path.is_dir() {
            return Ok(as_path.to_path_buf());
        }

        let searched = bases
            .iter()
            .map(|base| base.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        bail!("template {name_or_path:?} not found; searched: [{searched}]");
    }
}

impl TemplateCatalog for FsTemplateStore {
    fn list(&self) -> Result<Vec<TemplateListing>> {
        let mut listings = Vec::new();

        for base in self.store_bases() {
            let Ok(entries) = std::fs::read_dir(&base) else {
                continue;
            };

            let mut candidates: Vec<PathBuf> = entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| path.is_dir() && path.join("scaffold.toml").is_file())
                .collect();
            candidates.sort();

            for path in candidates {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_string)
                    .unwrap_or_default();
                listings.push(TemplateListing {
                    name,
                    path,
                    base: base.clone(),
                });
            }
        }

        Ok(listings)
    }
}

/// 스토어명 후보는 빈 문자열이 아니어야 한다(구분자·`.`/`..`는 resolve에서 이미 배제됨).
fn validate_store_name(name_or_path: &str) -> Result<&str> {
    if name_or_path.is_empty() {
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
    fn xdg_config_home_is_used_when_no_scaffolder_home_or_template_dir() {
        let xdg_home = tempdir().expect("tempdir");
        let template_path = xdg_home.path().join("scaffolder").join("fromxdg");
        std::fs::create_dir_all(&template_path).expect("create template dir");
        std::fs::write(template_path.join("scaffold.toml"), "").expect("write manifest");

        let store = FsTemplateStore::new(None);

        let resolved = with_env_vars(
            &[
                ("SCAFFOLDER_HOME", None),
                (
                    "XDG_CONFIG_HOME",
                    Some(xdg_home.path().to_str().expect("utf8 path")),
                ),
            ],
            || store.resolve("fromxdg"),
        )
        .expect("store name should resolve");

        assert_eq!(resolved, template_path);
    }

    #[test]
    fn scaffolder_home_takes_priority_over_xdg_config_home() {
        let scaffolder_home = tempdir().expect("tempdir");
        let xdg_home = tempdir().expect("tempdir");

        let winner = scaffolder_home.path().join("shared");
        std::fs::create_dir_all(&winner).expect("create winner dir");
        std::fs::write(winner.join("scaffold.toml"), "").expect("write manifest");

        let loser = xdg_home.path().join("scaffolder").join("shared");
        std::fs::create_dir_all(&loser).expect("create loser dir");
        std::fs::write(loser.join("scaffold.toml"), "").expect("write manifest");

        let store = FsTemplateStore::new(None);

        let resolved = with_env_vars(
            &[
                (
                    "SCAFFOLDER_HOME",
                    Some(scaffolder_home.path().to_str().expect("utf8 path")),
                ),
                (
                    "XDG_CONFIG_HOME",
                    Some(xdg_home.path().to_str().expect("utf8 path")),
                ),
            ],
            || store.resolve("shared"),
        )
        .expect("store name should resolve");

        assert_eq!(resolved, winner);
    }

    #[test]
    fn empty_scaffolder_home_is_skipped_in_favor_of_next_tier() {
        let xdg_home = tempdir().expect("tempdir");
        let template_path = xdg_home.path().join("scaffolder").join("fromxdg");
        std::fs::create_dir_all(&template_path).expect("create template dir");
        std::fs::write(template_path.join("scaffold.toml"), "").expect("write manifest");

        let store = FsTemplateStore::new(None);

        let resolved = with_env_vars(
            &[
                ("SCAFFOLDER_HOME", Some("")),
                (
                    "XDG_CONFIG_HOME",
                    Some(xdg_home.path().to_str().expect("utf8 path")),
                ),
            ],
            || store.resolve("fromxdg"),
        )
        .expect("empty SCAFFOLDER_HOME should be skipped, resolving via XDG_CONFIG_HOME");

        assert_eq!(resolved, template_path);
    }

    #[test]
    fn missing_template_reports_searched_locations() {
        let template_dir = tempdir().expect("tempdir");
        // dirs::home_dir()는 $HOME을 읽으므로, 실제 개발자 홈에 우연히 같은 이름의 스토어
        // 엔트리가 있어도 이 테스트가 오염되지 않게 가짜 홈으로 격리한다.
        let fake_home = tempdir().expect("tempdir");
        let store = FsTemplateStore::new(Some(template_dir.path().to_path_buf()));

        let err = with_env_vars(
            &[
                ("SCAFFOLDER_HOME", None),
                ("XDG_CONFIG_HOME", None),
                ("HOME", Some(fake_home.path().to_str().expect("utf8 path"))),
            ],
            || store.resolve("does-not-exist"),
        )
        .expect_err("missing template should error");

        let message = err.to_string();
        assert!(message.contains("does-not-exist"));
        assert!(message.contains(template_dir.path().to_str().expect("utf8 path")));
    }

    #[test]
    fn falls_through_to_scaffolder_home_when_name_absent_from_template_dir() {
        let template_dir = tempdir().expect("tempdir");
        let scaffolder_home = tempdir().expect("tempdir");

        let template_path = scaffolder_home.path().join("onlyhome");
        std::fs::create_dir_all(&template_path).expect("create template dir");
        std::fs::write(template_path.join("scaffold.toml"), "").expect("write manifest");

        let store = FsTemplateStore::new(Some(template_dir.path().to_path_buf()));

        let resolved = with_env_vars(
            &[
                (
                    "SCAFFOLDER_HOME",
                    Some(scaffolder_home.path().to_str().expect("utf8 path")),
                ),
                ("XDG_CONFIG_HOME", None),
            ],
            || store.resolve("onlyhome"),
        )
        .expect("template-dir miss should fall through to SCAFFOLDER_HOME");

        assert_eq!(resolved, template_path);
    }

    #[test]
    fn path_like_missing_local_directory_gives_local_path_error_not_store_name_error() {
        // TempDir이 스코프를 벗어나며 자체 정리되므로, 그 경로는 확실히 존재하지 않는다.
        let vanished = tempdir().expect("tempdir").path().join("gone");
        let vanished_str = vanished.to_str().expect("utf8 path").to_string();
        let store = FsTemplateStore::new(None);

        let err = store
            .resolve(&vanished_str)
            .expect_err("missing local path should error");

        let message = err.to_string();
        assert!(message.contains("local template path"));
        assert!(!message.contains("single path component"));
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

    #[test]
    fn list_enumerates_templates_across_bases_in_priority_order() {
        let template_dir = tempdir().expect("tempdir");
        let scaffolder_home = tempdir().expect("tempdir");
        let fake_home = tempdir().expect("tempdir");

        let from_template_dir = template_dir.path().join("alpha");
        std::fs::create_dir_all(&from_template_dir).expect("create template dir");
        std::fs::write(from_template_dir.join("scaffold.toml"), "").expect("write manifest");

        let from_scaffolder_home = scaffolder_home.path().join("beta");
        std::fs::create_dir_all(&from_scaffolder_home).expect("create template dir");
        std::fs::write(from_scaffolder_home.join("scaffold.toml"), "").expect("write manifest");

        let store = FsTemplateStore::new(Some(template_dir.path().to_path_buf()));

        let listings = with_env_vars(
            &[
                (
                    "SCAFFOLDER_HOME",
                    Some(scaffolder_home.path().to_str().expect("utf8 path")),
                ),
                ("XDG_CONFIG_HOME", None),
                ("HOME", Some(fake_home.path().to_str().expect("utf8 path"))),
            ],
            || store.list(),
        )
        .expect("list should succeed");

        assert_eq!(listings.len(), 2);
        assert_eq!(listings[0].name, "alpha");
        assert_eq!(listings[0].path, from_template_dir);
        assert_eq!(listings[0].base, template_dir.path());
        assert_eq!(listings[1].name, "beta");
        assert_eq!(listings[1].path, from_scaffolder_home);
        assert_eq!(listings[1].base, scaffolder_home.path());
    }

    #[test]
    fn list_excludes_directories_without_scaffold_toml() {
        let template_dir = tempdir().expect("tempdir");
        let fake_home = tempdir().expect("tempdir");

        let valid = template_dir.path().join("valid");
        std::fs::create_dir_all(&valid).expect("create template dir");
        std::fs::write(valid.join("scaffold.toml"), "").expect("write manifest");

        let not_a_template = template_dir.path().join("not-a-template");
        std::fs::create_dir_all(&not_a_template).expect("create plain dir");

        let store = FsTemplateStore::new(Some(template_dir.path().to_path_buf()));

        let listings = with_env_vars(
            &[
                ("SCAFFOLDER_HOME", None),
                ("XDG_CONFIG_HOME", None),
                ("HOME", Some(fake_home.path().to_str().expect("utf8 path"))),
            ],
            || store.list(),
        )
        .expect("list should succeed");

        assert_eq!(listings.len(), 1);
        assert_eq!(listings[0].name, "valid");
    }

    #[test]
    fn list_skips_nonexistent_base_without_error() {
        let template_dir = tempdir().expect("tempdir");
        let missing_scaffolder_home = template_dir.path().join("does-not-exist");
        let fake_home = tempdir().expect("tempdir");

        let store = FsTemplateStore::new(Some(template_dir.path().to_path_buf()));

        let listings = with_env_vars(
            &[
                (
                    "SCAFFOLDER_HOME",
                    Some(missing_scaffolder_home.to_str().expect("utf8 path")),
                ),
                ("XDG_CONFIG_HOME", None),
                ("HOME", Some(fake_home.path().to_str().expect("utf8 path"))),
            ],
            || store.list(),
        )
        .expect("missing base should be skipped, not an error");

        assert!(listings.is_empty());
    }
}
