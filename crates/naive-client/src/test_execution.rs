//! Bounded suite execution shared by the CLI and the editor command worker.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use naive_core::command::{CommandResponse, TestTimeoutSeconds};
use serde::{Deserialize, Serialize};

use crate::test_runner::TestResult;

const MAX_RESULT_BYTES: u64 = 4 * 1024 * 1024;
const MAX_OUTPUT_BYTES: u64 = 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct WorkerRequest {
    project_root: PathBuf,
    file: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TestCaseReport {
    pub file: String,
    #[serde(flatten)]
    pub result: TestResult,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TestSuiteReport {
    pub passed: usize,
    pub failed: usize,
    pub files: usize,
    pub tests: Vec<TestCaseReport>,
    #[serde(default)]
    pub output: String,
}

/// Called only by the hidden CLI worker, never on the engine thread.
fn execute_suite(request: WorkerRequest) -> Result<TestSuiteReport, String> {
    let root = request
        .project_root
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let files = match request.file {
        Some(file) => {
            let path = crate::project_path::resolve(&root, &file)?;
            if !path.is_file() {
                return Err(format!("Test file not found: {file}"));
            }
            vec![path]
        }
        None => {
            let config = crate::project_config::load_config(&root.join("naive.yaml"))
                .map_err(|e| e.to_string())?;
            crate::project_config::discover_test_files(&config, &root)?
        }
    };
    let mut report = TestSuiteReport {
        passed: 0,
        failed: 0,
        files: files.len(),
        tests: Vec::new(),
        output: String::new(),
    };
    for file in files {
        let shown = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .display()
            .to_string();
        for result in crate::test_runner::run_test_file_quiet(&root, &file) {
            if result.passed {
                report.passed += 1;
            } else {
                report.failed += 1;
            }
            report.tests.push(TestCaseReport {
                file: shown.clone(),
                result,
            });
        }
    }
    Ok(report)
}

/// The worker writes a separate result file so Lua print output cannot corrupt
/// the protocol. Output is captured separately with a fixed size limit.
pub fn run_worker(request_path: &Path, result_path: &Path) -> Result<(), String> {
    let outcome = fs::read(request_path)
        .map_err(|e| e.to_string())
        .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.to_string()))
        .and_then(execute_suite);
    let bytes = serde_json::to_vec(&outcome).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_RESULT_BYTES {
        let error: Result<TestSuiteReport, String> =
            Err("Test result exceeds the 4 MiB response limit".into());
        return fs::write(
            result_path,
            serde_json::to_vec(&error).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string());
    }
    fs::write(result_path, bytes).map_err(|e| e.to_string())
}

struct WorkerDirectory(PathBuf);

impl WorkerDirectory {
    fn create() -> Result<Self, String> {
        use std::os::unix::fs::DirBuilderExt;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        for _ in 0..32 {
            let path = std::env::temp_dir().join(format!(
                "naive-tests-{}-{epoch}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.to_string()),
            }
        }
        Err("Could not create a private test worker directory".into())
    }
}

impl Drop for WorkerDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct WorkerChild(Child);

impl Drop for WorkerChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn run_isolated_tests(
    executable: &Path,
    project_root: &Path,
    file: Option<&str>,
    timeout: TestTimeoutSeconds,
    cancelled: &AtomicBool,
) -> Result<TestSuiteReport, String> {
    run_process(
        executable,
        WorkerRequest {
            project_root: project_root.to_path_buf(),
            file: file.map(str::to_owned),
        },
        Duration::from_secs(u64::from(timeout.get())),
        cancelled,
    )
}

fn run_process(
    executable: &Path,
    request: WorkerRequest,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> Result<TestSuiteReport, String> {
    let started = Instant::now();
    let directory = WorkerDirectory::create()?;
    let request_path = directory.0.join("request.json");
    let result_path = directory.0.join("result.json");
    let output_path = directory.0.join("stdout.log");
    let error_path = directory.0.join("stderr.log");
    fs::write(
        &request_path,
        serde_json::to_vec(&request).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let mut child = WorkerChild(
        Command::new(executable)
            .arg("__test-worker")
            .arg("--request")
            .arg(&request_path)
            .arg("--result")
            .arg(&result_path)
            .env("RUST_LOG", "error")
            .stdin(Stdio::null())
            .stdout(fs::File::create(&output_path).map_err(|e| e.to_string())?)
            .stderr(fs::File::create(&error_path).map_err(|e| e.to_string())?)
            .spawn()
            .map_err(|e| format!("Could not start test worker: {e}"))?,
    );
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err("Test execution cancelled because the engine is shutting down".into());
        }
        if started.elapsed() >= timeout {
            return Err(format!(
                "Test suite timed out after {:.3} seconds; worker terminated",
                timeout.as_secs_f64()
            ));
        }
        let output_size = fs::metadata(&output_path).map_err(|e| e.to_string())?.len()
            + fs::metadata(&error_path).map_err(|e| e.to_string())?.len();
        if output_size > MAX_OUTPUT_BYTES {
            return Err("Test worker output exceeded 1 MiB; worker terminated".into());
        }
        if let Some(status) = child.0.try_wait().map_err(|e| e.to_string())? {
            if !status.success() {
                return Err(format!("Test worker exited unsuccessfully ({status})"));
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let metadata =
        fs::metadata(&result_path).map_err(|e| format!("Test worker produced no result: {e}"))?;
    if metadata.len() > MAX_RESULT_BYTES {
        return Err("Test worker result exceeds 4 MiB".into());
    }
    if fs::metadata(&output_path).map_err(|e| e.to_string())?.len()
        + fs::metadata(&error_path).map_err(|e| e.to_string())?.len()
        > MAX_OUTPUT_BYTES
    {
        return Err("Test worker output exceeded 1 MiB".into());
    }
    let bytes = fs::read(result_path).map_err(|e| e.to_string())?;
    let outcome: Result<TestSuiteReport, String> =
        serde_json::from_slice(&bytes).map_err(|e| format!("Invalid test worker result: {e}"))?;
    let mut report = outcome?;
    if report.tests.is_empty()
        || report.files == 0
        || report.passed + report.failed != report.tests.len()
    {
        return Err("Test worker returned an incomplete test report".into());
    }
    report.output =
        String::from_utf8_lossy(&fs::read(output_path).map_err(|e| e.to_string())?).into_owned();
    report.output.push_str(&String::from_utf8_lossy(
        &fs::read(error_path).map_err(|e| e.to_string())?,
    ));
    Ok(report)
}

/// Only the supervisor thread and child process run tests. The engine keeps
/// rendering and processing commands; one admitted suite bounds resource use.
#[derive(Default)]
pub struct TestJobs {
    running: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    supervisor: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl TestJobs {
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    pub fn start(&self, project_root: PathBuf, pending: crate::command::PendingCommand) {
        let request = match pending.request.validate() {
            Ok(naive_core::command::Command::RunTests {
                file,
                timeout_seconds,
            }) => (file, timeout_seconds.unwrap_or_default()),
            _ => {
                let _ = pending
                    .responder
                    .send(CommandResponse::error("Invalid run_tests request"));
                return;
            }
        };
        if self
            .running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            let _ = pending.responder.send(CommandResponse::error(
                "A test suite is already running; retry after it finishes",
            ));
            return;
        }
        let reply = Arc::new(Mutex::new(Some(pending.responder)));
        let thread_reply = reply.clone();
        let running = self.running.clone();
        let cancelled = self.cancelled.clone();
        let worker = std::thread::Builder::new()
            .name("naive-test-supervisor".into())
            .spawn(move || {
                struct Reset(Arc<AtomicBool>);
                impl Drop for Reset {
                    fn drop(&mut self) {
                        self.0.store(false, Ordering::Release);
                    }
                }
                let _reset = Reset(running);
                let result = std::panic::catch_unwind(|| {
                    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
                    run_isolated_tests(
                        &executable,
                        &project_root,
                        request.0.as_deref(),
                        request.1,
                        &cancelled,
                    )
                });
                let response = match result {
                    Ok(Ok(report)) => match serde_json::to_value(report) {
                        Ok(value) => CommandResponse::ok(value),
                        Err(error) => CommandResponse::error(error.to_string()),
                    },
                    Ok(Err(error)) => CommandResponse::error(error),
                    Err(_) => CommandResponse::error("Test worker supervisor failed"),
                };
                drop(_reset);
                if let Some(reply) = thread_reply.lock().unwrap().take() {
                    let _ = reply.send(response);
                }
            });
        match worker {
            Ok(thread) => {
                *self.supervisor.lock().unwrap() = Some(thread);
            }
            Err(error) => {
                self.running.store(false, Ordering::Release);
                if let Some(reply) = reply.lock().unwrap().take() {
                    let _ = reply.send(CommandResponse::error(format!(
                        "Could not start test supervisor: {error}"
                    )));
                }
            }
        }
    }
}

impl Drop for TestJobs {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        if let Some(thread) = self.supervisor.get_mut().unwrap().take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hung_worker_is_killed_and_reaped_at_the_hard_deadline() {
        let directory = WorkerDirectory::create().unwrap();
        let executable = directory.0.join("hang");
        fs::write(&executable, "#!/bin/sh\nwhile :; do :; done\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let started = Instant::now();
        let result = run_process(
            &executable,
            WorkerRequest {
                project_root: directory.0.clone(),
                file: None,
            },
            Duration::from_millis(50),
            &AtomicBool::new(false),
        );
        assert!(result.unwrap_err().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn crashed_or_missing_workers_cannot_report_success() {
        let directory = WorkerDirectory::create().unwrap();
        for executable in [Path::new("/bin/false"), &directory.0.join("missing")] {
            assert!(run_process(
                executable,
                WorkerRequest {
                    project_root: directory.0.clone(),
                    file: None
                },
                Duration::from_secs(1),
                &AtomicBool::new(false)
            )
            .is_err());
        }
    }
}
