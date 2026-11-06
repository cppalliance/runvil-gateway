//! Test he supervisor through the production binary: an ordinary launch serves
//! through a `--supervised` child, ends with it, stops it when killed, and
//! hands a second launch off to it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gateway_api_discovery::{GatewayDiscoveryFile, ValidatedConnection};

use super::race_config;
use crate::boot::write_config;
use crate::support::{GatewayProcess, wait_for_connection};

const BOOT_TIMEOUT: Duration = Duration::from_secs(30);
const EXIT_TIMEOUT: Duration = Duration::from_secs(10);

fn run_dir(temp: &tempfile::TempDir) -> PathBuf {
    temp.path().join(".runvil").join("run")
}

/// Polls `condition` until it holds, failing with `what` after `timeout`.
fn wait_until(timeout: Duration, what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;

    while !condition() {
        assert!(Instant::now() < deadline, "{what} within {timeout:?}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Whether `pid` has exited. A zombie counts: it has exited and only waits
/// for whoever inherited it to reap it.
fn process_exited(pid: u32) -> bool {
    let pid = sysinfo::Pid::from_u32(pid);
    let mut system = sysinfo::System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);

    system
        .process(pid)
        .is_none_or(|process| process.status() == sysinfo::ProcessStatus::Zombie)
}

/// Launches the supervisor made, counted by the launch record each child
/// writes to stdout and the supervisor forwards.
fn launches(stdout: &str) -> usize {
    let record = format!("runvil-gateway {} starting", env!("CARGO_PKG_VERSION"));

    stdout.matches(&record).count()
}

/// Asks the gateway serving on `port` to restart.
fn request_restart(port: u16) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let response = runtime
        .block_on(
            reqwest::Client::new()
                .post(format!("http://127.0.0.1:{port}/admin/restart"))
                .bearer_auth("test-token")
                .send(),
        )
        .expect("the restart request should send");

    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
}

/// Waits for a discovery file published by a process other than `old_pid`.
fn wait_for_new_child(run_dir: &Path, old_pid: u32) -> GatewayDiscoveryFile {
    let deadline = Instant::now() + BOOT_TIMEOUT;

    loop {
        if let Ok(Some(file)) = GatewayDiscoveryFile::read(run_dir)
            && file.pid != old_pid
        {
            return file;
        }

        assert!(
            Instant::now() < deadline,
            "a new child should publish within {BOOT_TIMEOUT:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Kills `pid` from outside its supervisor.
fn kill(pid: u32) {
    let pid = sysinfo::Pid::from_u32(pid);
    let mut system = sysinfo::System::new();
    system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);

    let killed = system
        .process(pid)
        .expect("the child should be running")
        .kill();

    assert!(killed, "the child should accept the kill");
}

/// `POST /shutdown` stops the child, and the supervisor exits with the
/// child's 0.
#[test]
fn a_shutdown_request_ends_the_child_and_the_supervisor_with_success() {
    let temp = tempfile::tempdir().unwrap();
    let config = race_config(&temp);
    let mut supervisor = GatewayProcess::spawn_headless_launch(&config, temp.path(), &[]);

    let connection = wait_for_connection(&run_dir(&temp), BOOT_TIMEOUT);
    let child_pid = connection.pid;
    assert_ne!(
        child_pid,
        supervisor.id(),
        "the supervisor's child should serve, not the supervisor"
    );

    let validated_connection =
        ValidatedConnection::validate_before(connection, Instant::now() + EXIT_TIMEOUT)
            .expect("the published connection should validate");

    gateway_api_discovery::request_shutdown(&validated_connection)
        .expect("the Gateway should accept a shutdown");

    let status = supervisor.wait_for_exit(EXIT_TIMEOUT);

    assert!(
        status.success(),
        "a stopped child should end the supervisor with 0, got {status}"
    );
    assert!(
        process_exited(child_pid),
        "the child exited before its supervisor"
    );
}

/// Killing the supervisor closes the child's stdin, and the child's
/// watchdog stops it cleanly: it removes its discovery file, which a
/// killed process would leave behind.
#[test]
fn killing_the_supervisor_stops_its_child() {
    let temp = tempfile::tempdir().unwrap();
    let config = race_config(&temp);
    let mut supervisor = GatewayProcess::spawn_headless_launch(&config, temp.path(), &[]);

    let connection = wait_for_connection(&run_dir(&temp), BOOT_TIMEOUT);
    let child_pid = connection.pid;

    supervisor.stop(EXIT_TIMEOUT);

    let file = gateway_api_discovery::gateway_discovery_file_path(&run_dir(&temp));
    wait_until(
        EXIT_TIMEOUT,
        "an orphaned child should remove its discovery file",
        || !file.exists(),
    );
    wait_until(EXIT_TIMEOUT, "an orphaned child should exit", || {
        process_exited(child_pid)
    });
}

/// A second ordinary launch finds the first supervisor's lock held and its
/// child live, hands off, and exits 0, leaving the first child serving.
/// `--login` keeps the handoff from opening a browser.
#[test]
fn a_second_launch_attaches_to_the_supervised_child() {
    let temp = tempfile::tempdir().unwrap();
    let config = race_config(&temp);

    let mut first = GatewayProcess::spawn_headless_launch(&config, temp.path(), &[]);

    let connection = wait_for_connection(&run_dir(&temp), BOOT_TIMEOUT);

    let mut second = GatewayProcess::spawn_headless_launch(&config, temp.path(), &["--login"]);

    let status = second.wait_for_exit(EXIT_TIMEOUT);
    assert!(
        status.success(),
        "a second launch should hand off with 0, got {status}"
    );

    let after = GatewayDiscoveryFile::read(&run_dir(&temp))
        .expect("read the gateway discovery file should not fail")
        .expect("the first child should still publish");

    assert_eq!(
        after.pid, connection.pid,
        "the first supervisor's child should still serve"
    );
    assert!(
        first.try_wait().is_none(),
        "the first supervisor should keep running"
    );

    first.stop(EXIT_TIMEOUT);
}

/// `POST /admin/restart` replaces the child: a new pid publishes on the
/// same port, and the supervisor keeps running.
#[test]
fn a_restart_request_relaunches_the_child_on_the_same_port() {
    let temp = tempfile::tempdir().unwrap();
    let config = race_config(&temp);
    let mut supervisor = GatewayProcess::spawn_headless_launch(&config, temp.path(), &[]);

    let first = wait_for_connection(&run_dir(&temp), BOOT_TIMEOUT);

    request_restart(first.port);

    let second = wait_for_new_child(&run_dir(&temp), first.pid);

    assert_eq!(
        second.port, first.port,
        "the relaunched child should bind the port the first published"
    );
    assert!(
        process_exited(first.pid),
        "the first child should have exited"
    );
    assert!(
        supervisor.try_wait().is_none(),
        "the supervisor should keep running across a restart"
    );

    supervisor.stop(EXIT_TIMEOUT);
}

/// A config that cannot load fails the first boot, and the supervisor exits
/// with the child's code after that one launch.
#[test]
fn a_failed_first_boot_ends_the_supervisor_without_a_relaunch() {
    let temp = tempfile::tempdir().unwrap();
    let config = write_config(&temp, "not a config".to_string());
    let mut supervisor = GatewayProcess::spawn_headless_launch_capturing(&config, temp.path(), &[]);

    let status = supervisor.wait_for_exit(BOOT_TIMEOUT);

    assert_eq!(
        status.code(),
        Some(1),
        "a failed first boot should end the supervisor with the child's code, got {status}"
    );
    assert_eq!(
        launches(&supervisor.stdout()),
        1,
        "a failed first boot should not be relaunched"
    );
}

/// A serving child killed from outside is a crash, not a restart: the
/// supervisor exits non-zero after that one launch.
#[test]
fn a_child_killed_from_outside_ends_the_supervisor_without_a_relaunch() {
    let temp = tempfile::tempdir().unwrap();
    let config = race_config(&temp);
    let mut supervisor = GatewayProcess::spawn_headless_launch_capturing(&config, temp.path(), &[]);

    let connection = wait_for_connection(&run_dir(&temp), BOOT_TIMEOUT);

    kill(connection.pid);

    let status = supervisor.wait_for_exit(EXIT_TIMEOUT);

    assert!(
        !status.success(),
        "a killed child should end the supervisor with a failure, got {status}"
    );
    assert_eq!(
        launches(&supervisor.stdout()),
        1,
        "a killed child should not be relaunched"
    );
}

/// A restart whose every launch fails (the config is broken and no `.prev`
/// exists to roll back to) stops after five launches and ends the
/// supervisor with the last child's code.
#[test]
fn a_restart_whose_launches_all_fail_stops_after_five() {
    let temp = tempfile::tempdir().unwrap();
    let config = race_config(&temp);
    let mut supervisor = GatewayProcess::spawn_headless_launch_capturing(&config, temp.path(), &[]);

    let connection = wait_for_connection(&run_dir(&temp), BOOT_TIMEOUT);

    std::fs::write(&config, "not a config").expect("breaking the config should succeed");
    request_restart(connection.port);

    // Five failed launches with backoffs of 250 ms to 2 s in between.
    let status = supervisor.wait_for_exit(BOOT_TIMEOUT);

    assert_eq!(
        status.code(),
        Some(1),
        "an exhausted restart should end the supervisor with the last child's code, got {status}"
    );
    assert_eq!(
        launches(&supervisor.stdout()),
        6,
        "the first boot and five restart launches"
    );
}
