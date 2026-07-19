//! `.scaffoldignore` loading (+ `.jinja` rendering) — `IgnoreSource`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::domain::answer::AnswerContext;
use crate::domain::ignore::{IgnoreMatcher, IgnoreSource};
use crate::domain::render::Renderer;
use crate::infra::load::trust::ensure_within_root;

const STATIC_NAME: &str = ".scaffoldignore";
const JINJA_NAME: &str = ".scaffoldignore.jinja";

/// `IgnoreSource` loading a static or rendered `.scaffoldignore` with gitignore semantics.
/// External symlinks are rejected without `trust`.
pub struct FsIgnoreSource<'a> {
    renderer: &'a dyn Renderer,
    root_canon: PathBuf,
    trust: bool,
}

impl<'a> FsIgnoreSource<'a> {
    pub fn new(renderer: &'a dyn Renderer, root_canon: PathBuf, trust: bool) -> Self {
        Self {
            renderer,
            root_canon,
            trust,
        }
    }
}

/// `Ok(true)` if `path` is a readable file, `Ok(false)` if absent. If the path exists (including
/// a symlink) but is not a file (e.g. a broken symlink), fail loud — do not silently treat it as
/// "absent" and lose the ignore filter.
fn ignore_file_present(path: &Path) -> Result<bool> {
    if path.is_file() {
        return Ok(true);
    }
    match path.symlink_metadata() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("failed to stat {}", path.display())),
        Ok(_) => bail!(
            "{} exists but could not be read (dangling symlink?)",
            path.display()
        ),
    }
}

impl IgnoreSource for FsIgnoreSource<'_> {
    fn load(&self, template_root: &Path, ctx: &AnswerContext) -> Result<Box<dyn IgnoreMatcher>> {
        let static_path = template_root.join(STATIC_NAME);
        let jinja_path = template_root.join(JINJA_NAME);
        let static_exists = ignore_file_present(&static_path)?;
        let jinja_exists = ignore_file_present(&jinja_path)?;

        let content = match (static_exists, jinja_exists) {
            (true, true) => bail!(
                "both {STATIC_NAME} and {JINJA_NAME} exist in {}; only one is allowed",
                template_root.display()
            ),
            (true, false) => {
                ensure_within_root(&static_path, &self.root_canon, self.trust)?;
                fs::read_to_string(&static_path)
                    .with_context(|| format!("failed to read {}", static_path.display()))?
            }
            (false, true) => {
                ensure_within_root(&jinja_path, &self.root_canon, self.trust)?;
                let template = fs::read_to_string(&jinja_path)
                    .with_context(|| format!("failed to read {}", jinja_path.display()))?;
                self.renderer
                    .render_str(&template, ctx)
                    .with_context(|| format!("failed to render {}", jinja_path.display()))?
            }
            (false, false) => return Ok(Box::new(GitignoreMatcher(Gitignore::empty()))),
        };

        let mut builder = GitignoreBuilder::new(".");
        for line in content.lines() {
            builder.add_line(None, line).with_context(|| {
                format!(
                    "invalid ignore pattern {line:?} in {}",
                    template_root.display()
                )
            })?;
        }
        let gitignore = builder.build().context("failed to build ignore matcher")?;

        Ok(Box::new(GitignoreMatcher(gitignore)))
    }
}

struct GitignoreMatcher(Gitignore);

impl IgnoreMatcher for GitignoreMatcher {
    fn is_ignored(&self, rel: &Path) -> bool {
        self.0.matched_path_or_any_parents(rel, false).is_ignore()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::answer::{ScaffolderBuiltins, build_context};
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn builtins() -> ScaffolderBuiltins {
        ScaffolderBuiltins {
            name: "demo".to_string(),
            target: PathBuf::from("/tmp/demo"),
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
            username: "bl4ckbird".to_string(),
        }
    }

    fn ctx_with_stacks(stacks: Vec<String>) -> AnswerContext {
        let mut answers = BTreeMap::new();
        answers.insert(
            "stacks".to_string(),
            crate::domain::answer::AnswerValue::List(stacks),
        );
        build_context(
            answers,
            Some(crate::domain::data::DataValue::empty_table()),
            builtins(),
        )
    }

    struct NoopRenderer;
    impl Renderer for NoopRenderer {
        fn render_str(&self, _template: &str, _context: &AnswerContext) -> Result<String> {
            unreachable!("static .scaffoldignore must not be rendered")
        }
    }

    #[test]
    fn static_ignore_file_matches_glob_against_output_path() {
        let dir = tempdir().unwrap();
        let root_canon = dir.path().canonicalize().unwrap();
        fs::write(dir.path().join(STATIC_NAME), "*.tmp\n").unwrap();

        let renderer = NoopRenderer;
        let source = FsIgnoreSource::new(&renderer, root_canon, false);
        let ctx = ctx_with_stacks(vec![]);
        let matcher = source.load(dir.path(), &ctx).unwrap();

        assert!(matcher.is_ignored(Path::new("foo.tmp")));
        assert!(!matcher.is_ignored(Path::new("foo.rs")));
    }

    #[test]
    fn jinja_ignore_file_is_rendered_with_answer_context() {
        let dir = tempdir().unwrap();
        let root_canon = dir.path().canonicalize().unwrap();
        fs::write(
            dir.path().join(JINJA_NAME),
            "{% if \"docker\" not in stacks %}Dockerfile{% endif %}\n",
        )
        .unwrap();

        let renderer = crate::infra::render::render::MiniJinjaRenderer::new();
        let source = FsIgnoreSource::new(&renderer, root_canon, false);

        let ctx_without_docker = ctx_with_stacks(vec![]);
        let matcher = source.load(dir.path(), &ctx_without_docker).unwrap();
        assert!(matcher.is_ignored(Path::new("Dockerfile")));

        let ctx_with_docker = ctx_with_stacks(vec!["docker".to_string()]);
        let matcher = source.load(dir.path(), &ctx_with_docker).unwrap();
        assert!(!matcher.is_ignored(Path::new("Dockerfile")));
    }

    #[test]
    fn missing_ignore_file_yields_matcher_that_ignores_nothing() {
        let dir = tempdir().unwrap();
        let root_canon = dir.path().canonicalize().unwrap();

        let renderer = NoopRenderer;
        let source = FsIgnoreSource::new(&renderer, root_canon, false);
        let ctx = ctx_with_stacks(vec![]);
        let matcher = source.load(dir.path(), &ctx).unwrap();

        assert!(!matcher.is_ignored(Path::new("anything.txt")));
    }

    #[test]
    fn static_ignore_file_directory_pattern_excludes_subtree_files() {
        let dir = tempdir().unwrap();
        let root_canon = dir.path().canonicalize().unwrap();
        fs::write(dir.path().join(STATIC_NAME), "build/\n").unwrap();

        let renderer = NoopRenderer;
        let source = FsIgnoreSource::new(&renderer, root_canon, false);
        let ctx = ctx_with_stacks(vec![]);
        let matcher = source.load(dir.path(), &ctx).unwrap();

        assert!(matcher.is_ignored(Path::new("build/x.txt")));
        assert!(!matcher.is_ignored(Path::new("src/main.rs")));
    }

    #[test]
    fn negation_pattern_unexcludes_matched_file() {
        let dir = tempdir().unwrap();
        let root_canon = dir.path().canonicalize().unwrap();
        fs::write(dir.path().join(STATIC_NAME), "*.log\n!keep.log\n").unwrap();

        let renderer = NoopRenderer;
        let source = FsIgnoreSource::new(&renderer, root_canon, false);
        let ctx = ctx_with_stacks(vec![]);
        let matcher = source.load(dir.path(), &ctx).unwrap();

        assert!(matcher.is_ignored(Path::new("a.log")));
        assert!(!matcher.is_ignored(Path::new("keep.log")));
    }

    #[test]
    fn both_static_and_jinja_ignore_files_present_is_an_error() {
        let dir = tempdir().unwrap();
        let root_canon = dir.path().canonicalize().unwrap();
        fs::write(dir.path().join(STATIC_NAME), "*.tmp\n").unwrap();
        fs::write(dir.path().join(JINJA_NAME), "*.tmp\n").unwrap();

        let renderer = NoopRenderer;
        let source = FsIgnoreSource::new(&renderer, root_canon, false);
        let ctx = ctx_with_stacks(vec![]);

        assert!(source.load(dir.path(), &ctx).is_err());
    }

    #[test]
    fn static_ignore_file_via_internal_symlink_is_allowed() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let root_canon = dir.path().canonicalize().unwrap();
        let real = dir.path().join("real.scaffoldignore");
        fs::write(&real, "*.tmp\n").unwrap();
        symlink(&real, dir.path().join(STATIC_NAME)).unwrap();

        let renderer = NoopRenderer;
        let source = FsIgnoreSource::new(&renderer, root_canon, false);
        let ctx = ctx_with_stacks(vec![]);
        let matcher = source.load(dir.path(), &ctx).unwrap();

        assert!(matcher.is_ignored(Path::new("foo.tmp")));
    }

    #[test]
    fn static_ignore_file_via_external_symlink_is_rejected_without_trust() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let root_canon = dir.path().canonicalize().unwrap();
        let outside = tempdir().unwrap();
        let external = outside.path().join(STATIC_NAME);
        fs::write(&external, "*.tmp\n").unwrap();
        symlink(&external, dir.path().join(STATIC_NAME)).unwrap();

        let renderer = NoopRenderer;
        let source = FsIgnoreSource::new(&renderer, root_canon, false);
        let ctx = ctx_with_stacks(vec![]);

        assert!(source.load(dir.path(), &ctx).is_err());
    }

    #[test]
    fn broken_symlink_static_ignore_file_is_rejected() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let root_canon = dir.path().canonicalize().unwrap();
        symlink(dir.path().join("nowhere"), dir.path().join(STATIC_NAME)).unwrap();

        let renderer = NoopRenderer;
        let source = FsIgnoreSource::new(&renderer, root_canon, false);
        let ctx = ctx_with_stacks(vec![]);

        assert!(source.load(dir.path(), &ctx).is_err());
    }

    #[test]
    fn broken_symlink_jinja_ignore_file_is_rejected() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let root_canon = dir.path().canonicalize().unwrap();
        symlink(dir.path().join("nowhere"), dir.path().join(JINJA_NAME)).unwrap();

        let renderer = NoopRenderer;
        let source = FsIgnoreSource::new(&renderer, root_canon, false);
        let ctx = ctx_with_stacks(vec![]);

        assert!(source.load(dir.path(), &ctx).is_err());
    }

    #[test]
    fn static_ignore_file_via_external_symlink_is_allowed_with_trust() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let root_canon = dir.path().canonicalize().unwrap();
        let outside = tempdir().unwrap();
        let external = outside.path().join(STATIC_NAME);
        fs::write(&external, "*.tmp\n").unwrap();
        symlink(&external, dir.path().join(STATIC_NAME)).unwrap();

        let renderer = NoopRenderer;
        let source = FsIgnoreSource::new(&renderer, root_canon, true);
        let ctx = ctx_with_stacks(vec![]);
        let matcher = source.load(dir.path(), &ctx).unwrap();

        assert!(matcher.is_ignored(Path::new("foo.tmp")));
    }
}
