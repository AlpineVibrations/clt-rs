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
    for kind in ["wait", "user", "replan", "repair"] {
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
