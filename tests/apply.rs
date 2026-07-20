//! End-to-end `scaffolder apply`: rendered/verbatim placement, overwrite confirm, dry-run.

use std::fs;

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;

mod common;

fn write_template(dir: &std::path::Path) {
    fs::write(
        dir.join("scaffold.toml"),
        r#"
            [[questions]]
            name = "project"
            type = "string"
        "#,
    )
    .expect("write scaffold.toml");

    let files = dir.join("files");
    fs::create_dir_all(files.join("src")).expect("mkdir files/src");
    fs::write(files.join("README.md.jinja"), "# {{ project }}").expect("write README.md.jinja");
    fs::write(files.join("src/main.rs"), "fn main(){}").expect("write src/main.rs");
}

/// Creates a resolvable store template at `store_dir/name`.
fn write_store_template(store_dir: &std::path::Path, name: &str) {
    let template_dir = store_dir.join(name);
    fs::create_dir_all(&template_dir).expect("mkdir store template dir");
    write_template(&template_dir);
}

/// Minimal template with no questions that places only `files/marker.txt` (content = `marker`) —
/// used to tell which of two candidate templates was actually applied by the placed file's content.
fn write_marker_template(dir: &std::path::Path, marker: &str) {
    fs::write(dir.join("scaffold.toml"), "").expect("write scaffold.toml");
    let files = dir.join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("marker.txt"), marker).expect("write marker.txt");
}

#[test]
fn apply_template_dir_resolves_store_name_and_writes_files() {
    let store_dir = tempfile::tempdir().expect("store tempdir");
    write_store_template(store_dir.path(), "mystore");
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .env("SCAFFOLDER_HOME", "")
        .env("XDG_CONFIG_HOME", "")
        .arg("apply")
        .arg("mystore")
        .arg(&target)
        .arg("--template-dir")
        .arg(store_dir.path())
        .arg("--answers")
        .arg("project=demo");

    cmd.assert().success();

    let readme = fs::read_to_string(target.join("README.md")).expect("read README.md");
    assert_eq!(readme, "# demo");

    let main_rs = fs::read_to_string(target.join("src/main.rs")).expect("read src/main.rs");
    assert_eq!(main_rs, "fn main(){}");
}

#[test]
fn apply_template_dir_missing_store_name_fails_with_searched_locations() {
    let store_dir = tempfile::tempdir().expect("store tempdir");
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    // Isolated stand-in for the developer's real home so an ambient ~/.scaffolder/ghost
    // can't make this "missing" case unexpectedly resolve.
    let fake_home = tempfile::tempdir().expect("fake home tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = common::scaffolder(fake_home.path());
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg("ghost")
        .arg(&target)
        .arg("--template-dir")
        .arg(store_dir.path());

    cmd.assert()
        .failure()
        .stderr(contains("ghost"))
        .stderr(contains(store_dir.path().to_str().expect("utf8 path")));

    assert!(
        !target.exists(),
        "missing template must not create the target directory"
    );
}

/// Regression: a bare store name shadowed by a same-named CWD directory would silently bypass
/// `--template-dir` — the store chain must win over CWD shadowing.
#[test]
fn apply_bare_store_name_wins_over_cwd_shadow_directory() {
    let store_dir = tempfile::tempdir().expect("store tempdir");
    let store_template = store_dir.path().join("shared");
    fs::create_dir_all(&store_template).expect("mkdir store template dir");
    write_marker_template(&store_template, "store");

    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let local_shadow = workdir.path().join("shared");
    fs::create_dir_all(&local_shadow).expect("mkdir local shadow dir");
    write_marker_template(&local_shadow, "local");

    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .env("SCAFFOLDER_HOME", "")
        .env("XDG_CONFIG_HOME", "")
        .arg("apply")
        .arg("shared")
        .arg(&target)
        .arg("--template-dir")
        .arg(store_dir.path());

    cmd.assert().success();

    let marker = fs::read_to_string(target.join("marker.txt")).expect("read marker.txt");
    assert_eq!(
        marker, "store",
        "--template-dir store must win over a CWD directory sharing the bare template name"
    );
}

/// When a bare name is in no store, it falls back to a same-named CWD directory (back-compat).
#[test]
fn apply_bare_name_falls_back_to_cwd_directory_when_absent_from_stores() {
    let store_dir = tempfile::tempdir().expect("store tempdir");
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let fake_home = tempfile::tempdir().expect("fake home tempdir");
    let local_template = workdir.path().join("localtpl");
    fs::create_dir_all(&local_template).expect("mkdir local template dir");
    write_marker_template(&local_template, "local");

    let target = workdir.path().join("demo");

    let mut cmd = common::scaffolder(fake_home.path());
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg("localtpl")
        .arg(&target)
        .arg("--template-dir")
        .arg(store_dir.path());

    cmd.assert().success();

    let marker = fs::read_to_string(target.join("marker.txt")).expect("read marker.txt");
    assert_eq!(marker, "local");
}

#[test]
fn apply_renders_and_writes_files() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_template(template.path());

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--answers").arg("project=demo");

    fx.cmd.assert().success();

    let readme = fs::read_to_string(fx.target.join("README.md")).expect("read README.md");
    assert_eq!(readme, "# demo");

    let main_rs = fs::read_to_string(fx.target.join("src/main.rs")).expect("read src/main.rs");
    assert_eq!(main_rs, "fn main(){}");
}

#[test]
fn apply_without_force_fails_on_existing_destination_noninteractively() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_template(template.path());

    let mut fx = common::apply(template.path());
    fs::create_dir_all(&fx.target).expect("mkdir target");
    fs::write(fx.target.join("README.md"), "existing").expect("seed existing README.md");
    fx.cmd.arg("--answers").arg("project=demo");

    fx.cmd.assert().failure();

    let readme = fs::read_to_string(fx.target.join("README.md")).expect("read README.md");
    assert_eq!(readme, "existing", "unapproved overwrite must not happen");
}

#[test]
fn apply_with_force_overwrites_existing_destination() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_template(template.path());

    let mut fx = common::apply(template.path());
    fs::create_dir_all(&fx.target).expect("mkdir target");
    fs::write(fx.target.join("README.md"), "existing").expect("seed existing README.md");
    fx.cmd.arg("--answers").arg("project=demo").arg("--force");

    fx.cmd.assert().success();

    let readme = fs::read_to_string(fx.target.join("README.md")).expect("read README.md");
    assert_eq!(readme, "# demo");
}

#[test]
fn apply_dry_run_does_not_write_and_prints_plan() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_template(template.path());

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--answers").arg("project=demo").arg("--dry-run");

    fx.cmd.assert().success().stdout(contains("README.md"));

    assert!(
        !fx.target.exists(),
        "dry-run must not create the target directory"
    );
}

/// Render template with `project` (has default) / `port` (int, no default) / `verbose` (bool,
/// has default) questions — exercises type preservation.
fn write_multi_type_template(dir: &std::path::Path) {
    fs::write(
        dir.join("scaffold.toml"),
        r#"
            [[questions]]
            name = "project"
            type = "string"
            default = "demo"

            [[questions]]
            name = "port"
            type = "int"

            [[questions]]
            name = "verbose"
            type = "boolean"
            default = false
        "#,
    )
    .expect("write scaffold.toml");

    let files = dir.join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(
        files.join("config.txt.jinja"),
        "{{ project }}:{% if port >= 3000 %}high{% else %}low{% endif %}:{% if verbose %}v{% else %}q{% endif %}",
    )
    .expect("write config.txt.jinja");
}

fn write_answers_toml(dir: &std::path::Path, contents: &str) -> std::path::PathBuf {
    let path = dir.join("answers.toml");
    fs::write(&path, contents).expect("write answers.toml");
    path
}

#[test]
fn apply_answers_flag_preserves_int_and_boolean_types() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_multi_type_template(template.path());

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--answers").arg("port=8080");

    fx.cmd.assert().success();

    let content = fs::read_to_string(fx.target.join("config.txt")).expect("read config.txt");
    assert_eq!(content, "demo:high:q");
}

#[test]
fn apply_answers_file_supplies_typed_values() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_multi_type_template(template.path());

    let mut fx = common::apply(template.path());
    let answers_path = write_answers_toml(
        fx.workdir(),
        r#"
            project = "fileproj"
            port = 2000
            verbose = true
        "#,
    );
    fx.cmd.arg("--answers-file").arg(&answers_path);

    fx.cmd.assert().success();

    let content = fs::read_to_string(fx.target.join("config.txt")).expect("read config.txt");
    assert_eq!(content, "fileproj:low:v");
}

#[test]
fn apply_answers_flag_overrides_answers_file() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_multi_type_template(template.path());

    let mut fx = common::apply(template.path());
    let answers_path = write_answers_toml(
        fx.workdir(),
        r#"
            project = "fileproj"
            port = 5000
        "#,
    );
    fx.cmd
        .arg("--answers-file")
        .arg(&answers_path)
        .arg("--answers")
        .arg("project=cliproj");

    fx.cmd.assert().success();

    // project comes from --answers, which overrides the file; port comes from --answers-file,
    // where 5000 >= 3000 selects "high"; verbose is supplied by neither, so it falls back to its
    // default of false, which renders as "q".
    let content = fs::read_to_string(fx.target.join("config.txt")).expect("read config.txt");
    assert_eq!(content, "cliproj:high:q");
}

#[test]
fn apply_defaults_flag_uses_question_defaults() {
    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(
        template.path().join("scaffold.toml"),
        r#"
            [[questions]]
            name = "project"
            type = "string"
            default = "demo"

            [[questions]]
            name = "port"
            type = "int"
            default = 4000
        "#,
    )
    .expect("write scaffold.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(
        files.join("config.txt.jinja"),
        "{{ project }}:{% if port >= 3000 %}high{% else %}low{% endif %}",
    )
    .expect("write config.txt.jinja");

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--defaults");

    fx.cmd.assert().success();

    let content = fs::read_to_string(fx.target.join("config.txt")).expect("read config.txt");
    assert_eq!(content, "demo:high");
}

#[test]
fn apply_unmatched_answers_key_warns_on_stderr_and_still_succeeds() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_template(template.path());

    let mut fx = common::apply(template.path());
    fx.cmd
        .arg("--answers")
        .arg("project=demo")
        .arg("--answers")
        .arg("stray=x");

    fx.cmd
        .assert()
        .success()
        .stderr(contains("does not match any question"));
}

/// `stacks` (multiselect) + `extra` (string, no default, `when = "'ci' in stacks"`) questions.
/// The template guards `extra` access with the same condition so that when inactive (absent from
/// context) the render never references `extra`.
fn write_when_no_default_template(dir: &std::path::Path, guarded: bool) {
    fs::write(
        dir.join("scaffold.toml"),
        r#"
            [[questions]]
            name = "stacks"
            type = "multiselect"
            choices = ["ci", "docker"]

            [[questions]]
            name = "extra"
            type = "string"
            when = "'ci' in stacks"
        "#,
    )
    .expect("write scaffold.toml");

    let files = dir.join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    let template = if guarded {
        "{% if 'ci' in stacks %}extra={{ extra }}{% else %}no-ci{% endif %}"
    } else {
        "extra={{ extra }}"
    };
    fs::write(files.join("config.txt.jinja"), template).expect("write config.txt.jinja");
}

#[test]
fn apply_when_inactive_without_default_leaves_context_absent_but_guarded_template_still_renders() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_when_no_default_template(template.path(), true);

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--answers").arg("stacks=docker");

    fx.cmd.assert().success();

    let content = fs::read_to_string(fx.target.join("config.txt")).expect("read config.txt");
    assert_eq!(content, "no-ci");
}

#[test]
fn apply_when_inactive_without_default_errors_if_template_references_it_unconditionally() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_when_no_default_template(template.path(), false);

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--answers").arg("stacks=docker");

    // extra is inactive and has no default, so it is absent from context; strict undefined fails the render.
    fx.cmd.assert().failure();
}

#[test]
fn apply_static_scaffoldignore_excludes_matching_output_paths() {
    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(template.path().join("scaffold.toml"), "").expect("write scaffold.toml");
    fs::write(template.path().join(".scaffoldignore"), "*.tmp\n").expect("write .scaffoldignore");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("keep.txt"), "keep").expect("write keep.txt");
    fs::write(files.join("scratch.tmp"), "scratch").expect("write scratch.tmp");

    let mut fx = common::apply(template.path());

    fx.cmd.assert().success();

    assert!(
        fx.target.join("keep.txt").exists(),
        "non-ignored file must be written"
    );
    assert!(
        !fx.target.join("scratch.tmp").exists(),
        "ignored file must not be written"
    );
}

#[test]
fn apply_jinja_scaffoldignore_excludes_output_path_based_on_answers() {
    fn write_docker_template(dir: &std::path::Path) {
        fs::write(
            dir.join("scaffold.toml"),
            r#"
                [[questions]]
                name = "stacks"
                type = "multiselect"
                choices = ["docker"]
                default = []
            "#,
        )
        .expect("write scaffold.toml");
        fs::write(
            dir.join(".scaffoldignore.jinja"),
            "{% if \"docker\" not in stacks %}Dockerfile{% endif %}\n",
        )
        .expect("write .scaffoldignore.jinja");
        let files = dir.join("files");
        fs::create_dir_all(&files).expect("mkdir files");
        fs::write(files.join("Dockerfile"), "FROM scratch").expect("write Dockerfile");
    }

    // stacks lacks docker: Dockerfile excluded.
    let template = tempfile::tempdir().expect("template tempdir");
    write_docker_template(template.path());

    let mut fx = common::apply(template.path());
    fx.cmd.assert().success();
    assert!(
        !fx.target.join("Dockerfile").exists(),
        "Dockerfile must be excluded when stacks lacks docker"
    );

    // stacks includes docker: Dockerfile placed.
    let template2 = tempfile::tempdir().expect("template tempdir");
    write_docker_template(template2.path());

    let mut fx2 = common::apply(template2.path());
    fx2.cmd.arg("--answers").arg("stacks=docker");
    fx2.cmd.assert().success();
    assert!(
        fx2.target.join("Dockerfile").exists(),
        "Dockerfile must be placed when stacks includes docker"
    );
}

#[test]
fn apply_scaffoldignore_matches_rendered_output_name_not_source_name() {
    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(template.path().join("scaffold.toml"), "").expect("write scaffold.toml");
    fs::write(template.path().join(".scaffoldignore"), "*.tmp\n").expect("write .scaffoldignore");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    // Source name ends in .tmp.jinja so it does not match *.tmp, but the rendered output name config.tmp does.
    fs::write(files.join("config.tmp.jinja"), "rendered").expect("write config.tmp.jinja");
    fs::write(files.join("keep.txt"), "keep").expect("write keep.txt");

    let mut fx = common::apply(template.path());

    fx.cmd.assert().success();

    assert!(
        !fx.target.join("config.tmp").exists(),
        "output name config.tmp must be excluded by *.tmp even though source is config.tmp.jinja"
    );
    assert!(
        fx.target.join("keep.txt").exists(),
        "non-ignored file must be written"
    );
}

#[test]
fn apply_dry_run_omits_ignored_files_from_plan_output() {
    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(template.path().join("scaffold.toml"), "").expect("write scaffold.toml");
    fs::write(template.path().join(".scaffoldignore"), "*.tmp\n").expect("write .scaffoldignore");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("keep.txt"), "keep").expect("write keep.txt");
    fs::write(files.join("scratch.tmp"), "scratch").expect("write scratch.tmp");

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--dry-run");

    fx.cmd
        .assert()
        .success()
        .stdout(contains("keep.txt"))
        .stdout(contains("scratch.tmp").not());

    assert!(
        !fx.target.exists(),
        "dry-run must not create the target directory"
    );
}

#[test]
fn apply_renders_partial_via_include() {
    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(
        template.path().join("scaffold.toml"),
        r#"
            [[questions]]
            name = "project"
            type = "string"
        "#,
    )
    .expect("write scaffold.toml");
    let partials = template.path().join("partials");
    fs::create_dir_all(&partials).expect("mkdir partials");
    fs::write(partials.join("header"), "# {{ project }} header").expect("write partial");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(
        files.join("README.md.jinja"),
        "{% include \"header\" %}\nbody",
    )
    .expect("write README.md.jinja");

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--answers").arg("project=demo");

    fx.cmd.assert().success();

    let readme = fs::read_to_string(fx.target.join("README.md")).expect("read README.md");
    assert_eq!(readme, "# demo header\nbody");
}

#[test]
fn apply_include_of_unregistered_partial_fails() {
    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(template.path().join("scaffold.toml"), "").expect("write scaffold.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    // Including a name outside `partials/` (or unregistered) fails the registered-template lookup — render error.
    fs::write(files.join("out.txt.jinja"), "{% include \"../escape\" %}")
        .expect("write out.txt.jinja");

    let mut fx = common::apply(template.path());

    fx.cmd.assert().failure();
    assert!(
        !fx.target.join("out.txt").exists(),
        "failed include must not produce output"
    );
}

#[test]
fn apply_exposes_merged_data_in_render_context() {
    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(
        template.path().join("scaffold.toml"),
        r#"
            [data]
            greeting = "hi"

            [[data.rules]]
            ext = "rs"

            [[data.rules]]
            ext = "toml"
        "#,
    )
    .expect("write scaffold.toml");
    let data_dir = template.path().join("data");
    fs::create_dir_all(&data_dir).expect("mkdir data");
    fs::write(data_dir.join("extra.toml"), "flag = true\n").expect("write data/extra.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(
        files.join("out.txt.jinja"),
        "{{ data.greeting }} {{ data.flag }}\n{% for r in data.rules %}{{ r.ext }},{% endfor %}",
    )
    .expect("write out.txt.jinja");

    let mut fx = common::apply(template.path());

    fx.cmd.assert().success();

    let out = fs::read_to_string(fx.target.join("out.txt")).expect("read out.txt");
    assert_eq!(out, "hi true\nrs,toml,");
}

#[test]
fn apply_when_cannot_reference_data() {
    // Data is merged only after all answers are finalized, so a `when` condition runs before the
    // data namespace exists at all. The render must reject not only member access like `data.flag`
    // but also a bare reference like `not data`: if we exposed data as an empty table instead,
    // `not data` would evaluate to true and quietly slip past the guard. This test locks that door.
    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(
        template.path().join("scaffold.toml"),
        r#"
            [data]
            flag = true

            [[questions]]
            name = "extra"
            type = "string"
            when = "not data"
        "#,
    )
    .expect("write scaffold.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("keep.txt"), "keep").expect("write keep.txt");

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--answers").arg("extra=given");

    fx.cmd.assert().failure();
}

#[test]
fn apply_broken_partial_fails_without_creating_target() {
    // Partial registration (syntax compilation) runs before target creation, so a broken partial
    // must fail without leaving an empty target.
    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(template.path().join("scaffold.toml"), "").expect("write scaffold.toml");
    let partials = template.path().join("partials");
    fs::create_dir_all(&partials).expect("mkdir partials");
    fs::write(partials.join("broken"), "{% if %}").expect("write broken partial");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("keep.txt"), "keep").expect("write keep.txt");

    let mut fx = common::apply(template.path());

    fx.cmd.assert().failure();
    assert!(
        !fx.target.exists(),
        "a partial-load failure must not leave an empty target directory"
    );
}

#[cfg(unix)]
#[test]
fn apply_applies_mode_prefix_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(template.path().join("scaffold.toml"), "").expect("write scaffold.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("executable_run.sh"), "#!/bin/sh\n").expect("write exec");
    fs::write(files.join("private_secret.txt"), "s").expect("write private");
    fs::write(files.join("readonly_notes.md"), "n").expect("write readonly");
    fs::write(files.join("plain.txt"), "p").expect("write plain");

    let mut fx = common::apply(template.path());
    fx.cmd.assert().success();

    let mode = |name: &str| {
        fs::metadata(fx.target.join(name))
            .expect("stat")
            .permissions()
            .mode()
            & 0o777
    };

    // Only assert on bits that were cleared, since those hold regardless of the umask: the umask
    // can only clear bits further, so asserting that a bit is set would depend on the environment.
    // Seeing bits removed that base mode 0o644 would have left set is positive evidence that the
    // mode was applied at all; the exact resulting bits are pinned down by the domain from_modes test.
    assert_eq!(
        mode("secret.txt") & 0o077,
        0,
        "private_ clears group/other bits"
    );
    assert_eq!(
        mode("notes.md") & 0o222,
        0,
        "readonly_ clears all write bits"
    );
    assert_eq!(
        mode("plain.txt") & 0o111,
        0,
        "plain file has no execute bits"
    );
}

#[test]
fn apply_render_failure_leaves_no_target() {
    // A strict-undefined render error fails in the plan phase. The target is created after plan, so
    // no empty target must be left behind.
    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(template.path().join("scaffold.toml"), "").expect("write scaffold.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("bad.txt.jinja"), "{{ undefined_var }}").expect("write bad jinja");

    let mut fx = common::apply(template.path());

    fx.cmd.assert().failure();
    assert!(
        !fx.target.exists(),
        "a render failure in the plan phase must not create the target directory"
    );
}

#[test]
fn apply_uses_scaffoldroot_effective_source_root() {
    // Place only `.scaffoldroot` at the repo top and the real template under `template/`. The
    // effective root must move down and read scaffold.toml and files/ from there.
    let repo = tempfile::tempdir().expect("repo tempdir");
    fs::write(repo.path().join(".scaffoldroot"), "template\n").expect("write .scaffoldroot");
    fs::write(repo.path().join("README.md"), "repo readme, not template")
        .expect("write repo readme");
    let inner = repo.path().join("template");
    fs::create_dir_all(inner.join("files")).expect("mkdir inner files");
    fs::write(
        inner.join("scaffold.toml"),
        "[[questions]]\nname = \"project\"\ntype = \"string\"\n",
    )
    .expect("write inner scaffold.toml");
    fs::write(inner.join("files/out.txt.jinja"), "{{ project }}").expect("write inner payload");

    let mut fx = common::apply(repo.path());
    fx.cmd.arg("--answers").arg("project=hi");
    fx.cmd.assert().success();

    let out = fs::read_to_string(fx.target.join("out.txt")).expect("read out.txt");
    assert_eq!(out, "hi");
    // The repo-top README is not template payload, so it is not placed.
    assert!(!fx.target.join("README.md").exists());
}

#[cfg(unix)]
#[test]
fn apply_force_replaces_existing_external_symlink_dest_in_place() {
    use std::os::unix::fs::symlink;

    // The target has an existing symlink pointing at an external file, and the template uses the
    // same name. This must be an overwrite (in-place replacement), not an external write — `--force`
    // replaces the link with a regular file and the external target must stay unchanged.
    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(template.path().join("scaffold.toml"), "").expect("write scaffold.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("data.txt"), "generated").expect("write payload");

    let outside = tempfile::tempdir().expect("outside tempdir");
    let external = outside.path().join("secret.txt");
    fs::write(&external, "SECRET").expect("seed external");

    let mut fx = common::apply(template.path());
    fs::create_dir_all(&fx.target).expect("mkdir target");
    symlink(&external, fx.target.join("data.txt")).expect("seed dest symlink");
    fx.cmd.arg("--force");
    fx.cmd.assert().success();

    let meta = fs::symlink_metadata(fx.target.join("data.txt")).expect("stat dest");
    assert!(
        !meta.file_type().is_symlink(),
        "dest symlink must be replaced by a regular file"
    );
    assert_eq!(
        fs::read_to_string(fx.target.join("data.txt")).unwrap(),
        "generated"
    );
    assert_eq!(
        fs::read_to_string(&external).unwrap(),
        "SECRET",
        "external target must be untouched"
    );
}

/// Template with a `project` question + inline before hook (writes `$SCAFFOLDER_PROJECT` to `hook-out.txt`).
fn write_hook_env_template(dir: &std::path::Path) {
    fs::write(
        dir.join("scaffold.toml"),
        r#"
            [[questions]]
            name = "project"
            type = "string"

            [[hooks.before]]
            run = "echo $SCAFFOLDER_PROJECT > hook-out.txt"
        "#,
    )
    .expect("write scaffold.toml");
    let files = dir.join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("marker.txt"), "marker").expect("write marker.txt");
}

#[test]
fn apply_inline_before_hook_runs_with_env_and_cwd_when_yes() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_hook_env_template(template.path());

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--answers").arg("project=demo").arg("--yes");

    fx.cmd.assert().success();

    let out = fs::read_to_string(fx.target.join("hook-out.txt")).expect("read hook-out.txt");
    assert_eq!(
        out.trim(),
        "demo",
        "hook must see SCAFFOLDER_PROJECT env and run with cwd=target"
    );
}

#[test]
fn apply_hook_confirm_required_without_yes_fails_noninteractively_with_no_side_effects() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_hook_env_template(template.path());

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--answers").arg("project=demo");

    // assert_cmd runs non-tty by default; with a hook and no --yes the confirm is refused and it must error.
    fx.cmd.assert().failure();

    assert!(
        !fx.target.exists(),
        "unconfirmed hook must abort before target creation (no side effects)"
    );
}

/// Template with a `project` question + a `.jinja` folder hook (`hooks/before/10-gen.sh.jinja`,
/// writes `{{ project }}` to `rendered-hook-out.txt`).
fn write_jinja_folder_hook_template(dir: &std::path::Path) {
    fs::write(
        dir.join("scaffold.toml"),
        r#"
            [[questions]]
            name = "project"
            type = "string"
        "#,
    )
    .expect("write scaffold.toml");
    let hooks_before = dir.join("hooks/before");
    fs::create_dir_all(&hooks_before).expect("mkdir hooks/before");
    fs::write(
        hooks_before.join("10-gen.sh.jinja"),
        "#!/bin/sh\necho \"{{ project }}\" > rendered-hook-out.txt\n",
    )
    .expect("write 10-gen.sh.jinja");
    let files = dir.join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("marker.txt"), "marker").expect("write marker.txt");
}

/// E2E regression: a `.jinja` folder hook has to run through the whole real chain — render it with
/// MiniJinja, write it to a temp file, then execute it — carrying the answer context all the way
/// through. Piecewise unit tests alone don't prove that context is actually threaded end to end.
#[test]
fn apply_jinja_folder_hook_renders_with_answer_context_and_executes() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_jinja_folder_hook_template(template.path());

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--answers").arg("project=demo").arg("--yes");

    fx.cmd.assert().success();

    let out = fs::read_to_string(fx.target.join("rendered-hook-out.txt"))
        .expect("read rendered-hook-out.txt");
    assert_eq!(
        out.trim(),
        "demo",
        "jinja folder hook must render with answer context and execute"
    );
}

/// Template with a payload file (`files/marker.txt` = "payload") + after hook (`cat marker.txt`).
fn write_after_hook_observes_payload_template(dir: &std::path::Path) {
    fs::write(
        dir.join("scaffold.toml"),
        r#"
            [[hooks.after]]
            run = "cat marker.txt > after-saw.txt"
        "#,
    )
    .expect("write scaffold.toml");
    let files = dir.join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("marker.txt"), "payload").expect("write marker.txt");
}

/// E2E regression: the after hook runs after write, so it must be able to actually read the placed
/// payload file (until now the ordering was only proven structurally).
#[test]
fn apply_after_hook_observes_written_payload_file() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_after_hook_observes_payload_template(template.path());

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--yes");

    fx.cmd.assert().success();

    let out = fs::read_to_string(fx.target.join("after-saw.txt")).expect("read after-saw.txt");
    assert_eq!(
        out.trim(),
        "payload",
        "after hook must observe the already-written payload file"
    );
}

#[test]
fn apply_dry_run_skips_hook_confirm_and_execution() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_hook_env_template(template.path());

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--answers").arg("project=demo").arg("--dry-run");

    // Even without --yes, dry-run skips the confirm and hook execution entirely, so it must succeed.
    fx.cmd.assert().success();

    assert!(
        !fx.target.exists(),
        "dry-run must not create the target directory or run hooks"
    );
}

/// A control file symlinked externally (outside the effective source root) must be refused without
/// `--trust` (abort before side effects); opting in with `--trust` loads it normally. Internal
/// symlinks are always allowed.
#[cfg(unix)]
#[test]
fn apply_rejects_externally_symlinked_manifest_without_trust() {
    use std::os::unix::fs::symlink;

    let template = tempfile::tempdir().expect("template tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let external_manifest = outside.path().join("scaffold.toml");
    fs::write(&external_manifest, "").expect("write external manifest");
    symlink(&external_manifest, template.path().join("scaffold.toml"))
        .expect("symlink scaffold.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("marker.txt"), "marker").expect("write marker.txt");

    let mut fx = common::apply(template.path());

    fx.cmd.assert().failure();
    assert!(
        !fx.target.exists(),
        "externally symlinked manifest must abort before target creation"
    );
}

#[cfg(unix)]
#[test]
fn apply_allows_externally_symlinked_manifest_with_trust() {
    use std::os::unix::fs::symlink;

    let template = tempfile::tempdir().expect("template tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let external_manifest = outside.path().join("scaffold.toml");
    fs::write(&external_manifest, "").expect("write external manifest");
    symlink(&external_manifest, template.path().join("scaffold.toml"))
        .expect("symlink scaffold.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("marker.txt"), "marker").expect("write marker.txt");

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--trust");

    fx.cmd.assert().success();
    assert_eq!(
        fs::read_to_string(fx.target.join("marker.txt")).expect("read marker.txt"),
        "marker"
    );
}

#[cfg(unix)]
#[test]
fn apply_rejects_externally_symlinked_hook_script_without_trust() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(template.path().join("scaffold.toml"), "").expect("write scaffold.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("marker.txt"), "marker").expect("write marker.txt");

    let outside = tempfile::tempdir().expect("outside tempdir");
    let external_script = outside.path().join("x.sh");
    fs::write(&external_script, "#!/bin/sh\necho hi > out.txt\n").expect("write external hook");
    let mut perms = fs::metadata(&external_script)
        .expect("metadata")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&external_script, perms).expect("chmod +x external hook");
    let hooks_before = template.path().join("hooks/before");
    fs::create_dir_all(&hooks_before).expect("mkdir hooks/before");
    symlink(&external_script, hooks_before.join("x.sh")).expect("symlink hook script");

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--yes");

    fx.cmd.assert().failure();
    assert!(
        !fx.target.exists(),
        "externally symlinked hook script must abort before target creation"
    );
}

/// If `.scaffoldroot` is itself a symlink to a file outside the source root, its contents (which
/// select the effective source root) must not be read without `--trust` — the external control-file
/// default-refuse contract applies to `.scaffoldroot` too.
#[cfg(unix)]
#[test]
fn apply_rejects_externally_symlinked_scaffoldroot_without_trust() {
    use std::os::unix::fs::symlink;

    let template = tempfile::tempdir().expect("template tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let external_scaffoldroot = outside.path().join("scaffoldroot-content");
    fs::write(&external_scaffoldroot, "template\n").expect("write external scaffoldroot content");
    symlink(
        &external_scaffoldroot,
        template.path().join(".scaffoldroot"),
    )
    .expect("symlink .scaffoldroot");

    let inner = template.path().join("template");
    fs::create_dir_all(inner.join("files")).expect("mkdir inner files");
    fs::write(inner.join("scaffold.toml"), "").expect("write inner scaffold.toml");
    fs::write(inner.join("files/marker.txt"), "marker").expect("write inner marker");

    let mut fx = common::apply(template.path());

    fx.cmd.assert().failure();
    assert!(
        !fx.target.exists(),
        "externally symlinked .scaffoldroot must abort before target creation"
    );
}

#[cfg(unix)]
#[test]
fn apply_allows_externally_symlinked_scaffoldroot_with_trust() {
    use std::os::unix::fs::symlink;

    let template = tempfile::tempdir().expect("template tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let external_scaffoldroot = outside.path().join("scaffoldroot-content");
    fs::write(&external_scaffoldroot, "template\n").expect("write external scaffoldroot content");
    symlink(
        &external_scaffoldroot,
        template.path().join(".scaffoldroot"),
    )
    .expect("symlink .scaffoldroot");

    let inner = template.path().join("template");
    fs::create_dir_all(inner.join("files")).expect("mkdir inner files");
    fs::write(inner.join("scaffold.toml"), "").expect("write inner scaffold.toml");
    fs::write(inner.join("files/marker.txt"), "marker").expect("write inner marker");

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--trust");

    fx.cmd.assert().success();
    assert_eq!(
        fs::read_to_string(fx.target.join("marker.txt")).expect("read marker.txt"),
        "marker"
    );
}

// --- target cleanup-on-failure e2e ---

/// Template whose before hook fails via `exit 1` — reproduces target cleanup on failure.
fn write_failing_before_hook_template(dir: &std::path::Path) {
    fs::write(
        dir.join("scaffold.toml"),
        r#"
            [[hooks.before]]
            run = "exit 1"
        "#,
    )
    .expect("write scaffold.toml");
    let files = dir.join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("marker.txt"), "generated").expect("write marker.txt");
}

#[test]
fn apply_failure_cleans_up_newly_created_target() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_failing_before_hook_template(template.path());

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--yes");
    fx.cmd.assert().failure();

    assert!(
        !fx.target.exists(),
        "newly created target must be cleaned up after before-hook failure"
    );
}

#[test]
fn apply_failure_preserves_preexisting_target_and_sentinel() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_failing_before_hook_template(template.path());

    let mut fx = common::apply(template.path());
    // nested sentinel — planted inside a subdirectory to also catch a delete-then-recreate defect.
    fs::create_dir_all(fx.target.join("nested")).expect("precreate nested");
    fs::write(fx.target.join("nested").join("deep.txt"), "user-data").expect("nested sentinel");

    fx.cmd.arg("--yes");
    fx.cmd.assert().failure();

    assert_eq!(
        fs::read_to_string(fx.target.join("nested").join("deep.txt"))
            .expect("nested sentinel must survive"),
        "user-data",
        "pre-existing target and nested user data must be preserved on failure"
    );
}

#[test]
fn apply_no_cleanup_flag_preserves_created_target_on_failure() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_failing_before_hook_template(template.path());

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--yes").arg("--no-cleanup-on-failure");
    fx.cmd.assert().failure();

    assert!(
        fx.target.exists(),
        "--no-cleanup-on-failure must preserve the created target"
    );
}

#[test]
fn apply_failure_cleanup_does_not_touch_sibling() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_failing_before_hook_template(template.path());

    let mut fx = common::apply(template.path());
    let sibling = fx.workdir().join("sibling.txt");
    fs::write(&sibling, "keep-sibling").expect("sibling sentinel");

    fx.cmd.arg("--yes");
    fx.cmd.assert().failure();

    assert!(!fx.target.exists(), "created target must be cleaned up");
    assert_eq!(
        fs::read_to_string(&sibling).expect("sibling must survive"),
        "keep-sibling",
        "cleanup must touch only the prepared target root, not siblings"
    );
}

#[test]
fn apply_with_dotdot_in_target_applies_at_normalized_effective_path() {
    // A target containing `..` still applies correctly at the effective path — target_root is
    // normalized once at the apply boundary, so hook cwd, write, ensure, and cleanup all use the
    // same path (base/demo).
    let template = tempfile::tempdir().expect("template tempdir");
    write_template(template.path());
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("sub").join("..").join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--answers")
        .arg("project=demo");
    cmd.assert().success();

    let effective = workdir.path().join("demo");
    assert_eq!(
        fs::read_to_string(effective.join("README.md")).expect("README at effective path"),
        "# demo"
    );
    assert!(
        !workdir.path().join("sub").exists(),
        "`..` resolution must not create a sibling directory"
    );
}

/// Template whose after hook fails via `exit 1` — reproduces cleanup (including outputs) after a successful write.
fn write_failing_after_hook_template(dir: &std::path::Path) {
    fs::write(
        dir.join("scaffold.toml"),
        r#"
            [[hooks.after]]
            run = "exit 1"
        "#,
    )
    .expect("write scaffold.toml");
    let files = dir.join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("marker.txt"), "generated").expect("write marker.txt");
}

#[test]
fn apply_after_hook_failure_cleans_up_created_target_with_contents() {
    // Even if the after-hook fails after write completes and marker.txt is placed, the newly created target is cleaned up wholesale.
    let template = tempfile::tempdir().expect("template tempdir");
    write_failing_after_hook_template(template.path());

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--yes");
    fx.cmd.assert().failure();

    assert!(
        !fx.target.exists(),
        "created target with generated contents must be cleaned up on after-hook failure"
    );
}

// --- out-of-target containment (§1.10) ---

/// A directory symlink inside the target that points outside makes a payload entry resolve outside
/// the target. Run non-interactively (no confirm is available), the escaping write is skipped —
/// apply still succeeds and nothing is written to the external location.
#[cfg(unix)]
#[test]
fn apply_external_write_outside_target_is_skipped_noninteractively() {
    use std::os::unix::fs::symlink;

    let template = tempfile::tempdir().expect("template tempdir");
    common::build_tree(
        template.path(),
        &[
            ("scaffold.toml", ""),
            ("files/escape/child.txt", "escaped-content"),
        ],
    );

    // The external directory the in-target symlink escapes to; it must stay empty.
    let outside = tempfile::tempdir().expect("outside tempdir");

    let mut fx = common::apply(template.path());
    // Pre-create the target with a directory symlink `escape` pointing outside it, so the payload
    // entry `escape/child.txt` resolves to `<outside>/child.txt` through the symlinked ancestor.
    fs::create_dir_all(&fx.target).expect("mkdir target");
    symlink(outside.path(), fx.target.join("escape")).expect("symlink escape -> outside");

    // Non-interactive: the external-write confirm is refused, so the entry is skipped (not an error).
    fx.cmd.assert().success().stderr(contains("escapes target"));

    assert!(
        !outside.path().join("child.txt").exists(),
        "escaping entry must not be written to the external location"
    );
    let escape_meta = fs::symlink_metadata(fx.target.join("escape")).expect("stat escape");
    assert!(
        escape_meta.file_type().is_symlink(),
        "the in-target symlink must be left untouched by the skip"
    );
}

// --- --trust forwarding to each control-file adapter (allow-with-trust + refuse-without-trust) ---
//
// These prove the CLI actually forwards `--trust` to every per-adapter external-symlink guard, not
// just that refuse is the default. Each allow test observes an effect that can only appear if the
// external control file was read via `--trust` (hook side effect, partial content, data value,
// exclusion), so a hardcoded `trust = false` on any adapter would surface as a failing allow test.

#[cfg(unix)]
#[test]
fn apply_allows_external_hook_script_with_trust_and_yes() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(template.path().join("scaffold.toml"), "").expect("write scaffold.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("marker.txt"), "marker").expect("write marker.txt");

    let outside = tempfile::tempdir().expect("outside tempdir");
    let external_script = outside.path().join("x.sh");
    fs::write(&external_script, "#!/bin/sh\necho hi > out.txt\n").expect("write external hook");
    let mut perms = fs::metadata(&external_script)
        .expect("metadata")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&external_script, perms).expect("chmod +x external hook");
    let hooks_before = template.path().join("hooks/before");
    fs::create_dir_all(&hooks_before).expect("mkdir hooks/before");
    symlink(&external_script, hooks_before.join("x.sh")).expect("symlink hook script");

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--trust").arg("--yes");

    fx.cmd.assert().success();
    // out.txt is written into the hook cwd (the target) only if the externally symlinked hook
    // actually ran — which requires --trust to reach the hook source.
    let out = fs::read_to_string(fx.target.join("out.txt")).expect("read out.txt");
    assert_eq!(out.trim(), "hi");
}

#[cfg(unix)]
#[test]
fn apply_allows_external_partial_with_trust() {
    use std::os::unix::fs::symlink;

    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(
        template.path().join("scaffold.toml"),
        "[[questions]]\nname = \"project\"\ntype = \"string\"\n",
    )
    .expect("write scaffold.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(
        files.join("README.md.jinja"),
        "{% include \"header\" %}\nbody",
    )
    .expect("write README.md.jinja");

    // partials/ is a directory symlink pointing outside the source root; the `header` partial is
    // reachable only if --trust reaches the partial source.
    let outside = tempfile::tempdir().expect("outside tempdir");
    let external_partials = outside.path().join("partials");
    fs::create_dir_all(&external_partials).expect("mkdir external partials");
    fs::write(external_partials.join("header"), "# {{ project }} header")
        .expect("write external partial");
    symlink(&external_partials, template.path().join("partials")).expect("symlink partials dir");

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--answers").arg("project=demo").arg("--trust");

    fx.cmd.assert().success();
    // The partial's rendered content appears in output only if the external partials dir was read.
    let readme = fs::read_to_string(fx.target.join("README.md")).expect("read README.md");
    assert_eq!(readme, "# demo header\nbody");
}

#[cfg(unix)]
#[test]
fn apply_allows_external_data_with_trust() {
    use std::os::unix::fs::symlink;

    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(template.path().join("scaffold.toml"), "").expect("write scaffold.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("out.txt.jinja"), "{{ data.greeting }}").expect("write out.txt.jinja");

    // data/ is a directory symlink pointing outside the source root; its value is exposed only if
    // --trust reaches the data source.
    let outside = tempfile::tempdir().expect("outside tempdir");
    let external_data = outside.path().join("data");
    fs::create_dir_all(&external_data).expect("mkdir external data");
    fs::write(external_data.join("extra.toml"), "greeting = \"hi\"\n")
        .expect("write external data file");
    symlink(&external_data, template.path().join("data")).expect("symlink data dir");

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--trust");

    fx.cmd.assert().success();
    // "hi" is rendered only if the external data toml was read.
    let out = fs::read_to_string(fx.target.join("out.txt")).expect("read out.txt");
    assert_eq!(out, "hi");
}

#[cfg(unix)]
#[test]
fn apply_allows_external_scaffoldignore_with_trust() {
    use std::os::unix::fs::symlink;

    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(template.path().join("scaffold.toml"), "").expect("write scaffold.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("keep.txt"), "keep").expect("write keep.txt");
    fs::write(files.join("scratch.tmp"), "scratch").expect("write scratch.tmp");

    // .scaffoldignore is a file symlink pointing outside the source root; its exclusion pattern
    // takes effect only if --trust reaches the ignore source.
    let outside = tempfile::tempdir().expect("outside tempdir");
    let external_ignore = outside.path().join("ignore");
    fs::write(&external_ignore, "*.tmp\n").expect("write external ignore");
    symlink(&external_ignore, template.path().join(".scaffoldignore"))
        .expect("symlink .scaffoldignore");

    let mut fx = common::apply(template.path());
    fx.cmd.arg("--trust");

    fx.cmd.assert().success();
    // Without reading the external pattern, scratch.tmp would be placed; its absence (and keep.txt's
    // presence) proves the external ignore was applied.
    assert!(
        fx.target.join("keep.txt").exists(),
        "non-matching file must be placed"
    );
    assert!(
        !fx.target.join("scratch.tmp").exists(),
        "matching file must be excluded by the external .scaffoldignore"
    );
}

#[cfg(unix)]
#[test]
fn apply_rejects_external_partial_without_trust() {
    use std::os::unix::fs::symlink;

    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(template.path().join("scaffold.toml"), "").expect("write scaffold.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("marker.txt"), "marker").expect("write marker.txt");

    let outside = tempfile::tempdir().expect("outside tempdir");
    let external_partials = outside.path().join("partials");
    fs::create_dir_all(&external_partials).expect("mkdir external partials");
    fs::write(external_partials.join("header"), "external").expect("write external partial");
    symlink(&external_partials, template.path().join("partials")).expect("symlink partials dir");

    let mut fx = common::apply(template.path());

    fx.cmd.assert().failure();
    assert!(
        !fx.target.exists(),
        "externally symlinked partials must abort before target creation"
    );
}

#[cfg(unix)]
#[test]
fn apply_rejects_external_data_without_trust() {
    use std::os::unix::fs::symlink;

    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(template.path().join("scaffold.toml"), "").expect("write scaffold.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("marker.txt"), "marker").expect("write marker.txt");

    let outside = tempfile::tempdir().expect("outside tempdir");
    let external_data = outside.path().join("data");
    fs::create_dir_all(&external_data).expect("mkdir external data");
    fs::write(external_data.join("extra.toml"), "greeting = \"hi\"\n")
        .expect("write external data file");
    symlink(&external_data, template.path().join("data")).expect("symlink data dir");

    let mut fx = common::apply(template.path());

    fx.cmd.assert().failure();
    assert!(
        !fx.target.exists(),
        "externally symlinked data must abort before target creation"
    );
}

#[cfg(unix)]
#[test]
fn apply_rejects_external_scaffoldignore_without_trust() {
    use std::os::unix::fs::symlink;

    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(template.path().join("scaffold.toml"), "").expect("write scaffold.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("keep.txt"), "keep").expect("write keep.txt");

    let outside = tempfile::tempdir().expect("outside tempdir");
    let external_ignore = outside.path().join("ignore");
    fs::write(&external_ignore, "*.tmp\n").expect("write external ignore");
    symlink(&external_ignore, template.path().join(".scaffoldignore"))
        .expect("symlink .scaffoldignore");

    let mut fx = common::apply(template.path());

    fx.cmd.assert().failure();
    assert!(
        !fx.target.exists(),
        "externally symlinked .scaffoldignore must abort before target creation"
    );
}
