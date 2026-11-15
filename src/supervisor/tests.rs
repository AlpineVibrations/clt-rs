use super::*;
use crate::test_support::prelude::*;
use crate::test_support::*;

fn fixture(
    name: &str,
) -> (
    PathBuf,
    PathBuf,
    agent::TursoAgentStore,
    agent::AgentProject,
) {
    let root = temp_root(name);
    let state = root.join("state");
    let path = root.join("project");
    init_tasks(&path, false).unwrap();
    insert_task(&path, TaskStatus::Doing, None,
        "Fix renderer. BLOCKED 2026-10-06: Transparent objects remain visible. codex:original-session", None).unwrap();
    add_task(&path, "Downstream integration must stay queued", None).unwrap();
    let store = agent::TursoAgentStore::open_blocking(&state).unwrap();
    store.register_project_blocking(&path, name).unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    (root, state, store, project)
}
fn enable(store: &agent::TursoAgentStore) {
    store
        .set_supervisor_settings_blocking(&SupervisorSettings {
            enabled: true,
            ..Default::default()
        })
        .unwrap();
}
fn result(root: &Path, decision: serde_json::Value) -> AgentRunResult {
    let stderr = root.join("review.err");
    fs::write(output_path(&stderr), serde_json::to_vec(&decision).unwrap()).unwrap();
    AgentRunResult {
        status: "success",
        exit_code: Some(0),
        log_dir: root.into(),
        stdout_path: root.join("review.out"),
        stderr_path: stderr,
        summary: "reviewed".into(),
        codex_session_id: None,
        session_run_token: None,
        control_action: None,
    }
}

#[test]
fn supervisor_settings_and_decisions_survive_restart_and_snapshot() {
    let (root, state, store, project) = fixture("supervisor-persistence");
    assert!(!store.supervisor_settings_blocking().unwrap().enabled);
    let settings = SupervisorSettings {
        enabled: true,
        provider: Some("openai".into()),
        model: Some("review-model".into()),
        thinking: Some("xhigh".into()),
        fast: true,
    };
    store.set_supervisor_settings_blocking(&settings).unwrap();
    begin_review(&store, &project).unwrap();
    let response = result(
        &root,
        json!({"decision":"user","task":1,"reason":"Need an acceptance decision","next_action":"Should transparent objects be fully concealed?"}),
    );
    finish_review(&store, &project, &response).unwrap();
    let saved = store
        .supervisor_review_blocking(project.id)
        .unwrap()
        .unwrap();
    drop(store);
    let store = agent::TursoAgentStore::open_blocking(&state).unwrap();
    assert_eq!(store.supervisor_settings_blocking().unwrap(), settings);
    assert_eq!(
        store.supervisor_review_blocking(project.id).unwrap(),
        Some(saved)
    );
    assert!(matches!(
        gate(&store, &project, true).unwrap(),
        Gate::Hold(_)
    ));
    let snapshot: serde_json::Value =
        serde_json::from_slice(&fs::read(state.join("registry.json")).unwrap()).unwrap();
    assert_eq!(snapshot["version"], 3);
    assert_eq!(
        snapshot["tables"]["supervisor_reviews"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn supervisor_repair_guidance_uses_board_position_and_preserves_working_proof() {
    for folders in [false, true] {
        let root = temp_root("supervisor-external-completion-guidance");
        let path = root.join("project");
        init_tasks(&path, folders).unwrap();
        let board = TaskBoard::new(get_tasks_dir(&path));
        for task in [
            "Keep first task stopped clt:stopped",
            "Keep second task stopped clt:stopped",
            "Apply elite and weak templates. BLOCKED 2026-11-01: Parent importer is uncommitted. codex:templates",
        ] {
            board.insert_content(TaskStatus::Todo, None, task).unwrap();
        }
        initialize_test_git_repository(&path);
        let store = agent::TursoAgentStore::open_blocking(&root.join("state")).unwrap();
        store.register_project_blocking(&path, "project").unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let start = capture_agent_git_start_state(&path, AgentGitMode::Commit).unwrap();
        let before = board.entries(TaskStatus::Todo).unwrap();
        store
            .create_git_finalization_blocking(agent::NewGitFinalization {
                project_id: project.id,
                codex_session_id: "templates",
                git_mode: AgentGitMode::Commit,
                starting_head: Some(&start.starting_head),
                branch_ref: start.branch_ref.as_deref(),
                upstream_ref: start.upstream_ref.as_deref(),
                worktree_baseline: &start.worktree_baseline,
                task_identity: durable_task_identity(&before[2].content).as_deref(),
                owner_run_token: None,
                created_at: "100",
            })
            .unwrap();
        let journal = store
            .git_finalization_blocking(project.id, "templates")
            .unwrap();
        enable(&store);
        begin_review(&store, &project).unwrap();
        let mut command = Command::new("codex");
        configure_command(&mut command, &store, &project, &root.join("run.err")).unwrap();
        let prompt = command.get_args().last().unwrap().to_string_lossy();
        assert!(prompt.contains("\"board_index\":3"), "{prompt}");
        assert!(prompt.contains("Choose user whenever acceptance or authorization is required"));
        assert!(prompt.contains("clt done todo <index>"));

        // Existing Repair decisions remain readable without a model re-review.
        finish_review(&store, &project, &result(&root, json!({
            "decision":"repair", "task":1,
            "reason":"The implementation already exists in a mixed-scope commit without a task trailer.",
            "next_action":"Reconcile the stale WORKING journal through external completion."
        }))).unwrap();
        let review = store
            .supervisor_review_blocking(project.id)
            .unwrap()
            .unwrap();
        let details = review_details(&store, &project, &review).unwrap();
        assert!(
            details.contains("Task: TODO #3 — Apply elite and weak templates."),
            "{details}"
        );
        assert!(details.contains("Action required: repair recommendation"));
        assert!(details.contains("does not run a repair automatically"));
        assert!(details.contains("clt list todo"));
        assert!(details.contains("clt done todo <index>"));
        assert!(details.contains("currently 3"));
        assert!(details.contains("explicit acceptance"));
        assert!(matches!(
            gate(&store, &project, true).unwrap(),
            Gate::Hold(_)
        ));
        assert_eq!(
            store
                .git_finalization_blocking(project.id, "templates")
                .unwrap(),
            journal
        );
        let after = board.entries(TaskStatus::Todo).unwrap();
        assert_eq!(after.len(), before.len());
        for (after, before) in after.iter().zip(&before) {
            assert_eq!(after.content, before.content);
            assert_eq!(after.source, before.source);
        }
        assert!(board.entries(TaskStatus::Done).unwrap().is_empty());
        assert_eq!(
            run_test_git(&path, &["rev-parse", "HEAD"]).trim(),
            start.starting_head
        );

        // A changed board cannot route a saved review's index to another task.
        board
            .insert_content(TaskStatus::Todo, Some(0), "New task clt:stopped")
            .unwrap();
        let stale = review_details(&store, &project, &review).unwrap();
        assert!(stale.contains("evidence has changed"));
        assert!(!stale.contains("clt done"));
        assert!(!stale.contains("TODO #3"));
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn supervisor_without_working_journal_does_not_offer_external_completion() {
    let (root, _, store, project) = fixture("supervisor-no-external-completion");
    enable(&store);
    begin_review(&store, &project).unwrap();
    finish_review(&store, &project, &result(&root, json!({
        "decision":"user", "task":1,
        "reason":"Requirements need a decision.", "next_action":"Which behavior should be implemented?"
    }))).unwrap();
    let review = store
        .supervisor_review_blocking(project.id)
        .unwrap()
        .unwrap();
    let details = review_details(&store, &project, &review).unwrap();
    assert!(details.contains("Action required: your input"));
    assert!(!details.contains("External completion option"));
    assert!(!details.contains("clt done"));
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

struct ReviewingRunner;
impl crate::runner::AgentRunner for ReviewingRunner {
    fn run_project(&self, request: crate::runner::AgentRunRequest<'_>) -> Result<AgentRunResult> {
        assert_eq!(request.task_selection, AgentTaskSelection::SupervisorReview);
        assert_eq!(request.project.git_mode, AgentGitMode::Off);
        assert_eq!(
            request.project.codex_reasoning_effort.as_deref(),
            Some("high")
        );
        Ok(result(
            &request.project.path,
            json!({"decision":"retry","task":1,"reason":"A depth prepass can preserve concealment","next_action":"Implement a depth prepass and run the transparent-object probe."}),
        ))
    }
}

#[test]
fn supervisor_reviews_before_downstream_work_then_retries_the_original_session() {
    let (root, state, store, project) = fixture("supervisor-scheduler");
    enable(&store);
    let mut pass = run_agent_scheduler_pass(&state, false, &[]).unwrap();
    assert_eq!(pass.jobs.len(), 1);
    assert_eq!(pass.pass.runs_started, 1);
    assert_eq!(
        pass.jobs[0].task_selection,
        AgentTaskSelection::SupervisorReview
    );
    let completion = run_agent_job(
        pass.jobs.pop().unwrap(),
        &ReviewingRunner,
        &new_agent_shutdown_signal(),
    )
    .unwrap();
    assert_eq!(completion.status, "success", "{}", completion.summary);
    let tasks = read_task_entries(&get_tasks_dir(&project.path), TaskStatus::Doing).unwrap();
    assert_eq!(tasks.len(), 1);
    assert!(task_entry_is_blocked(&tasks[0]));
    assert_eq!(
        recoverable_codex_session_id_from_task_content(&tasks[0].content),
        Some("original-session")
    );
    let pass = run_agent_scheduler_pass(&state, false, &[]).unwrap();
    assert_eq!(pass.jobs.len(), 1);
    assert_eq!(
        pass.jobs[0].task_selection,
        AgentTaskSelection::SupervisorRetry
    );
    assert_eq!(
        pass.jobs[0].resume_session_id.as_deref(),
        Some("original-session")
    );
    prepare_retry(&store, &project).unwrap();
    let tasks = read_task_entries(&get_tasks_dir(&project.path), TaskStatus::Doing).unwrap();
    assert!(!task_entry_is_blocked(&tasks[0]));
    // A crash after activation still routes to the approved exact session.
    assert!(matches!(
        gate(&store, &project, false).unwrap(),
        Gate::Retry(_)
    ));
    assert_eq!(
        read_task_entries(&get_tasks_dir(&project.path), TaskStatus::Todo)
            .unwrap()
            .len(),
        1
    );
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn supervisor_waits_without_rechecking_unchanged_evidence_and_off_restores_normal_scheduling() {
    for kind in ["user", "replan", "repair"] {
        let (root, state, store, project) = fixture("supervisor-hold");
        enable(&store);
        let before = evidence(&project).unwrap();
        begin_review(&store, &project).unwrap();
        finish_review(&store,&project,&result(&root,json!({"decision":kind,"task":1,"reason":"Prerequisite missing","next_action":"Complete FOG-03 first"}))).unwrap();
        assert_eq!(before, evidence(&project).unwrap());
        assert!(
            run_agent_scheduler_pass(&state, false, &[])
                .unwrap()
                .jobs
                .is_empty()
        );
        assert!(
            run_agent_scheduler_pass(&state, false, &[])
                .unwrap()
                .jobs
                .is_empty()
        );
        insert_task(
            &project.path,
            TaskStatus::Done,
            None,
            "Prerequisite finished",
            None,
        )
        .unwrap();
        assert!(matches!(
            gate(&store, &project, true).unwrap(),
            Gate::Review
        ));
        store
            .set_supervisor_settings_blocking(&SupervisorSettings::default())
            .unwrap();
        assert!(matches!(
            gate(&store, &project, true).unwrap(),
            Gate::Normal
        ));
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn supervisor_resume_reassesses_stopped_repair_and_preserves_the_original_work() {
    for folders in [false, true] {
        let root = temp_root("supervisor-resume-repair");
        let state = root.join("state");
        let path = root.join("project");
        init_tasks(&path, folders).unwrap();
        let board = TaskBoard::new(get_tasks_dir(&path));
        let waiting =
            "Finish ridge. BLOCKED 2026-10-23: Session stopped before sealing. codex:ridge-session";
        board
            .insert_content(TaskStatus::Todo, None, waiting)
            .unwrap();
        board
            .insert_content(
                TaskStatus::Todo,
                None,
                "Leave paused work alone clt:stopped",
            )
            .unwrap();
        initialize_test_git_repository(&path);
        let store = agent::TursoAgentStore::open_blocking(&state).unwrap();
        store.register_project_blocking(&path, "project").unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        store
            .set_project_git_mode_blocking(project.id, AgentGitMode::Commit)
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let start = capture_agent_git_start_state(&path, AgentGitMode::Commit).unwrap();
        store
            .create_git_finalization_blocking(agent::NewGitFinalization {
                project_id: project.id,
                codex_session_id: "ridge-session",
                git_mode: AgentGitMode::Commit,
                starting_head: Some(&start.starting_head),
                branch_ref: start.branch_ref.as_deref(),
                upstream_ref: start.upstream_ref.as_deref(),
                worktree_baseline: &start.worktree_baseline,
                task_identity: durable_task_identity(waiting).as_deref(),
                owner_run_token: Some("original-run"),
                created_at: "100",
            })
            .unwrap();
        fs::write(path.join("ridge.txt"), "verified staged implementation\n").unwrap();
        run_test_git(&path, &["add", "ridge.txt"]);
        fs::write(path.join("ridge.txt"), "independent unstaged edit\n").unwrap();
        let index = run_test_git(&path, &["ls-files", "--stage"]);
        let journal = store
            .git_finalization_blocking(project.id, "ridge-session")
            .unwrap();
        store
            .set_session_control_recovery_token_blocking(
                project.id,
                "ridge-session",
                "original-run",
            )
            .unwrap();
        store
            .set_session_control_state_blocking(
                project.id,
                "ridge-session",
                AgentSessionControlState::Stopped,
            )
            .unwrap();
        enable(&store);
        begin_review(&store, &project).unwrap();
        finish_review(
            &store,
            &project,
            &result(
                &root,
                json!({
                    "decision":"repair", "task":1, "reason":"The original session is stopped",
                    "next_action":"Resume the existing task with its session controls"
                }),
            ),
        )
        .unwrap();
        let before = evidence(&project).unwrap();
        assert!(
            run_agent_scheduler_pass(&state, false, &[])
                .unwrap()
                .jobs
                .is_empty()
        );
        assert_eq!(
            store
                .session_control_blocking(project.id, "ridge-session")
                .unwrap()
                .unwrap()
                .state,
            AgentSessionControlState::Stopped
        );

        let task = board.entries(TaskStatus::Todo).unwrap().remove(0);
        crate::session_control::toggle_tui_task_stop_at(
            &state,
            Some(project.id),
            &get_tasks_dir(&path),
            TaskStatus::Todo,
            &task,
        )
        .unwrap();
        assert_eq!(before, evidence(&project).unwrap());
        assert!(matches!(
            gate(&store, &project, true).unwrap(),
            Gate::Review
        ));
        let mut pass = run_agent_scheduler_pass(&state, false, &[]).unwrap();
        assert_eq!(pass.jobs.len(), 1, "folders={folders}");
        assert_eq!(
            pass.jobs[0].task_selection,
            AgentTaskSelection::SupervisorReview
        );
        let completion = run_agent_job(
            pass.jobs.pop().unwrap(),
            &ReviewingRunner,
            &new_agent_shutdown_signal(),
        )
        .unwrap();
        assert_eq!(completion.status, "success", "{}", completion.summary);
        let pass = run_agent_scheduler_pass(&state, false, &[]).unwrap();
        assert_eq!(pass.jobs.len(), 1);
        assert_eq!(
            pass.jobs[0].task_selection,
            AgentTaskSelection::SupervisorRetry
        );
        assert_eq!(
            pass.jobs[0].resume_session_id.as_deref(),
            Some("ridge-session")
        );
        assert_eq!(before, evidence(&project).unwrap());
        prepare_retry(&store, &project).unwrap();
        let todo = board.entries(TaskStatus::Todo).unwrap();
        assert_eq!(todo.len(), 2);
        assert!(!task_entry_is_blocked(&todo[0]));
        assert_eq!(
            recoverable_codex_session_id_from_task_content(&todo[0].content),
            Some("ridge-session")
        );
        assert_eq!(
            store
                .git_finalization_blocking(project.id, "ridge-session")
                .unwrap(),
            journal
        );
        assert_eq!(run_test_git(&path, &["ls-files", "--stage"]), index);
        assert_eq!(
            run_test_git(&path, &["rev-parse", "HEAD"]).trim(),
            start.starting_head
        );
        assert_eq!(
            fs::read_to_string(path.join("ridge.txt")).unwrap(),
            "independent unstaged edit\n"
        );
        assert!(task_entry_is_stopped(&todo[1]));
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn supervisor_resume_allows_read_only_review_of_an_idle_original_run_token() {
    let (root, state, store, project) = fixture("supervisor-resume-review");
    enable(&store);
    store
        .set_session_control_recovery_token_blocking(project.id, "original-session", "original-run")
        .unwrap();
    let pass = run_agent_scheduler_pass(&state, false, &[]).unwrap();
    assert_eq!(pass.jobs.len(), 1);
    assert_eq!(
        pass.jobs[0].task_selection,
        AgentTaskSelection::SupervisorReview
    );
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn supervisor_resume_discards_a_review_that_observed_the_old_stop() {
    let (root, _, store, project) = fixture("supervisor-resume-stale-review");
    enable(&store);
    store
        .set_session_control_state_blocking(
            project.id,
            "original-session",
            AgentSessionControlState::Stopped,
        )
        .unwrap();
    begin_review(&store, &project).unwrap();
    store
        .request_stopped_session_resume_blocking(project.id, "original-session", None)
        .unwrap();
    let before = evidence(&project).unwrap();
    let error = finish_review(
        &store,
        &project,
        &result(
            &root,
            json!({
                "decision":"repair", "task":1, "reason":"The session was stopped",
                "next_action":"Resume the existing session"
            }),
        ),
    )
    .unwrap_err();
    assert!(error.to_string().contains("evidence changed"), "{error:#}");
    assert_eq!(before, evidence(&project).unwrap());
    assert!(matches!(
        gate(&store, &project, true).unwrap(),
        Gate::Review
    ));
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn supervisor_resume_ignores_equivalent_idle_tokens_but_fences_live_owners() {
    let (root, _, store, project) = fixture("supervisor-resume-ownership");
    enable(&store);
    store
        .set_session_control_recovery_token_blocking(project.id, "original-session", "original-run")
        .unwrap();
    begin_review(&store, &project).unwrap();
    finish_review(
        &store,
        &project,
        &result(
            &root,
            json!({
                "decision":"user", "task":1, "reason":"Acceptance decision required",
                "next_action":"Decide whether transparent objects should be concealed"
            }),
        ),
    )
    .unwrap();
    store
        .set_session_control_recovery_token_blocking(
            project.id,
            "original-session",
            "replacement-idle-token",
        )
        .unwrap();
    store
        .set_session_control_state_blocking(
            project.id,
            "unrelated-session",
            AgentSessionControlState::Stopped,
        )
        .unwrap();
    assert!(matches!(
        gate(&store, &project, true).unwrap(),
        Gate::Hold(_)
    ));
    let idle = store
        .session_control_blocking(project.id, "original-session")
        .unwrap()
        .unwrap();
    assert!(control_allows_review(&idle));
    for state in [
        AgentSessionControlState::Stopped,
        AgentSessionControlState::ResumeRequested,
    ] {
        for owner in ["child", "interactive", "launch"] {
            let mut busy = idle.clone();
            busy.state = state;
            match owner {
                "child" => busy.child_pid = Some(std::process::id()),
                "interactive" => busy.interactive_holder = Some("interactive-owner".into()),
                _ => busy.interactive_launch_token = Some("launch-owner".into()),
            }
            assert!(
                !control_allows_review(&busy),
                "state={state:?} owner={owner}"
            );
        }
    }
    for state in [
        AgentSessionControlState::Running,
        AgentSessionControlState::StopRequested,
        AgentSessionControlState::InterruptRequested,
        AgentSessionControlState::ReadyInteractive,
        AgentSessionControlState::Interactive,
    ] {
        let mut busy = idle.clone();
        busy.state = state;
        assert!(!control_allows_review(&busy), "state={state:?}");
    }
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn supervisor_rejects_stale_invalid_stopped_and_exhausted_retries() {
    for scenario in ["stale", "invalid", "stopped", "budget"] {
        let (root, _, store, project) = fixture("supervisor-invalid");
        enable(&store);
        let mut review = begin_review(&store, &project).unwrap();
        if scenario == "stale" {
            add_task(&project.path, "New evidence", None).unwrap();
        }
        if scenario == "stopped" {
            store
                .set_session_control_state_blocking(
                    project.id,
                    "original-session",
                    AgentSessionControlState::Stopped,
                )
                .unwrap();
        }
        if scenario == "budget" {
            review.retries = MAX_RETRIES;
            store
                .save_supervisor_review_blocking(project.id, &review)
                .unwrap();
        }
        let before = evidence(&project).unwrap();
        let response = result(
            &root,
            json!({"decision":"retry","task":if scenario=="invalid" {99} else {1},"reason":"A new approach","next_action":"Try the depth prepass"}),
        );
        assert!(
            finish_review(&store, &project, &response).is_err(),
            "{scenario}"
        );
        assert_eq!(evidence(&project).unwrap(), before);
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn supervisor_runner_uses_read_only_settings_and_never_links_its_session_to_a_task() {
    use std::os::unix::fs::PermissionsExt;
    let (root, state, store, project) = fixture("supervisor-process");
    enable(&store);
    let mut settings = store.supervisor_settings_blocking().unwrap();
    settings.model = Some("supervisor-model".into());
    settings.provider = Some("openai".into());
    settings.fast = true;
    store.set_supervisor_settings_blocking(&settings).unwrap();
    let fake = root.join("codex");
    fs::write(&fake,r#"#!/bin/sh
printf 'arg=%s\n' "$@" >&2
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--output-last-message' ]; then shift; output="$1"; fi
  shift
done
printf '%s' '{"decision":"user","task":1,"reason":"Acceptance unclear","next_action":"Must transparent objects be concealed?"}' > "$output"
"#).unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let mut pass = run_agent_scheduler_pass(&state, false, &[]).unwrap();
    let runner = CodexAgentRunner::with_command(state, Duration::from_secs(10), fake);
    let completion = run_agent_job(
        pass.jobs.pop().unwrap(),
        &runner,
        &new_agent_shutdown_signal(),
    )
    .unwrap();
    assert_eq!(completion.status, "success", "{}", completion.summary);
    let log = fs::read_to_string(completion.stderr_path.unwrap()).unwrap();
    assert!(log.contains("arg=read-only"));
    assert!(!log.contains("arg=danger-full-access"));
    assert!(log.contains("arg=supervisor-model"));
    assert!(log.contains("model_reasoning_effort=\"high\""));
    assert!(log.contains("service_tier=\"fast\""));
    assert!(
        store
            .list_pending_git_finalizations_blocking(Some(project.id))
            .unwrap()
            .is_empty()
    );
    let tasks = read_task_entries(&get_tasks_dir(&project.path), TaskStatus::Doing).unwrap();
    assert_eq!(
        recoverable_codex_session_id_from_task_content(&tasks[0].content),
        Some("original-session")
    );
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn supervisor_retry_approval_survives_crash_before_activation_and_respects_later_stops() {
    let (root, _, store, project) = fixture("supervisor-retry-crash");
    enable(&store);
    begin_review(&store, &project).unwrap();
    finish_review(&store,&project,&result(&root,json!({"decision":"retry","task":1,"reason":"New evidence","next_action":"Try the depth prepass"}))).unwrap();
    let before = evidence(&project).unwrap();
    assert!(matches!(
        gate(&store, &project, true).unwrap(),
        Gate::Retry(_)
    ));
    store
        .set_session_control_state_blocking(
            project.id,
            "original-session",
            AgentSessionControlState::Stopped,
        )
        .unwrap();
    assert!(prepare_retry(&store, &project).is_err());
    assert_eq!(evidence(&project).unwrap(), before);
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn supervisor_keeps_the_original_git_boundary_while_approving_a_retry() {
    let (root, state, store, mut project) = fixture("supervisor-git-boundary");
    initialize_test_git_repository(&project.path);
    project.git_mode = AgentGitMode::Commit;
    let start = capture_agent_git_start_state(&project.path, AgentGitMode::Commit).unwrap();
    let identity = durable_task_identity("Fix renderer.").unwrap();
    assert!(
        store
            .create_git_finalization_blocking(agent::NewGitFinalization {
                project_id: project.id,
                codex_session_id: "original-session",
                git_mode: AgentGitMode::Commit,
                starting_head: Some(&start.starting_head),
                branch_ref: start.branch_ref.as_deref(),
                upstream_ref: start.upstream_ref.as_deref(),
                worktree_baseline: &start.worktree_baseline,
                task_identity: Some(&identity),
                owner_run_token: None,
                created_at: "100"
            })
            .unwrap()
    );
    let original = store
        .git_finalization_blocking(project.id, "original-session")
        .unwrap();
    enable(&store);
    assert!(
        store
            .ensure_pending_git_finalization_resume_requested_blocking(
                project.id,
                "original-session"
            )
            .unwrap()
    );
    let control = store
        .session_control_blocking(project.id, "original-session")
        .unwrap()
        .unwrap();
    assert!(control_allows_review(&control));
    let pass = run_agent_scheduler_pass(&state, false, &[]).unwrap();
    assert_eq!(pass.jobs.len(), 1);
    assert_eq!(
        pass.jobs[0].task_selection,
        AgentTaskSelection::SupervisorReview
    );
    begin_review(&store, &project).unwrap();
    finish_review(&store,&project,&result(&root,json!({"decision":"retry","task":1,"reason":"New probe","next_action":"Implement the depth prepass"}))).unwrap();
    prepare_retry(&store, &project).unwrap();
    assert_eq!(
        store
            .git_finalization_blocking(project.id, "original-session")
            .unwrap(),
        original
    );
    assert!(
        store
            .git_launch_state_for_project_blocking(project.id)
            .unwrap()
            .is_none()
    );
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn supervisor_and_legacy_recovery_allow_ready_todo_after_queued_blockers() {
    for folders in [false, true] {
        for mode in ["off", "on", "saved-repair"] {
            let root = temp_root("supervisor-ready-predecessor");
            let state = root.join("state");
            let path = root.join("project");
            init_tasks(&path, folders).unwrap();
            add_task(
                &path,
                "FOG-06. BLOCKED 2026-10-06: Wait for FOG-05. codex:downstream",
                None,
            )
            .unwrap();
            add_task(
                &path,
                "FOG-05. UNBLOCKED 2026-10-06: FOG-04 completed. codex:ready-predecessor",
                None,
            )
            .unwrap();
            let store = agent::TursoAgentStore::open_blocking(&state).unwrap();
            store.register_project_blocking(&path, "project").unwrap();
            let project = store.list_projects_blocking().unwrap().remove(0);
            if mode != "off" {
                enable(&store);
            }
            if mode == "saved-repair" {
                // Reproduce a decision persisted by 0.8.0 before the gate fix.
                begin_review(&store, &project).unwrap();
                finish_review(
                    &store,
                    &project,
                    &result(
                        &root,
                        json!({
                            "decision":"repair", "task":1, "reason":"Historical writer conflict",
                            "next_action":"Repair the completed predecessor"
                        }),
                    ),
                )
                .unwrap();
            }
            let before = evidence(&project).unwrap();
            let pass = run_agent_scheduler_pass(&state, false, &[]).unwrap();
            assert_eq!(pass.jobs.len(), 1, "folders={folders} mode={mode}");
            assert_eq!(pass.jobs[0].task_selection, AgentTaskSelection::NextTodo);
            assert_eq!(
                crate::worker::automated_codex_session_to_resume(
                    &path,
                    pass.jobs[0].task_selection
                )
                .unwrap()
                .as_deref(),
                Some("ready-predecessor")
            );
            assert_eq!(before, evidence(&project).unwrap());
            assert!(
                store
                    .supervisor_review_blocking(project.id)
                    .unwrap()
                    .is_none()
            );
            drop(store);
            fs::remove_dir_all(root).unwrap();
        }
    }
}

#[test]
fn queued_supervisor_wait_does_not_starve_interrupted_doing_session() {
    for folders in [false, true] {
        let root = temp_root("supervisor-interrupted-doing");
        let state = root.join("state");
        let path = root.join("project");
        init_tasks(&path, folders).unwrap();
        let board = TaskBoard::new(get_tasks_dir(&path));
        let waiting = "Qualify renderer. BLOCKED 2026-11-14: GPU unavailable. codex:gpu-wait";
        let interrupted = "Update defaults. codex:interrupted-defaults";
        board
            .insert_content(TaskStatus::Todo, None, waiting)
            .unwrap();
        board
            .insert_content(TaskStatus::Doing, None, interrupted)
            .unwrap();
        initialize_test_git_repository(&path);
        let store = agent::TursoAgentStore::open_blocking(&state).unwrap();
        store.register_project_blocking(&path, "project").unwrap();
        store
            .set_project_git_mode_blocking(1, AgentGitMode::Commit)
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let start = capture_agent_git_start_state(&path, AgentGitMode::Commit).unwrap();
        for (session, content) in [("gpu-wait", waiting), ("interrupted-defaults", interrupted)] {
            store
                .create_git_finalization_blocking(agent::NewGitFinalization {
                    project_id: project.id,
                    codex_session_id: session,
                    git_mode: AgentGitMode::Commit,
                    starting_head: Some(&start.starting_head),
                    branch_ref: start.branch_ref.as_deref(),
                    upstream_ref: start.upstream_ref.as_deref(),
                    worktree_baseline: &start.worktree_baseline,
                    task_identity: durable_task_identity(content).as_deref(),
                    owner_run_token: None,
                    created_at: "100",
                })
                .unwrap();
            store
                .ensure_pending_git_finalization_resume_requested_blocking(project.id, session)
                .unwrap();
        }
        store
            .record_run_outcome_blocking(agent::AgentRunOutcome {
                project_id: project.id,
                status: "failure",
                started_at: "100",
                finished_at: Some("101"),
                exit_code: None,
                log_dir: None,
                stdout_path: None,
                stderr_path: None,
                summary: Some("Worker heartbeat was stale for at least 60 seconds"),
                codex_session_id: Some("interrupted-defaults"),
            })
            .unwrap();
        enable(&store);
        // Reproduce the existing held review of the unrelated Todo task.
        begin_review(&store, &project).unwrap();
        finish_review(
            &store,
            &project,
            &result(
                &root,
                json!({
                    "decision":"wait", "task":1, "reason":"GPU unavailable",
                    "next_action":"Wait for GPU measurements"
                }),
            ),
        )
        .unwrap();
        let before = evidence(&project).unwrap();
        let journals = store
            .list_pending_git_finalizations_blocking(Some(project.id))
            .unwrap();
        let pass = run_agent_scheduler_pass(&state, false, &[]).unwrap();
        assert_eq!(pass.jobs.len(), 1, "folders={folders}");
        assert_eq!(
            pass.jobs[0].task_selection,
            AgentTaskSelection::ResumeSession
        );
        assert_eq!(
            pass.jobs[0].resume_session_id.as_deref(),
            Some("interrupted-defaults")
        );
        assert_eq!(before, evidence(&project).unwrap());
        assert_eq!(
            journals,
            store
                .list_pending_git_finalizations_blocking(Some(project.id))
                .unwrap()
        );
        assert!(
            store
                .supervisor_review_blocking(project.id)
                .unwrap()
                .is_none()
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn queued_wait_yields_only_to_admitted_automated_work() {
    let root = temp_root("supervisor-doing-admission");
    let path = root.join("project");
    init_tasks(&path, false).unwrap();
    let board = TaskBoard::new(get_tasks_dir(&path));
    board
        .insert_content(
            TaskStatus::Todo,
            None,
            "GPU-01 Qualify renderer. BLOCKED 2026-11-14: GPU unavailable. codex:gpu-wait",
        )
        .unwrap();
    board
        .insert_content(TaskStatus::Doing, None, "Update defaults. codex:defaults")
        .unwrap();
    let store = agent::TursoAgentStore::open_blocking(&root.join("state")).unwrap();
    store.register_project_blocking(&path, "project").unwrap();
    let project = store.list_projects_blocking().unwrap().remove(0);
    for (content, expected) in [
        ("Update defaults. codex:defaults", true),
        ("Update defaults. clt:stopped codex:defaults", false),
        ("Update defaults. clt:manual codex:defaults", false),
        ("Update defaults.", false),
        (
            "Update defaults. BLOCKED 2026-11-14: Missing input. codex:defaults",
            false,
        ),
        (
            "Update defaults. Dependencies: GPU-01. codex:defaults",
            false,
        ),
    ] {
        let entry = board.entries(TaskStatus::Doing).unwrap().remove(0);
        write_task_entry_content(board.path(), TaskStatus::Doing, &entry, content).unwrap();
        assert_eq!(
            queued_blockers_can_wait_with_store(&project, Some(&store)).unwrap(),
            expected,
            "{content}"
        );
    }
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn supervisor_reviews_repeated_doing_failure_before_unrelated_queued_wait() {
    let (root, _, store, project) = fixture("supervisor-doing-failure-priority");
    let board = TaskBoard::new(get_tasks_dir(&project.path));
    let entry = board.entries(TaskStatus::Todo).unwrap().remove(0);
    write_task_entry_content(
        board.path(),
        TaskStatus::Todo,
        &entry,
        "Qualify renderer. BLOCKED 2026-11-14: GPU unavailable. codex:gpu-wait",
    )
    .unwrap();
    let entry = board.entries(TaskStatus::Doing).unwrap().remove(0);
    write_task_entry_content(
        board.path(),
        TaskStatus::Doing,
        &entry,
        "Update defaults. codex:original-session",
    )
    .unwrap();
    for timestamp in ["100", "101"] {
        store
            .record_run_outcome_blocking(agent::AgentRunOutcome {
                project_id: project.id,
                status: "failure",
                started_at: timestamp,
                finished_at: Some(timestamp),
                exit_code: None,
                log_dir: None,
                stdout_path: None,
                stderr_path: None,
                summary: Some("Worker heartbeat was stale for at least 60 seconds"),
                codex_session_id: Some("original-session"),
            })
            .unwrap();
    }
    enable(&store);
    assert!(matches!(
        gate(&store, &project, true).unwrap(),
        Gate::Review
    ));
    let candidates = review_candidates(&store, &project).unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].0, TaskStatus::Doing);
    assert!(candidates[0].1.content.contains("codex:original-session"));
    begin_review(&store, &project).unwrap();
    let mut command = Command::new("codex");
    configure_command(&mut command, &store, &project, &root.join("run.err")).unwrap();
    let prompt = command.get_args().last().unwrap().to_string_lossy();
    let review_section = prompt
        .split("Blocked tasks:\n")
        .nth(1)
        .unwrap()
        .split("Board evidence")
        .next()
        .unwrap();
    assert!(review_section.contains("Worker heartbeat was stale"));
    assert!(!review_section.contains("GPU unavailable"));
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn supervisor_still_reviews_a_queue_with_no_ready_work() {
    let (root, state, store, project) = fixture("supervisor-stalled-queue");
    move_task(&project.path, TaskStatus::Doing, TaskStatus::Todo, "1").unwrap();
    let board = get_tasks_dir(&project.path);
    let ready = task_entry_at(&board, TaskStatus::Todo, 1).unwrap();
    write_task_entry_content(
        &board,
        TaskStatus::Todo,
        &ready,
        "Paused prerequisite clt:stopped",
    )
    .unwrap();
    enable(&store);
    let pass = run_agent_scheduler_pass(&state, false, &[]).unwrap();
    assert_eq!(pass.jobs.len(), 1);
    assert_eq!(
        pass.jobs[0].task_selection,
        AgentTaskSelection::SupervisorReview
    );
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn supervisor_prompt_uses_only_the_blocked_tasks_exact_session_logs() {
    let (root, _, store, project) = fixture("supervisor-scoped-logs");
    for (session, summary, stderr) in [
        (
            "original-session",
            "Current task probe failed",
            "current-task.err",
        ),
        (
            "completed-predecessor",
            "Old thread already has an active writer",
            "unrelated-writer.err",
        ),
    ] {
        store
            .record_run_outcome_blocking(agent::AgentRunOutcome {
                project_id: project.id,
                status: "failure",
                started_at: "100",
                finished_at: Some("101"),
                exit_code: Some(1),
                log_dir: None,
                stdout_path: None,
                stderr_path: Some(stderr),
                summary: Some(summary),
                codex_session_id: Some(session),
            })
            .unwrap();
    }
    enable(&store);
    begin_review(&store, &project).unwrap();
    let mut command = Command::new("codex");
    configure_command(&mut command, &store, &project, &root.join("run.err")).unwrap();
    let prompt = command.get_args().last().unwrap().to_string_lossy();
    assert!(prompt.contains("Current task probe failed"));
    assert!(prompt.contains("current-task.err"));
    assert!(prompt.contains("original-session"));
    assert!(!prompt.contains("Old thread already has an active writer"));
    assert!(!prompt.contains("unrelated-writer.err"));
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn supervisor_wait_defers_work_and_runs_ready_prerequisite_with_journal_preserved() {
    for folders in [false, true] {
        let root = temp_root("supervisor-dependency-progress");
        let state = root.join("state");
        let path = root.join("project");
        init_tasks(&path, folders).unwrap();
        let board = TaskBoard::new(get_tasks_dir(&path));
        let waiting = "UI-07 Stripe. BLOCKED 2026-10-07: Complete UI-06 first. codex:downstream";
        board
            .insert_content(TaskStatus::Doing, None, waiting)
            .unwrap();
        board
            .insert_content(TaskStatus::Todo, None, "UI-06 Payments")
            .unwrap();
        initialize_test_git_repository(&path);
        let store = agent::TursoAgentStore::open_blocking(&state).unwrap();
        store.register_project_blocking(&path, "project").unwrap();
        store
            .set_project_git_mode_blocking(1, AgentGitMode::Commit)
            .unwrap();
        let project = store.list_projects_blocking().unwrap().remove(0);
        let start = capture_agent_git_start_state(&path, AgentGitMode::Commit).unwrap();
        store
            .create_git_finalization_blocking(agent::NewGitFinalization {
                project_id: project.id,
                codex_session_id: "downstream",
                git_mode: AgentGitMode::Commit,
                starting_head: Some(&start.starting_head),
                branch_ref: start.branch_ref.as_deref(),
                upstream_ref: start.upstream_ref.as_deref(),
                worktree_baseline: &start.worktree_baseline,
                task_identity: durable_task_identity(waiting).as_deref(),
                owner_run_token: None,
                created_at: "100",
            })
            .unwrap();
        store
            .ensure_pending_git_finalization_resume_requested_blocking(project.id, "downstream")
            .unwrap();
        let journal = store
            .git_finalization_blocking(project.id, "downstream")
            .unwrap();
        enable(&store);
        begin_review(&store, &project).unwrap();
        finish_review(
            &store,
            &project,
            &result(
                &root,
                json!({
                    "decision":"wait", "task":1, "reason":"UI-06 must finish first",
                    "next_action":"Run UI-06, then resume UI-07"
                }),
            ),
        )
        .unwrap();
        assert!(board.entries(TaskStatus::Doing).unwrap().is_empty());
        let todo = board.entries(TaskStatus::Todo).unwrap();
        assert_eq!(todo[1].content.trim_end(), waiting);
        assert!(task_entry_is_blocked(&todo[1]));
        assert_eq!(
            store
                .git_finalization_blocking(project.id, "downstream")
                .unwrap(),
            journal
        );
        // Several scans must not repair the waiting task back into Doing or
        // let its saved Git resume request steal the next ready task's slot.
        // A closed Git-off conversation lost its board link, but its resume
        // control survived. Retire it before the other session's Git lease.
        store
            .mark_session_running_with_git_mode_blocking(
                project.id,
                "abandoned-conversation",
                12345,
                "old-run",
                &root.join("old.out"),
                &root.join("old.err"),
                AgentGitMode::Off,
            )
            .unwrap();
        store
            .set_session_control_recovery_token_blocking(
                project.id,
                "abandoned-conversation",
                "old-run",
            )
            .unwrap();
        let todo_contents = || {
            board
                .entries(TaskStatus::Todo)
                .unwrap()
                .into_iter()
                .map(|task| task.content)
                .collect::<Vec<_>>()
        };
        let todo_before = todo_contents();
        fs::write(path.join("concurrent.txt"), "uncommitted user work\n").unwrap();
        for _ in 0..2 {
            let pass = run_agent_scheduler_pass(&state, false, &[]).unwrap();
            assert_eq!(pass.jobs.len(), 1);
            assert_eq!(pass.jobs[0].task_selection, AgentTaskSelection::NextTodo);
            assert!(pass.jobs[0].resume_session_id.is_none());
            store
                .release_lease_blocking(project.id, &pass.jobs[0].holder)
                .unwrap();
            assert!(board.entries(TaskStatus::Doing).unwrap().is_empty());
            assert_eq!(todo_contents(), todo_before);
            assert_eq!(
                fs::read_to_string(path.join("concurrent.txt")).unwrap(),
                "uncommitted user work\n"
            );
            let retired = store
                .session_control_blocking(project.id, "abandoned-conversation")
                .unwrap()
                .unwrap();
            assert_eq!(retired.state, AgentSessionControlState::Stopped);
            assert_eq!(retired.run_token.as_deref(), Some("old-run"));
            assert_eq!(
                store
                    .git_finalization_blocking(project.id, "downstream")
                    .unwrap(),
                journal
            );
        }
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn supervisor_wait_without_ready_work_does_not_spin_or_override_stop() {
    for stopped in [false, true] {
        let (root, state, store, project) = fixture("supervisor-wait-no-work");
        fs::write(project.path.join("tasks/todo.md"), "# Todo\n").unwrap();
        if stopped {
            store
                .set_session_control_state_blocking(
                    project.id,
                    "original-session",
                    AgentSessionControlState::Stopped,
                )
                .unwrap();
        }
        enable(&store);
        begin_review(&store, &project).unwrap();
        finish_review(
            &store,
            &project,
            &result(
                &root,
                json!({
                    "decision":"wait", "task":1, "reason":"External prerequisite unavailable",
                    "next_action":"Resume once the prerequisite is available"
                }),
            ),
        )
        .unwrap();
        for _ in 0..2 {
            assert!(
                run_agent_scheduler_pass(&state, false, &[])
                    .unwrap()
                    .jobs
                    .is_empty()
            );
        }
        assert_eq!(
            read_task_entries(&get_tasks_dir(&project.path), TaskStatus::Doing)
                .unwrap()
                .len(),
            usize::from(stopped)
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn old_wait_hold_gets_one_fresh_assessment_without_resetting_retry_budget() {
    let (root, _, store, project) = fixture("supervisor-upgrade-wait");
    enable(&store);
    let mut old = begin_review(&store, &project).unwrap();
    old.state = "held".into();
    old.attempts = MAX_REVIEWS;
    old.retries = 1;
    old.decision = Some(Decision {
        decision: DecisionKind::Wait,
        task: 1,
        reason: "Prerequisite is incomplete".into(),
        next_action: "Run prerequisite first".into(),
    });
    store
        .save_supervisor_review_blocking(project.id, &old)
        .unwrap();
    assert!(matches!(
        gate(&store, &project, true).unwrap(),
        Gate::Review
    ));
    let upgraded = begin_review(&store, &project).unwrap();
    assert_eq!(upgraded.attempts, 1);
    assert_eq!(upgraded.retries, 1);
    finish_review(
        &store,
        &project,
        &result(
            &root,
            json!({
                "decision":"wait", "task":1, "reason":"Prerequisite is incomplete",
                "next_action":"Run prerequisite first"
            }),
        ),
    )
    .unwrap();
    assert!(matches!(
        gate(&store, &project, true).unwrap(),
        Gate::Normal
    ));
    drop(store);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn repeated_exact_session_failures_are_reviewed_without_a_blocked_note() {
    for interrupted_by_success in [false, true] {
        let (root, state, store, project) = fixture("supervisor-repeated-runtime-failure");
        enable(&store);
        fs::write(
            project.path.join("tasks/doing.md"),
            "# Doing Tasks\n- Fix renderer. codex:original-session\n",
        )
        .unwrap();
        for (index, status) in [
            "failure",
            if interrupted_by_success {
                "success"
            } else {
                "timeout"
            },
        ]
        .iter()
        .enumerate()
        {
            store
                .record_run_outcome_blocking(agent::AgentRunOutcome {
                    project_id: project.id,
                    status,
                    started_at: &format!("{}", 100 + index),
                    finished_at: Some(&format!("{}", 101 + index)),
                    exit_code: Some(1),
                    log_dir: None,
                    stdout_path: None,
                    stderr_path: None,
                    summary: Some("Repeated task runtime failure"),
                    codex_session_id: Some("original-session"),
                })
                .unwrap();
            if index == 0 {
                assert!(matches!(
                    gate(&store, &project, false).unwrap(),
                    Gate::Normal
                ));
            }
        }
        if interrupted_by_success {
            assert!(matches!(
                gate(&store, &project, false).unwrap(),
                Gate::Normal
            ));
        } else {
            assert!(matches!(
                gate(&store, &project, false).unwrap(),
                Gate::Review
            ));
            let pass = run_agent_scheduler_pass(&state, false, &[]).unwrap();
            assert_eq!(pass.jobs.len(), 1);
            assert_eq!(
                pass.jobs[0].task_selection,
                AgentTaskSelection::SupervisorReview
            );
            store
                .release_lease_blocking(project.id, &pass.jobs[0].holder)
                .unwrap();
            fs::write(
                project.path.join("tasks/doing.md"),
                "# Doing Tasks\n- Fix renderer. clt:manual codex:original-session\n",
            )
            .unwrap();
            assert!(matches!(
                gate(&store, &project, false).unwrap(),
                Gate::Normal
            ));
        }
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn supervisor_rechecks_failed_resume_boundary_and_keeps_latest_error() {
    let (root, _, store, mut project) = fixture("supervisor-resume-check");
    initialize_test_git_repository(&project.path);
    project.git_mode = AgentGitMode::Commit;
    let start = capture_agent_git_start_state(&project.path, project.git_mode).unwrap();
    store
        .create_git_finalization_blocking(agent::NewGitFinalization {
            project_id: project.id,
            codex_session_id: "original-session",
            git_mode: project.git_mode,
            starting_head: Some(&start.starting_head),
            branch_ref: start.branch_ref.as_deref(),
            upstream_ref: start.upstream_ref.as_deref(),
            worktree_baseline: &start.worktree_baseline,
            task_identity: durable_task_identity("Fix renderer.").as_deref(),
            owner_run_token: None,
            created_at: "100",
        })
        .unwrap();
    store
        .ensure_pending_git_finalization_resume_requested_blocking(project.id, "original-session")
        .unwrap();
    store
        .record_run_outcome_blocking(agent::AgentRunOutcome {
            project_id: project.id,
            status: "failure",
            started_at: "101",
            finished_at: Some("102"),
            exit_code: None,
            log_dir: None,
            stdout_path: None,
            stderr_path: None,
            summary: Some("Launch failed: selected task was not committed exactly once"),
            codex_session_id: Some("original-session"),
        })
        .unwrap();
    let original = store
        .git_finalization_blocking(project.id, "original-session")
        .unwrap();
    let board = evidence(&project).unwrap();
    enable(&store);
    begin_review(&store, &project).unwrap();
    finish_review(&store, &project, &result(&root, json!({
        "decision":"repair", "task":1, "reason":"Historical launch failed", "next_action":"Inspect handoff"
    }))).unwrap();
    assert!(matches!(
        gate(&store, &project, true).unwrap(),
        Gate::Hold(_)
    ));
    // An older saved hold lacks the current launch checks. Upgrade its evidence
    // once, without clearing a resume control, task note or journal.
    let mut old = store
        .supervisor_review_blocking(project.id)
        .unwrap()
        .unwrap();
    let mut old_evidence: serde_json::Value = serde_json::from_str(&old.evidence).unwrap();
    for eligibility in old_evidence["session_eligibility"].as_array_mut().unwrap() {
        eligibility
            .as_object_mut()
            .unwrap()
            .remove("resume_git_check");
    }
    old.evidence = serde_json::to_string(&old_evidence).unwrap();
    store
        .save_supervisor_review_blocking(project.id, &old)
        .unwrap();
    assert!(matches!(
        gate(&store, &project, true).unwrap(),
        Gate::Review
    ));
    begin_review(&store, &project).unwrap();
    let mut command = Command::new("codex");
    configure_command(&mut command, &store, &project, &root.join("review.err")).unwrap();
    let prompt = command.get_args().last().unwrap().to_string_lossy();
    assert!(prompt.contains("Launch failed: selected task was not committed exactly once"));
    assert!(prompt.contains("resume_git_check"));
    assert!(prompt.contains("verified"));
    assert!(prompt.contains("normal queued state"));
    assert_eq!(
        store
            .git_finalization_blocking(project.id, "original-session")
            .unwrap(),
        original
    );
    assert_eq!(evidence(&project).unwrap(), board);
    let mismatch =
        resume_git_check(&store, &project, "original-session", "A different task.").unwrap();
    assert_eq!(mismatch["status"], "blocked");
    assert!(mismatch["error"].as_str().unwrap().contains("found 0"));
    drop(store);
    fs::remove_dir_all(root).unwrap();
}
