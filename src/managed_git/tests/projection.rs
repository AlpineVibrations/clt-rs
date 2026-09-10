use std::{collections::BTreeSet, fs, sync::Barrier, thread};

use crate::{
    managed_git::{
        create_agent_git_tree_projection, run_agent_git_projection_command,
        stage_projected_task_tree,
    },
    test_support::{initialize_test_git_repository, run_test_git},
};

#[test]
fn concurrent_git_tree_projections_are_private_and_cleaned_up_independently() {
    const PROJECTIONS: usize = 32;
    let repository = tempfile::tempdir().unwrap();
    let root = repository.path();
    fs::create_dir(root.join("tasks")).unwrap();
    fs::write(root.join("tasks/todo.md"), "- Frozen task\n").unwrap();
    fs::write(root.join("implementation.txt"), "initial\n").unwrap();
    initialize_test_git_repository(root);
    let source_tree = run_test_git(root, &["rev-parse", "HEAD^{tree}"]);

    // Projection reads and writes must leave the live index and worktree alone.
    fs::write(root.join("tasks/todo.md"), "- Staged task\n").unwrap();
    run_test_git(root, &["add", "tasks/todo.md"]);
    fs::write(root.join("tasks/todo.md"), "- Unstaged task\n").unwrap();
    let live_index = fs::read(root.join(".git/index")).unwrap();

    let start = Barrier::new(PROJECTIONS);
    let projections = thread::scope(|scope| {
        let handles = (0..PROJECTIONS)
            .map(|_| {
                scope.spawn(|| {
                    start.wait();
                    create_agent_git_tree_projection(root, &source_tree).unwrap()
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    let roots = projections
        .iter()
        .map(|(projection, _, _)| projection.path().to_path_buf())
        .collect::<BTreeSet<_>>();
    assert_eq!(roots.len(), PROJECTIONS);

    for (projection, index, worktree) in &projections {
        assert_eq!(index.parent(), Some(projection.path()));
        assert_eq!(worktree.parent(), Some(projection.path()));
        assert_eq!(
            fs::read_to_string(worktree.join("tasks/todo.md")).unwrap(),
            "- Frozen task\n"
        );
        assert!(!worktree.join("implementation.txt").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(projection.path())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }

    let mut projections = projections.into_iter();
    let (first, index, worktree) = projections.next().unwrap();
    fs::write(worktree.join("tasks/todo.md"), "- Projected edit\n").unwrap();
    assert_ne!(
        stage_projected_task_tree(root, &index, &worktree, "edit one projection").unwrap(),
        source_tree
    );
    let first_root = first.path().to_path_buf();
    drop(first);
    assert!(!first_root.exists());

    for (projection, index, worktree) in projections {
        assert_eq!(
            fs::read_to_string(worktree.join("tasks/todo.md")).unwrap(),
            "- Frozen task\n"
        );
        assert_eq!(
            run_agent_git_projection_command(
                root,
                &index,
                None,
                &["write-tree"],
                "check an independent projection",
            )
            .unwrap(),
            source_tree
        );
        drop(projection);
    }
    assert!(roots.iter().all(|path| !path.exists()));
    assert_eq!(fs::read(root.join(".git/index")).unwrap(), live_index);
    assert_eq!(
        fs::read_to_string(root.join("tasks/todo.md")).unwrap(),
        "- Unstaged task\n"
    );
}
