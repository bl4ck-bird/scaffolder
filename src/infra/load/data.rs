//! `data/` loading and merging — `DataSource`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::domain::data::{DataSource, DataValue, merge};
use crate::infra::load::toml_to_data_value;
use crate::infra::load::trust::ensure_within_root;

/// Merges `<template_root>/data/*.toml` onto `base` (the manifest `[data]`), folding the files in
/// one pass in lexical order by name. If there is no `data/` directory, `base` is returned
/// unchanged. Read and metadata errors are propagated rather than swallowed, so we never proceed
/// with static data that only partially loaded. If `data/` itself is a symlink pointing outside
/// the template it is rejected unless the caller passed `trust`, because `read_dir` follows a
/// directory symlink. Symlinks among the files inside are simply skipped, since `file_type()`
/// does not follow them, so they are not a way to read outside the template.
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
            let entry =
                entry.with_context(|| format!("failed to read entry in {}", data_dir.display()))?;
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
    use std::os::unix::fs::symlink;
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
        assert_eq!(
            get(&loaded, "shared"),
            Some(&DataValue::Str("from-b".into()))
        );
        assert_eq!(get(&loaded, "only_a"), Some(&DataValue::Int(1)));
        assert_eq!(get(&loaded, "only_b"), Some(&DataValue::Int(2)));
    }

    #[test]
    fn single_fold_does_not_resurrect_replaced_values() {
        // Fold order: base (manifest) has settings.a=1, then a.toml sets settings="reset" (a table
        // becomes a scalar), then b.toml sets settings.b=2. Applied in that order the result is
        // {settings:{b:2}}. If the two files were merged with each other first and only then onto
        // base, the base's a=1 would come back — the wrong answer.
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

    #[test]
    fn in_dir_file_symlink_to_external_is_skipped_not_followed() {
        let dir = TempDir::new().unwrap();
        let data = dir.path().join("data");
        fs::create_dir_all(&data).unwrap();
        let outside = TempDir::new().unwrap();
        // Malformed TOML: if a regression ever followed and parsed this symlink, the load would
        // fail loudly instead of silently returning an empty table.
        let secret = outside.path().join("secret.toml");
        fs::write(&secret, "leaked = = =\n").unwrap();
        symlink(&secret, data.join("a.toml")).unwrap();

        let loaded = source(dir.path())
            .load(dir.path(), DataValue::empty_table())
            .unwrap();

        // file_type() does not follow the symlink, so the entry is not a file and is skipped:
        // no external read, no trust bypass, and no error.
        assert_eq!(loaded, DataValue::Table(BTreeMap::new()));
    }

    #[test]
    fn non_toml_files_in_data_dir_are_ignored() {
        let dir = TempDir::new().unwrap();
        let data = dir.path().join("data");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("a.toml"), "k = 1\n").unwrap();
        // Non-.toml files are skipped by extension, so this invalid-TOML content is never parsed.
        fs::write(data.join("notes.txt"), "this is not toml === {{{").unwrap();
        fs::write(data.join("README"), "no extension").unwrap();

        let loaded = source(dir.path())
            .load(dir.path(), DataValue::empty_table())
            .unwrap();

        assert_eq!(get(&loaded, "k"), Some(&DataValue::Int(1)));
    }
}
