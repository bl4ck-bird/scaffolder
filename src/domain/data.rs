//! 정적 데이터 병합 → `data.*`와 `DataSource` 포트.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

/// 렌더 컨텍스트의 `data.*`로 노출되는 정적 값 트리. 도메인 순수성을 위해 toml/minijinja 타입을
/// 누수하지 않는 자체 표현이다. infra가 TOML을 이 타입으로 변환하고, 렌더러가 이 타입을 Jinja
/// 값으로 변환한다.
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
    /// 빈 테이블. 데이터 없는 컨텍스트의 기본값.
    pub fn empty_table() -> Self {
        DataValue::Table(BTreeMap::new())
    }
}

/// `base` 위에 `overlay`를 deep-merge한다. 양쪽이 Table이면 키 단위로 재귀 병합하고, 그 외에는
/// overlay가 base를 대체한다(non-dict replace). §1.5.
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

/// `data/*.toml`을 로드해 하나의 `DataValue` 테이블로 병합하는 포트. `[data]`(매니페스트) 위에
/// lexical 순서로 overlay된다. infra가 TOML 파싱으로 구현한다.
pub trait DataSource {
    fn load(&self, template_root: &Path) -> Result<DataValue>;
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
