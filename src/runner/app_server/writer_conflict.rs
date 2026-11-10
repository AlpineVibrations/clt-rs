//! Best-effort evidence only; never changes Codex locks or CLT ownership.
use super::LaunchSpec;
use crate::agent::open_agent_store_at;
use serde_json::Value;

pub(super) fn diagnostic(spec: &LaunchSpec, error: &Value) -> Option<String> {
    let session = spec.resume.as_deref()?;
    if error["code"].as_i64() != Some(-32600)
        || error["message"].as_str()
            != Some(&format!("thread {session} already has an active writer"))
    {
        return None;
    }
    let registration = match open_agent_store_at(&spec.state_dir)
        .and_then(|store| store.session_control_blocking(spec.project_id, session))
    {
        Ok(Some(control)) => format!(
            "CLT registration: state={:?}, child_pid={}, run_token={}, interactive_holder={}, same_attempt={}",
            control.state,
            control
                .child_pid
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "none".into()),
            control.run_token.as_deref().unwrap_or("none"),
            control.interactive_holder.as_deref().unwrap_or("none"),
            control.run_token.as_deref() == Some(spec.run_token.as_str()),
        ),
        Ok(None) => "No CLT registration for this conversation.".into(),
        Err(error) => format!("CLT registration unavailable: {error:#}"),
    };
    Some(format!(
        "CLT writer-conflict diagnostic at {}: conversation={session}, attempting_adapter_pid={}, attempted_run={}\n{registration}\nCodex did not identify the lock owner. The registered child may be this failed attempt; registration alone does not prove lock ownership.",
        chrono::Utc::now().to_rfc3339(),
        std::process::id(),
        spec.run_token,
    ))
}
