//! Exclusion glob matching for output paths, and the `IgnoreSource` port.

use std::path::Path;

use anyhow::Result;

use crate::domain::answer::AnswerContext;

/// Decides whether an output path is excluded (a computed output path — taken as `Path`,
/// not `RelPath`, since it is checked before `safe_rel_path`). Infra implements gitignore semantics.
pub trait IgnoreMatcher {
    fn is_ignored(&self, rel: &Path) -> bool;
}

/// Port loading `.scaffoldignore`(`.jinja`); implemented by infra.
pub trait IgnoreSource {
    fn load(&self, template_root: &Path, ctx: &AnswerContext) -> Result<Box<dyn IgnoreMatcher>>;
}
