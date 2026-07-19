//! Static data merging exposed as `data.*`, and the `DataSource` port.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

/// Static value tree exposed as `data.*`. An own representation so toml/minijinja types do
/// not leak into the domain: infra converts TOML into it, the renderer converts it to Jinja.
#[derive(Debug, Clone, PartialEq)]
pub enum DataValue {
    Table(BTreeMap<String, DataValue>),
    Array(Vec<DataValue>),
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl Default for DataValue {
    fn default() -> Self {
        DataValue::Table(BTreeMap::new())
    }
}

impl DataValue {
    pub fn empty_table() -> Self {
        DataValue::Table(BTreeMap::new())
    }
}

/// Deep-merges `overlay` onto `base`: two tables merge key-by-key recursively, otherwise
/// `overlay` replaces `base`.
pub fn merge(base: DataValue, overlay: DataValue) -> DataValue {
    match (base, overlay) {
        (DataValue::Table(mut base), DataValue::Table(overlay)) => {
            for (key, value) in overlay {
                let merged = match base.remove(&key) {
                    Some(existing) => merge(existing, value),
                    None => value,
                };
                base.insert(key, merged);
            }
            DataValue::Table(base)
        }
        (_, overlay) => overlay,
    }
}

/// Port deep-merging `data/*.toml` onto `base` in lexical order. `base` is the manifest
/// `[data]`; merging is a single left-fold `[data] ▷ f1 ▷ f2 …` — deep-merge is not
/// associative across table→scalar→table, so files must not be merged together first.
/// Implemented by infra via TOML parsing.
pub trait DataSource {
    fn load(&self, template_root: &Path, base: DataValue) -> Result<DataValue>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(pairs: Vec<(&str, DataValue)>) -> DataValue {
        DataValue::Table(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    #[test]
    fn merge_deep_merges_nested_tables() {
        let base = table(vec![(
            "a",
            table(vec![("x", DataValue::Int(1)), ("y", DataValue::Int(2))]),
        )]);
        let overlay = table(vec![(
            "a",
            table(vec![("y", DataValue::Int(20)), ("z", DataValue::Int(3))]),
        )]);

        let merged = merge(base, overlay);

        assert_eq!(
            merged,
            table(vec![(
                "a",
                table(vec![
                    ("x", DataValue::Int(1)),
                    ("y", DataValue::Int(20)),
                    ("z", DataValue::Int(3)),
                ]),
            )])
        );
    }

    #[test]
    fn merge_replaces_non_table_values() {
        let base = table(vec![("k", DataValue::Str("old".into()))]);
        let overlay = table(vec![("k", DataValue::Array(vec![DataValue::Int(1)]))]);

        let merged = merge(base, overlay);

        assert_eq!(
            merged,
            table(vec![("k", DataValue::Array(vec![DataValue::Int(1)]))])
        );
    }

    #[test]
    fn merge_overlay_onto_non_table_base_replaces() {
        let merged = merge(DataValue::Int(1), DataValue::Str("x".into()));
        assert_eq!(merged, DataValue::Str("x".into()));
    }
}
