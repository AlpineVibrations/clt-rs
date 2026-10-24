//! Cooperative, process-lifetime ownership of staging through commit.
//! The kernel releases the lock after the command and its descendants exit.
use crate::task::{
    TaskBoard, TaskStatus, get_tasks_dir, recoverable_codex_session_id_from_task_content,
};
use anyhow::{Context, Result};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, Write},
    path::Path,
    process::Command,
};

const TOKEN_ENV: &str = "CLT_FINALIZATION_TOKEN";

pub(crate) struct Guard {
    _file: Option<File>,
    pub token: String,
}

pub(crate) fn acquire(root: &Path, owner: &str) -> Result<Guard> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--git-path", "clt-finalization.lock"])
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "Git finalization coordination needs a Git repository"
    );
    let raw = String::from_utf8(output.stdout)?;
    let path = root.join(raw.trim());
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => {
            let token = uuid::Uuid::new_v4().to_string();
            file.set_len(0)?;
            write!(file, "{owner}\n{token}\n")?;
            file.flush()?;
            Ok(Guard {
                _file: Some(file),
                token,
            })
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            let mut record = String::new();
            file.rewind()?;
            file.read_to_string(&mut record)?;
            let mut lines = record.lines();
            let held_by = lines.next().unwrap_or("another task");
            let token = lines.next().unwrap_or_default();
            if held_by == owner
                && !token.is_empty()
                && std::env::var(TOKEN_ENV).ok().as_deref() == Some(token)
            {
                Ok(Guard {
                    _file: None,
                    token: token.into(),
                })
            } else {
                anyhow::bail!(
                    "Git staging/finalization is owned by task {held_by}; retry when its finalize command exits. Independent editing may continue"
                )
            }
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn run(root: &Path, status: TaskStatus, index: usize, command: &[String]) -> Result<()> {
    let board = get_tasks_dir(root);
    let entry = TaskBoard::new(&board).entry(status, index)?;
    let owner = recoverable_codex_session_id_from_task_content(&entry.content)
        .context("Finalization requires the task's existing conversation link")?;
    if let Ok(current) = std::env::var("CODEX_THREAD_ID") {
        anyhow::ensure!(current == owner, "Finalize only this conversation's task");
    }
    let guard = acquire(root, owner)?;
    if status != TaskStatus::Done {
        super::ensure_admitted(&board, status, index)?;
    }
    let snapshot = super::Snapshot::load(&board)?;
    anyhow::ensure!(
        !snapshot
            .tasks
            .iter()
            .any(|task| task.completion_wait.is_some()
                && recoverable_codex_session_id_from_task_content(&task.entry.content)
                    != Some(owner)),
        "Another task has unfinished Git proof; let CLT finish that commit/publication before staging"
    );
    let (program, args) = command
        .split_first()
        .context("Pass the staging, sealing and commit command after --")?;
    let mut child = Command::new(program);
    child
        .args(args)
        .current_dir(root)
        .env(TOKEN_ENV, &guard.token);
    #[cfg(unix)]
    if let Some(file) = &guard._file {
        use std::{os::fd::AsRawFd, os::unix::process::CommandExt};
        let fd = file.as_raw_fd();
        // Only this command inherits ownership. Clearing CLOEXEC in the parent
        // would leak the lock to unrelated subprocesses on other threads.
        unsafe {
            child.pre_exec(move || {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let result = child.status()?;
    anyhow::ensure!(
        result.success(),
        "Finalization command exited with {result}; preserve staging and reseal before retrying a failed commit"
    );
    Ok(())
}
