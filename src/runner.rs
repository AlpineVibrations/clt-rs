use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AutomatedAgentChildContext {
    pub(super) project_id: i64,
    pub(super) run_token: String,
}

#[cfg(not(test))]
pub(super) fn automated_agent_child_context() -> Result<Option<AutomatedAgentChildContext>> {
    automated_agent_child_context_from_values(
        std::env::var_os(AGENT_PROJECT_ID_ENV),
        std::env::var_os(AGENT_RUN_TOKEN_ENV),
    )
}

#[cfg(test)]
pub(super) fn automated_agent_child_context() -> Result<Option<AutomatedAgentChildContext>> {
    Ok(None)
}

#[cfg(not(test))]
pub(super) fn automated_agent_child_context_from_values(
    project_id: Option<OsString>,
    run_token: Option<OsString>,
) -> Result<Option<AutomatedAgentChildContext>> {
    let (project_id, run_token) = match (project_id, run_token) {
        (None, None) => return Ok(None),
        (Some(project_id), Some(run_token)) => (project_id, run_token),
        _ => {
            anyhow::bail!(
                "Incomplete automated agent context: {AGENT_PROJECT_ID_ENV} and {AGENT_RUN_TOKEN_ENV} must be set together"
            )
        }
    };
    let project_id = project_id
        .to_str()
        .context("Automated agent project ID is not valid UTF-8")?
        .parse::<i64>()
        .with_context(|| format!("{AGENT_PROJECT_ID_ENV} must be a positive integer"))?;
    if project_id <= 0 {
        anyhow::bail!("{AGENT_PROJECT_ID_ENV} must be a positive integer");
    }
    let run_token = run_token
        .into_string()
        .map_err(|_| anyhow::anyhow!("{AGENT_RUN_TOKEN_ENV} is not valid UTF-8"))?;
    validate_agent_worker_token(&run_token)
        .with_context(|| format!("Invalid {AGENT_RUN_TOKEN_ENV}"))?;

    Ok(Some(AutomatedAgentChildContext {
        project_id,
        run_token,
    }))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AgentRunResult {
    pub(super) status: &'static str,
    pub(super) exit_code: Option<i64>,
    pub(super) log_dir: PathBuf,
    pub(super) stdout_path: PathBuf,
    pub(super) stderr_path: PathBuf,
    pub(super) summary: String,
    pub(super) codex_session_id: Option<String>,
    pub(super) session_run_token: Option<String>,
    pub(super) control_action: Option<AgentSessionControlAction>,
}

#[derive(Debug)]
pub(super) struct AgentChildTerminationUnproven(pub(super) String);

impl std::fmt::Display for AgentChildTerminationUnproven {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for AgentChildTerminationUnproven {}

pub(super) fn unproven_agent_child_termination(
    error: anyhow::Error,
    context: &str,
) -> anyhow::Error {
    anyhow::Error::new(AgentChildTerminationUnproven(format!(
        "{context}: {error:#}"
    )))
}

pub(super) trait AgentRunner: Send + Sync {
    fn run_project(
        &self,
        project: &agent::AgentProject,
        task_selection: AgentTaskSelection,
        resume_session_id: Option<&str>,
        lease_holder: &str,
        run_token: Option<&str>,
        shutdown: &AgentShutdownSignal,
    ) -> Result<AgentRunResult>;
}

pub(super) struct CodexAgentRunner {
    pub(super) state_dir: PathBuf,
    pub(super) timeout: Duration,
    pub(super) heartbeat_interval: Duration,
    pub(super) lease_timeout: Duration,
    pub(super) lease_renew_interval: Duration,
    pub(super) command: PathBuf,
    pub(super) worker_token: Option<String>,
}

pub(super) const AGENT_NO_TASKS_LEFT_MARKER: &str = "NO_TASKS_LEFT";
pub(super) const CLT_TASK_MANAGEMENT_SKILL_NAME: &str = "clt-task-management";
pub(super) const GIT_COMMIT_SKILL_NAME: &str = "git-commit";
pub(super) const EMBEDDED_CLT_TASK_MANAGEMENT_SKILL: &str =
    include_str!("../skills/clt-task-management/SKILL.md");
pub(super) const EMBEDDED_GIT_COMMIT_SKILL: &str = include_str!("../skills/git-commit/SKILL.md");

pub(super) const AGENT_CODEX_PROMPT_BASE: &str = r#"You are working in this repo.

Use the existing task-management CLI tooling: clt.

Your job for this run:

1. Inspect the task board using the task CLI.
2. Pick the next available unblocked TODO / ready task.
3. If there are no available tasks, say exactly: NO_TASKS_LEFT
4. If there is a task:
   - inspect its full content and applicable repository instructions before starting work
   - follow the applicable mode-specific pre-task Git boundary; when a Git appendix says CLT already prepared and froze the checkout, do not sync or switch it yourself
   - move it to doing only after that preparation
   - if the first non-whitespace token is exactly `/goal`, treat that as an explicit Goal mode request: remove that token, trim the remaining task content, create a persistent goal from the result without including `/goal` in the goal objective, and then work toward it
   - if `/goal` has no non-empty objective after it, add a concise `BLOCKED YYYY-MM-DD:` note explaining that the goal objective is missing and stop
   - do not create a goal when `/goal` appears anywhere except at the start of the task content
   - complete that task
   - run the relevant checks/tests
   - update the task using the task CLI
   - mark it done if completed
   - include a concise note with what changed and what commands/tests ran
5. Stop after that one task.
6. Do not start another task.
7. Exit when finished.

Safety rules:
- Do not overwrite unrelated user changes.
- Before making edits, inspect the current repo state.
- A dirty worktree is expected when people, interactive sessions, or independent workers share a repository; it is not a blocker by itself.
- Treat the initial status and diff as the baseline, preserve pre-existing changes, and continue with non-conflicting work.
- Another change in the same file is not automatically a blocker. Re-read the affected area and keep both changes when the intended combined result is clear.
- Stop for Git overlap only when the required edits genuinely conflict and the correct combined result cannot be determined safely.
- During normal TODO selection, skip tasks whose latest dated state note is `BLOCKED YYYY-MM-DD:`.
- Inspect task details when needed; a folder-backed task's list summary may not show its blocker notes.
- If the task is blocked or cannot be completed safely, update it with a concise `BLOCKED YYYY-MM-DD:` note instead of forcing it.
"#;
pub(super) const AGENT_GIT_COMMIT_PROMPT_APPENDIX: &str = r#"

Git commit:
- This finalization contract is authoritative for the automated run and overrides older installed skill guidance when they differ.
- Before this process was released, CLT completed the scheduler-owned startup preparation, using a safe fast-forward-only sync only when no older WORKING journal required preserving its history, then froze HEAD, the worktree baseline, branch, and upstream state and persisted that launch record. The selected task must already be committed exactly once on the board. Do not pull, fetch or otherwise synchronize, merge, rebase, switch branches, reset history, or reconfigure Git after release.
- Move the selected Todo task to Doing before implementation. CLT rechecks the frozen launch record and binds it to the session's durable WORKING journal at that transition; do not edit or commit implementation first.
- After completing and verifying the task, run all formatting, lint, signing, and hook checks that can mutate files before sealing. Add its dated COMPLETED note. Stage the implementation and the active Doing task, including its terminal `codex:<session-id>` marker, then inspect the staged diff.
- Run `clt done` only after that staged diff is complete. CLT seals its durable task manifest and makes the board move provisionally; it is not terminal completion by itself.
- Stage only the resulting board transition, inspect the complete staged diff again, then use the $git-commit skill to create exactly one normal git commit containing the sealed implementation, completion note, and complete task-board move.
- Give that commit one exact final message paragraph: `CLT-Task: codex:<session-id>`.
- If a commit hook changes files or fails after the seal, fix and stage the complete corrected payload, run `clt done done <index>` to reseal that provisional Done entry, inspect it, and retry the one commit.
- Pre-existing unstaged changes do not prevent a commit. Stage only this task's paths or hunks, verify the staged diff, and leave unrelated changes untouched.
- A Todo or other task-board edit added during the run may also remain unstaged. Preserve it and stage only the selected task's board transition; CLT's exact staged-tree proof keeps the concurrent edit outside the sealed commit.
- Do not require the worktree to be clean before committing.
- The scheduler supplies the isolated Git identity `CLT Agent <clt-agent@localhost>` for clear automated-commit attribution; do not change Git configuration.
- Do not exit merely because the task appears in Done. Inspect the created commit and keep working until CLT can prove the task-specific commit. If this is a resumed finalization, inspect existing Git state before committing and never duplicate an already-created task commit.
- Do not commit when there are no tasks left, the task is blocked, checks fail, or the work cannot be completed safely.
"#;
pub(super) const AGENT_GIT_PUSH_PROMPT_APPENDIX: &str = r#"

Git push:
- This project is configured for commit and push. CLT already froze the attached branch's single intended push URL and upstream before release.
- Do not run `git push`. After CLT proves the sealed local commit, its finalizer sends exactly that frozen OID to exactly the frozen URL and merge ref with an explicit non-force refspec, then independently proves the remote result.
- Exit after creating and inspecting the verified commit. The task remains PUSH-PENDING until CLT's bounded push and remote proof succeed.
- If the remote advanced and rejects publication, CLT leaves the task PUSH-PENDING for a later scheduler retry or explicit external recovery; do not pull, fetch, merge, rebase, amend, switch branches, or change the destination.
- Never force-push.
"#;
pub(super) const AGENT_RESUME_DOING_PROMPT_APPENDIX: &str = r#"

Interrupted task recovery:
- A previous agent run was interrupted after moving a task to doing.
- Resume and finish exactly one existing doing task.
- Do not pick or move a TODO task; this recovery instruction replaces steps 2-4 above.
- If there is no doing task to resume, say exactly: NO_TASKS_LEFT
"#;
pub(super) const AGENT_RECOVER_BLOCKED_PROMPT_APPENDIX: &str = r#"

Blocked-task monitor:
- The scheduler found at least one blocked task in todo or doing and is reconsidering blockers before starting fresh Todo work.
- Review the existing blocker notes and choose exactly one blocked task from todo or doing.
- Re-evaluate whether the recorded blocking conditions still exist instead of assuming the task remains blocked.
- If the selected task is in todo, move it to doing before working on it.
- Try to resolve that task's blocker and finish the task, including the relevant checks.
- Update the existing task; do not create a replacement task.
- If the task is completed, add its completion note and move it to done.
- If its blocker is resolved but the task should be retried through the normal workflow, add a newer `UNBLOCKED YYYY-MM-DD:` note and move that same task back to todo.
- If it still cannot be completed safely, update its blocked note with what you tried and what is still needed, and leave it in doing.
- Do not select backlog work. Stop after handling that one blocked task.
- These recovery instructions replace steps 2-4 above.
"#;
pub(super) const AGENT_RESUME_SESSION_PROMPT_APPENDIX: &str = r#"

Interactive handoff recovery:
- Resume the exact task and Codex session that CLT handed back from interactive mode.
- If CLT reports this task as FINALIZING, inspect the existing commit first and continue only the first unproven local step. A PUSH-PENDING task is scheduler-owned and must not resume Codex merely to publish. Never create a duplicate completion commit or move a successfully committed task back to Doing.
- Inspect the linked task, current project state, and any interactive instructions, then continue from the next unfinished substantive step in the conversation context.
- A prior assistant plan, progress message, draft, summary, or claimed completion is not proof that requested work finished.
- If the linked task or interactive instructions request project, file, code, configuration, or task-board changes, do not mark the task done until those durable changes actually exist and the relevant checks pass.
- For a response-only task that does not request durable changes, a completed response may be the deliverable.
- If the task is already complete, verify its recorded completion and any requested durable output, then exit without selecting another task.
- Otherwise finish or update that same task using the normal task workflow and relevant checks.
- Do not select another Todo or Backlog task. Stop after handling this one session.
- These recovery instructions replace steps 2-4 above.
"#;

pub(super) fn agent_codex_command() -> PathBuf {
    agent_codex_path_env().unwrap_or_else(|| PathBuf::from("codex"))
}

pub(super) fn configure_automated_codex_subcommand(
    command: &mut Command,
    project: &agent::AgentProject,
    task_selection: AgentTaskSelection,
    resume_session_id: Option<&str>,
) -> Result<Option<String>> {
    let session_id = match resume_session_id {
        Some(session_id) => Some(session_id.to_string()),
        None => automated_codex_session_to_resume(&project.path, task_selection)?,
    };
    command.arg("exec");
    if let Some(session_id) = session_id.as_deref() {
        command
            .arg("resume")
            .arg("--skip-git-repo-check")
            .arg(session_id);
    } else {
        command
            .arg("--skip-git-repo-check")
            .arg("-C")
            .arg(&project.path);
    }
    command.arg(agent_codex_prompt(project, task_selection));
    Ok(session_id)
}

pub(super) fn automated_exec_gate_is_released(reader: &mut impl Read) -> io::Result<bool> {
    let mut release = [0_u8; 1];
    loop {
        return match reader.read(&mut release) {
            Ok(0) => Ok(false),
            Ok(_) => Ok(release[0] == b'x'),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => Err(error),
        };
    }
}

#[cfg(unix)]
pub(super) fn run_automated_exec_gate(program: &Path, arguments: &[OsString]) -> Result<()> {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    if !automated_exec_gate_is_released(&mut reader)
        .context("Failed to read automated Codex launch gate")?
    {
        return Ok(());
    }

    let mut command = Command::new(program);
    command.args(arguments);
    let error = command.exec();
    Err(error).with_context(|| format!("Failed to exec gated Codex command {}", program.display()))
}

#[cfg(not(unix))]
pub(super) fn run_automated_exec_gate(_program: &Path, _arguments: &[OsString]) -> Result<()> {
    anyhow::bail!("The automated Codex exec gate is only supported on Unix")
}

#[cfg(all(unix, not(test)))]
pub(super) fn automated_exec_gate_command(target: &Command) -> Result<Command> {
    let executable = std::env::current_exe()
        .context("Failed to resolve the CLT executable for the automated Codex launch gate")?;
    let mut gate = Command::new(executable);
    gate.arg("--local")
        .arg("agent")
        .arg("automated-exec-gate")
        .arg("--")
        .arg(target.get_program())
        .args(target.get_args());
    configure_automated_exec_gate_inheritance(&mut gate, target);
    Ok(gate)
}

#[cfg(all(unix, test))]
pub(super) fn automated_exec_gate_command(target: &Command) -> Result<Command> {
    // Unit-test executables are owned by libtest rather than this binary's CLI.
    // A POSIX shell supplies the same read/EOF/exec behavior for runner tests.
    let mut gate = Command::new("/bin/sh");
    gate.arg("-c")
        .arg("gate=$(/bin/dd bs=1 count=1 2>/dev/null)\n[ \"$gate\" = x ] || exit 0\nexec \"$@\"")
        .arg("clt-automated-exec-gate")
        .arg(target.get_program())
        .args(target.get_args());
    configure_automated_exec_gate_inheritance(&mut gate, target);
    Ok(gate)
}

#[cfg(unix)]
pub(super) fn configure_automated_exec_gate_inheritance(gate: &mut Command, target: &Command) {
    if let Some(current_dir) = target.get_current_dir() {
        gate.current_dir(current_dir);
    }
    for (key, value) in target.get_envs() {
        match value {
            Some(value) => {
                gate.env(key, value);
            }
            None => {
                gate.env_remove(key);
            }
        }
    }
    // Stdio::piped gives the helper only the read end. The parent owns the only
    // writer, so a parent crash before registration is observed as EOF.
    gate.stdin(Stdio::piped());
}

#[cfg(unix)]
pub(super) const AUTOMATED_SUPERVISOR_CONNECTED: u64 = 0;
#[cfg(unix)]
pub(super) const AUTOMATED_SUPERVISOR_STOP_REQUESTED: u64 = 1;
#[cfg(unix)]
pub(super) const AUTOMATED_SUPERVISOR_PARENT_DISCONNECTED: u64 = 2;
#[cfg(unix)]
pub(super) const AUTOMATED_SUPERVISOR_READY_PREFIX: &str = "clt-automated-child-pid:";
#[cfg(unix)]
pub(super) const AUTOMATED_SUPERVISOR_REAPED_PREFIX: &str = "clt-automated-child-reaped:";

#[cfg(unix)]
pub(super) struct AutomatedSupervisorChild {
    pub(super) process: Child,
    pub(super) control: std::process::ChildStdin,
    pub(super) child_pid: u32,
    pub(super) proof: BufReader<std::process::ChildStdout>,
}

#[cfg(unix)]
#[derive(Clone, Copy)]
pub(super) struct AutomatedSupervisorSpec<'a> {
    pub(super) state_dir: &'a Path,
    pub(super) project_id: i64,
    pub(super) run_token: &'a str,
    pub(super) lease_holder: &'a str,
    pub(super) stdout_path: &'a Path,
    pub(super) stderr_path: &'a Path,
}

#[cfg(unix)]
pub(super) struct AutomatedSupervisorWaitHandles<'a> {
    pub(super) process: &'a mut Child,
    pub(super) control: &'a mut Option<std::process::ChildStdin>,
    pub(super) proof: &'a mut BufReader<std::process::ChildStdout>,
}

#[cfg(all(unix, not(test)))]
pub(super) fn automated_session_supervisor_command(
    target: &Command,
    spec: AutomatedSupervisorSpec<'_>,
) -> Result<Command> {
    let executable = std::env::current_exe()
        .context("Failed to resolve the CLT automated-session supervisor executable")?;
    let mut supervisor = Command::new(executable);
    supervisor
        .arg("--local")
        .arg("agent")
        .arg("automated-session-supervisor")
        .arg("--state-dir")
        .arg(spec.state_dir)
        .arg("--project-id")
        .arg(spec.project_id.to_string())
        .arg("--run-token")
        .arg(spec.run_token)
        .arg("--lease-holder")
        .arg(spec.lease_holder)
        .arg("--stdout-path")
        .arg(spec.stdout_path)
        .arg("--stderr-path")
        .arg(spec.stderr_path)
        .arg("--")
        .arg(target.get_program())
        .args(target.get_args());
    configure_automated_exec_gate_inheritance(&mut supervisor, target);
    Ok(supervisor)
}

#[cfg(all(unix, test))]
pub(super) fn automated_session_supervisor_command(
    target: &Command,
    spec: AutomatedSupervisorSpec<'_>,
) -> Result<Command> {
    // A test binary is driven by libtest rather than the Clap entry point. Run
    // one exact helper test and pass the real supervisor arguments through its
    // environment so runner tests exercise the same ownership loop.
    let executable = std::env::current_exe()
        .context("Failed to resolve the CLT automated-session test supervisor")?;
    let mut supervisor = Command::new(executable);
    supervisor
        .arg("--exact")
        .arg("tests::automated_session_supervisor_process_entry")
        .arg("--nocapture")
        .env(TEST_AUTOMATED_SUPERVISOR_ENV, "1")
        .env("CLT_TEST_SUPERVISOR_STATE_DIR", spec.state_dir)
        .env(
            "CLT_TEST_SUPERVISOR_PROJECT_ID",
            spec.project_id.to_string(),
        )
        .env("CLT_TEST_SUPERVISOR_RUN_TOKEN", spec.run_token)
        .env("CLT_TEST_SUPERVISOR_LEASE_HOLDER", spec.lease_holder)
        .env("CLT_TEST_SUPERVISOR_STDOUT_PATH", spec.stdout_path)
        .env("CLT_TEST_SUPERVISOR_STDERR_PATH", spec.stderr_path)
        .env("CLT_TEST_SUPERVISOR_PROGRAM", target.get_program())
        .env(
            "CLT_TEST_SUPERVISOR_ARGUMENT_COUNT",
            target.get_args().count().to_string(),
        );
    for (index, argument) in target.get_args().enumerate() {
        supervisor.env(format!("CLT_TEST_SUPERVISOR_ARGUMENT_{index}"), argument);
    }
    configure_automated_exec_gate_inheritance(&mut supervisor, target);
    Ok(supervisor)
}

#[cfg(unix)]
pub(super) fn spawn_automated_session_supervisor(
    target: &Command,
    spec: AutomatedSupervisorSpec<'_>,
    supervisor_stderr: fs::File,
) -> Result<AutomatedSupervisorChild> {
    let mut command = automated_session_supervisor_command(target, spec)?;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(supervisor_stderr));
    let mut process = command
        .spawn()
        .context("Failed to start the automated Codex session supervisor")?;
    let control = process
        .stdin
        .take()
        .context("Automated Codex supervisor did not open its control pipe")?;
    let readiness = process
        .stdout
        .take()
        .context("Automated Codex supervisor did not open its readiness pipe")?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    thread::Builder::new()
        .name(format!(
            "clt-automated-supervisor-ready-{}",
            spec.project_id
        ))
        .spawn(move || {
            let mut reader = BufReader::new(readiness);
            let result = loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        break Err(anyhow::anyhow!(
                            "Automated Codex supervisor closed before reporting its child PID"
                        ));
                    }
                    Ok(_) => {
                        if let Some(pid) = line
                            .trim_end()
                            .strip_prefix(AUTOMATED_SUPERVISOR_READY_PREFIX)
                        {
                            break pid
                                .parse::<u32>()
                                .context("Automated supervisor reported an invalid Codex PID")
                                .map(|child_pid| (child_pid, reader));
                        }
                    }
                    Err(error) => {
                        break Err(error).context(
                            "Failed to read the automated Codex supervisor readiness pipe",
                        );
                    }
                }
            };
            let _ = sender.send(result);
        })
        .context("Failed to start the automated supervisor readiness reader")?;

    match receiver.recv_timeout(Duration::from_secs(AGENT_SUPERVISOR_READY_TIMEOUT_SECONDS)) {
        Ok(Ok((child_pid, proof))) => Ok(AutomatedSupervisorChild {
            process,
            control,
            child_pid,
            proof,
        }),
        Ok(Err(error)) => {
            drop(control);
            let _ = process.wait();
            Err(error)
        }
        Err(error) => {
            drop(control);
            let _ = process.wait();
            Err(error).context("Timed out waiting for the automated Codex supervisor to start")
        }
    }
}

#[cfg(unix)]
pub(super) fn run_automated_session_supervisor(
    spec: AutomatedSupervisorSpec<'_>,
    program: &Path,
    arguments: &[OsString],
) -> Result<i32> {
    let stdout_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(spec.stdout_path)
        .with_context(|| {
            format!(
                "Failed to open supervised Codex stdout {:?}",
                spec.stdout_path
            )
        })?;
    let stderr_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(spec.stderr_path)
        .with_context(|| {
            format!(
                "Failed to open supervised Codex stderr {:?}",
                spec.stderr_path
            )
        })?;
    let mut target = Command::new(program);
    target.args(arguments);
    let mut command = automated_exec_gate_command(&target)?;
    command
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));
    configure_agent_child_command(&mut command);
    let mut child = command.spawn().with_context(|| {
        format!(
            "Failed to start supervised automated Codex command {}",
            program.display()
        )
    })?;
    let child_pid = child.id();
    let Some(mut launch_gate) = child.stdin.take() else {
        let _ = stop_supervised_automated_child_until_reaped(
            &mut child,
            "its launch-gate pipe was unavailable",
        );
        anyhow::bail!("Supervised automated Codex launch gate did not open its release pipe");
    };

    let readiness_result = (|| -> Result<()> {
        let mut output = stdout().lock();
        writeln!(output, "{AUTOMATED_SUPERVISOR_READY_PREFIX}{child_pid}")?;
        output.flush()?;
        Ok(())
    })();
    if let Err(error) = readiness_result {
        drop(launch_gate);
        let _status = stop_supervised_automated_child_until_reaped(
            &mut child,
            "its parent disconnected before readiness",
        );
        finalize_disconnected_automated_supervisor(
            spec.state_dir,
            spec.project_id,
            child_pid,
            spec.run_token,
            spec.lease_holder,
        );
        return Err(error).context("Failed to report the supervised Codex child PID");
    }

    let mut parent_input = io::stdin().lock();
    let parent_released = match automated_exec_gate_is_released(&mut parent_input) {
        Ok(released) => released,
        Err(error) => {
            drop(parent_input);
            drop(launch_gate);
            let status = stop_supervised_automated_child_until_reaped(
                &mut child,
                "its launch-release pipe failed",
            );
            finalize_disconnected_automated_supervisor(
                spec.state_dir,
                spec.project_id,
                child_pid,
                spec.run_token,
                spec.lease_holder,
            );
            eprintln!("Failed to read automated supervisor launch release: {error}");
            return report_automated_supervisor_reaped(status);
        }
    };
    if !parent_released {
        drop(parent_input);
        drop(launch_gate);
        let status = stop_supervised_automated_child_until_reaped(
            &mut child,
            "its parent disconnected before launch",
        );
        finalize_disconnected_automated_supervisor(
            spec.state_dir,
            spec.project_id,
            child_pid,
            spec.run_token,
            spec.lease_holder,
        );
        return report_automated_supervisor_reaped(status);
    }
    drop(parent_input);
    if let Err(error) = launch_gate
        .write_all(b"x")
        .and_then(|_| launch_gate.flush())
    {
        drop(launch_gate);
        let _ = stop_supervised_automated_child_until_reaped(
            &mut child,
            "its inner launch gate could not be released",
        );
        eprintln!("Failed to release supervised automated Codex launch gate: {error}");
        return report_automated_supervisor_reaped(None);
    }
    drop(launch_gate);

    let parent_state = Arc::new(AtomicU64::new(AUTOMATED_SUPERVISOR_CONNECTED));
    let lifeline_state = Arc::clone(&parent_state);
    let lifeline_result = thread::Builder::new()
        .name(format!(
            "clt-automated-supervisor-lifeline-{}",
            spec.project_id
        ))
        .spawn(move || {
            let mut input = io::stdin();
            let mut buffer = [0_u8; 1];
            loop {
                match input.read(&mut buffer) {
                    Ok(0) | Err(_) => {
                        lifeline_state
                            .store(AUTOMATED_SUPERVISOR_PARENT_DISCONNECTED, Ordering::SeqCst);
                        break;
                    }
                    Ok(_) if buffer[0] == b's' => {
                        let _ = lifeline_state.compare_exchange(
                            AUTOMATED_SUPERVISOR_CONNECTED,
                            AUTOMATED_SUPERVISOR_STOP_REQUESTED,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        );
                    }
                    Ok(_) => {}
                }
            }
        });
    if let Err(error) = lifeline_result {
        let status = stop_supervised_automated_child_until_reaped(
            &mut child,
            "its parent lifeline could not start",
        );
        eprintln!("Failed to start automated supervisor parent lifeline: {error}");
        return report_automated_supervisor_reaped(status);
    }

    // The runner owns session-control polling while it is connected and sends a
    // stop byte through the supervisor lifeline when a control is requested.
    // Keeping the supervisor out of the multiprocess agent database prevents a
    // storage-engine panic here from orphaning the Codex process it alone can
    // reap. If the runner crashes, EOF on the same lifeline remains an
    // independent, database-free shutdown signal.
    let monitor_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        loop {
            #[cfg(test)]
            if spec.run_token.starts_with("panic-supervisor-after-launch-") {
                panic!("injected automated supervisor monitor panic");
            }

            match interactive_child_exited_without_reaping(&child) {
                Ok(true) => {
                    break stop_supervised_automated_child_until_reaped(
                        &mut child,
                        "the Codex group leader exited",
                    );
                }
                Ok(false) => {}
                Err(error) => {
                    eprintln!("Automated supervisor could not poll its Codex child: {error:#}");
                    break stop_supervised_automated_child_until_reaped(
                        &mut child,
                        "polling the Codex child failed",
                    );
                }
            }

            if parent_state.load(Ordering::SeqCst) != AUTOMATED_SUPERVISOR_CONNECTED {
                break stop_supervised_automated_child_until_reaped(
                    &mut child,
                    "its runner requested shutdown or disconnected",
                );
            }
            thread::sleep(Duration::from_millis(100));
        }
    }));
    let status = match monitor_result {
        Ok(status) => status,
        Err(_) => {
            eprintln!(
                "Automated supervisor monitor panicked; stopping its owned Codex process group"
            );
            stop_supervised_automated_child_until_reaped(
                &mut child,
                "its supervisor monitor panicked",
            )
        }
    };

    let parent_disconnected =
        parent_state.load(Ordering::SeqCst) == AUTOMATED_SUPERVISOR_PARENT_DISCONNECTED;
    // A connected runner owns durable worker finalization. It already has the
    // reaping proof and will transition the exact session generation before it
    // records the run. Releasing or transferring its lease here would fence
    // that outer worker between `run_project` and transactional finalization.
    // The supervisor takes over only when that owner actually disconnects.
    if parent_disconnected {
        finalize_disconnected_automated_supervisor(
            spec.state_dir,
            spec.project_id,
            child_pid,
            spec.run_token,
            spec.lease_holder,
        );
    }
    report_automated_supervisor_reaped(status)
}

#[cfg(unix)]
pub(super) fn automated_supervisor_exit_code(status: Option<ExitStatus>) -> i32 {
    status.and_then(|status| status.code()).unwrap_or(1)
}

#[cfg(unix)]
pub(super) fn report_automated_supervisor_reaped(status: Option<ExitStatus>) -> Result<i32> {
    let exit_code = automated_supervisor_exit_code(status);
    let mut output = stdout().lock();
    writeln!(output, "{AUTOMATED_SUPERVISOR_REAPED_PREFIX}{exit_code}")
        .context("Failed to report automated Codex process-group shutdown")?;
    output
        .flush()
        .context("Failed to flush automated Codex process-group shutdown proof")?;
    Ok(exit_code)
}

#[cfg(unix)]
pub(super) fn supervised_session_control(
    store: &agent::TursoAgentStore,
    project_id: i64,
    child_pid: u32,
    run_token: &str,
) -> Result<Option<agent::AgentSessionControlRecord>> {
    Ok(store
        .session_controls_for_project_blocking(project_id)?
        .into_iter()
        .find(|control| {
            control.child_pid == Some(child_pid) && control.run_token.as_deref() == Some(run_token)
        }))
}

#[cfg(unix)]
pub(super) fn finalize_reaped_unregistered_agent_worker(
    store: &agent::TursoAgentStore,
    project_id: i64,
    run_token: &str,
    lease_holder: &str,
) -> Result<bool> {
    if let Some(worker) = store
        .list_active_workers_blocking()?
        .into_iter()
        .find(|worker| worker.worker_token == run_token)
    {
        if worker.project_id != project_id || worker.lease_holder != lease_holder {
            anyhow::bail!(
                "Reaped supervisor worker {run_token} does not match its exact project and lease fence"
            );
        }
        let lease = store.lease_for_project_blocking(project_id)?;
        let permitted_successor_holder = lease
            .as_ref()
            .filter(|lease| lease.holder != worker.lease_holder)
            .map(|lease| lease.holder.as_str());
        return store.abandon_worker_blocking(agent::AgentWorkerAbandonment {
            worker_token: run_token,
            expected_state: &worker.state,
            expected_worker_pid: worker.worker_pid,
            expected_heartbeat_at: worker.heartbeat_at.as_deref(),
            finished_at: &agent_timestamp(),
            error: "Automated runner disconnected after its supervised Codex process group was proven reaped",
            permitted_successor_holder,
        });
    }

    if let Some(worker) = store
        .list_terminal_workers_blocking()?
        .into_iter()
        .find(|worker| worker.worker_token == run_token)
    {
        if worker.project_id != project_id || worker.lease_holder != lease_holder {
            anyhow::bail!(
                "Terminal supervisor worker {run_token} does not match its exact project and lease fence"
            );
        }
        store.release_lease_blocking(project_id, lease_holder)?;
        return Ok(true);
    }

    if store
        .git_launch_state_blocking(project_id, run_token)?
        .is_some()
    {
        anyhow::bail!(
            "Reaped Git launch {run_token} has no exact durable worker to finalize; preserving its launch boundary and lease"
        );
    }
    store.release_lease_blocking(project_id, lease_holder)?;
    Ok(true)
}

#[cfg(unix)]
pub(super) fn stop_supervised_automated_child_until_reaped(
    child: &mut Child,
    reason: &str,
) -> Option<ExitStatus> {
    let process_group = match i32::try_from(child.id()) {
        Ok(process_group) => process_group,
        Err(error) => {
            eprintln!(
                "Automated supervisor cannot identify its owned Codex process group after {reason}: {error}"
            );
            loop {
                thread::sleep(Duration::from_secs(1));
            }
        }
    };
    let mut leader_status = None;
    let mut last_warning: Option<Instant> = None;
    loop {
        if let Some(status) = leader_status {
            match agent_process_group_exists(process_group) {
                Ok(false) => return Some(status),
                // Once the leader is reaped, its numeric PGID is no longer
                // anchored against reuse. Keep the generation fenced and only
                // observe from here; signaling again could target a new group.
                Ok(true) => {}
                Err(error) => {
                    let should_warn = last_warning
                        .is_none_or(|warning| warning.elapsed() >= Duration::from_secs(5));
                    if should_warn {
                        eprintln!(
                            "Automated supervisor cannot yet prove Codex group {process_group} exited after {reason}: {error:#}"
                        );
                        last_warning = Some(Instant::now());
                    }
                }
            }
        } else {
            match stop_agent_child_process(child) {
                Ok(status) => return status,
                Err(error) => {
                    let should_warn = last_warning
                        .is_none_or(|warning| warning.elapsed() >= Duration::from_secs(5));
                    if should_warn {
                        eprintln!(
                            "Automated supervisor retains its owned Codex group after {reason}: {error:#}"
                        );
                        last_warning = Some(Instant::now());
                    }
                    match child.try_wait() {
                        Ok(Some(status)) => leader_status = Some(status),
                        Ok(None) => {}
                        Err(error) => eprintln!(
                            "Automated supervisor could not poll its Codex group leader: {error:#}"
                        ),
                    }
                }
            }
        }
        thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(unix)]
pub(super) fn finalize_disconnected_automated_supervisor(
    state_dir: &Path,
    project_id: i64,
    child_pid: u32,
    run_token: &str,
    lease_holder: &str,
) {
    let mut last_warning: Option<Instant> = None;
    loop {
        let result = with_agent_store_at(state_dir, |store| {
            store.finalize_reaped_automated_session_blocking(
                project_id,
                child_pid,
                run_token,
                lease_holder,
                agent_lease_timeout()?.as_secs().max(60),
            )
        });
        match result {
            Ok(true) => return,
            Ok(false) => match with_agent_store_at(state_dir, |store| {
                supervised_session_control(store, project_id, child_pid, run_token)
            }) {
                Ok(None) => {
                    match with_agent_store_at(state_dir, |store| {
                        finalize_reaped_unregistered_agent_worker(
                            store,
                            project_id,
                            run_token,
                            lease_holder,
                        )
                    }) {
                        Ok(true) => return,
                        Ok(false) => {}
                        Err(error) => {
                            let should_warn = last_warning
                                .is_none_or(|warning| warning.elapsed() >= Duration::from_secs(5));
                            if should_warn {
                                eprintln!(
                                    "Automated supervisor is retrying exact worker finalization after reaping Codex: {error:#}"
                                );
                                last_warning = Some(Instant::now());
                            }
                        }
                    }
                }
                Ok(Some(_)) => {}
                Err(error) => {
                    let should_warn = last_warning
                        .is_none_or(|warning| warning.elapsed() >= Duration::from_secs(5));
                    if should_warn {
                        eprintln!(
                            "Automated supervisor is retrying its post-reap state check: {error:#}"
                        );
                        last_warning = Some(Instant::now());
                    }
                }
            },
            Err(error) => {
                let should_warn =
                    last_warning.is_none_or(|warning| warning.elapsed() >= Duration::from_secs(5));
                if should_warn {
                    eprintln!("Automated supervisor is retrying post-reap finalization: {error:#}");
                    last_warning = Some(Instant::now());
                }
            }
        }
        thread::sleep(Duration::from_millis(250));
    }
}

pub(super) fn format_agent_run_line(run: &agent::AgentRunRecord) -> String {
    format!(
        "run={} project={} {} status={} started_at={} finished_at={} exit_code={} summary={} stdout={} stderr={} path={}",
        run.id,
        run.project_id,
        run.project_name,
        run.status,
        format_agent_timestamp(&run.started_at),
        format_optional_agent_timestamp(run.finished_at.as_deref()),
        run.exit_code
            .map(|exit_code| exit_code.to_string())
            .unwrap_or_else(|| "-".to_string()),
        run.summary.as_deref().unwrap_or("-"),
        run.stdout_path.as_deref().unwrap_or("-"),
        run.stderr_path.as_deref().unwrap_or("-"),
        run.project_path.display()
    )
}

pub(super) fn print_agent_log_tail(label: &str, path: Option<&str>) -> Result<()> {
    print_agent_log_tail_with_limit(label, path, 20)
}

pub(super) fn print_agent_log_tail_with_limit(
    label: &str,
    path: Option<&str>,
    limit: usize,
) -> Result<()> {
    let Some(path) = path else {
        println!("{label}=<not recorded>");
        return Ok(());
    };
    let path = Path::new(path);
    println!("{label}={}", path.display());
    match fs::read_to_string(path) {
        Ok(content) => {
            let tail = tail_lines(&content, limit);
            if tail.is_empty() {
                println!("  <empty>");
            } else {
                for line in tail {
                    println!("  {line}");
                }
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            println!("  <missing>");
        }
        Err(err) => {
            return Err(err).with_context(|| format!("Failed to read agent log {:?}", path));
        }
    }

    Ok(())
}

pub(super) fn tail_lines(content: &str, limit: usize) -> Vec<&str> {
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(limit);
    lines[start..].to_vec()
}

impl CodexAgentRunner {
    pub(super) fn new(state_dir: PathBuf) -> Result<Self> {
        Self::new_with_worker_token(state_dir, None)
    }

    pub(super) fn new_with_worker_token(
        state_dir: PathBuf,
        worker_token: Option<String>,
    ) -> Result<Self> {
        let lease_timeout = agent_lease_timeout()?;
        Ok(Self {
            state_dir,
            timeout: agent_run_timeout()?,
            heartbeat_interval: agent_poll_interval()?,
            lease_timeout,
            lease_renew_interval: agent_lease_renew_interval(lease_timeout),
            command: agent_codex_command(),
            worker_token,
        })
    }

    #[cfg(test)]
    pub(super) fn with_command(state_dir: PathBuf, timeout: Duration, command: PathBuf) -> Self {
        let lease_timeout = Duration::from_secs(60);
        Self {
            state_dir,
            timeout,
            heartbeat_interval: Duration::from_secs(AGENT_DEFAULT_POLL_INTERVAL_SECONDS),
            lease_timeout,
            lease_renew_interval: agent_lease_renew_interval(lease_timeout),
            command,
            worker_token: None,
        }
    }
}

pub(super) fn agent_codex_prompt(
    project: &agent::AgentProject,
    task_selection: AgentTaskSelection,
) -> String {
    let clt_skill_available =
        agent_skill_is_available(&project.path, CLT_TASK_MANAGEMENT_SKILL_NAME);
    let git_skill_available = project.git_mode == AgentGitMode::Off
        || agent_skill_is_available(&project.path, GIT_COMMIT_SKILL_NAME);
    build_agent_codex_prompt(
        project,
        task_selection,
        clt_skill_available,
        git_skill_available,
    )
}

pub(super) fn effective_agent_git_mode(
    store: &agent::TursoAgentStore,
    project: &agent::AgentProject,
    resume_session_id: Option<&str>,
) -> Result<AgentGitMode> {
    let Some(session_id) = resume_session_id else {
        return Ok(project.git_mode);
    };
    let Some(finalization) = store.git_finalization_blocking(project.id, session_id)? else {
        return Ok(project.git_mode);
    };
    Ok(if finalization.state.is_terminal() {
        project.git_mode
    } else {
        finalization.git_mode
    })
}

pub(super) fn build_agent_codex_prompt(
    project: &agent::AgentProject,
    task_selection: AgentTaskSelection,
    clt_skill_available: bool,
    git_skill_available: bool,
) -> String {
    let mut prompt = AGENT_CODEX_PROMPT_BASE.to_string();
    if clt_skill_available {
        prompt.push_str(
            "\nTask workflow:\n- Use the $clt-task-management skill for the task-board workflow.\n",
        );
    }
    match task_selection {
        AgentTaskSelection::NextTodo => {}
        AgentTaskSelection::ResumeDoing => {
            prompt.push_str(AGENT_RESUME_DOING_PROMPT_APPENDIX);
        }
        AgentTaskSelection::RecoverBlocked => {
            prompt.push_str(AGENT_RECOVER_BLOCKED_PROMPT_APPENDIX);
        }
        AgentTaskSelection::ResumeSession => {
            prompt.push_str(AGENT_RESUME_SESSION_PROMPT_APPENDIX);
        }
    }
    match project.git_mode {
        AgentGitMode::Off => {}
        AgentGitMode::Commit => prompt.push_str(AGENT_GIT_COMMIT_PROMPT_APPENDIX),
        AgentGitMode::CommitAndPush => {
            prompt.push_str(AGENT_GIT_COMMIT_PROMPT_APPENDIX);
            prompt.push_str(AGENT_GIT_PUSH_PROMPT_APPENDIX);
        }
    }
    if !clt_skill_available {
        append_embedded_agent_skill(
            &mut prompt,
            CLT_TASK_MANAGEMENT_SKILL_NAME,
            EMBEDDED_CLT_TASK_MANAGEMENT_SKILL,
        );
    }
    if project.git_mode != AgentGitMode::Off && !git_skill_available {
        append_embedded_agent_skill(
            &mut prompt,
            GIT_COMMIT_SKILL_NAME,
            EMBEDDED_GIT_COMMIT_SKILL,
        );
    }
    prompt
}

pub(super) fn append_embedded_agent_skill(prompt: &mut String, name: &str, contents: &str) {
    prompt.push_str("\n\nEmbedded skill fallback:\n");
    prompt.push_str("- The $");
    prompt.push_str(name);
    prompt.push_str(
        " skill was not found in a standard Codex skill directory. Follow this bundled version for this run.\n\n<skill>\n<name>",
    );
    prompt.push_str(name);
    prompt.push_str("</name>\n<source>embedded in clt</source>\n");
    prompt.push_str(contents);
    if !contents.ends_with('\n') {
        prompt.push('\n');
    }
    prompt.push_str("</skill>");
}

pub(super) fn agent_skill_is_available(project_root: &Path, skill_name: &str) -> bool {
    agent_skill_search_roots(project_root)
        .iter()
        .any(|root| agent_skill_root_contains_name(root, skill_name))
}

pub(super) fn agent_skill_search_roots(project_root: &Path) -> Vec<PathBuf> {
    let repository_root =
        get_task_root_at(project_root, false).unwrap_or_else(|_| project_root.to_path_buf());
    let mut roots = Vec::new();
    let mut directory = project_root.to_path_buf();

    loop {
        roots.push(directory.join(".agents/skills"));
        if directory == repository_root
            || !directory.pop()
            || !directory.starts_with(&repository_root)
        {
            break;
        }
    }

    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".agents/skills"));
    }
    roots.push(PathBuf::from("/etc/codex/skills"));
    roots
}

pub(super) fn agent_skill_root_contains_name(root: &Path, skill_name: &str) -> bool {
    let Ok(entries) = fs::read_dir(root) else {
        return false;
    };

    entries.filter_map(Result::ok).any(|entry| {
        fs::read_to_string(entry.path().join("SKILL.md"))
            .ok()
            .and_then(|contents| skill_frontmatter_name(&contents).map(str::to_string))
            .is_some_and(|name| name == skill_name)
    })
}

pub(super) fn skill_frontmatter_name(contents: &str) -> Option<&str> {
    let mut lines = contents.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }

    for line in lines {
        let line = line.trim();
        if line == "---" {
            break;
        }
        if let Some(name) = line.strip_prefix("name:") {
            return Some(name.trim().trim_matches(['\"', '\'']));
        }
    }
    None
}

impl AgentRunner for CodexAgentRunner {
    fn run_project(
        &self,
        project: &agent::AgentProject,
        task_selection: AgentTaskSelection,
        resume_session_id: Option<&str>,
        lease_holder: &str,
        run_token: Option<&str>,
        shutdown: &AgentShutdownSignal,
    ) -> Result<AgentRunResult> {
        let store = open_agent_store_at(&self.state_dir)?;
        let known_session_id = match resume_session_id {
            Some(session_id) => Some(session_id.to_string()),
            None => automated_codex_session_to_resume(&project.path, task_selection)?,
        };
        let mut effective_project = project.clone();
        effective_project.git_mode =
            effective_agent_git_mode(&store, project, known_session_id.as_deref())?;
        let project = &effective_project;
        let effective_worker_token = run_token
            .map(str::to_string)
            .or_else(|| self.worker_token.clone());
        if run_token.is_some()
            && self.worker_token.is_some()
            && run_token != self.worker_token.as_deref()
        {
            anyhow::bail!("Agent runner received conflicting durable worker tokens");
        }
        let run_file_stem = effective_worker_token
            .clone()
            .unwrap_or_else(|| agent_log_file_stem(project.id));
        ensure_agent_git_index_preflight(project, known_session_id.is_some())?;
        let existing_git_finalization = known_session_id
            .as_deref()
            .map(|session_id| store.git_finalization_blocking(project.id, session_id))
            .transpose()?
            .flatten();
        let git_start_state = prepare_agent_git_start_state_for_run(
            &store,
            project,
            task_selection,
            known_session_id.is_some(),
            existing_git_finalization.is_some(),
            &run_file_stem,
        )?;
        let doing_task_contents_before =
            task_contents_for_status(&project.path, TaskStatus::Doing).unwrap_or_default();
        let blocked_task_snapshots_before =
            blocked_task_snapshots(&project.path).unwrap_or_default();
        let log_dir = agent_project_run_log_dir(&self.state_dir, project)?;
        fs::create_dir_all(&log_dir)
            .with_context(|| format!("Failed to create agent run log directory {:?}", log_dir))?;

        let stdout_path = log_dir.join(format!("{run_file_stem}.out"));
        let stderr_path = log_dir.join(format!("{run_file_stem}.err"));
        let stdout_file = fs::File::create(&stdout_path)
            .with_context(|| format!("Failed to create stdout log {:?}", stdout_path))?;
        fs::File::create(&stderr_path)
            .with_context(|| format!("Failed to create stderr log {:?}", stderr_path))?;
        let stderr_file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&stderr_path)
            .with_context(|| format!("Failed to reopen stderr log {:?}", stderr_path))?;

        let mut command = Command::new(&self.command);
        command
            .arg("--sandbox")
            .arg("danger-full-access")
            .arg("--ask-for-approval")
            .arg("never")
            .arg("--enable")
            .arg("goals");
        let model_target = if let Some(model_id) = project.codex_model.as_ref() {
            agent::AgentModelDefaults {
                provider_id: Some(
                    project
                        .codex_provider
                        .clone()
                        .unwrap_or_else(|| "openai".to_string()),
                ),
                model_id: Some(model_id.clone()),
            }
        } else {
            store.resolve_model_target_blocking(project)?
        };
        if let (Some(provider), Some(model)) = (
            model_target.provider_id.as_deref(),
            model_target.model_id.as_deref(),
        ) {
            command
                .arg("--config")
                .arg(format!("model_provider={provider:?}"));
            command.arg("--model").arg(model);
        }
        let model_reasoning_effort = if project.codex_reasoning_effort.is_none() {
            match (
                model_target.provider_id.as_deref(),
                model_target.model_id.as_deref(),
            ) {
                (Some(provider), Some(model)) => {
                    store.model_target_reasoning_blocking(provider, model)?
                }
                _ => None,
            }
        } else {
            None
        };
        if let Some(reasoning_effort) = project
            .codex_reasoning_effort
            .as_deref()
            .or(model_reasoning_effort.as_deref())
        {
            command
                .arg("--config")
                .arg(format!("model_reasoning_effort=\"{reasoning_effort}\""));
        }
        if project.codex_fast_enabled {
            command
                .arg("--enable")
                .arg("fast_mode")
                .arg("--config")
                .arg("service_tier=\"fast\"");
        } else {
            command.arg("--disable").arg("fast_mode");
        }
        let configured_session_id = configure_automated_codex_subcommand(
            &mut command,
            project,
            task_selection,
            resume_session_id,
        )?;
        #[cfg(not(unix))]
        if let Some(session_id) = configured_session_id.as_deref() {
            anyhow::bail!(
                "Automated Codex session resume {session_id} is unsupported on this platform because CLT cannot register it before launch"
            );
        }
        command.current_dir(&project.path);
        configure_agent_git_identity(&mut command, project.git_mode);
        configure_automated_agent_child_context(
            &mut command,
            &self.state_dir,
            project.id,
            &run_file_stem,
        );

        let persist_git_launch_state = || -> Result<bool> {
            let Some(git_start_state) = git_start_state.as_ref() else {
                return Ok(false);
            };
            verify_agent_git_start_state_unchanged(
                &project.path,
                project.git_mode,
                git_start_state,
            )?;
            store.record_git_launch_state_blocking(
                project.id,
                &run_file_stem,
                project.git_mode,
                git_start_state,
                &agent_timestamp(),
            )
        };

        #[cfg(unix)]
        let spawn_result: Result<_> = (|| {
            drop(stdout_file);
            let supervised = spawn_automated_session_supervisor(
                &command,
                AutomatedSupervisorSpec {
                    state_dir: &self.state_dir,
                    project_id: project.id,
                    run_token: &run_file_stem,
                    lease_holder,
                    stdout_path: &stdout_path,
                    stderr_path: &stderr_path,
                },
                stderr_file,
            )?;
            let AutomatedSupervisorChild {
                mut process,
                control,
                child_pid,
                mut proof,
            } = supervised;
            let git_launch_state_was_created = match persist_git_launch_state() {
                Ok(created) => created,
                Err(error) => {
                    drop(control);
                    wait_for_automated_supervisor_reaped(&mut process, &mut proof).with_context(
                        || {
                            format!(
                                "Persisting the prelaunch Git state failed ({error:#}), and the automated supervisor did not prove Codex stopped"
                            )
                        },
                    )?;
                    return Err(error).context(
                        "Failed to persist the prelaunch Git state behind the Codex launch gate",
                    );
                }
            };
            Ok((
                process,
                child_pid,
                Some(control),
                Some(proof),
                git_launch_state_was_created,
            ))
        })();
        #[cfg(not(unix))]
        let spawn_result: Result<_> = (|| {
            let git_launch_state_was_created = persist_git_launch_state()?;
            command
                .stdout(Stdio::from(stdout_file))
                .stderr(Stdio::from(stderr_file));
            configure_agent_child_command(&mut command);
            match command.spawn() {
                Ok(child) => {
                    let child_pid = child.id();
                    Ok((child, child_pid, None, None, git_launch_state_was_created))
                }
                Err(error) => {
                    if git_launch_state_was_created
                        && !store.delete_git_launch_state_blocking(project.id, &run_file_stem)?
                    {
                        anyhow::bail!(
                            "The Codex process failed to spawn and its exact Git launch boundary could not be deleted"
                        );
                    }
                    Err(error.into())
                }
            }
        })();

        let (
            mut child,
            child_pid,
            mut supervisor_control,
            mut supervisor_proof,
            _git_launch_state_was_created,
        ) = match spawn_result {
            Ok(child) => child,
            Err(err) => {
                let summary = format!(
                    "Failed to start Codex command {} in {}: {err}",
                    self.command.display(),
                    project.path.display()
                );
                append_agent_log_line(&stderr_path, &summary)?;
                return Ok(AgentRunResult {
                    status: "failure",
                    exit_code: None,
                    log_dir,
                    stdout_path,
                    stderr_path,
                    summary,
                    codex_session_id: configured_session_id,
                    session_run_token: None,
                    control_action: None,
                });
            }
        };
        let mut last_heartbeat_stderr_bytes = 0;
        let mut observed_session_id = configured_session_id;
        let mut session_linked = false;
        let mut session_registered = false;
        let mut session_link_error_logged = false;
        let mut last_session_control_poll: Option<Instant> = None;
        let mut last_lease_renewal = Instant::now();
        let requested_control_cell = Cell::new(None);
        if let Some(session_id) = observed_session_id.as_deref() {
            let registration_result = store.register_known_session_with_child_blocking(
                agent::AgentKnownSessionRegistration {
                    project_id: project.id,
                    codex_session_id: session_id,
                    child_pid,
                    run_token: &run_file_stem,
                    stdout_path: &stdout_path,
                    stderr_path: &stderr_path,
                    lease_holder,
                    lease_timeout_seconds: self.lease_timeout.as_secs(),
                    claim_requested_resume: task_selection == AgentTaskSelection::ResumeSession
                        && resume_session_id == Some(session_id),
                },
            );
            let registration_error = match registration_result {
                Ok(true) => None,
                Ok(false) => Some(anyhow::anyhow!(
                    "Known-session control or its live project lease changed before Codex launch"
                )),
                Err(error) => Some(error),
            };
            if let Some(error) = registration_error {
                #[cfg(unix)]
                {
                    supervisor_control.take();
                    wait_for_automated_supervisor_reaped(
                        &mut child,
                        supervisor_proof
                            .as_mut()
                            .expect("Unix automated supervisor has a proof pipe"),
                    )
                    .with_context(|| {
                        format!(
                            "Known-session registration failed ({error:#}), and its supervisor could not prove Codex stopped"
                        )
                    })?;
                }
                #[cfg(not(unix))]
                stop_agent_child_process(&mut child).with_context(|| {
                    format!(
                        "Known-session registration failed ({error:#}), and CLT could not prove its spawned Codex process stopped"
                    )
                })?;
                return Err(error).context("Failed to register known Codex child before launch");
            }
            session_registered = true;
            ensure_agent_git_working_record(
                &store,
                project,
                session_id,
                &run_file_stem,
                git_start_state.as_ref(),
            )?;
            let _ =
                bind_agent_git_working_task_identity(&store, project, session_id, &run_file_stem)?;
        }
        #[cfg(unix)]
        if let Err(error) = supervisor_control
            .as_mut()
            .expect("Unix automated supervisor has a control pipe")
            .write_all(b"x")
            .and_then(|_| {
                supervisor_control
                    .as_mut()
                    .expect("Unix automated supervisor has a control pipe")
                    .flush()
            })
        {
            supervisor_control.take();
            wait_for_automated_supervisor_reaped(
                &mut child,
                supervisor_proof
                    .as_mut()
                    .expect("Unix automated supervisor has a proof pipe"),
            )
            .with_context(|| {
                format!(
                    "Automated supervisor launch release failed ({error}), and it could not prove Codex stopped"
                )
            })?;
            return Err(error).context("Failed to release supervised automated Codex launch gate");
        }
        #[cfg(unix)]
        let wait_result = wait_for_automated_supervisor_with_timeout_and_heartbeat(
            AutomatedSupervisorWaitHandles {
                process: &mut child,
                control: &mut supervisor_control,
                proof: supervisor_proof
                    .as_mut()
                    .expect("Unix automated supervisor has a proof pipe"),
            },
            self.timeout,
            self.heartbeat_interval,
            |elapsed| {
                print_agent_run_heartbeat(
                    project,
                    elapsed,
                    self.timeout,
                    &stdout_path,
                    &stderr_path,
                    &mut last_heartbeat_stderr_bytes,
                )
            },
            || {
                if last_lease_renewal.elapsed() >= self.lease_renew_interval {
                    let expires_at = agent_timestamp_after(self.lease_timeout.as_secs());
                    let renewed = if let Some(worker_token) = effective_worker_token.as_deref() {
                        store.renew_worker_blocking(
                            worker_token,
                            std::process::id(),
                            &agent_timestamp(),
                            &expires_at,
                        )?
                    } else {
                        store.renew_lease_blocking(project.id, lease_holder, &expires_at)?
                    };
                    if !renewed {
                        anyhow::bail!(
                            "Automated Codex lease is no longer held for project {}",
                            project.id
                        );
                    }
                    last_lease_renewal = Instant::now();
                }
                if observed_session_id.is_none() {
                    observed_session_id = agent_codex_session_id_from_log(&stderr_path)?;
                }
                if let Some(session_id) = observed_session_id.as_deref()
                    && !session_registered
                {
                    if project.git_mode == AgentGitMode::Off {
                        store.mark_session_running_blocking(
                            project.id,
                            session_id,
                            child_pid,
                            &run_file_stem,
                            &stdout_path,
                            &stderr_path,
                        )?;
                    } else {
                        store.mark_session_running_with_git_finalization_blocking(
                            project.id,
                            session_id,
                            child_pid,
                            &run_file_stem,
                            &stdout_path,
                            &stderr_path,
                            project.git_mode,
                        )?;
                    }
                    session_registered = true;
                }
                if let Some(session_id) = observed_session_id.as_deref()
                    && !session_linked
                {
                    match attach_codex_session_to_active_task(
                        &project.path,
                        task_selection,
                        &doing_task_contents_before,
                        &blocked_task_snapshots_before,
                        session_id,
                    ) {
                        Ok(attached) => {
                            session_linked = attached;
                            if attached {
                                if store
                                    .git_finalization_blocking(project.id, session_id)?
                                    .is_none()
                                    && project.git_mode != AgentGitMode::Off
                                {
                                    ensure_agent_git_working_record(
                                        &store,
                                        project,
                                        session_id,
                                        &run_file_stem,
                                        git_start_state.as_ref(),
                                    )?;
                                }
                                let _ = bind_agent_git_working_task_identity(
                                    &store,
                                    project,
                                    session_id,
                                    &run_file_stem,
                                )?;
                            }
                        }
                        Err(error) if !session_link_error_logged => {
                            append_agent_log_line(
                                &stderr_path,
                                &format!(
                                    "Failed to attach the live Codex session to its task: {error:#}"
                                ),
                            )?;
                            session_link_error_logged = true;
                        }
                        Err(_) => {}
                    }
                }
                let should_poll_control = last_session_control_poll.is_none_or(|last_poll| {
                    last_poll.elapsed() >= Duration::from_millis(AGENT_SESSION_CONTROL_POLL_MILLIS)
                });
                if should_poll_control {
                    if let Some(session_id) = observed_session_id.as_deref()
                        && let Some(control) =
                            store.session_control_blocking(project.id, session_id)?
                        && let Some(action) = automated_session_control_action_for_generation(
                            &control,
                            child_pid,
                            &run_file_stem,
                        )
                    {
                        requested_control_cell.set(Some(action));
                    }
                    last_session_control_poll = Some(Instant::now());
                }
                Ok(())
            },
            || shutdown.load(Ordering::SeqCst) || requested_control_cell.get().is_some(),
        );
        #[cfg(not(unix))]
        let wait_result = wait_for_child_with_timeout_and_heartbeat(
            &mut child,
            self.timeout,
            self.heartbeat_interval,
            |elapsed| {
                print_agent_run_heartbeat(
                    project,
                    elapsed,
                    self.timeout,
                    &stdout_path,
                    &stderr_path,
                    &mut last_heartbeat_stderr_bytes,
                )
            },
            || Ok(()),
            || shutdown.load(Ordering::SeqCst),
        );
        let wait_result = match wait_result {
            Ok(wait_result) => wait_result,
            Err(error) => {
                #[cfg(not(unix))]
                stop_agent_child_process(&mut child).with_context(|| {
                    format!(
                        "Codex run observation failed ({error:#}), and CLT could not prove its process stopped"
                    )
                })?;
                return Err(error).context("Failed while observing the Codex run");
            }
        };
        let stdout = fs::read_to_string(&stdout_path).unwrap_or_default();
        let codex_session_id = match observed_session_id {
            Some(session_id) => Some(session_id),
            None => agent_codex_session_id_from_log(&stderr_path)?,
        };
        if requested_control_cell.get().is_none()
            && let Some(session_id) = codex_session_id.as_deref()
            && let Some(control) = store.session_control_blocking(project.id, session_id)?
            && let Some(action) =
                automated_session_control_action_for_generation(&control, child_pid, &run_file_stem)
        {
            requested_control_cell.set(Some(action));
        }
        let requested_control = requested_control_cell.get();
        let (status, exit_code, summary) = if let Some(action) = requested_control {
            let exit_code = match wait_result {
                AgentProcessWait::Exited(status) => status.code().map(i64::from),
                AgentProcessWait::TimedOut(status) | AgentProcessWait::Interrupted(status) => {
                    status.and_then(|status| status.code().map(i64::from))
                }
            };
            match action {
                AgentSessionControlAction::Stop => {
                    append_agent_log_line(&stderr_path, "Codex stopped by task-session control.")?;
                    (
                        "stopped",
                        exit_code,
                        "Codex task session stopped and remains resumable.".to_string(),
                    )
                }
                AgentSessionControlAction::Interrupt => {
                    append_agent_log_line(
                        &stderr_path,
                        "Codex interrupted for an interactive session handoff.",
                    )?;
                    (
                        "handoff",
                        exit_code,
                        "Codex task session is ready for interactive handoff.".to_string(),
                    )
                }
            }
        } else {
            match wait_result {
                AgentProcessWait::Exited(exit_status) => {
                    let exit_code = exit_status.code().map(i64::from);
                    if stdout.contains(AGENT_NO_TASKS_LEFT_MARKER) {
                        (
                            "idle",
                            exit_code,
                            "Codex reported no available tasks.".to_string(),
                        )
                    } else if exit_status.success() {
                        (
                            "success",
                            exit_code,
                            "Codex run completed successfully.".to_string(),
                        )
                    } else {
                        (
                            "failure",
                            exit_code,
                            format!("Codex exited with status {exit_status}."),
                        )
                    }
                }
                AgentProcessWait::TimedOut(exit_status) => {
                    append_agent_log_line(
                        &stderr_path,
                        &format!("Codex timed out after {} seconds.", self.timeout.as_secs()),
                    )?;
                    (
                        "timeout",
                        exit_status.and_then(|status| status.code().map(i64::from)),
                        format!("Codex timed out after {} seconds.", self.timeout.as_secs()),
                    )
                }
                AgentProcessWait::Interrupted(exit_status) => {
                    append_agent_log_line(
                        &stderr_path,
                        "Codex stopped because the agent is shutting down.",
                    )?;
                    (
                        "interrupted",
                        exit_status.and_then(|status| status.code().map(i64::from)),
                        "Codex stopped because the agent is shutting down.".to_string(),
                    )
                }
            }
        };

        Ok(AgentRunResult {
            status,
            exit_code,
            log_dir,
            stdout_path,
            stderr_path,
            summary,
            codex_session_id,
            session_run_token: session_registered.then_some(run_file_stem),
            control_action: requested_control,
        })
    }
}

pub(super) enum AgentProcessWait {
    Exited(ExitStatus),
    TimedOut(Option<ExitStatus>),
    Interrupted(Option<ExitStatus>),
}

pub(super) fn resolve_agent_project_root(
    path: Option<&Path>,
    local: bool,
    default_root: &Path,
) -> Result<PathBuf> {
    match path {
        Some(path) => get_task_root_at(path, local),
        None => canonicalize_existing_path(default_root),
    }
}

pub(super) fn canonicalize_existing_path(path: &Path) -> Result<PathBuf> {
    fs::canonicalize(path).with_context(|| format!("Failed to resolve project path {:?}", path))
}

pub(super) fn agent_timestamp() -> String {
    agent_timestamp_seconds().to_string()
}

pub(super) fn agent_timestamp_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub(super) fn agent_timestamp_after(seconds: u64) -> String {
    std::time::SystemTime::now()
        .checked_add(std::time::Duration::from_secs(seconds))
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs().to_string())
        .unwrap_or_else(agent_timestamp)
}

pub(super) fn format_agent_timestamp(raw: &str) -> String {
    let Ok(seconds) = raw.parse::<i64>() else {
        return raw.to_string();
    };

    let Some(utc) = DateTime::<Utc>::from_timestamp(seconds, 0) else {
        return raw.to_string();
    };

    utc.with_timezone(&Local)
        .format("%Y-%m-%d %H:%M:%S %Z")
        .to_string()
}

pub(super) fn format_optional_agent_timestamp(raw: Option<&str>) -> String {
    raw.map(format_agent_timestamp)
        .unwrap_or_else(|| "-".to_string())
}

pub(super) fn agent_project_run_log_dir(
    state_dir: &Path,
    project: &agent::AgentProject,
) -> Result<PathBuf> {
    let slug = agent_project_slug(project);
    Ok(state_dir.join("runs").join(slug))
}

pub(super) fn agent_project_slug(project: &agent::AgentProject) -> String {
    let mut slug = String::new();
    let mut last_was_separator = false;

    for ch in project.name.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_was_separator = false;
        } else if (ch == '-' || ch == '_' || ch.is_whitespace()) && !last_was_separator {
            slug.push('-');
            last_was_separator = true;
        }
    }

    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        format!("project-{}", project.id)
    } else {
        format!("{}-{}", project.id, slug)
    }
}

pub(super) fn agent_log_file_stem(project_id: i64) -> String {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!(
        "{}-{:03}-p{}-{}",
        duration.as_secs(),
        duration.subsec_millis(),
        project_id,
        std::process::id()
    )
}

#[cfg(any(not(unix), test))]
pub(super) fn wait_for_child_with_timeout_and_heartbeat(
    child: &mut Child,
    timeout: Duration,
    heartbeat_interval: Duration,
    mut heartbeat: impl FnMut(Duration) -> Result<()>,
    mut observe: impl FnMut() -> Result<()>,
    mut should_shutdown: impl FnMut() -> bool,
) -> Result<AgentProcessWait> {
    let heartbeat_interval = if heartbeat_interval.is_zero() {
        Duration::from_millis(250)
    } else {
        heartbeat_interval
    };
    let started = Instant::now();
    let mut last_heartbeat = started;

    loop {
        observe()?;

        if let Some(status) = child.try_wait().context("Failed to poll Codex process")? {
            return Ok(AgentProcessWait::Exited(status));
        }

        if should_shutdown() {
            let status = stop_agent_child_process(child)
                .context("Failed to stop Codex process during agent shutdown")?;
            return Ok(AgentProcessWait::Interrupted(status));
        }

        if started.elapsed() >= timeout {
            let status = stop_agent_child_process(child)
                .context("Failed to stop timed out Codex process")?;
            return Ok(AgentProcessWait::TimedOut(status));
        }

        if last_heartbeat.elapsed() >= heartbeat_interval {
            heartbeat(started.elapsed())?;
            last_heartbeat = Instant::now();
        }

        thread::sleep(std::cmp::min(
            Duration::from_millis(250),
            heartbeat_interval,
        ));
    }
}

#[cfg(unix)]
pub(super) fn wait_for_automated_supervisor_with_timeout_and_heartbeat(
    handles: AutomatedSupervisorWaitHandles<'_>,
    timeout: Duration,
    heartbeat_interval: Duration,
    mut heartbeat: impl FnMut(Duration) -> Result<()>,
    mut observe: impl FnMut() -> Result<()>,
    mut should_shutdown: impl FnMut() -> bool,
) -> Result<AgentProcessWait> {
    let AutomatedSupervisorWaitHandles {
        process: supervisor,
        control,
        proof,
    } = handles;
    let heartbeat_interval = if heartbeat_interval.is_zero() {
        Duration::from_millis(250)
    } else {
        heartbeat_interval
    };
    let started = Instant::now();
    let mut last_heartbeat = started;

    loop {
        if let Err(error) = observe() {
            request_automated_supervisor_stop(control);
            wait_for_automated_supervisor_reaped(supervisor, proof).with_context(|| {
                format!(
                    "Automated Codex observation failed ({error:#}); its supervisor did not prove the process group reaped"
                )
            })?;
            return Err(error);
        }

        if let Some(status) = supervisor
            .try_wait()
            .context("Failed to poll automated Codex supervisor")?
        {
            let status = verify_automated_supervisor_reaped(status, proof)?;
            control.take();
            return Ok(AgentProcessWait::Exited(status));
        }

        if should_shutdown() {
            request_automated_supervisor_stop(control);
            let status = wait_for_automated_supervisor_reaped(supervisor, proof)?;
            return Ok(AgentProcessWait::Interrupted(Some(status)));
        }

        if started.elapsed() >= timeout {
            request_automated_supervisor_stop(control);
            let status = wait_for_automated_supervisor_reaped(supervisor, proof)?;
            return Ok(AgentProcessWait::TimedOut(Some(status)));
        }

        if last_heartbeat.elapsed() >= heartbeat_interval {
            if let Err(error) = heartbeat(started.elapsed()) {
                request_automated_supervisor_stop(control);
                wait_for_automated_supervisor_reaped(supervisor, proof).with_context(|| {
                    format!(
                        "Automated Codex heartbeat failed ({error:#}); its supervisor did not prove the process group reaped"
                    )
                })?;
                return Err(error);
            }
            last_heartbeat = Instant::now();
        }
        thread::sleep(std::cmp::min(
            Duration::from_millis(250),
            heartbeat_interval,
        ));
    }
}

#[cfg(unix)]
pub(super) fn request_automated_supervisor_stop(control: &mut Option<std::process::ChildStdin>) {
    let write_result = control
        .as_mut()
        .context("Automated Codex supervisor control pipe is already closed")
        .and_then(|control| {
            control
                .write_all(b"s")
                .and_then(|_| control.flush())
                .context("Failed to request supervised Codex shutdown")
        });
    if let Err(error) = write_result {
        eprintln!(
            "Automated Codex supervisor stop request failed; closing its lifeline instead: {error:#}"
        );
        control.take();
    }
}

#[cfg(unix)]
pub(super) fn wait_for_automated_supervisor_reaped(
    supervisor: &mut Child,
    proof: &mut BufReader<std::process::ChildStdout>,
) -> Result<ExitStatus> {
    let status = supervisor
        .wait()
        .context("Failed to wait for automated Codex supervisor")?;
    verify_automated_supervisor_reaped(status, proof)
}

#[cfg(unix)]
pub(super) fn verify_automated_supervisor_reaped(
    supervisor_status: ExitStatus,
    proof: &mut BufReader<std::process::ChildStdout>,
) -> Result<ExitStatus> {
    let mut remainder = String::new();
    proof
        .read_to_string(&mut remainder)
        .context("Failed to read automated Codex supervisor shutdown proof")?;
    let reported_exit_code = remainder.lines().find_map(|line| {
        line.trim()
            .strip_prefix(AUTOMATED_SUPERVISOR_REAPED_PREFIX)
            .and_then(|code| code.parse::<i32>().ok())
    });
    if reported_exit_code.is_none() {
        return Err(unproven_agent_child_termination(
            anyhow::anyhow!("supervisor exited with {supervisor_status} without a reap marker"),
            "The automated Codex supervisor exited without proving its owned process group stopped",
        ));
    }
    Ok(supervisor_status)
}

pub(super) fn configure_automated_agent_child_context(
    command: &mut Command,
    state_dir: &Path,
    project_id: i64,
    run_token: &str,
) {
    command
        .env(AGENT_STATE_DIR_ENV, state_dir)
        .env(AGENT_PROJECT_ID_ENV, project_id.to_string())
        .env(AGENT_RUN_TOKEN_ENV, run_token);
}

pub(super) fn append_agent_log_line(path: &Path, line: &str) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("Failed to append to agent log {:?}", path))?;
    writeln!(file, "{line}").with_context(|| format!("Failed to write agent log {:?}", path))
}

pub(super) fn parse_agent_codex_session_id(line: &str) -> Option<String> {
    line.trim()
        .strip_prefix("session id:")
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
        .map(str::to_string)
}

pub(super) fn agent_codex_session_id_from_log(path: &Path) -> Result<Option<String>> {
    let file = fs::File::open(path)
        .with_context(|| format!("Failed to open recorded agent output {path:?}"))?;

    for line in BufReader::new(file).lines().take(100) {
        if let Some(session_id) = parse_agent_codex_session_id(&line?) {
            return Ok(Some(session_id));
        }
    }

    Ok(None)
}

pub(super) fn latest_agent_log_path(log_dir: &Path, extension: &str) -> Result<Option<PathBuf>> {
    if !log_dir.exists() {
        return Ok(None);
    }

    let mut paths = Vec::new();
    for entry in fs::read_dir(log_dir)
        .with_context(|| format!("Failed to read agent log directory {:?}", log_dir))?
    {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_file() && path.extension() == Some(OsStr::new(extension)) {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths.pop())
}

pub(super) fn preferred_recorded_agent_output_path(run: &agent::AgentRunRecord) -> Option<PathBuf> {
    let stdout_path = run.stdout_path.as_ref().map(PathBuf::from);
    let stdout_has_output = stdout_path
        .as_ref()
        .and_then(|path| fs::metadata(path).ok())
        .is_some_and(|metadata| metadata.len() > 0);

    if stdout_has_output {
        stdout_path
    } else {
        run.stderr_path.as_ref().map(PathBuf::from).or(stdout_path)
    }
}
