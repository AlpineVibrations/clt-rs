use anyhow::Result;

mod agent;
mod application;
mod cli;
mod managed_git;
mod manual_sessions;
mod platform;
mod runner;
mod scheduler;
mod session_control;
mod session_recovery;
mod skills;
mod supervisor;
mod task;
#[cfg(test)]
mod test_support;
mod tui;
mod worker;

/// Runs the CLT command-line application.
pub fn run() -> Result<()> {
    cli::run()
}
