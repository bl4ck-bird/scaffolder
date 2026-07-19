//! Store lookup and creation (XDG / `--template-dir` priority) — `TemplateStore`, `TemplateInitializer`.

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::domain::skeleton::SkeletonEntry;
use crate::domain::store::{TemplateCatalog, TemplateInitializer, TemplateListing, TemplateStore};

/// `TemplateStore` searching in the order `--template-dir` > `$SCAFFOLDER_HOME` >
/// `$XDG_CONFIG_HOME/scaffolder` > `~/.scaffolder`.
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
        // Always reject "."/".." even if they exist as directories — keep this guard ahead of the
        // path-like branch so CWD/parent can't be used as an implicit template (prevents out-of-base refs).
        if name_or_path == "." || name_or_path == ".." {
            bail!("template name {name_or_path:?} must be a single path component");
        }

        // A separator means an explicit path: resolve it locally at once, without the store chain.
        if name_or_path.contains('/') {
            let as_path = Path::new(name_or_path);
            return if as_path.is_dir() {
                Ok(as_path.to_path_buf())
            } else {
                bail!("local template path {name_or_path:?} not found or is not a directory");
            };
        }

        // A bare single component is a store-name candidate — walk the priority chain first so
        // --template-dir etc. aren't silently shadowed by a same-named CWD directory (local is a
        // fallback only on a store miss).
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
        // The exists guard runs before writing any entry — to abort with no side effects on a
        // re-run against an existing name (reject whether it's a directory or a file).
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

/// A store-name candidate must be non-empty (separators and `.`/`..` are already excluded by resolve).
fn validate_store_name(name_or_path: &str) -> Result<&str> {
    if name_or_path.is_empty() {
        bail!("template name {name_or_path:?} must be a single path component");
    }
    Ok(name_or_path)
}

#[cfg(test)]
mod tests;
