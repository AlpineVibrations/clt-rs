//! Explicit project task codes in existing boards, without rewriting their prose.
use anyhow::{Context, Result};
use regex::Regex;
use std::{collections::BTreeSet, sync::LazyLock};

static CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Z][A-Z0-9]*(?:-[A-Z0-9]+)*-[0-9]+$").unwrap());
static DEPENDENCIES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[.\n])\s*Dependencies:\s*([^\n.]*)").unwrap());

#[derive(Clone, Debug, Default)]
pub(crate) struct References {
    pub code: Option<String>,
    pub depends_on: Vec<String>,
}

impl References {
    pub(crate) fn read(content: &str) -> Result<Self> {
        let text = crate::task::thinking::strip(&crate::task::task_content_for_edit(content));
        let text = text.trim();
        let text = text
            .strip_prefix("/goal")
            .filter(|rest| rest.starts_with(char::is_whitespace))
            .unwrap_or(text)
            .trim_start();
        let title = text.trim_start_matches('#').trim_start();
        let code = title
            .split_whitespace()
            .next()
            .map(|word| word.trim_end_matches(['.', ':']))
            .filter(|word| CODE.is_match(word))
            .map(str::to_owned);
        let mut depends_on = BTreeSet::new();
        for clause in DEPENDENCIES.captures_iter(text) {
            let value = clause[1].trim();
            // Ordinary dependency prose (packages, services, etc.) is not a
            // task reference. Once a clause names a code, require exact syntax
            // rather than silently dropping an ambiguous prerequisite.
            if !value
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
                .any(|word| CODE.is_match(word))
            {
                continue;
            }
            for item in value.split(',').map(str::trim) {
                if CODE.is_match(item) {
                    depends_on.insert(item.to_owned());
                    continue;
                }
                let words: Vec<_> = item.split_whitespace().collect();
                let [first, "through", last] = words.as_slice() else {
                    anyhow::bail!(
                        "Invalid Dependencies clause {value:?}; use comma-separated task codes or CODE-001 through CODE-003"
                    );
                };
                anyhow::ensure!(
                    CODE.is_match(first) && CODE.is_match(last),
                    "Invalid task-code range {item:?}"
                );
                let (prefix, start) = first.rsplit_once('-').unwrap();
                let (last_prefix, end) = last.rsplit_once('-').unwrap();
                let start_number: u64 = start.parse().context("Task-code range is too large")?;
                let end_number: u64 = end.parse().context("Task-code range is too large")?;
                anyhow::ensure!(
                    prefix == last_prefix
                        && start.len() == end.len()
                        && start_number <= end_number
                        && end_number - start_number < 1000,
                    "Invalid task-code range {item:?}; use the same prefix and numeric width, in ascending order, with at most 1000 tasks"
                );
                for number in start_number..=end_number {
                    depends_on.insert(format!("{prefix}-{number:0width$}", width = start.len()));
                }
            }
        }
        Ok(Self {
            code,
            depends_on: depends_on.into_iter().collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::References;

    #[test]
    fn explicit_codes_lists_and_ranges_preserve_spelling() {
        let refs = References::read("/think high /goal MM-020 Add stamps. Dependencies: MM-001 through MM-003, MM-019. Scope: assets.").unwrap();
        assert_eq!(refs.code.as_deref(), Some("MM-020"));
        assert_eq!(refs.depends_on, ["MM-001", "MM-002", "MM-003", "MM-019"]);
        let refs = References::read(
            "# API-V2-004\n\nUse the API.\nDependencies: API-V2-003, API-V2-003\n\nDetails.",
        )
        .unwrap();
        assert_eq!(refs.code.as_deref(), Some("API-V2-004"));
        assert_eq!(refs.depends_on, ["API-V2-003"]);
    }

    #[test]
    fn ordinary_mentions_and_dependency_prose_are_not_references() {
        for text in [
            "Discuss MM-019 and MM-020.",
            "Upgrade dependencies: npm, rust.",
            "Check setup. Dependencies: Node.js and npm.",
            "Document `Dependencies: MM-019` syntax.",
            "Read docs/roadmap.md#mm-019.",
        ] {
            let refs = References::read(text).unwrap();
            assert!(refs.code.is_none(), "{text}");
            assert!(refs.depends_on.is_empty(), "{text}");
        }
    }

    #[test]
    fn malformed_code_dependencies_do_not_become_ready_silently() {
        for dependencies in [
            "MM-019 or MM-020",
            "MM-019, maybe",
            "MM-019 through XX-020",
            "MM-020 through MM-019",
            "MM-001 through MM-9999",
            "MM-01 through MM-003",
            "MM-1 through MM-999999999999999999999999999999999",
        ] {
            assert!(
                References::read(&format!("MM-021 Work. Dependencies: {dependencies}.")).is_err(),
                "{dependencies}"
            );
        }
    }
}
