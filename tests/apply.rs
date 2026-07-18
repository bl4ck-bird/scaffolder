//! `scaffolder apply` e2e: 렌더/verbatim 배치, overwrite confirm, dry-run.

use std::fs;

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
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

/// `store_dir/name`에 조회 가능한 스토어 템플릿을 만든다.
fn write_store_template(store_dir: &std::path::Path, name: &str) {
    let template_dir = store_dir.join(name);
    fs::create_dir_all(&template_dir).expect("mkdir store template dir");
    write_template(&template_dir);
}

/// 질문 없이 `files/marker.txt`(내용=`marker`)만 배치하는 최소 템플릿 — 두 후보 템플릿 중
/// 어느 쪽이 실제로 적용됐는지 배치된 파일 내용으로 구분하기 위함.
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

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .env("SCAFFOLDER_HOME", "")
        .env("XDG_CONFIG_HOME", "")
        .env("HOME", fake_home.path())
        .arg("apply")
        .arg("ghost")
        .arg(&target)
        .arg("--template-dir")
        .arg(store_dir.path());

    cmd.assert()
        .failure()
        .stderr(contains("ghost"))
        .stderr(contains(store_dir.path().to_str().expect("utf8 path")));

    assert!(!target.exists(), "missing template must not create the target directory");
}

/// 회귀: bare 스토어 이름이 CWD의 동명 디렉토리에 가려지면 `--template-dir`가 조용히
/// 우회된다 — store 체인이 CWD 셰도잉보다 우선해야 한다.
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

/// bare 이름이 어느 store에도 없으면 CWD 기준 동명 디렉토리로 fallback한다(기존 호환).
#[test]
fn apply_bare_name_falls_back_to_cwd_directory_when_absent_from_stores() {
    let store_dir = tempfile::tempdir().expect("store tempdir");
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let fake_home = tempfile::tempdir().expect("fake home tempdir");
    let local_template = workdir.path().join("localtpl");
    fs::create_dir_all(&local_template).expect("mkdir local template dir");
    write_marker_template(&local_template, "local");

    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .env("SCAFFOLDER_HOME", "")
        .env("XDG_CONFIG_HOME", "")
        .env("HOME", fake_home.path())
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

/// `stacks`(multiselect) + `private`(boolean, default=false, `when = "'ci' in stacks"`) 질문과
/// 그 값을 렌더하는 템플릿.
fn write_when_template(dir: &std::path::Path) {
    fs::write(
        dir.join("scaffold.toml"),
        r#"
            [[questions]]
            name = "stacks"
            type = "multiselect"
            choices = ["ci", "docker"]

            [[questions]]
            name = "private"
            type = "boolean"
            default = false
            when = "'ci' in stacks"
        "#,
    )
    .expect("write scaffold.toml");

    let files = dir.join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("config.txt.jinja"), "private={{ private }}").expect("write config.txt.jinja");
}

#[test]
fn apply_when_active_uses_given_answer_over_default() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_when_template(template.path());
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--answers")
        .arg("stacks=ci")
        .arg("--answers")
        .arg("private=true");

    cmd.assert().success();

    let content = fs::read_to_string(target.join("config.txt")).expect("read config.txt");
    assert_eq!(content, "private=true");
}

#[test]
fn apply_when_inactive_uses_default_and_ignores_given_answer() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_when_template(template.path());
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--answers")
        .arg("stacks=docker")
        .arg("--answers")
        .arg("private=true");

    cmd.assert().success();

    // stacks에 'ci'가 없으므로 private은 inactive: 준 답변(true)을 무시하고 default(false)를 쓴다.
    let content = fs::read_to_string(target.join("config.txt")).expect("read config.txt");
    assert_eq!(content, "private=false");
}

/// `stacks`(multiselect) + `extra`(string, default 없음, `when = "'ci' in stacks"`) 질문.
/// 템플릿은 동일 조건으로 `extra` 접근을 가드해, inactive일 때(컨텍스트 부재) 렌더가 절대
/// `extra`를 참조하지 않게 한다.
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
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--answers")
        .arg("stacks=docker");

    cmd.assert().success();

    let content = fs::read_to_string(target.join("config.txt")).expect("read config.txt");
    assert_eq!(content, "no-ci");
}

#[test]
fn apply_when_inactive_without_default_errors_if_template_references_it_unconditionally() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_when_no_default_template(template.path(), false);
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--answers")
        .arg("stacks=docker");

    // extra는 inactive이고 default가 없어 컨텍스트에서 부재한다; strict undefined로 렌더가 실패한다.
    cmd.assert().failure();
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

    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target);

    cmd.assert().success();

    assert!(target.join("keep.txt").exists(), "non-ignored file must be written");
    assert!(
        !target.join("scratch.tmp").exists(),
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

    // stacks에 docker 미포함: Dockerfile 제외.
    let template = tempfile::tempdir().expect("template tempdir");
    write_docker_template(template.path());
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path()).arg("apply").arg(template.path()).arg(&target);
    cmd.assert().success();
    assert!(
        !target.join("Dockerfile").exists(),
        "Dockerfile must be excluded when stacks lacks docker"
    );

    // stacks에 docker 포함: Dockerfile 배치.
    let template2 = tempfile::tempdir().expect("template tempdir");
    write_docker_template(template2.path());
    let target2 = workdir.path().join("demo-docker");

    let mut cmd2 = Command::cargo_bin("scaffolder").expect("binary");
    cmd2.current_dir(workdir.path())
        .arg("apply")
        .arg(template2.path())
        .arg(&target2)
        .arg("--answers")
        .arg("stacks=docker");
    cmd2.assert().success();
    assert!(
        target2.join("Dockerfile").exists(),
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
    // 소스명은 .tmp.jinja로 끝나 *.tmp에 매치되지 않지만, 렌더된 출력명 config.tmp는 매치된다.
    fs::write(files.join("config.tmp.jinja"), "rendered").expect("write config.tmp.jinja");
    fs::write(files.join("keep.txt"), "keep").expect("write keep.txt");

    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target);

    cmd.assert().success();

    assert!(
        !target.join("config.tmp").exists(),
        "output name config.tmp must be excluded by *.tmp even though source is config.tmp.jinja"
    );
    assert!(target.join("keep.txt").exists(), "non-ignored file must be written");
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

    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--dry-run");

    cmd.assert()
        .success()
        .stdout(contains("keep.txt"))
        .stdout(contains("scratch.tmp").not());

    assert!(!target.exists(), "dry-run must not create the target directory");
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
    assert_eq!(readme, "# demo header\nbody");
}

#[test]
fn apply_include_of_unregistered_partial_fails() {
    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(template.path().join("scaffold.toml"), "").expect("write scaffold.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    // `partials/` 밖(또는 미등록) 이름 include는 등록 템플릿 조회에 실패해 렌더 에러.
    fs::write(
        files.join("out.txt.jinja"),
        "{% include \"../escape\" %}",
    )
    .expect("write out.txt.jinja");

    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target);

    cmd.assert().failure();
    assert!(
        !target.join("out.txt").exists(),
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

    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target);

    cmd.assert().success();

    let out = fs::read_to_string(target.join("out.txt")).expect("read out.txt");
    assert_eq!(out, "hi true\nrs,toml,");
}

#[test]
fn apply_dedup_lines_over_included_partial() {
    // 대표 시나리오: partial을 `{% include %}`로 조립한 결과를 `{% filter dedup_lines %}`로
    // 중복 제거.
    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(template.path().join("scaffold.toml"), "").expect("write scaffold.toml");
    let partials = template.path().join("partials");
    fs::create_dir_all(&partials).expect("mkdir partials");
    fs::write(partials.join("gitignore-docker"), "/target\n/docker-artifacts").expect("write partial");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(
        files.join(".gitignore.jinja"),
        "{% filter dedup_lines %}/target\n{% include \"gitignore-docker\" %}{% endfilter %}",
    )
    .expect("write .gitignore.jinja");

    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target);

    cmd.assert().success();

    let gitignore = fs::read_to_string(target.join(".gitignore")).expect("read .gitignore");
    assert_eq!(gitignore, "/target\n/docker-artifacts");
}

#[test]
fn apply_when_cannot_reference_data() {
    // data는 answer 확정(step 2) 이후 병합(step 3)되므로 `when`은 data 네임스페이스 자체를
    // 보지 못한다. 멤버 접근(`data.flag`)뿐 아니라 네임스페이스 참조(`not data`)도 미정의로 실패해야
    // 한다(빈 테이블로 노출하면 `not data`가 성공해 우회 가능 — 그 우회를 잠근다).
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

    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--answers")
        .arg("extra=given");

    cmd.assert().failure();
}

#[test]
fn apply_broken_partial_fails_without_creating_target() {
    // partial 등록(구문 컴파일)은 target 생성 전에 수행하므로, 잘못된 partial은 빈 target을
    // 남기지 않고 실패해야 한다.
    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(template.path().join("scaffold.toml"), "").expect("write scaffold.toml");
    let partials = template.path().join("partials");
    fs::create_dir_all(&partials).expect("mkdir partials");
    fs::write(partials.join("broken"), "{% if %}").expect("write broken partial");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("keep.txt"), "keep").expect("write keep.txt");

    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target);

    cmd.assert().failure();
    assert!(
        !target.exists(),
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

    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target);
    cmd.assert().success();

    let mode = |name: &str| {
        fs::metadata(target.join(name)).expect("stat").permissions().mode() & 0o777
    };

    // umask에 무관한 "비트가 제거됨" 불변식만 검사한다(umask는 비트를 추가로 제거만 하므로 "set"
    // 단언은 환경 의존적). 이 clear 불변식들은 mode 적용의 양성 증거다 — base(0o644)라면 남았을
    // 비트가 제거됐음을 보인다. 정확한 비트값은 domain from_modes 테스트가 잠근다.
    assert_eq!(mode("secret.txt") & 0o077, 0, "private_ clears group/other bits");
    assert_eq!(mode("notes.md") & 0o222, 0, "readonly_ clears all write bits");
    assert_eq!(mode("plain.txt") & 0o111, 0, "plain file has no execute bits");
}

#[test]
fn apply_render_failure_leaves_no_target() {
    // strict undefined 렌더 에러는 plan 단계에서 실패한다. target은 plan 이후 생성되므로 빈 target이
    // 남지 않아야 한다.
    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(template.path().join("scaffold.toml"), "").expect("write scaffold.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("bad.txt.jinja"), "{{ undefined_var }}").expect("write bad jinja");

    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target);

    cmd.assert().failure();
    assert!(
        !target.exists(),
        "a render failure in the plan phase must not create the target directory"
    );
}

#[test]
fn apply_uses_scaffoldroot_effective_source_root() {
    // repo top에 `.scaffoldroot`만 두고 실제 템플릿은 하위 `template/`에 둔다. 실효 루트가 하위로
    // 이동해 거기의 scaffold.toml·files/를 읽어야 한다.
    let repo = tempfile::tempdir().expect("repo tempdir");
    fs::write(repo.path().join(".scaffoldroot"), "template\n").expect("write .scaffoldroot");
    fs::write(repo.path().join("README.md"), "repo readme, not template").expect("write repo readme");
    let inner = repo.path().join("template");
    fs::create_dir_all(inner.join("files")).expect("mkdir inner files");
    fs::write(
        inner.join("scaffold.toml"),
        "[[questions]]\nname = \"project\"\ntype = \"string\"\n",
    )
    .expect("write inner scaffold.toml");
    fs::write(inner.join("files/out.txt.jinja"), "{{ project }}").expect("write inner payload");

    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(repo.path())
        .arg(&target)
        .arg("--answers")
        .arg("project=hi");
    cmd.assert().success();

    let out = fs::read_to_string(target.join("out.txt")).expect("read out.txt");
    assert_eq!(out, "hi");
    // repo top의 README는 템플릿 payload가 아니므로 배치되지 않는다.
    assert!(!target.join("README.md").exists());
}

#[cfg(unix)]
#[test]
fn apply_force_replaces_existing_external_symlink_dest_in_place() {
    use std::os::unix::fs::symlink;

    // target에 외부 파일을 가리키는 기존 심링크가 있고 템플릿이 같은 이름을 쓴다. 이는
    // 외부쓰기가 아니라 overwrite(제자리 교체)여야 한다 — `--force`로 링크가 일반 파일로 교체되고
    // 외부 대상은 불변이어야 한다.
    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(template.path().join("scaffold.toml"), "").expect("write scaffold.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("data.txt"), "generated").expect("write payload");

    let outside = tempfile::tempdir().expect("outside tempdir");
    let external = outside.path().join("secret.txt");
    fs::write(&external, "SECRET").expect("seed external");

    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");
    fs::create_dir_all(&target).expect("mkdir target");
    symlink(&external, target.join("data.txt")).expect("seed dest symlink");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--force");
    cmd.assert().success();

    let meta = fs::symlink_metadata(target.join("data.txt")).expect("stat dest");
    assert!(!meta.file_type().is_symlink(), "dest symlink must be replaced by a regular file");
    assert_eq!(
        fs::read_to_string(target.join("data.txt")).unwrap(),
        "generated"
    );
    assert_eq!(fs::read_to_string(&external).unwrap(), "SECRET", "external target must be untouched");
}

/// `project` 질문 + 인라인 before 훅(`$SCAFFOLDER_PROJECT`를 `hook-out.txt`에 씀)이 있는 템플릿.
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
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--answers")
        .arg("project=demo")
        .arg("--yes");

    cmd.assert().success();

    let out = fs::read_to_string(target.join("hook-out.txt")).expect("read hook-out.txt");
    assert_eq!(out.trim(), "demo", "hook must see SCAFFOLDER_PROJECT env and run with cwd=target");
}

#[test]
fn apply_inline_hook_when_false_is_skipped() {
    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(
        template.path().join("scaffold.toml"),
        r#"
            [[hooks.before]]
            when = "false"
            run = "echo ran > hook-out.txt"
        "#,
    )
    .expect("write scaffold.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("marker.txt"), "marker").expect("write marker.txt");

    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--yes");

    cmd.assert().success();

    assert!(
        !target.join("hook-out.txt").exists(),
        "when=false inline hook must not run"
    );
}

#[cfg(unix)]
#[test]
fn apply_inline_hooks_run_before_folder_hooks_in_declaration_and_lexical_order() {
    use std::os::unix::fs::PermissionsExt;

    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(
        template.path().join("scaffold.toml"),
        r#"
            [[hooks.before]]
            run = "echo a >> order.txt"
        "#,
    )
    .expect("write scaffold.toml");
    let hooks_before = template.path().join("hooks/before");
    fs::create_dir_all(&hooks_before).expect("mkdir hooks/before");
    let script_path = hooks_before.join("z.sh");
    fs::write(&script_path, "#!/bin/sh\necho b >> order.txt\n").expect("write z.sh");
    let mut perms = fs::metadata(&script_path).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).expect("chmod +x z.sh");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("marker.txt"), "marker").expect("write marker.txt");

    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--yes");

    cmd.assert().success();

    let order = fs::read_to_string(target.join("order.txt")).expect("read order.txt");
    assert_eq!(order, "a\nb\n", "inline hooks must run before folder hooks (lexical)");
}

#[test]
fn apply_hook_confirm_required_without_yes_fails_noninteractively_with_no_side_effects() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_hook_env_template(template.path());
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--answers")
        .arg("project=demo");

    // assert_cmd 실행은 기본적으로 비-tty다; --yes 없이 훅이 있으면 confirm이 거절되어 에러여야 한다.
    cmd.assert().failure();

    assert!(
        !target.exists(),
        "unconfirmed hook must abort before target creation (no side effects)"
    );
}

/// `project` 질문 + `.jinja` 폴더훅(`hooks/before/10-gen.sh.jinja`, `{{ project }}`를
/// `rendered-hook-out.txt`에 씀)이 있는 템플릿.
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

/// E2E 회귀: `.jinja` 폴더훅은 실제 MiniJinja 렌더 → temp 파일 → exec 체인을 answer 컨텍스트로
/// 거쳐야 한다(piecewise 단위테스트만으로는 렌더 컨텍스트가 실제로 전달되는지 증명되지 않는다).
#[test]
fn apply_jinja_folder_hook_renders_with_answer_context_and_executes() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_jinja_folder_hook_template(template.path());
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--answers")
        .arg("project=demo")
        .arg("--yes");

    cmd.assert().success();

    let out = fs::read_to_string(target.join("rendered-hook-out.txt")).expect("read rendered-hook-out.txt");
    assert_eq!(out.trim(), "demo", "jinja folder hook must render with answer context and execute");
}

/// payload 파일(`files/marker.txt` = "payload") + after 훅(`cat marker.txt`)이 있는 템플릿.
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

/// E2E 회귀: after 훅은 write 이후에 실행되므로 payload로 배치된 파일을 실제로 읽을 수 있어야
/// 한다(지금까지는 순서가 구조적으로만 증명되었다).
#[test]
fn apply_after_hook_observes_written_payload_file() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_after_hook_observes_payload_template(template.path());
    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--yes");

    cmd.assert().success();

    let out = fs::read_to_string(target.join("after-saw.txt")).expect("read after-saw.txt");
    assert_eq!(out.trim(), "payload", "after hook must observe the already-written payload file");
}

#[test]
fn apply_dry_run_skips_hook_confirm_and_execution() {
    let template = tempfile::tempdir().expect("template tempdir");
    write_hook_env_template(template.path());
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

    // --yes 없이도 dry-run은 confirm·훅 실행 자체를 생략하므로 성공해야 한다.
    cmd.assert().success();

    assert!(!target.exists(), "dry-run must not create the target directory or run hooks");
}

/// 외부(실효 소스 루트 밖) 심링크 제어파일은 `--trust` 없이는 거부되어야 한다
/// (부작용 전 abort); `--trust`로 opt-in하면 정상 로드된다. 내부 심링크는 항상 허용된다.
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

    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target);

    cmd.assert().failure();
    assert!(!target.exists(), "externally symlinked manifest must abort before target creation");
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

    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--trust");

    cmd.assert().success();
    assert_eq!(
        fs::read_to_string(target.join("marker.txt")).expect("read marker.txt"),
        "marker"
    );
}

#[cfg(unix)]
#[test]
fn apply_allows_internally_symlinked_manifest_without_trust() {
    use std::os::unix::fs::symlink;

    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(template.path().join("real-scaffold.toml"), "").expect("write real manifest");
    symlink(
        template.path().join("real-scaffold.toml"),
        template.path().join("scaffold.toml"),
    )
    .expect("symlink scaffold.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("marker.txt"), "marker").expect("write marker.txt");

    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target);

    cmd.assert().success();
    assert_eq!(
        fs::read_to_string(target.join("marker.txt")).expect("read marker.txt"),
        "marker"
    );
}

#[cfg(unix)]
#[test]
fn apply_rejects_externally_symlinked_hook_script_without_trust() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(template.path().join("scaffold.toml"), "").expect("write scaffold.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("marker.txt"), "marker").expect("write marker.txt");

    let outside = tempfile::tempdir().expect("outside tempdir");
    let external_script = outside.path().join("x.sh");
    fs::write(&external_script, "#!/bin/sh\necho hi > out.txt\n").expect("write external hook");
    let mut perms = fs::metadata(&external_script).expect("metadata").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&external_script, perms).expect("chmod +x external hook");
    let hooks_before = template.path().join("hooks/before");
    fs::create_dir_all(&hooks_before).expect("mkdir hooks/before");
    symlink(&external_script, hooks_before.join("x.sh")).expect("symlink hook script");

    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--yes");

    cmd.assert().failure();
    assert!(!target.exists(), "externally symlinked hook script must abort before target creation");
}

/// `.scaffoldroot` 자체가 소스 루트 밖 파일로의 심링크면, 그 내용(실효 소스 루트 선택)을
/// `--trust` 없이 읽어서는 안 된다 — 외부 제어파일 default-refuse 계약이 `.scaffoldroot`에도 적용된다.
#[cfg(unix)]
#[test]
fn apply_rejects_externally_symlinked_scaffoldroot_without_trust() {
    use std::os::unix::fs::symlink;

    let template = tempfile::tempdir().expect("template tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    let external_scaffoldroot = outside.path().join("scaffoldroot-content");
    fs::write(&external_scaffoldroot, "template\n").expect("write external scaffoldroot content");
    symlink(&external_scaffoldroot, template.path().join(".scaffoldroot"))
        .expect("symlink .scaffoldroot");

    let inner = template.path().join("template");
    fs::create_dir_all(inner.join("files")).expect("mkdir inner files");
    fs::write(inner.join("scaffold.toml"), "").expect("write inner scaffold.toml");
    fs::write(inner.join("files/marker.txt"), "marker").expect("write inner marker");

    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target);

    cmd.assert().failure();
    assert!(
        !target.exists(),
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
    symlink(&external_scaffoldroot, template.path().join(".scaffoldroot"))
        .expect("symlink .scaffoldroot");

    let inner = template.path().join("template");
    fs::create_dir_all(inner.join("files")).expect("mkdir inner files");
    fs::write(inner.join("scaffold.toml"), "").expect("write inner scaffold.toml");
    fs::write(inner.join("files/marker.txt"), "marker").expect("write inner marker");

    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--trust");

    cmd.assert().success();
    assert_eq!(
        fs::read_to_string(target.join("marker.txt")).expect("read marker.txt"),
        "marker"
    );
}

#[cfg(unix)]
#[test]
fn apply_fails_on_broken_symlinked_hook_script() {
    use std::os::unix::fs::symlink;

    let template = tempfile::tempdir().expect("template tempdir");
    fs::write(template.path().join("scaffold.toml"), "").expect("write scaffold.toml");
    let files = template.path().join("files");
    fs::create_dir_all(&files).expect("mkdir files");
    fs::write(files.join("marker.txt"), "marker").expect("write marker.txt");

    let hooks_before = template.path().join("hooks/before");
    fs::create_dir_all(&hooks_before).expect("mkdir hooks/before");
    symlink(template.path().join("nowhere"), hooks_before.join("x.sh"))
        .expect("symlink broken hook script");

    let workdir = tempfile::tempdir().expect("workdir tempdir");
    let target = workdir.path().join("demo");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.current_dir(workdir.path())
        .arg("apply")
        .arg(template.path())
        .arg(&target)
        .arg("--yes");

    cmd.assert().failure();
    assert!(!target.exists(), "broken hook script symlink must abort before target creation");
}
