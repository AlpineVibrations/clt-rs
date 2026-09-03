use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_WORKSPACE: AtomicU64 = AtomicU64::new(0);

struct TestWorkspace {
    root: PathBuf,
}

impl TestWorkspace {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = NEXT_WORKSPACE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "clt-cli-{label}-{}-{nonce}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create isolated CLI test workspace");
        Self { root }
    }

    fn path(&self) -> &Path {
        &self.root
    }

    fn run(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_clt"))
            .current_dir(&self.root)
            .arg("--local")
            .args(arguments)
            .env("CLT_AGENT_STATE_DIR", self.root.join("agent-state"))
            .env_remove("CLT_AGENT_PROJECT_ID")
            .env_remove("CLT_AGENT_RUN_TOKEN")
            .output()
            .expect("run clt binary")
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn output_text(output: &Output) -> (String, String) {
    (
        String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8"),
        String::from_utf8(output.stderr.clone()).expect("stderr is UTF-8"),
    )
}

fn assert_success(output: &Output) -> (String, String) {
    let (stdout, stderr) = output_text(output);
    assert!(
        output.status.success(),
        "command failed with {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status.code()
    );
    (stdout, stderr)
}

#[test]
fn help_is_reported_on_stdout_with_a_success_exit() {
    let workspace = TestWorkspace::new("help");
    let output = workspace.run(&["--help"]);
    let (stdout, stderr) = assert_success(&output);

    assert!(stdout.contains("A simple file-system-backed task management system"));
    assert!(stdout.contains("Usage:"));
    for command in [
        "init",
        "expand",
        "add",
        "status",
        "done",
        "delete",
        "list",
        "shell-init",
        "agent",
    ] {
        assert!(
            stdout.contains(command),
            "help omitted {command}:\n{stdout}"
        );
    }
    assert!(stderr.is_empty(), "unexpected stderr:\n{stderr}");
}

#[test]
fn shell_init_prints_sourceable_wrappers_without_creating_a_board() {
    let workspace = TestWorkspace::new("shell-init");

    let bash = workspace.run(&["shell-init", "bash"]);
    let (bash_stdout, bash_stderr) = assert_success(&bash);
    assert!(bash_stdout.starts_with("clt() {\n"));
    assert!(bash_stdout.contains("command clt --cwd-file \"$cwd_file\" \"$@\""));
    assert!(bash_stdout.contains("builtin cd -- \"$cwd\""));
    assert!(bash_stdout.ends_with("}\n"));
    assert!(bash_stderr.is_empty());

    let zsh = workspace.run(&["shell-init", "zsh"]);
    let (zsh_stdout, zsh_stderr) = assert_success(&zsh);
    assert_eq!(zsh_stdout, bash_stdout);
    assert!(zsh_stderr.is_empty());
    assert!(!workspace.path().join("tasks").exists());
}

#[test]
fn markdown_board_commands_complete_a_full_task_lifecycle() {
    let workspace = TestWorkspace::new("lifecycle");

    let (init_stdout, init_stderr) = assert_success(&workspace.run(&["init"]));
    assert!(init_stdout.contains("Initialization complete."));
    assert!(init_stderr.is_empty());
    for (file, heading) in [
        ("backlog.md", "# Backlog Tasks\n"),
        ("todo.md", "# To Do Tasks\n"),
        ("doing.md", "# Doing Tasks\n"),
        ("done.md", "# Done Tasks\n"),
    ] {
        assert_eq!(
            fs::read_to_string(workspace.path().join("tasks").join(file)).unwrap(),
            heading
        );
    }

    let first = workspace.run(&["add", "Write", "black-box", "contracts", "TEST, HIGH"]);
    let (first_stdout, first_stderr) = assert_success(&first);
    assert_eq!(first_stdout, "Task added successfully.\n");
    assert!(first_stderr.is_empty());
    assert_success(&workspace.run(&["add", "Remove", "obsolete", "script"]));

    let (todo_stdout, todo_stderr) = assert_success(&workspace.run(&["list", "todo"]));
    assert_eq!(
        todo_stdout,
        "\n--- TODO ---\n1. Write black-box contracts (TEST, HIGH)\n2. Remove obsolete script\n"
    );
    assert!(todo_stderr.is_empty());

    let (status_stdout, status_stderr) =
        assert_success(&workspace.run(&["status", "todo", "1", "doing"]));
    assert!(status_stdout.is_empty());
    assert!(status_stderr.is_empty());
    let (doing_stdout, _) = assert_success(&workspace.run(&["list", "doing"]));
    assert_eq!(
        doing_stdout,
        "\n--- DOING ---\n1. Write black-box contracts (TEST, HIGH)\n"
    );

    let (done_stdout, done_stderr) = assert_success(&workspace.run(&["done", "doing", "1"]));
    assert_eq!(done_stdout, "Task 1 from doing marked as done.\n");
    assert!(done_stderr.is_empty());
    let (done_list, _) = assert_success(&workspace.run(&["list", "done"]));
    assert_eq!(
        done_list,
        "\n--- DONE ---\n1. Write black-box contracts (TEST, HIGH)\n"
    );

    let (delete_stdout, delete_stderr) = assert_success(&workspace.run(&["delete", "todo", "1"]));
    assert_eq!(delete_stdout, "Task 1 from todo deleted successfully.\n");
    assert!(delete_stderr.is_empty());
    let (empty_todo, _) = assert_success(&workspace.run(&["list", "todo"]));
    assert_eq!(empty_todo, "\n--- TODO ---\n");
}

#[test]
fn list_without_a_filter_reports_every_board_status() {
    let workspace = TestWorkspace::new("list-all");
    assert_success(&workspace.run(&["init"]));
    assert_success(&workspace.run(&["add", "Listed task"]));

    let (stdout, stderr) = assert_success(&workspace.run(&["list"]));
    assert_eq!(
        stdout,
        "\n--- TODO ---\n1. Listed task\n\n--- DOING ---\n\n--- DONE ---\n\n--- BACKLOG ---\n"
    );
    assert!(stderr.is_empty());
}

#[test]
fn expand_preserves_markdown_and_keeps_tasks_visible() {
    let workspace = TestWorkspace::new("expand");
    assert_success(&workspace.run(&["init"]));
    assert_success(&workspace.run(&["add", "First task"]));
    assert_success(&workspace.run(&["add", "Second task"]));

    let (stdout, stderr) = assert_success(&workspace.run(&["expand", "todo"]));
    assert!(stdout.contains("Expanded todo to"));
    assert!(stdout.contains("with 2 task file(s)"));
    assert!(stdout.contains("todo.md.bak"));
    assert!(stderr.is_empty());

    let tasks = workspace.path().join("tasks");
    assert!(tasks.join("todo").is_dir());
    assert!(!tasks.join("todo.md").exists());
    assert_eq!(
        fs::read_to_string(tasks.join("todo.md.bak")).unwrap(),
        "# To Do Tasks\n- First task\n- Second task\n"
    );
    assert_eq!(
        fs::read_to_string(tasks.join("todo/0001-first-task.md")).unwrap(),
        "First task\n"
    );
    assert_eq!(
        fs::read_to_string(tasks.join("todo/0002-second-task.md")).unwrap(),
        "Second task\n"
    );

    let (list_stdout, list_stderr) = assert_success(&workspace.run(&["list", "todo"]));
    assert_eq!(
        list_stdout,
        "\n--- TODO ---\n1. First task\n2. Second task\n"
    );
    assert!(list_stderr.is_empty());
}

#[test]
fn folder_initialization_creates_all_status_directories() {
    let workspace = TestWorkspace::new("folder-init");
    let (stdout, stderr) = assert_success(&workspace.run(&["init", "--folders"]));
    assert!(stdout.contains("Initialization complete."));
    assert!(stderr.is_empty());

    for status in ["backlog", "todo", "doing", "done"] {
        assert!(workspace.path().join("tasks").join(status).is_dir());
        assert!(
            !workspace
                .path()
                .join("tasks")
                .join(format!("{status}.md"))
                .exists()
        );
    }
}

#[test]
fn invalid_inputs_fail_on_stderr_without_mutating_the_board() {
    let workspace = TestWorkspace::new("failures");
    assert_success(&workspace.run(&["init"]));
    let todo_before = fs::read_to_string(workspace.path().join("tasks/todo.md")).unwrap();

    let zero_index = workspace.run(&["status", "todo", "0", "doing"]);
    let (zero_stdout, zero_stderr) = output_text(&zero_index);
    assert_eq!(zero_index.status.code(), Some(1));
    assert!(zero_stdout.is_empty());
    assert!(zero_stderr.contains("Task index must be 1 or greater."));

    let bad_status = workspace.run(&["list", "waiting"]);
    let (bad_status_stdout, bad_status_stderr) = output_text(&bad_status);
    assert_eq!(bad_status.status.code(), Some(1));
    assert_eq!(bad_status_stdout, "\n--- WAITING ---\n");
    assert!(bad_status_stderr.contains("Invalid status."));

    let missing_task = workspace.run(&["delete", "todo", "1"]);
    let (missing_stdout, missing_stderr) = output_text(&missing_task);
    assert_eq!(missing_task.status.code(), Some(1));
    assert!(missing_stdout.is_empty());
    assert!(missing_stderr.contains("Task index 1 out of range"));

    let clap_error = workspace.run(&["add"]);
    let (clap_stdout, clap_stderr) = output_text(&clap_error);
    assert_eq!(clap_error.status.code(), Some(2));
    assert!(clap_stdout.is_empty());
    assert!(clap_stderr.contains("required arguments were not provided"));

    assert_eq!(
        fs::read_to_string(workspace.path().join("tasks/todo.md")).unwrap(),
        todo_before
    );
}
