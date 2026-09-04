use std::{
    fs,
    path::{Path, PathBuf},
};

fn rust_sources(directory: &Path) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if path.file_name().and_then(|name| name.to_str()) != Some("tests") {
                sources.extend(rust_sources(&path));
            }
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs")
            && !matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("tests.rs" | "test_support.rs")
            )
        {
            sources.push(path);
        }
    }
    sources
}

#[test]
fn launcher_and_library_expose_one_entry_point() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let main = fs::read_to_string(root.join("src/main.rs")).unwrap();
    let library = fs::read_to_string(root.join("src/lib.rs")).unwrap();

    assert_eq!(
        main,
        "fn main() -> anyhow::Result<()> {\n    clt_rs::run()\n}\n"
    );
    let public_items = library
        .lines()
        .filter(|line| line.starts_with("pub "))
        .collect::<Vec<_>>();
    assert_eq!(public_items, ["pub fn run() -> Result<()> {"]);
}

#[test]
fn production_modules_have_explicit_imports_and_no_reexport_layer() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for path in rust_sources(&source_root) {
        let source = fs::read_to_string(&path).unwrap();
        assert!(
            !source.contains("use super::*;"),
            "{} still has a wildcard parent import",
            path.display()
        );
        assert!(
            !source.lines().any(|line| line.trim_end().ends_with("::*;")),
            "{} still has a production wildcard import",
            path.display()
        );
        assert!(
            !source.lines().any(|line| {
                let line = line.trim_start();
                line.starts_with("pub use ") || line.starts_with("pub(super) use ")
            }),
            "{} still has a transitional re-export",
            path.display()
        );
    }
}

#[test]
fn agent_store_owns_one_runtime_and_repository_boundaries() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let facade = fs::read_to_string(root.join("agent.rs")).unwrap();
    let runtime = fs::read_to_string(root.join("agent/runtime.rs")).unwrap();
    let repositories = [
        "projects_models.rs",
        "workers_leases.rs",
        "sessions_runs.rs",
        "git_journals.rs",
    ];

    assert!(!facade.contains("tokio::runtime::Runtime::new()"));
    assert_eq!(runtime.matches("tokio::runtime::Runtime::new()").count(), 1);
    for repository in repositories {
        let source = fs::read_to_string(root.join("agent/repositories").join(repository)).unwrap();
        assert!(source.contains("impl TursoAgentStore"));
        assert!(!source.contains("tokio::runtime::Runtime::new()"));
    }
}

#[test]
fn final_module_map_is_recorded() {
    let architecture =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("ARCHITECTURE.md")).unwrap();

    for module in [
        "`cli`",
        "`application`",
        "`task`",
        "`agent`",
        "`managed_git`",
        "`scheduler`",
        "`worker`",
        "`runner`",
        "`session_control`",
        "`tui`",
    ] {
        assert!(
            architecture.contains(module),
            "missing {module} from module map"
        );
    }
}
