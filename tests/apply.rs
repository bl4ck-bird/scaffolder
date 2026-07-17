//! `scaffolder apply` e2e: 렌더/verbatim 배치, overwrite confirm, dry-run.

use std::fs;

use assert_cmd::Command;
use predicates::str::contains;

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

#[test]
fn apply_renders_and_writes_files() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_template(template.path());
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--answers")
        .arg("project=demo");

    cmd.assert().success();

    let readme = fs::read_to_string(target.join("README.md")).expect("read README.md");
    assert_eq!(readme, "# demo");

    let main_rs = fs::read_to_string(target.join("src/main.rs")).expect("read src/main.rs");
    assert_eq!(main_rs, "fn main(){}");
}

#[test]
fn apply_without_force_fails_on_existing_destination_noninteractively() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_template(template.path());
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");
    fs::create_dir_all(&target).expect("mkdir target");
    fs::write(target.join("README.md"), "existing").expect("seed existing README.md");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--answers")
        .arg("project=demo");

    cmd.assert().failure();

    let readme = fs::read_to_string(target.join("README.md")).expect("read README.md");
    assert_eq!(readme, "existing", "unapproved overwrite must not happen");
}

#[test]
fn apply_with_force_overwrites_existing_destination() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_template(template.path());
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");
    fs::create_dir_all(&target).expect("mkdir target");
    fs::write(target.join("README.md"), "existing").expect("seed existing README.md");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--answers")
        .arg("project=demo")
        .arg("--force");

    cmd.assert().success();

    let readme = fs::read_to_string(target.join("README.md")).expect("read README.md");
    assert_eq!(readme, "# demo");
}

#[test]
fn apply_dry_run_does_not_write_and_prints_plan() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_template(template.path());
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--answers")
        .arg("project=demo")
        .arg("--dry-run");

    cmd.assert().success().stdout(contains("README.md"));

    assert!(!target.exists(), "dry-run must not create the target directory");
}

/// `project`(default 있음)/`port`(int, default 없음)/`verbose`(bool, default 있음) 질문과
/// 타입 보존을 확인하는 렌더 템플릿.
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
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--answers")
        .arg("port=8080");

    cmd.assert().success();

    let content = fs::read_to_string(target.join("config.txt")).expect("read config.txt");
    assert_eq!(content, "demo:high:q");
}

#[test]
fn apply_answers_file_supplies_typed_values() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_multi_type_template(template.path());
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");
    let answers_path = write_answers_toml(
        workdir.path(),
        r#"
            project = "fileproj"
            port = 2000
            verbose = true
        "#,
    );

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--answers-file")
        .arg(&answers_path);

    cmd.assert().success();

    let content = fs::read_to_string(target.join("config.txt")).expect("read config.txt");
    assert_eq!(content, "fileproj:low:v");
}

#[test]
fn apply_answers_flag_overrides_answers_file() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_multi_type_template(template.path());
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");
    let answers_path = write_answers_toml(
        workdir.path(),
        r#"
            project = "fileproj"
            port = 5000
        "#,
    );

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--answers-file")
        .arg(&answers_path)
        .arg("--answers")
        .arg("project=cliproj");

    cmd.assert().success();

    // project는 --answers(override); port는 --answers-file(5000 >= 3000 → high);
    // verbose는 어느 쪽에도 없으니 default(false → q)로 떨어진다.
    let content = fs::read_to_string(target.join("config.txt")).expect("read config.txt");
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

    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--defaults");

    cmd.assert().success();

    let content = fs::read_to_string(target.join("config.txt")).expect("read config.txt");
    assert_eq!(content, "demo:high");
}

#[test]
fn apply_defaults_flag_fails_when_required_answer_has_no_default() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_multi_type_template(template.path()); // port has no default
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--defaults");

    cmd.assert().failure();
}

#[test]
fn apply_unmatched_answers_key_warns_on_stderr_and_still_succeeds() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_template(template.path());
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--answers")
        .arg("project=demo")
        .arg("--answers")
        .arg("stray=x");

    cmd.assert()
        .success()
        .stderr(contains("does not match any question"));
}

#[test]
fn apply_noninteractive_without_required_answer_fails() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_multi_type_template(template.path()); // port has no default
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target);

    cmd.assert().failure();
}
