//! `data/` 로드·병합 — `DataSource`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::domain::data::{merge, DataSource, DataValue};
use crate::infra::load::trust::ensure_within_root;
use crate::infra::load::toml_to_data_value;

/// `<template_root>/data/*.toml`을 파일명 lexical 순서로 `base`(매니페스트 `[data]`) 위에
/// 단일 left-fold한다. `data/`가 없으면 `base` 그대로. 읽기/메타데이터 오류는 조용히 넘기지
/// 않고 전파한다(불완전한 정적 데이터로 진행 방지). `data/` 디렉토리 자체가 외부 심링크면
/// `trust` 없이 거부한다(`read_dir`가 dir 심링크를 follow). 디렉토리 안 leaf 심링크는
/// `file_type()`(no-follow)로 여전히 skip — 외부 읽기 표면이 아니다.
pub struct FsDataSource {
    pub root_canon: PathBuf,
    pub trust: bool,
}

impl DataSource for FsDataSource {
    fn load(&self, template_root: &Path, base: DataValue) -> Result<DataValue> {
        let data_dir = template_root.join("data");
        if !data_dir.exists() {
            return Ok(base);
        }
        ensure_within_root(&data_dir, &self.root_canon, self.trust)?;

        let mut files: Vec<PathBuf> = Vec::new();
        for entry in fs::read_dir(&data_dir)
            .with_context(|| format!("failed to read data dir {}", data_dir.display()))?
        {
            let entry = entry
                .with_context(|| format!("failed to read entry in {}", data_dir.display()))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .with_context(|| format!("failed to stat {}", path.display()))?;
            if file_type.is_file() && path.extension().is_some_and(|ext| ext == "toml") {
                files.push(path);
            }
        }
        files.sort();

        let mut acc = base;
        for path in files {
            let text = fs::read_to_string(&path)
                .with_context(|| format!("failed to read data file {}", path.display()))?;
            let value: toml::Value = toml::from_str(&text)
                .with_context(|| format!("invalid TOML in data file {}", path.display()))?;
            acc = merge(acc, toml_to_data_value(&value));
        }

        Ok(acc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn table(pairs: Vec<(&str, DataValue)>) -> DataValue {
        DataValue::Table(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    fn get<'a>(dv: &'a DataValue, key: &str) -> Option<&'a DataValue> {
        match dv {
            DataValue::Table(map) => map.get(key),
            _ => None,
        }
    }

    fn source(dir: &std::path::Path) -> FsDataSource {
        FsDataSource {
            root_canon: dir.canonicalize().unwrap(),
            trust: false,
        }
    }

    #[test]
    fn merges_data_files_in_lexical_order_onto_base() {
        let dir = TempDir::new().unwrap();
        let data = dir.path().join("data");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("a.toml"), "shared = \"from-a\"\nonly_a = 1\n").unwrap();
        fs::write(data.join("b.toml"), "shared = \"from-b\"\nonly_b = 2\n").unwrap();

        let base = table(vec![("from_base", DataValue::Int(0))]);
        let loaded = source(dir.path()).load(dir.path(), base).unwrap();

        assert_eq!(get(&loaded, "from_base"), Some(&DataValue::Int(0)));
        assert_eq!(get(&loaded, "shared"), Some(&DataValue::Str("from-b".into())));
        assert_eq!(get(&loaded, "only_a"), Some(&DataValue::Int(1)));
        assert_eq!(get(&loaded, "only_b"), Some(&DataValue::Int(2)));
    }

    #[test]
    fn single_fold_does_not_resurrect_replaced_values() {
        // base(manifest) settings.a=1 → a.toml settings="reset"(table→scalar) → b.toml settings.b=2.
        // 정본 순서는 {settings:{b:2}}. 파일끼리 먼저 병합 후 base에 합치면 a가 부활해 오답이 된다.
        let dir = TempDir::new().unwrap();
        let data = dir.path().join("data");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("a.toml"), "settings = \"reset\"\n").unwrap();
        fs::write(data.join("b.toml"), "[settings]\nb = 2\n").unwrap();

        let base = table(vec![("settings", table(vec![("a", DataValue::Int(1))]))]);
        let loaded = source(dir.path()).load(dir.path(), base).unwrap();

        assert_eq!(
            get(&loaded, "settings"),
            Some(&table(vec![("b", DataValue::Int(2))])),
            "replaced-then-retabled key must not resurrect base's a=1"
        );
    }

    #[test]
    fn absent_data_dir_returns_base() {
        let dir = TempDir::new().unwrap();
        let base = table(vec![("k", DataValue::Bool(true))]);
        let loaded = source(dir.path()).load(dir.path(), base.clone()).unwrap();
        assert_eq!(loaded, base);
    }

    #[test]
    fn empty_base_and_absent_dir_is_empty_table() {
        let dir = TempDir::new().unwrap();
        let loaded = source(dir.path())
            .load(dir.path(), DataValue::empty_table())
            .unwrap();
        assert_eq!(loaded, DataValue::Table(BTreeMap::new()));
    }

    #[test]
    fn internal_symlinked_data_dir_is_allowed() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let real_data = dir.path().join("real-data");
        fs::create_dir_all(&real_data).unwrap();
        fs::write(real_data.join("a.toml"), "k = 1\n").unwrap();
        symlink(&real_data, dir.path().join("data")).unwrap();

        let loaded = source(dir.path())
            .load(dir.path(), DataValue::empty_table())
            .unwrap();
        assert_eq!(get(&loaded, "k"), Some(&DataValue::Int(1)));
    }

    #[test]
    fn external_symlinked_data_dir_is_rejected_without_trust() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let external_data = outside.path().join("data");
        fs::create_dir_all(&external_data).unwrap();
        fs::write(external_data.join("a.toml"), "k = 1\n").unwrap();
        symlink(&external_data, dir.path().join("data")).unwrap();

        let result = source(dir.path()).load(dir.path(), DataValue::empty_table());
        assert!(result.is_err());
    }

    #[test]
    fn external_symlinked_data_dir_is_allowed_with_trust() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let external_data = outside.path().join("data");
        fs::create_dir_all(&external_data).unwrap();
        fs::write(external_data.join("a.toml"), "k = 1\n").unwrap();
        symlink(&external_data, dir.path().join("data")).unwrap();

        let trusted = FsDataSource {
            root_canon: dir.path().canonicalize().unwrap(),
            trust: true,
        };
        let loaded = trusted.load(dir.path(), DataValue::empty_table()).unwrap();
        assert_eq!(get(&loaded, "k"), Some(&DataValue::Int(1)));
    }
}
