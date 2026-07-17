//! `data/` 로드·병합 — `DataSource`.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

use crate::domain::data::{merge, DataSource, DataValue};
use crate::infra::load::toml_to_data_value;

/// `<template_root>/data/*.toml`을 파일명 lexical 순서로 deep-merge한다. `data/`가 없으면 빈 테이블.
/// 매니페스트의 `[data]` 위 overlay는 호출부(pipeline)가 수행한다.
pub struct FsDataSource;

impl DataSource for FsDataSource {
    fn load(&self, template_root: &Path) -> Result<DataValue> {
        let data_dir = template_root.join("data");
        let mut acc = DataValue::empty_table();
        if !data_dir.exists() {
            return Ok(acc);
        }

        let mut files: Vec<_> = fs::read_dir(&data_dir)
            .with_context(|| format!("failed to read data dir {}", data_dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file() && p.extension().is_some_and(|ext| ext == "toml"))
            .collect();
        files.sort();

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

    fn get<'a>(dv: &'a DataValue, key: &str) -> Option<&'a DataValue> {
        match dv {
            DataValue::Table(map) => map.get(key),
            _ => None,
        }
    }

    #[test]
    fn merges_data_files_in_lexical_order() {
        let dir = TempDir::new().unwrap();
        let data = dir.path().join("data");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("a.toml"), "shared = \"from-a\"\nonly_a = 1\n").unwrap();
        fs::write(data.join("b.toml"), "shared = \"from-b\"\nonly_b = 2\n").unwrap();

        let loaded = FsDataSource.load(dir.path()).unwrap();

        // b.toml이 lexical 후순위라 shared를 덮는다.
        assert_eq!(get(&loaded, "shared"), Some(&DataValue::Str("from-b".into())));
        assert_eq!(get(&loaded, "only_a"), Some(&DataValue::Int(1)));
        assert_eq!(get(&loaded, "only_b"), Some(&DataValue::Int(2)));
    }

    #[test]
    fn absent_data_dir_returns_empty_table() {
        let dir = TempDir::new().unwrap();
        let loaded = FsDataSource.load(dir.path()).unwrap();
        assert_eq!(loaded, DataValue::Table(BTreeMap::new()));
    }
}
