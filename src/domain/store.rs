//! Template store ports: `TemplateStore`, `TemplateInitializer`, `SourceRootSource`,
//! `TemplateCatalog`.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::domain::skeleton::SkeletonEntry;

/// Port resolving a store name or local path to a template root.
pub trait TemplateStore {
    fn resolve(&self, name_or_path: &str) -> Result<PathBuf>;
}

/// Port creating a new template skeleton in the store. An already-existing name must error
/// with no side effects (re-run safety for `template new`).
pub trait TemplateInitializer {
    fn create(&self, name: &str, entries: &[SkeletonEntry]) -> Result<PathBuf>;
}

/// Port resolving `.scaffoldroot` to the effective source root.
pub trait SourceRootSource {
    fn resolve(&self, template_root: &Path) -> Result<PathBuf>;
}

/// Port enumerating template directories across the store bases.
pub trait TemplateCatalog {
    fn list(&self) -> Result<Vec<TemplateListing>>;
}

/// One enumerated template — name, root path, and owning base.
///
/// Duplicate names across bases are all returned (disambiguated by base); dedup and
/// priority display are the presentation layer's job.
pub struct TemplateListing {
    pub name: String,
    pub path: PathBuf,
    pub base: PathBuf,
}

/// Validates that `name` is a single path component: rejects empty, path separators, and
/// `.`/`..`. Matches the rule `FsTemplateStore::resolve` enforces (that is for looking up an
/// existing entry; this validates a new name for `template new`).
pub fn validate_template_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        bail!("template name {name:?} must be a single path component");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_name() {
        assert!(validate_template_name("").is_err());
    }

    #[test]
    fn rejects_path_separator() {
        assert!(validate_template_name("a/b").is_err());
    }

    #[test]
    fn rejects_current_dir_component() {
        assert!(validate_template_name(".").is_err());
    }

    #[test]
    fn rejects_parent_dir_component() {
        assert!(validate_template_name("..").is_err());
    }

    #[test]
    fn accepts_single_component_name() {
        assert!(validate_template_name("my-template").is_ok());
        assert!(validate_template_name("rust_starter").is_ok());
    }
}
