//! 커스텀 필터: heck case + slug + `dedup_lines`.

use std::collections::HashSet;

use heck::{
    ToKebabCase, ToLowerCamelCase, ToShoutySnakeCase, ToSnakeCase, ToTitleCase, ToUpperCamelCase,
};
use minijinja::Environment;

/// case 필터와 `slug`·`dedup_lines`를 `Environment`에 등록한다. 렌더와 `when` 표현식 평가가
/// 공유하는 `base_environment`에서 호출한다.
pub(crate) fn register(env: &mut Environment<'static>) {
    env.add_filter("snake_case", |s: String| s.to_snake_case());
    env.add_filter("kebab_case", |s: String| s.to_kebab_case());
    env.add_filter("pascal_case", |s: String| s.to_upper_camel_case());
    env.add_filter("camel_case", |s: String| s.to_lower_camel_case());
    env.add_filter("shouty_snake_case", |s: String| s.to_shouty_snake_case());
    env.add_filter("title_case", |s: String| s.to_title_case());
    env.add_filter("slug", |s: String| slug(&s));
    env.add_filter("dedup_lines", |s: String| dedup_lines(&s));
}

/// 영숫자 run을 유지(소문자화)하고 그 사이 비영숫자 run을 단일 `-`로 접는다. 양끝 `-`는 제거한다.
fn slug(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut pending_sep = false;
    for ch in input.chars() {
        if ch.is_alphanumeric() {
            if pending_sep && !out.is_empty() {
                out.push('-');
            }
            pending_sep = false;
            out.extend(ch.to_lowercase());
        } else {
            pending_sep = true;
        }
    }
    out
}

/// 비어 있지 않은 라인의 첫 등장만 남긴다(전역 dedup, 순서 보존). 빈 라인은 구조적 구분자이므로
/// 보존한다. trailing newline은 round-trip한다.
fn dedup_lines(input: &str) -> String {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut kept: Vec<&str> = Vec::new();
    for line in input.split('\n') {
        // CRLF payload는 라인 끝에 `\r`을 남긴다. dedup 키에서 `\r`을 제외해 LF/CRLF 혼재
        // (예: LF payload + CRLF partial)에서도 같은 라인으로 취급한다. 출력은 원본을 보존한다.
        let key = line.strip_suffix('\r').unwrap_or(line);
        if key.is_empty() || seen.insert(key) {
            kept.push(line);
        }
    }
    kept.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(template: &str) -> String {
        let mut env = Environment::new();
        register(&mut env);
        env.render_str(template, ()).expect("render")
    }

    #[test]
    fn case_filters_convert_identifiers() {
        assert_eq!(render("{{ 'HelloWorld' | snake_case }}"), "hello_world");
        assert_eq!(render("{{ 'HelloWorld' | kebab_case }}"), "hello-world");
        assert_eq!(render("{{ 'hello_world' | pascal_case }}"), "HelloWorld");
        assert_eq!(render("{{ 'hello_world' | camel_case }}"), "helloWorld");
        assert_eq!(
            render("{{ 'helloWorld' | shouty_snake_case }}"),
            "HELLO_WORLD"
        );
        assert_eq!(render("{{ 'hello_world' | title_case }}"), "Hello World");
    }

    #[test]
    fn slug_lowercases_and_collapses_separators() {
        assert_eq!(slug("Hello, World!"), "hello-world");
        assert_eq!(slug("  a__b  "), "a-b");
        assert_eq!(slug("already-slug"), "already-slug");
        assert_eq!(slug("MixedCASE123"), "mixedcase123");
        assert_eq!(slug("!!!"), "");
    }

    #[test]
    fn slug_filter_in_template() {
        assert_eq!(render("{{ 'My Project!' | slug }}"), "my-project");
    }

    #[test]
    fn dedup_lines_keeps_first_occurrence_globally() {
        assert_eq!(dedup_lines("/target\n/target\n/foo\n"), "/target\n/foo\n");
        assert_eq!(dedup_lines("a\nb\na\nc\nb\n"), "a\nb\nc\n");
    }

    #[test]
    fn dedup_lines_preserves_blank_lines() {
        assert_eq!(dedup_lines("a\n\nb\n\na\n"), "a\n\nb\n\n");
    }

    #[test]
    fn dedup_lines_treats_crlf_and_lf_as_same_line() {
        // CRLF `/target\r`와 LF `/target`은 같은 라인으로 dedup되고, 출력은 첫 등장(원본)을 보존한다.
        assert_eq!(dedup_lines("/target\r\n/target\n/log"), "/target\r\n/log");
    }

    #[test]
    fn dedup_lines_filter_in_template() {
        let out = render("{% filter dedup_lines %}/target\n/target\n/log\n{% endfilter %}");
        assert_eq!(out, "/target\n/log\n");
    }
}
