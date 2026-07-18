//! `scaffolder template list` e2e: 정렬 출력, 빈 스토어 안내, 중복 name base 힌트.

use std::fs;

use assert_cmd::Command;
use predicates::str::contains;

/// `store_dir/name`에 조회 가능한 스토어 템플릿을 만든다.
fn write_store_template(store_dir: &std::path::Path, name: &str) {
    let template_dir = store_dir.join(name);
    fs::create_dir_all(&template_dir).expect("mkdir store template dir");
    fs::write(template_dir.join("scaffold.toml"), "").expect("write scaffold.toml");
}

#[test]
fn template_list_prints_names_sorted() {
    let store_dir = tempfile::tempdir().expect("store tempdir");
    write_store_template(store_dir.path(), "zeta");
    write_store_template(store_dir.path(), "alpha");
    // 실제 개발자 `~/.scaffolder`가 열거에 새지 않도록 가짜 HOME으로 격리한다.
    let fake_home = tempfile::tempdir().expect("fake home tempdir");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.env("SCAFFOLDER_HOME", "")
        .env("XDG_CONFIG_HOME", "")
        .env("HOME", fake_home.path())
        .arg("template")
        .arg("list")
        .arg("--template-dir")
        .arg(store_dir.path());

    let assert = cmd.assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines, vec!["alpha", "zeta"]);
}

#[test]
fn template_list_empty_store_prints_guidance() {
    let store_dir = tempfile::tempdir().expect("store tempdir");
    let fake_home = tempfile::tempdir().expect("fake home tempdir");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.env("SCAFFOLDER_HOME", "")
        .env("XDG_CONFIG_HOME", "")
        .env("HOME", fake_home.path())
        .arg("template")
        .arg("list")
        .arg("--template-dir")
        .arg(store_dir.path());

    cmd.assert().success().stdout(contains("No templates found."));
}

#[test]
fn template_list_duplicate_name_across_bases_shows_base_hint() {
    let template_dir = tempfile::tempdir().expect("template_dir tempdir");
    let scaffolder_home = tempfile::tempdir().expect("scaffolder_home tempdir");
    write_store_template(template_dir.path(), "shared");
    write_store_template(scaffolder_home.path(), "shared");
    let fake_home = tempfile::tempdir().expect("fake home tempdir");

    let mut cmd = Command::cargo_bin("scaffolder").expect("binary");
    cmd.env("SCAFFOLDER_HOME", scaffolder_home.path())
        .env("XDG_CONFIG_HOME", "")
        .env("HOME", fake_home.path())
        .arg("template")
        .arg("list")
        .arg("--template-dir")
        .arg(template_dir.path());

    cmd.assert()
        .success()
        .stdout(contains(template_dir.path().to_str().expect("utf8 path")))
        .stdout(contains(scaffolder_home.path().to_str().expect("utf8 path")));
}
