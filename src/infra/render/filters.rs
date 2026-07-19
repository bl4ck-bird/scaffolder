//! Custom filters: heck case conversions, `slug`, and `dedup_lines`.

use std::collections::HashSet;

use heck::{
    ToKebabCase, ToLowerCamelCase, ToShoutySnakeCase, ToSnakeCase, ToTitleCase, ToUpperCamelCase,
};
use minijinja::Environment;

/// Registers the case filters plus `slug` and `dedup_lines` on the `Environment`. Called from
/// `base_environment`, shared by rendering and `when` expression evaluation.
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

/// Keeps alphanumeric runs (lowercased) and folds non-alphanumeric runs between them into a
/// single `-`; trims leading/trailing `-`.
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

/// Keeps the first occurrence of each non-empty line (global dedup, order preserved). Blank
/// lines are structural separators and are kept. A trailing newline round-trips.
fn dedup_lines(input: &str) -> String {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut kept: Vec<&str> = Vec::new();
    for line in input.split('\n') {
        // A CRLF file leaves a trailing `\r` on each line. Strip it before using the line as the
        // dedup key, so a line that appears once with LF endings and once with CRLF endings (for
        // example an LF payload that includes a CRLF partial) counts as the same line. The output
        // still keeps whichever ending the original line had.
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
    fn every_case_filter_is_registered_on_the_environment() {
        for name in [
            "snake_case",
            "kebab_case",
            "pascal_case",
            "camel_case",
            "shouty_snake_case",
            "title_case",
        ] {
            let rendered = render(&format!("{{{{ 'HelloWorld' | {name} }}}}"));
            assert!(!rendered.is_empty(), "{name} produced no output");
        }
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
        // CRLF `/target\r` and LF `/target` dedup as the same line; the output keeps the first (original).
        assert_eq!(dedup_lines("/target\r\n/target\n/log"), "/target\r\n/log");
    }

    #[test]
    fn dedup_lines_filter_in_template() {
        let out = render("{% filter dedup_lines %}/target\n/target\n/log\n{% endfilter %}");
        assert_eq!(out, "/target\n/log\n");
    }
}
