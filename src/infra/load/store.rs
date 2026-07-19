//! 스토어 조회·생성(XDG·`--template-dir` 우선순위) — `TemplateStore`, `TemplateInitializer`.

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::domain::skeleton::SkeletonEntry;
use crate::domain::store::{TemplateCatalog, TemplateInitializer, TemplateListing, TemplateStore};

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

impl TemplateInitializer for FsTemplateStore {
    fn create(&self, name: &str, entries: &[SkeletonEntry]) -> Result<PathBuf> {
        let bases = self.store_bases();
        let Some(base) = bases.first() else {
            bail!("no store location available");
        };
        std::fs::create_dir_all(base)?;

        let template_root = base.join(name);
        // exists 가드는 어떤 entry도 쓰기 전에 검사한다 — 재실행 시 기존 디렉토리를
        // 부작용 없이 abort하기 위함(디렉토리든 파일이든 이미 있으면 거부).
        if template_root.symlink_metadata().is_ok() {
            bail!(
                "template {name:?} already exists at {}",
                template_root.display()
            );
        }

        for entry in entries {
            let path = template_root.join(&entry.rel);
            match entry.content {
                None => std::fs::create_dir_all(&path)?,
                Some(content) => {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&path, content)?;
                }
            }
        }

        Ok(template_root)
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
mod tests;
