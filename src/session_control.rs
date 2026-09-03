use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InteractiveCodexResumeMode {
    ResumeExec,
    WritableIdle,
    WritableShared,
}

impl InteractiveCodexResumeMode {
    pub(super) fn resumes_exec(self) -> bool {
        self == Self::ResumeExec
    }

    pub(super) fn shares_project(self) -> bool {
        self == Self::WritableShared
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InteractiveGuardianDisposition {
    ResumeExec,
    PreserveIdleSession,
    RestoreStopped,
    PreserveSharedSession,
    RestoreStoppedShared,
}

impl InteractiveGuardianDisposition {
    pub(super) fn from_handoff(mode: InteractiveCodexResumeMode, from_holder: &str) -> Self {
        if mode.resumes_exec() {
            Self::ResumeExec
        } else if mode.shares_project() {
            if is_stopped_shared_interactive_holder(from_holder) {
                Self::RestoreStoppedShared
            } else {
                Self::PreserveSharedSession
            }
        } else if from_holder.starts_with("clt-stopped-interactive-") {
            Self::RestoreStopped
        } else {
            Self::PreserveIdleSession
        }
    }

    pub(super) fn holds_project_lease(self) -> bool {
        !matches!(
            self,
            Self::PreserveSharedSession | Self::RestoreStoppedShared
        )
    }

    pub(super) fn guardian_holder_prefix(self) -> &'static str {
        match self {
            Self::ResumeExec => "clt-interactive-worker",
            // Keep the established holder prefixes so a newer CLT can recover
            // guardians started by an older binary.
            Self::PreserveIdleSession => "clt-idle-interactive-worker",
            Self::RestoreStopped => "clt-stopped-interactive-worker",
            Self::PreserveSharedSession => "clt-shared-interactive-worker",
            Self::RestoreStoppedShared => "clt-stopped-shared-interactive-worker",
        }
    }

    pub(super) fn from_guardian_holder(holder: &str) -> Option<Self> {
        // Recognize guardians left by the brief read-only implementation so a
        // newer CLT can still recover their persisted session controls.
        if holder.starts_with("clt-stopped-readonly-interactive-worker-") {
            return Some(Self::RestoreStoppedShared);
        }
        if holder.starts_with("clt-readonly-interactive-worker-") {
            return Some(Self::PreserveSharedSession);
        }
        [
            Self::ResumeExec,
            Self::PreserveIdleSession,
            Self::RestoreStopped,
            Self::PreserveSharedSession,
            Self::RestoreStoppedShared,
        ]
        .into_iter()
        .find(|disposition| holder.starts_with(disposition.guardian_holder_prefix()))
    }

    pub(super) fn guardian_process_is_proven_dead(holder: &str) -> bool {
        Self::guardian_process_id(holder)
            .is_some_and(|pid| local_process_is_running(pid) == Some(false))
    }

    pub(super) fn guardian_process_id(holder: &str) -> Option<u32> {
        let disposition = Self::from_guardian_holder(holder)?;
        holder
            .strip_prefix(disposition.guardian_holder_prefix())
            .and_then(|suffix| suffix.strip_prefix('-'))
            .and_then(|suffix| suffix.split('-').next())
            .and_then(|pid| pid.parse::<u32>().ok())
    }
}

pub(super) fn is_stopped_shared_interactive_holder(holder: &str) -> bool {
    holder.starts_with("clt-stopped-shared-interactive-")
        || holder.starts_with("clt-stopped-readonly-interactive-")
}

pub(super) fn automated_session_control_action_for_generation(
    control: &agent::AgentSessionControlRecord,
    child_pid: u32,
    run_token: &str,
) -> Option<AgentSessionControlAction> {
    if control.run_token.as_deref() != Some(run_token) {
        return None;
    }
    if control.child_pid == Some(child_pid) {
        return control.state.requested_action();
    }
    match control.state {
        AgentSessionControlState::Stopped => Some(AgentSessionControlAction::Stop),
        AgentSessionControlState::ReadyInteractive => Some(AgentSessionControlAction::Interrupt),
        _ => None,
    }
}

pub(super) fn configure_interactive_codex_resume_command(
    command: &mut Command,
    project_root: &Path,
    session_id: &str,
) {
    command
        .arg("resume")
        .arg("--include-non-interactive")
        .arg("--sandbox")
        .arg("workspace-write")
        .arg("--ask-for-approval")
        .arg("on-request")
        .arg("-C")
        .arg(project_root)
        .arg(session_id)
        .current_dir(project_root);
}

#[cfg(unix)]
pub(super) fn set_descriptor_close_on_exec(fd: libc::c_int, close_on_exec: bool) -> io::Result<()> {
    // SAFETY: fcntl only inspects or updates descriptor flags for the supplied
    // live descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let updated = if close_on_exec {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    };
    // SAFETY: `updated` is derived from the descriptor's current flag set.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, updated) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn configure_inherited_child_control(
    command: &mut Command,
) -> Result<(i32, UnixStream)> {
    let (child_control, parent_control) =
        UnixStream::pair().context("Failed to create the interactive guardian control channel")?;
    set_descriptor_close_on_exec(child_control.as_raw_fd(), true)
        .context("Failed to protect the child end of the interactive control channel")?;
    set_descriptor_close_on_exec(parent_control.as_raw_fd(), true)
        .context("Failed to protect the parent end of the interactive control channel")?;
    let control_fd = child_control.as_raw_fd();

    // SAFETY: the closure only performs async-signal-safe fcntl calls. It owns
    // `child_control`, keeping that exact descriptor allocated through fork,
    // and clears CLOEXEC only in the child that was explicitly given its
    // numeric descriptor on the command line.
    unsafe {
        command.pre_exec(move || {
            let child_fd = child_control.as_raw_fd();
            let flags = libc::fcntl(child_fd, libc::F_GETFD);
            if flags < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::fcntl(child_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    Ok((control_fd, parent_control))
}

#[cfg(unix)]
pub(super) fn inherited_child_control_reader(control_fd: Option<i32>) -> Result<fs::File> {
    let control_fd = control_fd.context("Interactive helper did not receive its control FD")?;
    if control_fd <= libc::STDERR_FILENO {
        anyhow::bail!("Interactive helper received an invalid control FD {control_fd}");
    }
    // SAFETY: the hidden helper receives this descriptor from the parent that
    // kept it allocated across exec. Taking ownership here ensures it closes
    // when the helper finishes reading the channel.
    let control = unsafe { fs::File::from_raw_fd(control_fd) };
    set_descriptor_close_on_exec(control.as_raw_fd(), true)
        .context("Failed to contain the inherited interactive control FD")?;
    Ok(control)
}

#[cfg(unix)]
pub(super) fn run_interactive_exec_gate(
    control_fd: Option<i32>,
    program: &Path,
    arguments: &[OsString],
) -> Result<()> {
    let mut reader = inherited_child_control_reader(control_fd)?;
    if !automated_exec_gate_is_released(&mut reader)
        .context("Failed to read interactive Codex launch gate")?
    {
        return Ok(());
    }
    drop(reader);

    let mut command = Command::new(program);
    command.args(arguments);
    let error = command.exec();
    Err(error).with_context(|| {
        format!(
            "Failed to exec gated interactive Codex command {}",
            program.display()
        )
    })
}

#[cfg(not(unix))]
pub(super) fn run_interactive_exec_gate(
    _control_fd: Option<i32>,
    program: &Path,
    arguments: &[OsString],
) -> Result<()> {
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    if !automated_exec_gate_is_released(&mut reader)
        .context("Failed to read interactive Codex launch gate")?
    {
        return Ok(());
    }

    let terminal_input = interactive_terminal_input()?;
    let status = Command::new(program)
        .args(arguments)
        .stdin(Stdio::from(terminal_input))
        .status()
        .with_context(|| {
            format!(
                "Failed to start gated interactive Codex command {}",
                program.display()
            )
        })?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("Interactive Codex exited with status {status}")
    }
}

pub(super) struct InteractiveExecGateCommand {
    pub(super) command: Command,
    pub(super) launch_gate: Option<Box<dyn Write>>,
}

impl InteractiveExecGateCommand {
    pub(super) fn command_mut(&mut self) -> &mut Command {
        &mut self.command
    }

    pub(super) fn spawn(mut self) -> Result<(Child, Box<dyn Write>)> {
        let mut child = self.command.spawn()?;
        let launch_gate = match self.launch_gate.take() {
            Some(launch_gate) => launch_gate,
            None => match child.stdin.take() {
                Some(launch_gate) => Box::new(launch_gate),
                None => {
                    let _ = child.kill();
                    let _ = child.wait();
                    anyhow::bail!("Interactive Codex launch gate did not open its release pipe");
                }
            },
        };
        Ok((child, launch_gate))
    }
}

#[cfg(all(unix, not(test)))]
pub(super) fn interactive_exec_gate_command(
    target: &Command,
) -> Result<InteractiveExecGateCommand> {
    let executable = std::env::current_exe()
        .context("Failed to resolve the CLT executable for the interactive Codex launch gate")?;
    let mut gate = Command::new(executable);
    configure_automated_exec_gate_inheritance(&mut gate, target);
    gate.stdin(Stdio::inherit());
    let (control_fd, launch_gate) = configure_inherited_child_control(&mut gate)?;
    gate.arg("--local")
        .arg("agent")
        .arg("interactive-exec-gate")
        .arg("--control-fd")
        .arg(control_fd.to_string())
        .arg("--")
        .arg(target.get_program())
        .args(target.get_args());
    Ok(InteractiveExecGateCommand {
        command: gate,
        launch_gate: Some(Box::new(launch_gate)),
    })
}

#[cfg(all(unix, test))]
pub(super) fn interactive_exec_gate_command(
    target: &Command,
) -> Result<InteractiveExecGateCommand> {
    // A test binary is driven by libtest rather than the Clap entry point. Run
    // one exact helper test so launch-phase tests exercise the real FD reader.
    let executable = std::env::current_exe()
        .context("Failed to resolve the CLT interactive exec-gate test helper")?;
    let mut gate = Command::new(executable);
    configure_automated_exec_gate_inheritance(&mut gate, target);
    gate.stdin(Stdio::inherit());
    let (control_fd, launch_gate) = configure_inherited_child_control(&mut gate)?;
    gate.arg("--exact")
        .arg("tests::interactive_exec_gate_process_entry")
        .arg("--nocapture")
        .env(TEST_INTERACTIVE_EXEC_GATE_ENV, "1")
        .env(
            "CLT_TEST_INTERACTIVE_GATE_CONTROL_FD",
            control_fd.to_string(),
        )
        .env("CLT_TEST_INTERACTIVE_GATE_PROGRAM", target.get_program())
        .env(
            "CLT_TEST_INTERACTIVE_GATE_ARGUMENT_COUNT",
            target.get_args().count().to_string(),
        );
    for (index, argument) in target.get_args().enumerate() {
        gate.env(
            format!("CLT_TEST_INTERACTIVE_GATE_ARGUMENT_{index}"),
            argument,
        );
    }
    Ok(InteractiveExecGateCommand {
        command: gate,
        launch_gate: Some(Box::new(launch_gate)),
    })
}

#[cfg(not(unix))]
pub(super) fn interactive_exec_gate_command(
    target: &Command,
) -> Result<InteractiveExecGateCommand> {
    let executable = std::env::current_exe()
        .context("Failed to resolve the CLT executable for the interactive Codex launch gate")?;
    let mut gate = Command::new(executable);
    gate.arg("--local")
        .arg("agent")
        .arg("interactive-exec-gate")
        .arg("--")
        .arg(target.get_program())
        .args(target.get_args());
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
    gate.stdin(Stdio::piped());
    Ok(InteractiveExecGateCommand {
        command: gate,
        launch_gate: None,
    })
}

pub(super) struct InteractiveAgentLease {
    pub(super) state_dir: PathBuf,
    pub(super) project_id: i64,
    pub(super) holder: String,
    pub(super) released: bool,
}

pub(super) struct PendingInteractiveHandoff {
    state_dir: PathBuf,
    project_id: i64,
    session_id: String,
    holder: String,
    armed: bool,
}

impl PendingInteractiveHandoff {
    pub(super) fn new(state_dir: &Path, project_id: i64, session_id: &str, holder: &str) -> Self {
        Self {
            state_dir: state_dir.to_path_buf(),
            project_id,
            session_id: session_id.to_string(),
            holder: holder.to_string(),
            armed: true,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PendingInteractiveHandoff {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let _ = with_agent_store_at(&self.state_dir, |store| {
            let cancel_result = store.cancel_session_interrupt_handoff_blocking(
                self.project_id,
                &self.session_id,
                &self.holder,
            );
            let release_result = store.release_lease_blocking(self.project_id, &self.holder);
            cancel_result?;
            release_result.map(|_| ())
        });
    }
}

impl InteractiveAgentLease {
    pub(super) fn holder_for_current_process_with_prefix(prefix: &str) -> String {
        let generation = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = INTERACTIVE_LEASE_GENERATION.fetch_add(1, Ordering::Relaxed);
        format!("{prefix}-{}-{generation}-{sequence}", std::process::id())
    }

    pub(super) fn holder_for_current_process() -> String {
        Self::holder_for_current_process_with_prefix("clt-interactive")
    }

    pub(super) fn holder_for_idle_session() -> String {
        Self::holder_for_current_process_with_prefix("clt-idle-interactive")
    }

    pub(super) fn holder_for_stopped_session() -> String {
        Self::holder_for_current_process_with_prefix("clt-stopped-interactive")
    }

    pub(super) fn holder_for_shared_session(restore_stopped: bool) -> String {
        let prefix = if restore_stopped {
            "clt-stopped-shared-interactive"
        } else {
            "clt-shared-interactive"
        };
        Self::holder_for_current_process_with_prefix(prefix)
    }

    pub(super) fn try_acquire_idle(project_id: i64, restore_stopped: bool) -> Result<Option<Self>> {
        let state_dir = ensure_agent_state_dir()?;
        let timeout_seconds = TUI_SESSION_HANDOFF_TIMEOUT_SECONDS.max(60);
        let holder = if restore_stopped {
            Self::holder_for_stopped_session()
        } else {
            Self::holder_for_idle_session()
        };
        Self::try_acquire_with_holder_at(&state_dir, project_id, &holder, timeout_seconds)
    }

    #[cfg(test)]
    pub(super) fn try_acquire_at(
        state_dir: &Path,
        project_id: i64,
        timeout_seconds: u64,
    ) -> Result<Option<Self>> {
        let holder = Self::holder_for_current_process();
        Self::try_acquire_with_holder_at(state_dir, project_id, &holder, timeout_seconds)
    }

    pub(super) fn try_acquire_with_holder_at(
        state_dir: &Path,
        project_id: i64,
        holder: &str,
        timeout_seconds: u64,
    ) -> Result<Option<Self>> {
        ensure_agent_state_dir_at(state_dir)?;
        let acquired_at = agent_timestamp();
        let expires_at = agent_timestamp_after(timeout_seconds);
        let acquired = with_agent_store_at(state_dir, |store| {
            store.try_acquire_lease_blocking(project_id, holder, &acquired_at, &expires_at)
        })?;

        Ok(acquired.then(|| Self {
            state_dir: state_dir.to_path_buf(),
            project_id,
            holder: holder.to_string(),
            released: false,
        }))
    }

    pub(super) fn adopt_at(
        state_dir: &Path,
        project_id: i64,
        holder: &str,
    ) -> Result<Option<Self>> {
        let lease = with_agent_store_at(state_dir, |store| {
            store.lease_for_project_blocking(project_id)
        })?;
        Ok(lease.filter(|lease| lease.holder == holder).map(|_| Self {
            state_dir: state_dir.to_path_buf(),
            project_id,
            holder: holder.to_string(),
            released: false,
        }))
    }

    pub(super) fn release(mut self) -> Result<()> {
        let mut last_error = None;
        for attempt in 0..TUI_LEASE_RELEASE_ATTEMPTS {
            match with_agent_store_at(&self.state_dir, |store| {
                store.release_lease_blocking(self.project_id, &self.holder)
            }) {
                Ok(true) => {
                    self.released = true;
                    return Ok(());
                }
                Ok(false) => {
                    let lease = with_agent_store_at(&self.state_dir, |store| {
                        store.lease_for_project_blocking(self.project_id)
                    })?;
                    if lease
                        .as_ref()
                        .is_none_or(|lease| lease.holder != self.holder)
                    {
                        self.released = true;
                        return Ok(());
                    }
                    last_error = Some(anyhow::anyhow!(
                        "Interactive Codex lease is still held after its release request"
                    ));
                }
                Err(error) => last_error = Some(error),
            }
            if attempt + 1 < TUI_LEASE_RELEASE_ATTEMPTS {
                thread::sleep(Duration::from_millis(TUI_LEASE_RELEASE_RETRY_MILLIS));
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Failed to release interactive lease")))
    }
}

impl Drop for InteractiveAgentLease {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let _ = with_agent_store_at(&self.state_dir, |store| {
            store
                .release_lease_blocking(self.project_id, &self.holder)
                .map(|_| ())
        });
    }
}

pub(super) fn resume_codex_session_interactively(
    project_root: &Path,
    project_id: i64,
    session_id: &str,
    from_holder: &str,
    mode: InteractiveCodexResumeMode,
) -> Result<ExitStatus> {
    let executable = std::env::current_exe().context("Failed to resolve the CLT executable")?;
    let state_dir = ensure_agent_state_dir()?;
    let store = open_agent_store_at(&state_dir)?;
    let mut command = Command::new(&executable);
    command
        .arg("--local")
        .arg("agent")
        .arg("interactive-session-worker")
        .arg("--project-id")
        .arg(project_id.to_string())
        .arg("--session-id")
        .arg(session_id)
        .arg("--from-holder")
        .arg(from_holder)
        .current_dir(project_root);
    if mode.resumes_exec() {
        command.arg("--resume-exec");
    }
    if mode.shares_project() {
        command.arg("--shared-project");
    }
    #[cfg(unix)]
    let (control_fd, guardian_lifeline) = configure_inherited_child_control(&mut command)?;
    #[cfg(unix)]
    command.arg("--control-fd").arg(control_fd.to_string());
    #[cfg(not(unix))]
    command.stdin(Stdio::piped());
    let mut guardian = command.spawn().with_context(|| {
        format!(
            "Failed to start the interactive Codex guardian with {} in {}",
            executable.display(),
            project_root.display()
        )
    })?;
    drop(command);
    #[cfg(unix)]
    let lifeline: Box<dyn Write> = Box::new(guardian_lifeline);
    #[cfg(not(unix))]
    let Some(lifeline) = guardian.stdin.take() else {
        let _ = guardian.kill();
        let _ = guardian.wait();
        anyhow::bail!("Interactive Codex guardian did not open its lifeline");
    };
    let mut lifeline = Some(lifeline);
    let start_result = lifeline
        .as_mut()
        .expect("interactive guardian lifeline is present before launch")
        .write_all(&[1])
        .context("Failed to start the interactive Codex guardian")
        .and_then(|_| {
            lifeline
                .as_mut()
                .expect("interactive guardian lifeline is present while launching")
                .flush()
                .context("Failed to flush the interactive Codex guardian lifeline")
        });
    if let Err(error) = start_result {
        drop(lifeline);
        let _ = guardian.wait();
        return Err(error);
    }
    let guardian_pid = guardian.id();
    let status_result = loop {
        match guardian.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(error) => {
                break Err(error).context("Failed to wait for the interactive Codex guardian");
            }
        }

        if lifeline.is_some()
            && store
                .session_control_blocking(project_id, session_id)
                .is_ok_and(|control| {
                    control.is_some_and(|control| {
                        interactive_guardian_stop_requested(&control, guardian_pid)
                    })
                })
        {
            // Closing the database-free lifeline asks the child-owning guardian
            // to stop and reap its exact Codex process group. The parent TUI is
            // the only process that owns this writer, so another TUI can request
            // a safe stop without ever signaling a numeric PID itself.
            drop(lifeline.take());
        }
        thread::sleep(Duration::from_millis(100));
    };
    drop(lifeline);
    let foreground_result = restore_parent_terminal_after_interactive_guardian();
    match status_result {
        Ok(status) => {
            foreground_result?;
            Ok(status)
        }
        Err(error) => {
            let _ = guardian.wait();
            match foreground_result {
                Ok(()) => Err(error),
                Err(foreground_error) => Err(error.context(format!(
                    "restoring the parent terminal foreground also failed: {foreground_error:#}"
                ))),
            }
        }
    }
}

pub(super) fn interactive_guardian_stop_requested(
    control: &agent::AgentSessionControlRecord,
    guardian_pid: u32,
) -> bool {
    control.state == AgentSessionControlState::StopRequested
        && control.interactive_holder.as_deref().is_some_and(|holder| {
            InteractiveGuardianDisposition::guardian_process_id(holder) == Some(guardian_pid)
        })
}

pub(super) fn run_agent_session_resume_worker(project_id: i64, session_id: &str) -> Result<()> {
    if session_id.is_empty()
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        anyhow::bail!("Invalid Codex session ID for exact-session resume");
    }

    let state_dir = ensure_agent_state_dir()?;
    let store = open_agent_store_at(&state_dir)?;
    let project = store
        .list_projects_blocking()?
        .into_iter()
        .find(|project| project.id == project_id)
        .with_context(|| format!("Registered project {project_id} no longer exists"))?;
    let scan = scan_agent_project(&project.path);
    let blocked_task_count_before = scan.blocked_task_count();
    let done_task_contents_before = completed_task_contents(&project.path).unwrap_or_default();
    let blocked_task_snapshots_before = blocked_task_snapshots(&project.path).unwrap_or_default();
    let runner = CodexAgentRunner::new(state_dir.clone())?;
    let holder = agent_lease_holder();
    let lease_timeout = agent_lease_timeout()?;
    loop {
        let control = store.session_control_blocking(project_id, session_id)?;
        match control {
            Some(control) if control.state == AgentSessionControlState::ResumeRequested => {}
            Some(control) if control.state == AgentSessionControlState::Running => {
                let lease = store.lease_for_project_blocking(project_id)?;
                if lease.as_ref().is_some_and(|lease| {
                    !agent_lease_is_reclaimable(lease, false, agent_timestamp_seconds())
                }) {
                    thread::sleep(Duration::from_millis(
                        TUI_SESSION_RESUME_WORKER_RETRY_MILLIS,
                    ));
                    continue;
                }
                if !control.child_pid.is_some_and(|child_pid| {
                    automated_agent_process_group_is_running(child_pid) == Some(false)
                }) {
                    thread::sleep(Duration::from_millis(
                        TUI_SESSION_RESUME_WORKER_RETRY_MILLIS,
                    ));
                    continue;
                }
                store.recover_stale_automated_session_control_blocking(
                    project_id,
                    session_id,
                    AgentSessionControlState::Running,
                    AgentSessionControlState::ResumeRequested,
                    control.child_pid.expect("checked recorded child PID"),
                    control.run_token.as_deref(),
                )?;
                continue;
            }
            Some(_) | None => return Ok(()),
        }

        let acquired_at = agent_timestamp();
        let expires_at = agent_timestamp_after(lease_timeout.as_secs());
        if !store.try_acquire_lease_blocking(project_id, &holder, &acquired_at, &expires_at)? {
            if let Some(lease) = store.lease_for_project_blocking(project_id)?
                && agent_lease_is_reclaimable(&lease, false, agent_timestamp_seconds())
            {
                store.release_lease_blocking(project_id, &lease.holder)?;
                continue;
            }
            thread::sleep(Duration::from_millis(
                TUI_SESSION_RESUME_WORKER_RETRY_MILLIS,
            ));
            continue;
        }

        let completion = match run_agent_job(
            AgentRunJob {
                state_dir: state_dir.clone(),
                project: project.clone(),
                holder: holder.clone(),
                worker_token: None,
                max_global_jobs: agent_max_global_jobs()?,
                task_selection: AgentTaskSelection::ResumeSession,
                resume_session_id: Some(session_id.to_string()),
                blocked_task_count_before,
                done_task_contents_before: done_task_contents_before.clone(),
                blocked_task_snapshots_before: blocked_task_snapshots_before.clone(),
            },
            &runner,
            &new_agent_shutdown_signal(),
        ) {
            Ok(completion) => completion,
            Err(error) => {
                eprintln!(
                    "Exact-session resume worker could not run Codex session {session_id}: {error:#}"
                );
                thread::sleep(Duration::from_millis(
                    TUI_SESSION_RESUME_WORKER_RETRY_MILLIS,
                ));
                continue;
            }
        };
        print_agent_run_completion(&completion)?;
        if matches!(completion.status, "failure" | "timeout") {
            thread::sleep(agent_failure_backoff()?);
            continue;
        }
        return Ok(());
    }
}

pub(super) fn run_agent_interactive_session_worker(
    project_id: i64,
    session_id: &str,
    from_holder: &str,
    mode: InteractiveCodexResumeMode,
    control_fd: Option<i32>,
) -> Result<()> {
    if session_id.is_empty()
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        anyhow::bail!("Invalid Codex session ID for interactive guardian");
    }

    #[cfg(unix)]
    let mut parent_control: Box<dyn Read + Send> =
        Box::new(inherited_child_control_reader(control_fd)?);
    #[cfg(not(unix))]
    let mut parent_control: Box<dyn Read + Send> = Box::new(io::stdin());
    let mut startup_gate = [0_u8; 1];
    parent_control
        .read_exact(&mut startup_gate)
        .context("Interactive guardian parent disconnected before startup")?;
    let parent_connected = Arc::new(AtomicBool::new(true));
    let lifeline = Arc::clone(&parent_connected);
    thread::Builder::new()
        .name(format!("clt-interactive-lifeline-{project_id}"))
        .spawn(move || {
            let mut buffer = [0_u8; 1];
            loop {
                match parent_control.read(&mut buffer) {
                    Ok(0) | Err(_) => {
                        lifeline.store(false, Ordering::SeqCst);
                        break;
                    }
                    Ok(_) => {}
                }
            }
        })
        .context("Failed to start interactive guardian lifeline")?;

    let state_dir = ensure_agent_state_dir()?;
    let store = open_agent_store_at(&state_dir)?;
    let project = store
        .list_projects_blocking()?
        .into_iter()
        .find(|project| project.id == project_id)
        .with_context(|| format!("Registered project {project_id} no longer exists"))?;
    let terminal_input = interactive_terminal_input()?;
    let lease_timeout = agent_lease_timeout()?;
    let disposition = InteractiveGuardianDisposition::from_handoff(mode, from_holder);
    let guardian_holder = interactive_guardian_holder(disposition);
    if !store.adopt_interactive_guardian_blocking(
        project_id,
        Some(session_id),
        from_holder,
        &guardian_holder,
        lease_timeout.as_secs().max(60),
    )? {
        anyhow::bail!("Interactive handoff changed before its guardian could adopt it");
    }

    let interaction_result = run_guarded_interactive_codex(
        &store,
        &project,
        session_id,
        &guardian_holder,
        lease_timeout,
        terminal_input,
        &parent_connected,
    );
    let interaction_failure = match interaction_result {
        Ok(Some(status)) if !status.success() => Some(anyhow::anyhow!(
            "Interactive Codex session {session_id} exited with status {status}"
        )),
        Ok(_) => None,
        Err(error) => Some(error),
    };
    let resume_exec = finish_interactive_guardian_after_reap(
        &store,
        project_id,
        session_id,
        &guardian_holder,
        lease_timeout,
        disposition,
    )?;
    if resume_exec {
        spawn_agent_session_resume_worker(&project.path, project_id, session_id)?;
    }
    interaction_failure.map_or(Ok(()), Err)
}

pub(super) fn finish_interactive_guardian_after_reap(
    store: &agent::TursoAgentStore,
    project_id: i64,
    session_id: &str,
    guardian_holder: &str,
    lease_timeout: Duration,
    disposition: InteractiveGuardianDisposition,
) -> Result<bool> {
    let renewal_interval = agent_lease_renew_interval(lease_timeout);
    let mut last_renewal = Instant::now();
    let mut last_warning: Option<Instant> = None;

    loop {
        match store.finish_interactive_guardian_blocking(
            project_id,
            session_id,
            guardian_holder,
            disposition,
        ) {
            Ok(changed) => match store.session_control_blocking(project_id, session_id) {
                Ok(control) => {
                    if control.is_none() && disposition.holds_project_lease() {
                        match store.release_lease_blocking(project_id, guardian_holder) {
                            Ok(_) => {
                                anyhow::bail!(
                                    "Interactive Codex session {session_id} disappeared after its guarded child was reaped; CLT released the orphaned project reservation"
                                );
                            }
                            Err(error) => {
                                let should_warn = last_warning.is_none_or(|warning| {
                                    warning.elapsed() >= Duration::from_secs(5)
                                });
                                if should_warn {
                                    eprintln!(
                                        "Interactive guardian is retrying orphaned lease cleanup after its session disappeared: {error:#}"
                                    );
                                    last_warning = Some(Instant::now());
                                }
                                continue;
                            }
                        }
                    }
                    let finalized = match disposition {
                        InteractiveGuardianDisposition::ResumeExec => {
                            control.as_ref().and_then(|control| {
                                if control.interactive_holder.as_deref() == Some(guardian_holder) {
                                    return None;
                                }
                                match control.state {
                                    AgentSessionControlState::ResumeRequested
                                    | AgentSessionControlState::Running => Some(true),
                                    AgentSessionControlState::Stopped => Some(false),
                                    _ => None,
                                }
                            })
                        }
                        InteractiveGuardianDisposition::PreserveIdleSession
                        | InteractiveGuardianDisposition::PreserveSharedSession
                        | InteractiveGuardianDisposition::RestoreStopped
                        | InteractiveGuardianDisposition::RestoreStoppedShared => control
                            .is_some_and(|control| {
                                control.state == AgentSessionControlState::Stopped
                                    && control.interactive_holder.is_none()
                            })
                            .then_some(false),
                    };
                    if let Some(resume_exec) = finalized {
                        return Ok(resume_exec);
                    }
                    if changed {
                        anyhow::bail!(
                            "Interactive guardian finalized its child but left an unexpected session state"
                        );
                    }
                    anyhow::bail!(
                        "Interactive guardian state changed before its reaped child could be finalized"
                    );
                }
                Err(error) => {
                    let should_warn = last_warning
                        .is_none_or(|warning| warning.elapsed() >= Duration::from_secs(5));
                    if should_warn {
                        eprintln!(
                            "Interactive guardian is retrying its post-reap state check: {error:#}"
                        );
                        last_warning = Some(Instant::now());
                    }
                }
            },
            Err(error) => {
                let should_warn =
                    last_warning.is_none_or(|warning| warning.elapsed() >= Duration::from_secs(5));
                if should_warn {
                    eprintln!(
                        "Interactive guardian is retrying post-reap database finalization: {error:#}"
                    );
                    last_warning = Some(Instant::now());
                }
            }
        }

        if last_renewal.elapsed() >= renewal_interval {
            let expires_at = agent_timestamp_after(lease_timeout.as_secs().max(60));
            let _ = store.renew_lease_blocking(project_id, guardian_holder, &expires_at);
            last_renewal = Instant::now();
        }
        thread::sleep(Duration::from_millis(250));
    }
}

pub(super) fn interactive_guardian_holder(disposition: InteractiveGuardianDisposition) -> String {
    let generation = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = INTERACTIVE_LEASE_GENERATION.fetch_add(1, Ordering::Relaxed);
    let prefix = disposition.guardian_holder_prefix();
    format!("{prefix}-{}-{generation}-{sequence}", std::process::id())
}

pub(super) fn run_guarded_interactive_codex(
    store: &agent::TursoAgentStore,
    project: &agent::AgentProject,
    session_id: &str,
    guardian_holder: &str,
    lease_timeout: Duration,
    terminal_input: fs::File,
    parent_connected: &AtomicBool,
) -> Result<Option<ExitStatus>> {
    if !parent_connected.load(Ordering::SeqCst) {
        return Ok(None);
    }

    let disposition = InteractiveGuardianDisposition::from_guardian_holder(guardian_holder)
        .context("Interactive Codex guardian has an unrecognized holder")?;
    let mut terminal_foreground = InteractiveTerminalForeground::capture(&terminal_input)?;
    let codex_command = agent_codex_command();
    let mut target = Command::new(&codex_command);
    configure_interactive_codex_resume_command(&mut target, &project.path, session_id);
    let mut command = interactive_exec_gate_command(&target)?;
    configure_interactive_child_command(command.command_mut());
    let (mut child, mut launch_gate) = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "Failed to start the gated interactive Codex session {session_id} with {} in {}",
                    codex_command.display(),
                    project.path.display()
                )
            });
        }
    };
    let child_pid = child.id();
    let registered = store.register_interactive_guardian_child_blocking(
        project.id,
        session_id,
        guardian_holder,
        child_pid,
        lease_timeout.as_secs().max(60),
    );
    match registered {
        Ok(true) => {}
        Ok(false) => {
            drop(launch_gate);
            let _ = child.wait();
            anyhow::bail!(
                "Interactive handoff changed before gated Codex child {child_pid} could be registered"
            );
        }
        Err(error) => {
            drop(launch_gate);
            let _ = child.wait();
            return Err(error).context("Failed to register gated interactive Codex before launch");
        }
    }
    if !parent_connected.load(Ordering::SeqCst) {
        drop(launch_gate);
        let _ = child.wait();
        return Ok(None);
    }
    if let Err(foreground_error) = terminal_foreground.give_to_child(&child) {
        drop(launch_gate);
        let _ = stop_interactive_child_until_reaped(
            &mut child,
            store,
            project.id,
            guardian_holder,
            lease_timeout,
            "terminal foreground handoff failed",
        );
        restore_interactive_terminal_before_handoff(
            &mut terminal_foreground,
            store,
            project.id,
            guardian_holder,
            lease_timeout,
            parent_connected,
        );
        return Err(foreground_error);
    }
    let release_result = launch_gate
        .write_all(b"x")
        .context("Failed to release the registered interactive Codex launch gate")
        .and_then(|_| {
            launch_gate
                .flush()
                .context("Failed to flush the interactive Codex launch gate")
        });
    drop(launch_gate);
    if let Err(error) = release_result {
        let _ = stop_interactive_child_until_reaped(
            &mut child,
            store,
            project.id,
            guardian_holder,
            lease_timeout,
            "interactive launch-gate release failed",
        );
        restore_interactive_terminal_before_handoff(
            &mut terminal_foreground,
            store,
            project.id,
            guardian_holder,
            lease_timeout,
            parent_connected,
        );
        return Err(error);
    }

    let renew_interval = agent_lease_renew_interval(lease_timeout);
    let mut last_renewal = Instant::now();

    let interaction_result = loop {
        #[cfg(unix)]
        match interactive_child_exited_without_reaping(&child) {
            Ok(true) => {
                let status = stop_interactive_child_until_reaped(
                    &mut child,
                    store,
                    project.id,
                    guardian_holder,
                    lease_timeout,
                    "the interactive Codex leader exited",
                );
                break Ok(status);
            }
            Ok(false) => {}
            Err(error) => {
                eprintln!("Interactive Codex polling failed: {error:#}");
                let status = stop_interactive_child_until_reaped(
                    &mut child,
                    store,
                    project.id,
                    guardian_holder,
                    lease_timeout,
                    "polling the interactive Codex child failed",
                );
                break Ok(status);
            }
        }
        #[cfg(not(unix))]
        match child.try_wait() {
            Ok(Some(status)) => {
                break Ok(Some(status));
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!("Interactive Codex polling failed: {error}");
                let status = stop_interactive_child_until_reaped(
                    &mut child,
                    store,
                    project.id,
                    guardian_holder,
                    lease_timeout,
                    "polling the interactive Codex child failed",
                );
                break Ok(status);
            }
        }
        if !parent_connected.load(Ordering::SeqCst) {
            let status = stop_interactive_child_until_reaped(
                &mut child,
                store,
                project.id,
                guardian_holder,
                lease_timeout,
                "the CLT parent disconnected",
            );
            break Ok(status);
        }
        if disposition.holds_project_lease() && last_renewal.elapsed() >= renew_interval {
            let expires_at = agent_timestamp_after(lease_timeout.as_secs().max(60));
            match store.renew_lease_blocking(project.id, guardian_holder, &expires_at) {
                Ok(true) => last_renewal = Instant::now(),
                Ok(false) => {
                    eprintln!("Interactive guardian no longer holds its project lease");
                    let status = stop_interactive_child_until_reaped(
                        &mut child,
                        store,
                        project.id,
                        guardian_holder,
                        lease_timeout,
                        "the interactive guardian lost its project lease",
                    );
                    break Ok(status);
                }
                Err(error) => {
                    eprintln!("Failed to renew interactive guardian lease: {error:#}");
                    let status = stop_interactive_child_until_reaped(
                        &mut child,
                        store,
                        project.id,
                        guardian_holder,
                        lease_timeout,
                        "renewing the interactive guardian lease failed",
                    );
                    break Ok(status);
                }
            }
        }
        thread::sleep(Duration::from_millis(250));
    };

    restore_interactive_terminal_before_handoff(
        &mut terminal_foreground,
        store,
        project.id,
        guardian_holder,
        lease_timeout,
        parent_connected,
    );
    interaction_result
}

#[cfg(unix)]
pub(super) fn stop_interactive_child_until_reaped(
    child: &mut Child,
    store: &agent::TursoAgentStore,
    project_id: i64,
    guardian_holder: &str,
    lease_timeout: Duration,
    reason: &str,
) -> Option<ExitStatus> {
    let renewal_interval = agent_lease_renew_interval(lease_timeout);
    let mut last_renewal = Instant::now();
    let mut last_warning: Option<Instant> = None;
    let child_process_label = child.id().to_string();
    let process_group = loop {
        match i32::try_from(child.id()).context("Interactive Codex process ID exceeded pid_t") {
            Ok(process_group) => break process_group,
            Err(error) => {
                let should_warn =
                    last_warning.is_none_or(|warning| warning.elapsed() >= Duration::from_secs(5));
                if should_warn {
                    eprintln!(
                        "Interactive guardian cannot identify the owned Codex process group after {reason}: {error:#}"
                    );
                    last_warning = Some(Instant::now());
                }
            }
        }
        renew_interactive_guardian_cleanup_lease(
            store,
            project_id,
            guardian_holder,
            lease_timeout,
            &child_process_label,
            &mut last_renewal,
            renewal_interval,
        );
        thread::sleep(Duration::from_millis(250));
    };
    let mut leader_status = None;

    loop {
        if let Some(status) = leader_status {
            match agent_process_group_exists(process_group) {
                Ok(false) => return Some(status),
                Ok(true) => {}
                Err(error) => {
                    let should_warn = last_warning
                        .is_none_or(|warning| warning.elapsed() >= Duration::from_secs(5));
                    if should_warn {
                        eprintln!(
                            "Interactive guardian cannot yet prove Codex process group {process_group} exited after {reason}: {error:#}"
                        );
                        last_warning = Some(Instant::now());
                    }
                }
            }
        } else {
            match stop_interactive_child_process(child) {
                Ok(Some(status)) => return Some(status),
                Ok(None) => {}
                Err(error) => {
                    let should_warn = last_warning
                        .is_none_or(|warning| warning.elapsed() >= Duration::from_secs(5));
                    if should_warn {
                        eprintln!(
                            "Interactive guardian is retaining its lease after {reason}; the owned Codex process group is not yet proven stopped: {error:#}"
                        );
                        last_warning = Some(Instant::now());
                    }
                    match child.try_wait() {
                        Ok(Some(status)) => leader_status = Some(status),
                        Ok(None) => {}
                        Err(poll_error) => {
                            let should_warn = last_warning
                                .is_none_or(|warning| warning.elapsed() >= Duration::from_secs(5));
                            if should_warn {
                                eprintln!(
                                    "Interactive guardian could not determine whether the Codex group leader was reaped: {poll_error:#}"
                                );
                                last_warning = Some(Instant::now());
                            }
                        }
                    }
                }
            }
        }

        renew_interactive_guardian_cleanup_lease(
            store,
            project_id,
            guardian_holder,
            lease_timeout,
            &child_process_label,
            &mut last_renewal,
            renewal_interval,
        );
        thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(unix)]
pub(super) fn renew_interactive_guardian_cleanup_lease(
    store: &agent::TursoAgentStore,
    project_id: i64,
    guardian_holder: &str,
    lease_timeout: Duration,
    process_group: &str,
    last_renewal: &mut Instant,
    renewal_interval: Duration,
) {
    if InteractiveGuardianDisposition::from_guardian_holder(guardian_holder)
        .is_some_and(|disposition| !disposition.holds_project_lease())
    {
        return;
    }
    if last_renewal.elapsed() < renewal_interval {
        return;
    }
    let expires_at = agent_timestamp_after(lease_timeout.as_secs().max(60));
    match store.renew_lease_blocking(project_id, guardian_holder, &expires_at) {
        Ok(true) => {}
        Ok(false) => eprintln!(
            "Interactive guardian could not renew its lease while stopping Codex process group {process_group}"
        ),
        Err(error) => eprintln!(
            "Interactive guardian lease renewal failed while stopping Codex process group {process_group}: {error:#}"
        ),
    }
    *last_renewal = Instant::now();
}

#[cfg(not(unix))]
pub(super) fn stop_interactive_child_until_reaped(
    child: &mut Child,
    store: &agent::TursoAgentStore,
    project_id: i64,
    guardian_holder: &str,
    lease_timeout: Duration,
    reason: &str,
) -> Option<ExitStatus> {
    let holds_project_lease = InteractiveGuardianDisposition::from_guardian_holder(guardian_holder)
        .is_none_or(InteractiveGuardianDisposition::holds_project_lease);
    let renewal_interval = agent_lease_renew_interval(lease_timeout);
    let mut last_renewal = Instant::now();
    let mut last_warning: Option<Instant> = None;

    loop {
        match stop_interactive_child_process(child) {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {}
            Err(error) => {
                let should_warn =
                    last_warning.is_none_or(|warning| warning.elapsed() >= Duration::from_secs(5));
                if should_warn {
                    eprintln!(
                        "Interactive guardian is retaining its lease after {reason}; the owned Codex child is not yet proven reaped: {error:#}"
                    );
                    last_warning = Some(Instant::now());
                }
            }
        }

        if holds_project_lease && last_renewal.elapsed() >= renewal_interval {
            let expires_at = agent_timestamp_after(lease_timeout.as_secs().max(60));
            match store.renew_lease_blocking(project_id, guardian_holder, &expires_at) {
                Ok(true) => {}
                Ok(false) => eprintln!(
                    "Interactive guardian could not renew its lease while waiting to reap its owned Codex child"
                ),
                Err(error) => eprintln!(
                    "Interactive guardian lease renewal failed while waiting to reap its owned Codex child: {error:#}"
                ),
            }
            last_renewal = Instant::now();
        }

        thread::sleep(Duration::from_millis(250));
    }
}

pub(super) fn task_supports_interactive_codex_resume(status: TaskStatus, task: &TaskEntry) -> bool {
    matches!(status, TaskStatus::Done | TaskStatus::Doing)
        || status == TaskStatus::Todo && task_entry_is_blocked(task)
}

pub(super) fn codex_session_task_supports_interactive_resume(
    project_root: &Path,
    session_id: &str,
) -> Result<bool> {
    let mut matches = Vec::new();
    collect_codex_session_tasks_in_board(&get_tasks_dir(project_root), session_id, &mut matches)?;
    Ok(matches.len() == 1
        && matches
            .first()
            .is_some_and(|(status, task)| task_supports_interactive_codex_resume(*status, task)))
}

pub(super) fn collect_codex_session_tasks_in_board(
    board_dir: &Path,
    session_id: &str,
    matches: &mut Vec<(TaskStatus, TaskEntry)>,
) -> Result<()> {
    for status in TaskStatus::SESSION_SEARCH_ORDER {
        let tasks = read_task_entries(board_dir, status)?;
        for task in tasks {
            if recoverable_codex_session_id_from_task_content(&task.content) == Some(session_id) {
                matches.push((status, task.clone()));
            }
            if task.has_subtasks
                && let TaskSource::Path { path, is_dir: true } = &task.source
            {
                collect_codex_session_tasks_in_board(path, session_id, matches)?;
            }
        }
    }
    Ok(())
}

pub(super) fn codex_session_for_task(task: &TaskEntry) -> Option<String> {
    recoverable_codex_session_id_from_task_content(&task.content).map(str::to_string)
}

pub(super) fn toggle_tui_codex_session_stop(project_id: i64, session_id: &str) -> Result<String> {
    let state_dir = ensure_agent_state_dir()?;
    toggle_tui_codex_session_stop_at(&state_dir, project_id, session_id)
}

pub(super) fn toggle_tui_codex_session_stop_at(
    state_dir: &Path,
    project_id: i64,
    session_id: &str,
) -> Result<String> {
    let store = open_agent_store_at(state_dir)?;
    let Some(control) = store.session_control_blocking(project_id, session_id)? else {
        return Ok(
            "This task does not have a live or stopped Codex session to control.".to_string(),
        );
    };

    match control.state {
        AgentSessionControlState::Running => {
            let child_pid = control
                .child_pid
                .context("The Codex session is still registering its child process; try again")?;
            let run_token = control
                .run_token
                .as_deref()
                .context("The Codex session is still registering its run; try again")?;
            if store.request_session_stop_blocking(project_id, session_id, child_pid, run_token)? {
                Ok(
                    "Stopping this Codex task session; press s again once stopped to resume it."
                        .to_string(),
                )
            } else {
                Ok("The Codex session changed before it could be stopped; try again.".to_string())
            }
        }
        AgentSessionControlState::StopRequested => {
            Ok("This Codex task session is already stopping.".to_string())
        }
        AgentSessionControlState::Stopped => {
            if store.request_stopped_session_resume_blocking(
                project_id,
                session_id,
                control.run_token.as_deref(),
            )? {
                Ok("Resuming this stopped Codex task session in automated exec mode.".to_string())
            } else {
                Ok(
                    "The stopped Codex session changed before it could be resumed; try again."
                        .to_string(),
                )
            }
        }
        AgentSessionControlState::Interactive => {
            let child_pid = control
                .child_pid
                .context("The interactive Codex session is still registering; try again")?;
            let interactive_holder = control.interactive_holder.as_deref().context(
                "The interactive Codex session has no guardian identity; leave it fenced",
            )?;
            if store.request_interactive_session_stop_blocking(
                project_id,
                session_id,
                child_pid,
                interactive_holder,
            )? {
                Ok(
                    "Stopping this interactive Codex session safely; CLT will reap it and release its reservation."
                        .to_string(),
                )
            } else {
                Ok(
                    "The interactive Codex session changed before it could be stopped; try again."
                        .to_string(),
                )
            }
        }
        AgentSessionControlState::InterruptRequested
        | AgentSessionControlState::ReadyInteractive => {
            Ok("This Codex session is being used for an interactive handoff.".to_string())
        }
        AgentSessionControlState::ResumeRequested => {
            Ok("This Codex task session is already queued to resume in exec mode.".to_string())
        }
    }
}

pub(super) fn prepare_tui_codex_session_interrupt(
    project_id: i64,
    session_id: &str,
) -> Result<InteractiveAgentLease> {
    let state_dir = ensure_agent_state_dir()?;
    let lease_timeout_seconds = TUI_SESSION_HANDOFF_TIMEOUT_SECONDS.max(60);
    prepare_tui_codex_session_interrupt_at(
        &state_dir,
        project_id,
        session_id,
        lease_timeout_seconds,
        Duration::from_secs(TUI_SESSION_HANDOFF_TIMEOUT_SECONDS),
    )
}

pub(super) fn prepare_tui_codex_session_interrupt_at(
    state_dir: &Path,
    project_id: i64,
    session_id: &str,
    lease_timeout_seconds: u64,
    handoff_timeout: Duration,
) -> Result<InteractiveAgentLease> {
    let store = open_agent_store_at(state_dir)?;
    let control = store
        .session_control_blocking(project_id, session_id)?
        .with_context(|| {
            format!("Codex session {session_id} is not registered as running or stopped")
        })?;
    let interactive_holder = InteractiveAgentLease::holder_for_current_process();

    match control.state {
        AgentSessionControlState::Running => {
            let child_pid = control
                .child_pid
                .context("The Codex session is still registering its child process; try again")?;
            let run_token = control
                .run_token
                .as_deref()
                .context("The Codex session is still registering its run; try again")?;
            if !store.request_session_interrupt_blocking(
                project_id,
                session_id,
                child_pid,
                run_token,
                &interactive_holder,
            )? {
                anyhow::bail!(
                    "The Codex session changed before it could be interrupted; try again"
                );
            }
        }
        AgentSessionControlState::Stopped => {
            let lease = InteractiveAgentLease::try_acquire_with_holder_at(
                state_dir,
                project_id,
                &interactive_holder,
                lease_timeout_seconds,
            )?
            .context("The project became busy before the stopped Codex session could open")?;
            if !store.begin_stopped_session_interactive_blocking(
                project_id,
                session_id,
                &interactive_holder,
                control.run_token.as_deref(),
            )? {
                anyhow::bail!(
                    "The stopped Codex session changed before it could open interactively"
                );
            }
            return Ok(lease);
        }
        AgentSessionControlState::StopRequested => {
            anyhow::bail!("This Codex session is still stopping; try again when it is stopped")
        }
        AgentSessionControlState::InterruptRequested
        | AgentSessionControlState::ReadyInteractive
        | AgentSessionControlState::Interactive => {
            anyhow::bail!("This Codex session already has an interactive handoff in progress")
        }
        AgentSessionControlState::ResumeRequested => {
            anyhow::bail!("This Codex session is already queued to resume in exec mode")
        }
    }

    let mut pending_handoff =
        PendingInteractiveHandoff::new(state_dir, project_id, session_id, &interactive_holder);
    let started = Instant::now();
    loop {
        let control = store
            .session_control_blocking(project_id, session_id)?
            .with_context(|| format!("Codex session {session_id} disappeared during handoff"))?;
        match control.state {
            AgentSessionControlState::ReadyInteractive
                if control.interactive_holder.as_deref() == Some(interactive_holder.as_str()) =>
            {
                let lease =
                    InteractiveAgentLease::adopt_at(state_dir, project_id, &interactive_holder)?
                        .context(
                            "The Codex runner did not transfer its project lease for handoff",
                        )?;
                pending_handoff.disarm();
                return Ok(lease);
            }
            AgentSessionControlState::InterruptRequested => {}
            AgentSessionControlState::ResumeRequested => {
                anyhow::bail!("The automated runner could not complete the interactive handoff")
            }
            state => anyhow::bail!(
                "Codex session {session_id} entered unexpected state {} during handoff",
                state.database_value()
            ),
        }
        if started.elapsed() >= handoff_timeout {
            anyhow::bail!("Timed out waiting for the Codex runner to enter interactive mode");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

pub(super) fn queue_tui_codex_session_exec_resume(
    project_id: i64,
    session_id: &str,
    interactive_holder: &str,
) -> Result<()> {
    let state_dir = ensure_agent_state_dir()?;
    let store = open_agent_store_at(&state_dir)?;
    if store.cancel_session_interrupt_handoff_blocking(
        project_id,
        session_id,
        interactive_holder,
    )? {
        return Ok(());
    }
    let control = store
        .session_control_blocking(project_id, session_id)?
        .with_context(|| format!("Codex session {session_id} disappeared before exec resume"))?;
    if matches!(
        control.state,
        AgentSessionControlState::ResumeRequested | AgentSessionControlState::Running
    ) {
        Ok(())
    } else {
        anyhow::bail!(
            "Codex session {session_id} changed to {} before exec resume could be queued",
            control.state.database_value()
        )
    }
}

pub(super) fn reserve_tui_idle_codex_session_interactive(
    project_id: i64,
    session_id: &str,
    interactive_holder: &str,
    expected_stopped_run_token: Option<&str>,
) -> Result<bool> {
    let state_dir = ensure_agent_state_dir()?;
    with_agent_store_at(&state_dir, |store| {
        store.reserve_idle_session_interactive_blocking(
            project_id,
            session_id,
            interactive_holder,
            expected_stopped_run_token,
        )
    })
}

pub(super) fn reserve_tui_shared_codex_session_interactive(
    project_id: i64,
    session_id: &str,
    interactive_holder: &str,
    expected_stopped_run_token: Option<&str>,
) -> Result<bool> {
    let state_dir = ensure_agent_state_dir()?;
    with_agent_store_at(&state_dir, |store| {
        store.reserve_shared_session_interactive_blocking(
            project_id,
            session_id,
            interactive_holder,
            expected_stopped_run_token,
        )
    })
}

pub(super) fn cancel_tui_idle_codex_session_interactive(
    project_id: i64,
    session_id: &str,
    interactive_holder: &str,
) -> Result<bool> {
    let state_dir = ensure_agent_state_dir()?;
    with_agent_store_at(&state_dir, |store| {
        if store.cancel_idle_session_interactive_blocking(
            project_id,
            session_id,
            interactive_holder,
        )? {
            return Ok(true);
        }
        let control = store.session_control_blocking(project_id, session_id)?;
        Ok(match control {
            None => true,
            Some(control) => {
                control.state == AgentSessionControlState::Stopped
                    && control.interactive_holder.is_none()
            }
        })
    })
}

pub(super) fn spawn_agent_session_resume_worker(
    project_root: &Path,
    project_id: i64,
    session_id: &str,
) -> Result<PathBuf> {
    let executable = std::env::current_exe().context("Failed to resolve the CLT executable")?;
    let state_dir = ensure_agent_state_dir()?;
    let log_dir = state_dir.join("resume-workers");
    fs::create_dir_all(&log_dir)
        .with_context(|| format!("Failed to create resume-worker log directory {log_dir:?}"))?;
    let log_path = agent_session_resume_worker_log_path(&state_dir, project_id, session_id);
    let stderr_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("Failed to open resume-worker log {log_path:?}"))?;
    let stdout_file = stderr_file
        .try_clone()
        .with_context(|| format!("Failed to clone resume-worker log {log_path:?}"))?;
    let mut command = Command::new(&executable);
    command
        .arg("--local")
        .arg("agent")
        .arg("resume-session-worker")
        .arg("--project-id")
        .arg(project_id.to_string())
        .arg("--session-id")
        .arg(session_id)
        .current_dir(project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));
    configure_agent_child_command(&mut command);
    let mut child = command.spawn().with_context(|| {
        format!(
            "Failed to start CLT exact-session resume worker with {}",
            executable.display()
        )
    })?;
    thread::Builder::new()
        .name(format!("clt-resume-worker-{project_id}"))
        .spawn(move || {
            let _ = child.wait();
        })
        .context("Failed to start exact-session resume-worker reaper")?;
    Ok(log_path)
}

pub(super) fn agent_session_resume_worker_log_path(
    state_dir: &Path,
    project_id: i64,
    session_id: &str,
) -> PathBuf {
    state_dir
        .join("resume-workers")
        .join(format!("p{project_id}-{session_id}.log"))
}

pub(super) fn tui_stopped_codex_session_control(
    project_id: i64,
    session_id: &str,
) -> Result<Option<agent::AgentSessionControlRecord>> {
    let state_dir = ensure_agent_state_dir()?;
    with_agent_store_at(&state_dir, |store| {
        Ok(store
            .session_control_blocking(project_id, session_id)?
            .filter(|control| control.state == AgentSessionControlState::Stopped))
    })
}
