use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use super::{DaemonHealthOutcome, supervise_daemon};

#[tokio::test]
async fn heartbeat_continues_while_scheduler_work_takes_longer_than_stale_timeout() {
    let completed = Arc::new(AtomicUsize::new(0));
    let counter = completed.clone();
    let outcome = supervise_daemon(
        async {
            tokio::time::sleep(Duration::from_millis(1_200)).await;
            Ok(())
        },
        move || {
            let counter = counter.clone();
            tokio::spawn(async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        },
        Duration::from_millis(30),
        Duration::from_millis(500),
    )
    .await;
    assert!(matches!(outcome, DaemonHealthOutcome::Stopped(Ok(()))));
    assert!(completed.load(Ordering::SeqCst) >= 3);
}

#[tokio::test]
async fn stuck_heartbeat_requests_restart_without_spawning_overlapping_writes() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = attempts.clone();
    let outcome = supervise_daemon(
        std::future::pending(),
        move || {
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(std::future::pending())
        },
        Duration::from_millis(10),
        Duration::from_millis(250),
    )
    .await;
    assert!(matches!(outcome, DaemonHealthOutcome::Unresponsive(_)));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn failed_heartbeats_retry_and_report_the_underlying_error() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = attempts.clone();
    let outcome = supervise_daemon(
        std::future::pending(),
        move || {
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async { anyhow::bail!("registry write blocked") })
        },
        Duration::from_millis(10),
        Duration::from_millis(250),
    )
    .await;
    let DaemonHealthOutcome::Unresponsive(error) = outcome else {
        panic!("an unresponsive registry must request a service restart");
    };
    assert!(format!("{error:#}").contains("registry write blocked"));
    assert!(attempts.load(Ordering::SeqCst) > 1);
}

#[tokio::test]
async fn transient_heartbeat_failure_does_not_restart_a_recovered_service() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = attempts.clone();
    let outcome = supervise_daemon(
        async {
            tokio::time::sleep(Duration::from_millis(700)).await;
            Ok(())
        },
        move || {
            let attempt = counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                anyhow::ensure!(attempt >= 2, "temporary registry contention");
                Ok(())
            })
        },
        Duration::from_millis(20),
        Duration::from_millis(300),
    )
    .await;
    assert!(matches!(outcome, DaemonHealthOutcome::Stopped(Ok(()))));
    assert!(attempts.load(Ordering::SeqCst) > 2);
}

#[tokio::test]
async fn daemon_exit_waits_for_in_flight_heartbeat_before_checkin_cleanup() {
    let started = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let daemon_started = started.clone();
    let heartbeat_finished = finished.clone();
    let outcome = supervise_daemon(
        async move {
            while !daemon_started.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            Ok(())
        },
        move || {
            let started = started.clone();
            let finished = heartbeat_finished.clone();
            tokio::spawn(async move {
                started.store(true, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(100)).await;
                finished.store(true, Ordering::SeqCst);
                Ok(())
            })
        },
        Duration::from_millis(10),
        Duration::from_secs(1),
    )
    .await;
    assert!(matches!(outcome, DaemonHealthOutcome::Stopped(Ok(()))));
    assert!(finished.load(Ordering::SeqCst));
}
