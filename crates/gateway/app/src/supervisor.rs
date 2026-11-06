//! The supervisor is the process an ordinary launch runs. It runs the serving
//! gateway as its `--supervised` child, copying the child's output to its own
//! and exiting with the child's code. It holds the launch lock for its whole
//! lifetime.
//!
//! The supervisor does not start the logging system, the child gateway process
//! owns the log file.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, ExitCode, ExitStatus, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gateway::{AppOptions, RESTART_REQUESTED};
use gateway_api_discovery::{GatewayDiscoveryFile, LaunchLock};

/// How long a launched child has to publish its discovery file.
const PUBLICATION_TIMEOUT: Duration = Duration::from_secs(30);

/// How often a launching child is checked for publication or exit.
const PUBLICATION_POLL: Duration = Duration::from_millis(25);

/// The launches one restart may make before the supervisor gives up.
const MAX_RESTART_LAUNCHES: u32 = 5;

/// The wait before a restart's second launch; it doubles for each launch after.
const BASE_BACKOFF_TIMEOUT: Duration = Duration::from_millis(250);

/// Supervisor entry point: starts and manages the child gateway process,
/// returning the process exit code when it ends. Holds the launch lock while
/// the gateway process is running, so another launch attempt attaches to the
/// live child instead of launching a new gateway.
pub(crate) fn run(options: &AppOptions, _launch_lock: LaunchLock) -> ExitCode {
    let Some(run_dir) = options.serve.resolved_run_dir() else {
        eprintln!("error: no user profile directory for the gateway's run directory");
        return ExitCode::FAILURE;
    };

    let mut browser = options.serve.browser;

    // The port the last child published. Every relaunch asks for it, so a
    // restart keeps the Settings URL; the first child binds as configured.
    let mut preferred_port = None;

    // Launches made for the pending restart: 0 on the first boot and while
    // a child serves.
    let mut restart_launch_count = 0;

    loop {
        let child_options = {
            let mut child_options = options.clone();
            child_options.serve = child_options
                .serve
                .with_browser(browser)
                .with_preferred_port(preferred_port);
            child_options
        };

        let mut child = match SupervisedChild::spawn(&child_options, &run_dir) {
            Ok(child) => child,
            Err(error) => {
                eprintln!("error: could not start the gateway: {error}");
                return ExitCode::FAILURE;
            }
        };

        // Only the first launch opens the browser (or not), subsequent restarts never should.
        browser = false;

        let publication_result = match child.await_publication() {
            Ok(Launch::Published(port)) => {
                preferred_port = Some(port);

                match child.wait_for_exit() {
                    Ok(status) if status.code() == Some(i32::from(RESTART_REQUESTED)) => {
                        restart_launch_count = 1;
                        continue;
                    }

                    Ok(status) => return exit_code(status),

                    Err(error) => {
                        eprintln!("error: could not wait for the gateway: {error}");
                        return ExitCode::FAILURE;
                    }
                }
            }

            // A child that exits 0 before publishing handed off to a Gateway
            // it found serving; the supervisor ends with it.
            Ok(Launch::Exited(status)) if status.success() => return ExitCode::SUCCESS,

            Ok(Launch::Exited(status)) => exit_code(status),

            Ok(Launch::TimedOut) => {
                eprintln!(
                    "error: the gateway published no connection within {PUBLICATION_TIMEOUT:?}; stopping it"
                );
                child.kill();
                ExitCode::FAILURE
            }

            Err(error) => {
                eprintln!("error: could not watch the gateway start: {error}");
                child.kill();
                ExitCode::FAILURE
            }
        };

        // A failed first boot is never retried. A failed restart launch is,
        // until the restart has used all its launches.
        if restart_launch_count == 0 || restart_launch_count >= MAX_RESTART_LAUNCHES {
            return publication_result;
        }

        std::thread::sleep(backoff(restart_launch_count));
        restart_launch_count += 1;
    }
}

/// The wait after a restart's `failed`-th launch fails: 250 ms, then
/// doubling.
fn backoff(failed: u32) -> Duration {
    BASE_BACKOFF_TIMEOUT * 2u32.pow(failed - 1)
}

/// What a launched child did first.
enum Launch {
    /// It published its discovery file on this port and is serving.
    Published(u16),

    /// It exited before publishing.
    Exited(ExitStatus),

    /// It published nothing within the timeout and is still running.
    TimedOut,
}

/// A running child Gateway, with its held-open stdin and the threads
/// forwarding its output.
struct SupervisedChild<'a> {
    process: Child,

    #[expect(
        dead_code,
        reason = "Needs to be held for the child's lifetime or the child's watchdog will kill it. We never write to it, so its unused."
    )]
    stdin: Option<ChildStdin>,

    forwarders: Vec<JoinHandle<()>>,

    run_dir: &'a PathBuf,
}

impl<'a> SupervisedChild<'a> {
    fn spawn(options: &AppOptions, run_dir: &'a PathBuf) -> std::io::Result<Self> {
        let mut process = build_child_command(options).spawn()?;

        let stdin = process.stdin.take();

        let forwarders = [
            process
                .stdout
                .take()
                .map(|pipe| OutputStream::Stdout.forward(pipe)),
            process
                .stderr
                .take()
                .map(|pipe| OutputStream::Stderr.forward(pipe)),
        ]
        .into_iter()
        .flatten()
        .collect();

        Ok(Self {
            process,
            stdin,
            forwarders,
            run_dir,
        })
    }

    /// Waits until the child publishes its discovery file, exits, or runs
    /// out of time.
    fn await_publication(&mut self) -> std::io::Result<Launch> {
        let deadline = Instant::now() + PUBLICATION_TIMEOUT;

        loop {
            // Checks if the process has already exited
            if self.process.try_wait()?.is_some() {
                return self.wait_for_exit().map(Launch::Exited);
            }

            // The pid tells this child's record from one the last child left
            // behind.
            if let Ok(Some(file)) = GatewayDiscoveryFile::read(self.run_dir)
                && file.pid == self.process.id()
            {
                return Ok(Launch::Published(file.port));
            }

            if Instant::now() >= deadline {
                return Ok(Launch::TimedOut);
            }

            std::thread::sleep(PUBLICATION_POLL);
        }
    }

    /// Waits for the child to exit and for its last output to be forwarded.
    fn wait_for_exit(&mut self) -> std::io::Result<ExitStatus> {
        let status = self.process.wait();

        // The pipes close when the child exits; joining flushes its last lines.
        for forwarder in self.forwarders.drain(..) {
            let _ = forwarder.join();
        }

        status
    }

    /// Stops the child and waits for it.
    fn kill(&mut self) {
        let _ = self.process.kill();
        let _ = self.wait_for_exit();
    }
}

/// Builds the command to launch the child Gateway: this binary with `--supervised`
/// flag set, passing currently set app options on to the child.
fn build_child_command(options: &AppOptions) -> Command {
    // A missing executable path makes `spawn` fail with a clear error.
    let exe = std::env::current_exe().unwrap_or_else(|_| "runvil-gateway".into());
    let mut command = Command::new(exe);

    command.arg("--supervised");

    if !options.tray {
        command.arg("--no-tray");
    }

    // Already resolved against RUNVIL_GATEWAY_CONFIG, so the child needs no
    // second lookup.
    if let Some(config) = &options.serve.config_path {
        command.arg("--config").arg(config);
    }

    if let Some(profile) = &options.serve.profile {
        command.arg("--profile").arg(profile.as_str());
    }

    if options.serve.browser {
        command.arg("--browser");
    }

    if let Some(port) = options.serve.preferred_port {
        command.arg("--preferred-port").arg(port.to_string());
    }

    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    command
}

/// Control forwarding of the child's output to the supervisor
#[derive(Clone, Copy)]
enum OutputStream {
    Stdout,
    Stderr,
}

impl OutputStream {
    /// Copies every line from one of the child's pipes to the matching stream
    /// of this process, as bytes, so output that is not UTF-8 passes through.
    fn forward(self, pipe: impl Read + Send + 'static) -> JoinHandle<()> {
        std::thread::spawn(move || {
            for line in BufReader::new(pipe).split(b'\n') {
                let Ok(mut line) = line else { break };
                line.push(b'\n');

                let _ = match self {
                    OutputStream::Stdout => std::io::stdout().write_all(&line),
                    OutputStream::Stderr => std::io::stderr().write_all(&line),
                };
            }
        })
    }
}

/// Builds the supervisor's exit code from the child's `ExitStatus`:
/// the child processes i16 `ExitStatus::code` is translated to a u8 `ExitCode`,
/// or `ExitCode::FAILURE` for a code outside the range of u8.
fn exit_code(status: ExitStatus) -> ExitCode {
    status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .map_or(ExitCode::FAILURE, ExitCode::from)
}

#[cfg(test)]
#[path = "supervisor-tests.rs"]
mod tests;
