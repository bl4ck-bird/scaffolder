//! Exclusion glob matching for output paths, and the `IgnoreSource` port.

use std::path::Path;

use anyhow::Result;

use crate::domain::answer::AnswerContext;

/// Decides whether a given output path should be excluded. The path is one we have computed for
/// the output, and it is passed as a plain `Path` rather than a validated `RelPath` because this
/// check runs before `safe_rel_path` has had a chance to vet it. Infra implements the gitignore
/// matching semantics.
pub trait IgnoreMatcher {
    fn is_ignored(&self, rel: &Path) -> bool;
}

/// Port loading `.scaffoldignore`(`.jinja`); implemented by infra.
pub trait IgnoreSource {
    fn load(&self, template_root: &Path, ctx: &AnswerContext) -> Result<Box<dyn IgnoreMatcher>>;
}
