//! 훅 오케스트레이션: 인라인 `when` 판정, confirm 설명 조립, phase별 실행(인라인 선언 순서 →
//! 폴더 스크립트 lexical). `pipeline`은 이 모듈을 호출해 포트 배선만 담당한다.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::domain::answer::{AnswerContext, ConditionEvaluator};
use crate::domain::hook::{Hook, HookRunner, HookScript};
use crate::domain::render::Renderer;

/// 인라인 훅 중 active한 것만 선언 순서로 남긴다. `when` 없으면 active, 있으면 질문의 `when`과
/// 같은 evaluator로 판정한다.
pub fn collect_active_inline<'a>(
    hooks: &'a [Hook],
    ctx: &AnswerContext,
    evaluator: &dyn ConditionEvaluator,
) -> Result<Vec<&'a Hook>> {
    let mut active = Vec::with_capacity(hooks.len());
    for hook in hooks {
        let is_active = match &hook.when {
            Some(when) => evaluator.is_active(when, ctx)?,
            None => true,
        };
        if is_active {
            active.push(hook);
        }
    }
    Ok(active)
}

/// 부작용 전 단일 confirm 게이트에 쓸 설명. before/after 각각 인라인(선언 순서) →
/// 폴더 스크립트(lexical) 순으로 한 줄씩 나열한다.
pub fn confirm_description(
    before_inline: &[&Hook],
    before_scripts: &[HookScript],
    after_inline: &[&Hook],
    after_scripts: &[HookScript],
) -> String {
    let mut lines = Vec::new();
    push_phase_lines(&mut lines, "before", before_inline, before_scripts);
    push_phase_lines(&mut lines, "after", after_inline, after_scripts);
    lines.join("\n")
}

fn push_phase_lines(lines: &mut Vec<String>, phase: &str, inline: &[&Hook], scripts: &[HookScript]) {
    for hook in inline {
        lines.push(format!("{phase}: {}", sanitize_for_display(&hook.run)));
    }
    for script in scripts {
        lines.push(format!("{phase}: run {}", sanitize_for_display(script_name(script))));
    }
}

fn script_name(script: &HookScript) -> &str {
    match script {
        HookScript::Executable { name, .. } => name,
        HookScript::Template { name, .. } => name,
    }
}

/// confirm 프롬프트는 임의 코드 실행 앞의 유일 방어선이라, untrusted 템플릿 저자가 넣은
/// 제어문자(CR·ANSI 이스케이프 등)로 터미널 표시를 스푸핑하지 못하게 이스케이프한다. printable
/// 문자와 비-ASCII는 그대로 보존한다(`str::escape_default`는 이들까지 과도하게 이스케이프하므로
/// 쓰지 않는다).
fn sanitize_for_display(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

/// 한 phase를 실행한다: 인라인(선언 순서) 먼저, 그 다음 폴더 스크립트(이미 lexical). Template
/// 스크립트는 `renderer`로 렌더 후 `run_rendered`로 실행한다.
pub fn run_phase(
    runner: &dyn HookRunner,
    renderer: &dyn Renderer,
    ctx: &AnswerContext,
    inline: &[&Hook],
    scripts: &[HookScript],
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> Result<()> {
    for hook in inline {
        runner.run_inline(&hook.run, cwd, env)?;
    }
    for script in scripts {
        match script {
            HookScript::Executable { path, .. } => runner.run_script_file(path, cwd, env)?,
            HookScript::Template { name, raw } => {
                let rendered = renderer.render_str(raw, ctx)?;
                runner.run_rendered(name, rendered.as_bytes(), cwd, env)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::answer::{build_context, ScaffolderBuiltins};
    use crate::domain::data::DataValue;
    use std::cell::RefCell;
    use std::path::PathBuf;

    fn builtins() -> ScaffolderBuiltins {
        ScaffolderBuiltins {
            name: "demo".to_string(),
            target: PathBuf::from("/tmp/demo"),
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
            username: "tester".to_string(),
        }
    }

    fn ctx() -> AnswerContext {
        build_context(BTreeMap::new(), Some(DataValue::empty_table()), builtins())
    }

    fn hook(run: &str, when: Option<&str>) -> Hook {
        Hook { when: when.map(|w| w.to_string()), run: run.to_string() }
    }

    struct FixedConditionEvaluator(std::collections::HashMap<String, bool>);
    impl ConditionEvaluator for FixedConditionEvaluator {
        fn is_active(&self, when: &str, _ctx: &AnswerContext) -> Result<bool> {
            Ok(*self.0.get(when).unwrap_or(&false))
        }
    }

    #[test]
    fn collect_active_inline_keeps_no_when_and_active_when_in_declared_order() {
        let hooks = vec![hook("a", None), hook("b", Some("gate")), hook("c", None)];
        let evaluator = FixedConditionEvaluator(
            [("gate".to_string(), true)].into_iter().collect(),
        );

        let active = collect_active_inline(&hooks, &ctx(), &evaluator).expect("collect");

        let runs: Vec<&str> = active.iter().map(|h| h.run.as_str()).collect();
        assert_eq!(runs, vec!["a", "b", "c"]);
    }

    #[test]
    fn collect_active_inline_drops_inactive_when() {
        let hooks = vec![hook("a", Some("gate")), hook("b", None)];
        let evaluator = FixedConditionEvaluator(std::collections::HashMap::new());

        let active = collect_active_inline(&hooks, &ctx(), &evaluator).expect("collect");

        let runs: Vec<&str> = active.iter().map(|h| h.run.as_str()).collect();
        assert_eq!(runs, vec!["b"]);
    }

    #[test]
    fn confirm_description_lists_before_then_after_inline_then_folder() {
        let before_a = hook("echo a", None);
        let after_a = hook("echo z", None);
        let before_inline = vec![&before_a];
        let after_inline = vec![&after_a];
        let before_scripts = vec![HookScript::Executable {
            name: "10-setup.sh".to_string(),
            path: PathBuf::from("/tpl/hooks/before/10-setup.sh"),
        }];

        let desc = confirm_description(&before_inline, &before_scripts, &after_inline, &[]);

        assert_eq!(
            desc,
            "before: echo a\nbefore: run 10-setup.sh\nafter: echo z"
        );
    }

    #[test]
    fn confirm_description_escapes_control_chars_in_hook_run() {
        let spoofed = hook("echo a\x1b[2K\rmalicious", None);
        let before_inline = vec![&spoofed];

        let desc = confirm_description(&before_inline, &[], &[], &[]);

        assert!(!desc.contains('\x1b'), "raw ESC must not reach the terminal: {desc:?}");
        assert!(!desc.contains('\r'), "raw CR must not reach the terminal: {desc:?}");
        assert!(desc.contains("\\u{1b}") || desc.contains("\\x1b"), "escaped ESC must be visible: {desc:?}");
        assert!(desc.contains("\\r"), "escaped CR must be visible: {desc:?}");
    }

    #[test]
    fn confirm_description_escapes_control_chars_in_script_name() {
        let script = HookScript::Executable {
            name: "10-setup\x1b[2K\r.sh".to_string(),
            path: PathBuf::from("/tpl/hooks/before/10-setup.sh"),
        };

        let desc = confirm_description(&[], &[script], &[], &[]);

        assert!(!desc.contains('\x1b'), "raw ESC must not reach the terminal: {desc:?}");
        assert!(!desc.contains('\r'), "raw CR must not reach the terminal: {desc:?}");
    }

    struct RecordingRunner {
        calls: RefCell<Vec<String>>,
    }
    impl RecordingRunner {
        fn new() -> Self {
            Self { calls: RefCell::new(Vec::new()) }
        }
    }
    impl HookRunner for RecordingRunner {
        fn run_inline(&self, command: &str, _cwd: &Path, _env: &BTreeMap<String, String>) -> Result<()> {
            self.calls.borrow_mut().push(format!("inline:{command}"));
            Ok(())
        }
        fn run_script_file(&self, path: &Path, _cwd: &Path, _env: &BTreeMap<String, String>) -> Result<()> {
            self.calls.borrow_mut().push(format!("script:{}", path.display()));
            Ok(())
        }
        fn run_rendered(
            &self,
            name: &str,
            content: &[u8],
            _cwd: &Path,
            _env: &BTreeMap<String, String>,
        ) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(format!("rendered:{name}:{}", String::from_utf8_lossy(content)));
            Ok(())
        }
    }

    struct FakeRenderer;
    impl Renderer for FakeRenderer {
        fn render_str(&self, template: &str, _context: &AnswerContext) -> Result<String> {
            Ok(format!("rendered:{template}"))
        }
    }

    #[test]
    fn run_phase_runs_inline_before_folder_scripts_in_order() {
        let a = hook("cmd-a", None);
        let b = hook("cmd-b", None);
        let inline = vec![&a, &b];
        let scripts = vec![
            HookScript::Executable { name: "z.sh".to_string(), path: PathBuf::from("/tpl/hooks/before/z.sh") },
            HookScript::Template { name: "y.sh".to_string(), raw: "raw-content".to_string() },
        ];
        let runner = RecordingRunner::new();

        run_phase(
            &runner,
            &FakeRenderer,
            &ctx(),
            &inline,
            &scripts,
            Path::new("/target"),
            &BTreeMap::new(),
        )
        .expect("run_phase");

        assert_eq!(
            *runner.calls.borrow(),
            vec![
                "inline:cmd-a".to_string(),
                "inline:cmd-b".to_string(),
                "script:/tpl/hooks/before/z.sh".to_string(),
                "rendered:y.sh:rendered:raw-content".to_string(),
            ]
        );
    }

    #[test]
    fn run_phase_propagates_inline_failure_and_stops() {
        struct FailingRunner;
        impl HookRunner for FailingRunner {
            fn run_inline(&self, _command: &str, _cwd: &Path, _env: &BTreeMap<String, String>) -> Result<()> {
                anyhow::bail!("boom")
            }
            fn run_script_file(&self, _path: &Path, _cwd: &Path, _env: &BTreeMap<String, String>) -> Result<()> {
                panic!("must not run script after inline failure");
            }
            fn run_rendered(
                &self,
                _name: &str,
                _content: &[u8],
                _cwd: &Path,
                _env: &BTreeMap<String, String>,
            ) -> Result<()> {
                panic!("must not run rendered after inline failure");
            }
        }

        let a = hook("cmd-a", None);
        let inline = vec![&a];
        let scripts = vec![HookScript::Executable {
            name: "z.sh".to_string(),
            path: PathBuf::from("/tpl/hooks/before/z.sh"),
        }];

        let result = run_phase(
            &FailingRunner,
            &FakeRenderer,
            &ctx(),
            &inline,
            &scripts,
            Path::new("/target"),
            &BTreeMap::new(),
        );

        assert!(result.is_err());
    }
}
