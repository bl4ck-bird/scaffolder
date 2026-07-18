//! `template list`.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use clap::Args;

use crate::domain::store::{TemplateCatalog, TemplateListing};
use crate::infra::load::store::FsTemplateStore;

#[derive(Debug, Args)]
pub struct ListArgs {
    /// 스토어 조회 시 `$SCAFFOLDER_HOME`/`~/.scaffolder`보다 우선하는 디렉토리.
    #[arg(long = "template-dir", value_name = "PATH")]
    pub template_dir: Option<PathBuf>,
}

pub fn run(args: ListArgs) -> Result<()> {
    let store = FsTemplateStore::new(args.template_dir);
    let listings = store.list()?;
    println!("{}", format_listings(&listings));
    Ok(())
}

/// name 기준 정렬 출력. 여러 base에 동명 템플릿이 있으면 base 경로를 힌트로 병기해 구분한다.
fn format_listings(listings: &[TemplateListing]) -> String {
    if listings.is_empty() {
        return "No templates found.".to_string();
    }

    let mut name_counts: HashMap<&str, usize> = HashMap::new();
    for listing in listings {
        *name_counts.entry(listing.name.as_str()).or_insert(0) += 1;
    }

    let mut sorted: Vec<&TemplateListing> = listings.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.base.cmp(&b.base)));

    sorted
        .into_iter()
        .map(|listing| {
            if name_counts[listing.name.as_str()] > 1 {
                format!("{} ({})", listing.name, listing.base.display())
            } else {
                listing.name.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listing(name: &str, base: &str) -> TemplateListing {
        TemplateListing {
            name: name.to_string(),
            path: PathBuf::from(base).join(name),
            base: PathBuf::from(base),
        }
    }

    #[test]
    fn format_listings_empty_gives_guidance() {
        assert_eq!(format_listings(&[]), "No templates found.");
    }

    #[test]
    fn format_listings_sorts_by_name() {
        let listings = vec![listing("zeta", "/base"), listing("alpha", "/base")];
        assert_eq!(format_listings(&listings), "alpha\nzeta");
    }

    #[test]
    fn format_listings_appends_base_hint_for_duplicate_names() {
        let listings = vec![listing("shared", "/first"), listing("shared", "/second")];
        assert_eq!(
            format_listings(&listings),
            "shared (/first)\nshared (/second)"
        );
    }
}
