use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak, mpsc};
use std::thread;
use std::time::{Duration, Instant};

const QUEUE_CAPACITY: usize = 8;
const MAX_REQUEST: usize = 128 * 1024;
const MAX_RESPONSE: usize = 4 * 1024 * 1024;
type Done = Box<dyn FnOnce(Result<Value, String>) + Send>;

#[derive(Clone)]
pub struct Worker {
    inner: Arc<Inner>,
}

struct Inner {
    sender: Mutex<Option<mpsc::SyncSender<Task>>>,
    shared: Arc<Mutex<State>>,
}

struct State {
    generation: u64,
    shutdown: bool,
    child: Option<Child>,
    #[cfg(windows)]
    job: Option<Job>,
}

struct Task {
    request: Value,
    generation: u64,
    done: Done,
}

struct Transport {
    requests: mpsc::SyncSender<Vec<u8>>,
    responses: mpsc::Receiver<Result<Vec<u8>, String>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

fn finish(done: Done, result: Result<Value, String>) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| done(result)));
}

impl Default for Worker {
    fn default() -> Self {
        Self::new()
    }
}

impl Worker {
    pub fn new() -> Self {
        let shared = Arc::new(Mutex::new(State {
            generation: 0,
            shutdown: false,
            child: None,
            #[cfg(windows)]
            job: None,
        }));
        register_exit(&shared);
        Self {
            inner: Arc::new(Inner {
                sender: Mutex::new(None),
                shared,
            }),
        }
    }

    pub fn request(
        &self,
        request: Value,
        done: impl FnOnce(Result<Value, String>) + Send + 'static,
    ) -> Result<(), String> {
        validate_request(&request)?;
        let mut sender = lock(&self.inner.sender);
        if sender.is_none() {
            let (tx, rx) = mpsc::sync_channel(QUEUE_CAPACITY);
            let shared = self.inner.shared.clone();
            thread::Builder::new()
                .name("translation-supervisor".into())
                .spawn(move || supervise(rx, shared))
                .map_err(|_| "worker_unavailable".to_string())?;
            *sender = Some(tx);
        }
        let mut state = lock(&self.inner.shared);
        let control = matches!(request["op"].as_str(), Some("stop" | "restart"));
        let generation = state.generation.wrapping_add(u64::from(control));
        let task = Task {
            request,
            generation,
            done: Box::new(done),
        };
        sender
            .as_ref()
            .unwrap()
            .try_send(task)
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => "worker_busy".to_string(),
                mpsc::TrySendError::Disconnected(_) => "worker_unavailable".to_string(),
            })?;
        if control {
            state.generation = generation;
            if let Some(child) = state.child.as_mut() {
                let _ = child.kill();
            }
        }
        Ok(())
    }

    pub fn status(&self) -> Value {
        let mut state = lock(&self.inner.shared);
        let pid = state
            .child
            .as_mut()
            .and_then(|child| match child.try_wait() {
                Ok(None) => Some(child.id()),
                _ => None,
            });
        json!({ "running": pid.is_some(), "pid": pid })
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let mut state = lock(&self.shared);
        state.shutdown = true;
        state.generation = state.generation.wrapping_add(1);
        reap(&mut state);
        lock(&self.sender).take();
    }
}

fn reap(state: &mut State) {
    if let Some(mut child) = state.child.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    #[cfg(windows)]
    {
        state.job.take();
    }
}

fn supervise(receiver: mpsc::Receiver<Task>, shared: Arc<Mutex<State>>) {
    let mut transport = None;
    let mut id = 0u64;
    while let Ok(task) = receiver.recv() {
        let mut healthy_response = false;
        let result = (|| {
            {
                let state = lock(&shared);
                if state.shutdown
                    || UPDATING.load(std::sync::atomic::Ordering::SeqCst)
                    || state.generation != task.generation
                {
                    return Err("request_cancelled".into());
                }
            }
            let op = task.request["op"].as_str().unwrap_or_default();
            if matches!(op, "stop" | "restart") {
                transport = None;
                reap(&mut lock(&shared));
                if op == "stop" {
                    return Ok(json!({ "running": false, "pid": null }));
                }
            }
            {
                let mut state = lock(&shared);
                if state.shutdown
                    || UPDATING.load(std::sync::atomic::Ordering::SeqCst)
                    || state.generation != task.generation
                {
                    return Err("request_cancelled".into());
                }
                let alive = state
                    .child
                    .as_mut()
                    .is_some_and(|child| matches!(child.try_wait(), Ok(None)));
                if !alive {
                    transport = None;
                    reap(&mut state);
                    transport = Some(spawn(&mut state)?);
                }
            }
            let io = transport.as_ref().ok_or("worker_unavailable")?;
            let mut call = |request: Value| {
                exchange(
                    io,
                    &shared,
                    task.generation,
                    &mut id,
                    &mut healthy_response,
                    request,
                )
            };
            if matches!(op, "refresh" | "install") {
                downloads::run(&task.request, &mut call, &|| {
                    cancelled(&shared, task.generation)
                })
            } else {
                let mut request = task.request.clone();
                if op == "restart" {
                    request["op"] = json!("status");
                }
                call(request)
            }
        })();
        if result.is_err() && !healthy_response {
            transport = None;
            reap(&mut lock(&shared));
        }
        finish(task.done, result);
    }
    reap(&mut lock(&shared));
}

fn cancelled(shared: &Mutex<State>, generation: u64) -> Result<(), String> {
    let state = lock(shared);
    if state.shutdown
        || state.generation != generation
        || UPDATING.load(std::sync::atomic::Ordering::SeqCst)
    {
        Err("request_cancelled".into())
    } else {
        Ok(())
    }
}

fn exchange(
    io: &Transport,
    shared: &Mutex<State>,
    generation: u64,
    id: &mut u64,
    healthy: &mut bool,
    mut request: Value,
) -> Result<Value, String> {
    cancelled(shared, generation)?;
    *id = id.wrapping_add(1);
    request["id"] = json!(*id);
    let mut bytes = serde_json::to_vec(&request).map_err(|_| "invalid_request")?;
    bytes.push(b'\n');
    if bytes.len() > MAX_REQUEST {
        return Err("request_too_large".into());
    }
    *healthy = false;
    io.requests.try_send(bytes).map_err(|_| "runtime_failed")?;
    let deadline = Instant::now() + request_timeout(request["op"].as_str().unwrap_or_default());
    loop {
        cancelled(shared, generation)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("worker_timeout".into());
        }
        match io
            .responses
            .recv_timeout(remaining.min(Duration::from_millis(25)))
        {
            Ok(response) => {
                let parsed = parse_response(&response?, *id)?;
                *healthy = true;
                return parsed;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return Err("runtime_failed".into()),
        }
    }
}

fn request_timeout(op: &str) -> Duration {
    Duration::from_secs(match op {
        "install" | "install_local" => 600,
        "refresh" | "import_catalog" => 240,
        "translate" => 180,
        _ => 15,
    })
}

fn validate_request(request: &Value) -> Result<(), String> {
    if !request.is_object() {
        return Err("invalid_request".into());
    }
    if !matches!(
        request["op"].as_str(),
        Some("status" | "refresh" | "install" | "translate" | "stop" | "restart")
    ) {
        return Err("invalid_operation".into());
    }
    if serde_json::to_vec(request)
        .map_err(|_| "invalid_request")?
        .len()
        > MAX_REQUEST - 64
    {
        return Err("request_too_large".into());
    }
    Ok(())
}

fn executable_path(executable: &Path) -> Result<PathBuf, String> {
    let parent = executable
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or("worker_unavailable")?;
    Ok(parent.join("translation").join(if cfg!(windows) {
        "zapive-translate.exe"
    } else {
        "zapive-translate"
    }))
}

fn resolve_executable(executable: &Path) -> PathBuf {
    resolve_executable_with(executable, cfg!(debug_assertions), |path| path.is_file())
}

fn resolve_executable_with(
    executable: &Path,
    development: bool,
    exists: impl Fn(&Path) -> bool,
) -> PathBuf {
    let installed = executable_path(executable).unwrap_or_default();
    if exists(&installed) || !development {
        return installed;
    }
    if let Some(profile) = executable
        .parent()
        .filter(|p| p.file_name().is_some_and(|name| name == "debug"))
    {
        if let Some(target) = profile.parent() {
            let release = executable_path(
                &target
                    .join("release")
                    .join(executable.file_name().unwrap_or_default()),
            )
            .unwrap_or_default();
            if exists(&release) {
                return release;
            }
        }
    }
    installed
}

fn absolute_models(data: &Path, cwd: &Path) -> PathBuf {
    let path = data.join("translation").join("models");
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

fn spawn(state: &mut State) -> Result<Transport, String> {
    if UPDATING.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("request_cancelled".into());
    }
    let executable = std::env::current_exe().map_err(|_| "worker_unavailable")?;
    let cwd = std::env::current_dir().map_err(|_| "models_directory_required")?;
    let models = absolute_models(&crate::paths::data_dir(), &cwd);
    std::fs::create_dir_all(&models).map_err(|_| "models_directory_required")?;
    let worker = resolve_executable(&executable);
    log::info!("[translation] starting helper {}", worker.display());
    if !worker.is_file() {
        log::error!(
            "[translation] helper executable not found: {}",
            worker.display()
        );
        return Err("worker_unavailable".into());
    }
    let mut command = Command::new(worker);
    command
        .env("ARGOS_PACKAGES_DIR", models)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let child = command.spawn().map_err(|error| {
        log::error!("[translation] failed to start helper: {error}");
        "worker_unavailable"
    })?;
    state.child = Some(child);
    #[cfg(windows)]
    {
        state.job = Some(Job::attach(state.child.as_ref().unwrap())?);
    }
    let child = state.child.as_mut().unwrap();
    let mut stdin = child.stdin.take().ok_or("runtime_failed")?;
    let stdout = child.stdout.take().ok_or("runtime_failed")?;
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(1);
    let (results, responses) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("translation-io".into())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(bytes) = rx.recv() {
                let response = stdin
                    .write_all(&bytes)
                    .and_then(|_| stdin.flush())
                    .map_err(|_| "runtime_failed".to_string())
                    .and_then(|_| read_response(&mut reader));
                let failed = response.is_err();
                if results.send(response).is_err() || failed {
                    break;
                }
            }
        })
        .map_err(|_| "worker_unavailable")?;
    Ok(Transport {
        requests: tx,
        responses,
    })
}

fn read_response(reader: &mut impl BufRead) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_RESPONSE + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .map_err(|_| "runtime_failed")?;
    if bytes.len() > MAX_RESPONSE {
        return Err("response_too_large".into());
    }
    if bytes.last() != Some(&b'\n') {
        return Err("invalid_response".into());
    }
    Ok(bytes)
}

// Outer errors invalidate the protocol; inner errors are valid application replies.
fn parse_response(bytes: &[u8], id: u64) -> Result<Result<Value, String>, String> {
    let response: Value = serde_json::from_slice(bytes).map_err(|_| "invalid_response")?;
    if response["id"].as_u64() != Some(id) || !response["result"].is_object() {
        return Err("invalid_response".into());
    }
    match response["ok"].as_bool() {
        Some(true) if response["error"].as_str() == Some("") => Ok(Ok(response["result"].clone())),
        Some(false)
            if response["error"]
                .as_str()
                .is_some_and(|error| !error.is_empty()) =>
        {
            let error = sanitize_error(response["error"].as_str().unwrap());
            if error == "runtime_failed" {
                Err(error.into())
            } else {
                Ok(Err(error.into()))
            }
        }
        _ => Err("invalid_response".into()),
    }
}

fn sanitize_error(error: &str) -> &'static str {
    match error {
        "worker_outdated" => "worker_outdated",
        "insecure_download" => "insecure_download",
        "download_too_large" => "download_too_large",
        "models_directory_required" => "models_directory_required",
        "invalid_catalog" => "invalid_catalog",
        "pair_unavailable" => "pair_unavailable",
        "model_download_failed" => "model_download_failed",
        "network_tls_certificate" => "network_tls_certificate",
        "network_tls" => "network_tls",
        "network_timeout" => "network_timeout",
        "network_dns" => "network_dns",
        "network_access_denied" => "network_access_denied",
        "network_http_unauthorized" => "network_http_unauthorized",
        "network_http_forbidden" => "network_http_forbidden",
        "network_refused" => "network_refused",
        "network_interrupted" => "network_interrupted",
        "network_unreachable" => "network_unreachable",
        "network_rate_limited" => "network_rate_limited",
        "network_server" => "network_server",
        "network_proxy_auth" => "network_proxy_auth",
        "network_not_found" => "network_not_found",
        "network_http" => "network_http",
        "network_failed" => "network_failed",
        "invalid_model_archive" => "invalid_model_archive",
        "invalid_path" => "invalid_path",
        "source_required" => "source_required",
        "model_not_installed" => "model_not_installed",
        "invalid_request" => "invalid_request",
        "invalid_id" => "invalid_id",
        "invalid_operation" => "invalid_operation",
        "request_too_large" => "request_too_large",
        "invalid_language" => "invalid_language",
        "empty_text" => "empty_text",
        "invalid_json" => "invalid_json",
        _ => "runtime_failed",
    }
}

static WORKERS: OnceLock<Mutex<Vec<Weak<Mutex<State>>>>> = OnceLock::new();
static UPDATING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Resumes translation on failed installation (including early returns/unwinding).
pub struct UpdateGuard {
    committed: bool,
}

impl UpdateGuard {
    pub fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for UpdateGuard {
    fn drop(&mut self) {
        if !self.committed {
            UPDATING.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

/// Stop helpers until the guard is dropped, or until restart after commit.
pub fn shutdown_for_update() -> Result<UpdateGuard, String> {
    UPDATING
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        )
        .map_err(|_| "an update is already in progress")?;
    let guard = UpdateGuard { committed: false };
    if let Some(workers) = WORKERS.get() {
        for worker in lock(workers).iter().filter_map(Weak::upgrade) {
            let mut state = lock(&worker);
            state.generation = state.generation.wrapping_add(1);
            if let Some(child) = state.child.as_mut() {
                if child.try_wait().map_err(|e| e.to_string())?.is_none() {
                    child
                        .kill()
                        .map_err(|e| format!("cannot stop translation helper: {e}"))?;
                }
                child
                    .wait()
                    .map_err(|e| format!("cannot wait for translation helper: {e}"))?;
            }
            reap(&mut state);
        }
    }
    Ok(guard)
}

fn register_exit(shared: &Arc<Mutex<State>>) {
    let workers = WORKERS.get_or_init(|| {
        unsafe extern "C" {
            fn atexit(callback: extern "C" fn()) -> i32;
        }
        unsafe {
            atexit(cleanup_at_exit);
        }
        Mutex::new(Vec::new())
    });
    let mut workers = lock(workers);
    workers.retain(|worker| worker.strong_count() > 0);
    workers.push(Arc::downgrade(shared));
}

extern "C" fn cleanup_at_exit() {
    if let Some(workers) = WORKERS.get() {
        for worker in lock(workers).iter().filter_map(Weak::upgrade) {
            let mut state = lock(&worker);
            state.shutdown = true;
            reap(&mut state);
        }
    }
}

#[cfg(windows)]
struct Job(*mut std::ffi::c_void);
#[cfg(windows)]
unsafe impl Send for Job {}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateJobObjectW(
        attributes: *const std::ffi::c_void,
        name: *const u16,
    ) -> *mut std::ffi::c_void;
    fn SetInformationJobObject(
        job: *mut std::ffi::c_void,
        class: i32,
        info: *const std::ffi::c_void,
        size: u32,
    ) -> i32;
    fn AssignProcessToJobObject(job: *mut std::ffi::c_void, process: *mut std::ffi::c_void) -> i32;
    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
}

#[cfg(windows)]
impl Job {
    fn attach(child: &Child) -> Result<Self, String> {
        use std::os::windows::io::AsRawHandle;
        #[repr(C)]
        #[derive(Default)]
        struct Basic {
            process_time: i64,
            job_time: i64,
            flags: u32,
            minimum: usize,
            maximum: usize,
            processes: u32,
            affinity: usize,
            priority: u32,
            scheduling: u32,
        }
        #[repr(C)]
        #[derive(Default)]
        struct Extended {
            basic: Basic,
            io: [u64; 6],
            process_memory: usize,
            job_memory: usize,
            peak_process_memory: usize,
            peak_job_memory: usize,
        }
        let mut limits = Extended::default();
        limits.basic.flags = 0x2000;
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err("worker_unavailable".into());
            }
            let job = Self(handle);
            if SetInformationJobObject(
                handle,
                9,
                (&limits as *const Extended).cast(),
                std::mem::size_of::<Extended>() as u32,
            ) == 0
                || AssignProcessToJobObject(handle, child.as_raw_handle()) == 0
            {
                return Err("worker_unavailable".into());
            }
            Ok(job)
        }
    }
}

#[cfg(windows)]
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parses_only_matching_result_objects() {
        let response = br#"{"id":7,"ok":true,"error":"","result":{"text":"hello"}}"#;
        assert_eq!(
            parse_response(response, 7).unwrap().unwrap(),
            json!({"text":"hello"})
        );
        assert_eq!(parse_response(response, 8).unwrap_err(), "invalid_response");
        assert!(parse_response(br#"{"id":7,"ok":true,"error":"","result":[]}"#, 7).is_err());
        assert!(parse_response(b"not json", 7).is_err());
    }

    #[test]
    fn errors_never_echo_worker_content() {
        let response = json!({"id":1,"ok":false,"error":"private text /secret/path","result":{}});
        assert_eq!(
            parse_response(&serde_json::to_vec(&response).unwrap(), 1).unwrap_err(),
            "runtime_failed"
        );
        assert_eq!(sanitize_error("model_not_installed"), "model_not_installed");
    }

    #[test]
    fn domain_errors_are_valid_replies_but_protocol_errors_are_not() {
        for error in ["source_required", "model_not_installed", "invalid_language"] {
            let response = json!({"id":7,"ok":false,"error":error,"result":{}});
            assert_eq!(
                parse_response(&serde_json::to_vec(&response).unwrap(), 7).unwrap(),
                Err(error.into())
            );
        }
        for response in [
            json!({"id":8,"ok":false,"error":"source_required","result":{}}),
            json!({"id":7,"ok":false,"result":{}}),
            json!({"id":7,"ok":false,"error":"","result":{}}),
            json!({"id":7,"ok":false,"error":42,"result":{}}),
        ] {
            assert_eq!(
                parse_response(&serde_json::to_vec(&response).unwrap(), 7).unwrap_err(),
                "invalid_response"
            );
        }
    }

    #[test]
    fn failed_update_guard_resumes_existing_and_new_workers() {
        let existing = Worker::new();
        let generation = lock(&existing.inner.shared).generation;
        {
            let _guard = shutdown_for_update().unwrap();
            assert!(UPDATING.load(std::sync::atomic::Ordering::SeqCst));
            assert!(shutdown_for_update().is_err());
            assert_ne!(lock(&existing.inner.shared).generation, generation);
            assert!(!lock(&existing.inner.shared).shutdown);
            let created_during_update = Worker::new();
            assert!(!lock(&created_during_update.inner.shared).shutdown);
        }
        assert!(!UPDATING.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!lock(&existing.inner.shared).shutdown);
    }

    #[test]
    fn bounded_complete_lines_only() {
        assert_eq!(read_response(&mut Cursor::new(b"{}\n")).unwrap(), b"{}\n");
        assert!(read_response(&mut Cursor::new(b"{}")).is_err());
        assert!(read_response(&mut Cursor::new(Vec::<u8>::new())).is_err());
        assert_eq!(
            read_response(&mut Cursor::new(vec![b'x'; MAX_RESPONSE + 2])).unwrap_err(),
            "response_too_large"
        );
    }

    #[test]
    fn network_codes_survive_without_restarting_helper() {
        for code in [
            "network_tls_certificate",
            "network_tls",
            "network_timeout",
            "network_dns",
            "network_access_denied",
            "network_http_unauthorized",
            "network_http_forbidden",
            "network_refused",
            "network_interrupted",
            "network_unreachable",
            "network_rate_limited",
            "network_server",
            "network_proxy_auth",
            "network_not_found",
            "network_http",
            "network_failed",
        ] {
            let bytes =
                serde_json::to_vec(&json!({"id":1,"ok":false,"error":code,"result":{}})).unwrap();
            assert_eq!(parse_response(&bytes, 1).unwrap(), Err(code.to_string()));
        }
        assert_eq!(
            sanitize_error("https://user:secret@example.com/?token=private"),
            "runtime_failed"
        );
    }

    #[test]
    fn durable_absolute_paths() {
        let cwd = std::env::current_dir().unwrap();
        let expected = cwd.join("data/translation/models");
        assert_eq!(absolute_models(Path::new("data"), &cwd), expected);
        assert!(absolute_models(&crate::paths::data_dir(), &cwd).is_absolute());
        let executable = cwd.join("app.exe");
        let worker = executable_path(&executable).unwrap();
        assert_eq!(worker.parent().unwrap(), cwd.join("translation"));
        assert!(executable_path(Path::new("app")).is_err());
    }

    #[test]
    fn development_helper_fallback_is_not_used_in_production() {
        let exe = Path::new("target/debug/zapive.exe");
        let installed = executable_path(exe).unwrap();
        let release = executable_path(Path::new("target/release/zapive.exe")).unwrap();
        assert_eq!(resolve_executable_with(exe, true, |_| true), installed);
        assert_eq!(
            resolve_executable_with(exe, true, |p| p == release),
            release
        );
        assert_eq!(
            resolve_executable_with(exe, false, |p| p == release),
            installed
        );
        assert_eq!(resolve_executable_with(exe, true, |_| false), installed);
    }

    #[test]
    fn validates_requests_and_deadlines() {
        assert!(validate_request(&json!({"op":"stop"})).is_ok());
        assert!(validate_request(&json!({"op":"restart"})).is_ok());
        assert!(validate_request(&json!({"op":"unknown"})).is_err());
        assert!(validate_request(&json!([])).is_err());
        assert!(
            validate_request(&json!({"op":"translate","text":"x".repeat(MAX_REQUEST)})).is_err()
        );
        assert_eq!(request_timeout("status").as_secs(), 15);
        assert_eq!(request_timeout("refresh").as_secs(), 240);
        assert_eq!(request_timeout("import_catalog").as_secs(), 240);
        assert_eq!(request_timeout("translate").as_secs(), 180);
        assert_eq!(request_timeout("install").as_secs(), 600);
    }

    #[test]
    fn new_and_clones_are_lazy() {
        let worker = Worker::new();
        let clone = worker.clone();
        assert!(lock(&worker.inner.sender).is_none());
        assert_eq!(clone.status(), json!({"running":false,"pid":null}));
        drop(worker);
        assert!(!lock(&clone.inner.shared).shutdown);
    }
}

mod downloads {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::net::IpAddr;
    use ureq::unversioned::{
        resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver},
        transport::{DefaultConnector, NextTimeout},
    };
    use url::Url;

    const INDEX: &str =
        "https://raw.githubusercontent.com/argosopentech/argospm-index/main/index.json";
    const CATALOG_LIMIT: u64 = 8 * 1024 * 1024;
    const MODEL_LIMIT: u64 = 512 * 1024 * 1024;
    type Pair = (String, String);
    type Catalog = BTreeMap<Pair, Vec<String>>;
    type Check<'a> = &'a dyn Fn() -> Result<(), String>;

    fn version(status: &Value) -> Result<(), String> {
        if status["version"].as_u64() != Some(2) {
            return Err("worker_outdated".into());
        }
        Ok(())
    }

    fn code(s: &str) -> bool {
        let mut parts = s.split('-');
        let first = parts.next().unwrap_or_default();
        (2..=3).contains(&first.len())
            && first.bytes().all(|b| b.is_ascii_lowercase())
            && parts.next().is_none_or(|p| {
                (2..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_lowercase())
            })
            && parts.next().is_none()
    }

    fn pairs(status: &Value) -> Result<BTreeSet<Pair>, String> {
        let text = status["models"].as_str().ok_or("invalid_response")?;
        if text.is_empty() {
            return Ok(BTreeSet::new());
        }
        text.split(',')
            .map(|p| {
                let (a, b) = p.split_once(':').ok_or("invalid_response")?;
                if !code(a) || !code(b) {
                    return Err("invalid_response".into());
                }
                Ok((a.into(), b.into()))
            })
            .collect()
    }

    fn route(
        source: &str,
        target: &str,
        catalog: &Catalog,
        installed: &BTreeSet<Pair>,
    ) -> Option<Vec<Pair>> {
        let edges: BTreeSet<_> = catalog.keys().chain(installed.iter()).cloned().collect();
        let direct = (source.into(), target.into());
        if edges.contains(&direct) {
            return Some(vec![direct]);
        }
        edges
            .iter()
            .filter(|(a, b)| a == source && b != source && b != target)
            .filter_map(|first| {
                let second = (first.1.clone(), target.into());
                edges.contains(&second).then(|| vec![first.clone(), second])
            })
            .min_by_key(|r| {
                (
                    r.iter().filter(|p| !installed.contains(*p)).count(),
                    r[0].1 != "en",
                    r[0].1.clone(),
                )
            })
    }

    fn catalog_path(status: &Value, models: &Path) -> Result<PathBuf, String> {
        let path = PathBuf::from(status["catalog_path"].as_str().ok_or("invalid_catalog")?);
        let expected = models
            .parent()
            .ok_or("invalid_catalog")?
            .join("translation-index.json");
        if !path.is_absolute()
            || path.file_name() != expected.file_name()
            || std::fs::canonicalize(path.parent().ok_or("invalid_catalog")?)
                .map_err(|_| "invalid_catalog")?
                != std::fs::canonicalize(expected.parent().unwrap())
                    .map_err(|_| "invalid_catalog")?
        {
            return Err("invalid_catalog".into());
        }
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            if !meta.is_file() || meta.file_type().is_symlink() {
                return Err("invalid_catalog".into());
            }
            let actual = std::fs::canonicalize(&path).map_err(|_| "invalid_catalog")?;
            let parent =
                std::fs::canonicalize(models.parent().unwrap()).map_err(|_| "invalid_catalog")?;
            if actual.parent() != Some(parent.as_path()) {
                return Err("invalid_catalog".into());
            }
        }
        Ok(path)
    }

    fn read_catalog(path: &Path, check: Check<'_>) -> Result<Option<Catalog>, String> {
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("invalid_catalog".into()),
        };
        let mut bytes = Vec::new();
        copy(
            &mut file.take(CATALOG_LIMIT + 1),
            &mut bytes,
            CATALOG_LIMIT,
            check,
        )?;
        let entries: Value = serde_json::from_slice(&bytes).map_err(|_| "invalid_catalog")?;
        let mut catalog = Catalog::new();
        for entry in entries.as_array().ok_or("invalid_catalog")? {
            if entry
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("translate")
                != "translate"
            {
                continue;
            }
            let a = entry["from_code"].as_str().ok_or("invalid_catalog")?;
            let b = entry["to_code"].as_str().ok_or("invalid_catalog")?;
            if !code(a) || !code(b) {
                continue;
            }
            let links = entry["links"]
                .as_array()
                .ok_or("invalid_catalog")?
                .iter()
                .filter_map(Value::as_str)
                .filter(|s| allowed(s, false, false))
                .take(8)
                .map(str::to_owned)
                .collect();
            catalog.entry((a.into(), b.into())).or_insert(links);
        }
        Ok(Some(catalog))
    }

    pub(super) fn run(
        request: &Value,
        call: &mut impl FnMut(Value) -> Result<Value, String>,
        check: Check<'_>,
    ) -> Result<Value, String> {
        let cwd = std::env::current_dir().map_err(|_| "models_directory_required")?;
        let models = absolute_models(&crate::paths::data_dir(), &cwd);
        run_with_models(request, &models, call, check)
    }

    fn run_with_models(
        request: &Value,
        models: &Path,
        call: &mut impl FnMut(Value) -> Result<Value, String>,
        check: Check<'_>,
    ) -> Result<Value, String> {
        let mut status = call(json!({"op":"status"}))?;
        version(&status)?;
        if request["op"] == "refresh" {
            refresh(&status, &models, call, check)?;
            let status = call(json!({"op":"status"}))?;
            version(&status)?;
            return Ok(status);
        }
        let source = request["source"].as_str().ok_or("invalid_language")?;
        let target = request["target"].as_str().ok_or("invalid_language")?;
        if !code(source) || !code(target) || source == target {
            return Err("invalid_language".into());
        }
        let mut installed = pairs(&status)?;
        if installed.contains(&(source.into(), target.into())) {
            return Ok(status);
        }
        let mut catalog =
            read_catalog(&catalog_path(&status, &models)?, check)?.unwrap_or_default();
        if route(source, target, &catalog, &installed).is_none() {
            refresh(&status, &models, call, check)?;
            status = call(json!({"op":"status"}))?;
            version(&status)?;
            installed = pairs(&status)?;
            catalog =
                read_catalog(&catalog_path(&status, &models)?, check)?.ok_or("invalid_catalog")?;
        }
        let plan = route(source, target, &catalog, &installed).ok_or("pair_unavailable")?;
        for pair in plan {
            check()?;
            if installed.contains(&pair) {
                continue;
            }
            let links = catalog.get(&pair).ok_or("pair_unavailable")?;
            let mut failure = "model_download_failed".to_string();
            let mut downloaded = None;
            for link in links {
                match download(link, false, &models, check) {
                    Ok(file) => {
                        downloaded = Some(file);
                        break;
                    }
                    Err(e) => {
                        check()?;
                        failure = e;
                    }
                }
            }
            let file = downloaded.ok_or(failure)?;
            check()?;
            log::info!("[translation] installing local model");
            call(json!({"op":"install_local", "path":file.0, "source":pair.0, "target":pair.1}))?;
            drop(file);
        }
        let status = call(json!({"op":"status"}))?;
        version(&status)?;
        Ok(status)
    }

    fn refresh(
        status: &Value,
        models: &Path,
        call: &mut impl FnMut(Value) -> Result<Value, String>,
        check: Check<'_>,
    ) -> Result<(), String> {
        catalog_path(status, models)?;
        let url = status["catalog_url"].as_str().ok_or("invalid_catalog")?;
        if url != INDEX {
            return Err("insecure_download".into());
        }
        let file = download(url, true, models, check)?;
        read_catalog(&file.0, check)?.ok_or("invalid_catalog")?;
        check()?;
        call(json!({"op":"import_catalog", "path":file.0}))?;
        Ok(())
    }

    fn allowed(input: &str, catalog: bool, redirected: bool) -> bool {
        let Ok(url) = Url::parse(input) else {
            return false;
        };
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port_or_known_default() != Some(443)
            || url.fragment().is_some()
        {
            return false;
        }
        if catalog {
            return input == INDEX;
        }
        match url.host_str().unwrap_or_default() {
            "argos-net.com" => true,
            "github.com" | "raw.githubusercontent.com" => url.path().starts_with("/argosopentech/"),
            "release-assets.githubusercontent.com" | "objects.githubusercontent.com" => redirected,
            _ => false,
        }
    }

    fn public_ip(ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(ip) => {
                let [a, b, c, _] = ip.octets();
                !(a == 0
                    || a == 10
                    || a == 127
                    || a >= 224
                    || (a == 100 && (64..=127).contains(&b))
                    || (a == 169 && b == 254)
                    || (a == 172 && (16..=31).contains(&b))
                    || (a == 192 && (b == 168 || b == 0 || (b == 88 && c == 99)))
                    || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                    || (a == 203 && b == 0 && c == 113))
            }
            IpAddr::V6(ip) => {
                let s = ip.segments();
                s[0] & 0xe000 == 0x2000
                    && s[0] != 0x2001
                    && s[0] != 0x2002
                    && !(s[0] == 0x3fff && s[1] <= 0x0fff)
            }
        }
    }
    #[derive(Debug)]
    struct PublicResolver;
    impl Resolver for PublicResolver {
        fn resolve(
            &self,
            uri: &ureq::http::Uri,
            config: &ureq::config::Config,
            timeout: NextTimeout,
        ) -> Result<ResolvedSocketAddrs, ureq::Error> {
            let addresses = DefaultResolver::default().resolve(uri, config, timeout)?;
            if addresses.iter().any(|a| !public_ip(a.ip())) {
                return Err(ureq::Error::HostNotFound);
            }
            Ok(addresses)
        }
    }

    struct Temporary(PathBuf);
    impl Drop for Temporary {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    fn temporary(models: &Path) -> Result<(Temporary, std::fs::File), String> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = models
            .parent()
            .ok_or("models_directory_required")?
            .join("downloads");
        std::fs::create_dir_all(&dir).map_err(|_| "models_directory_required")?;
        let parent = std::fs::canonicalize(models.parent().unwrap())
            .map_err(|_| "models_directory_required")?;
        let canonical = std::fs::canonicalize(&dir).map_err(|_| "models_directory_required")?;
        if canonical != parent.join("downloads") {
            return Err("models_directory_required".into());
        }
        for _ in 0..32 {
            let path = canonical.join(format!(
                "rust-{}-{}.tmp",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => return Ok((Temporary(path), file)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err("model_download_failed".into()),
            }
        }
        Err("model_download_failed".into())
    }

    fn copy(
        reader: &mut impl Read,
        writer: &mut impl Write,
        limit: u64,
        check: Check<'_>,
    ) -> Result<u64, String> {
        let mut total = 0u64;
        let mut buffer = [0; 64 * 1024];
        loop {
            check()?;
            let n = reader.read(&mut buffer).map_err(|e| io_error(&e))?;
            check()?;
            if n == 0 {
                return Ok(total);
            }
            total += n as u64;
            if total > limit {
                return Err("download_too_large".into());
            }
            writer
                .write_all(&buffer[..n])
                .map_err(|_| "model_download_failed")?;
        }
    }

    fn http_error(status: u16) -> &'static str {
        match status {
            401 => "network_http_unauthorized",
            403 => "network_http_forbidden",
            404 => "network_not_found",
            407 => "network_proxy_auth",
            429 => "network_rate_limited",
            500..=599 => "network_server",
            _ => "network_http",
        }
    }
    fn io_error(error: &std::io::Error) -> &'static str {
        if let Some(error) = error
            .get_ref()
            .and_then(|e| e.downcast_ref::<ureq::Error>())
        {
            return network_error(error);
        }
        use std::io::ErrorKind::*;
        match error.kind() {
            TimedOut | WouldBlock => "network_timeout",
            PermissionDenied => "network_access_denied",
            ConnectionRefused => "network_refused",
            ConnectionReset | ConnectionAborted | Interrupted | UnexpectedEof => {
                "network_interrupted"
            }
            _ => "network_failed",
        }
    }
    fn network_error(error: &ureq::Error) -> &'static str {
        match error {
            ureq::Error::Timeout(_) => "network_timeout",
            ureq::Error::HostNotFound => "network_dns",
            ureq::Error::Io(e) => io_error(e),
            ureq::Error::Rustls(e) => {
                if e.to_string().to_ascii_lowercase().contains("certificate") {
                    "network_tls_certificate"
                } else {
                    "network_tls"
                }
            }
            ureq::Error::Tls(_) | ureq::Error::TlsRequired => "network_tls",
            ureq::Error::ConnectionFailed => "network_unreachable",
            ureq::Error::StatusCode(s) => http_error(*s),
            _ => "network_failed",
        }
    }
    fn retryable(code: &str) -> bool {
        matches!(
            code,
            "network_timeout"
                | "network_dns"
                | "network_refused"
                | "network_interrupted"
                | "network_unreachable"
                | "network_rate_limited"
                | "network_server"
        )
    }

    fn download(
        input: &str,
        catalog: bool,
        models: &Path,
        check: Check<'_>,
    ) -> Result<Temporary, String> {
        if !allowed(input, catalog, false) {
            return Err("insecure_download".into());
        }
        let config = ureq::Agent::config_builder()
            .proxy(None)
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(15)))
            .timeout_recv_body(None)
            .timeout_global(Some(Duration::from_secs(300)))
            .build();
        let agent = ureq::Agent::with_parts(config, DefaultConnector::default(), PublicResolver);
        let deadline = Instant::now() + Duration::from_secs(if catalog { 120 } else { 300 });
        let limit = if catalog { CATALOG_LIMIT } else { MODEL_LIMIT };
        log::info!(
            "[translation] downloading {}",
            if catalog { "catalog" } else { "model" }
        );
        let mut failure = "network_failed".to_string();
        for attempt in 0..3 {
            check()?;
            if attempt > 0 {
                for _ in 0..(10 * attempt) {
                    check()?;
                    thread::sleep(Duration::from_millis(25));
                }
            }
            let (temporary, mut file) = temporary(models)?;
            let result = (|| {
                let mut url = Url::parse(input).map_err(|_| "insecure_download")?;
                for redirect in 0..=4 {
                    check()?;
                    if !allowed(url.as_str(), catalog, redirect > 0) {
                        return Err("insecure_download".into());
                    }
                    let remaining = deadline
                        .checked_duration_since(Instant::now())
                        .ok_or("network_timeout")?;
                    let mut response = agent
                        .get(url.as_str())
                        .config()
                        .timeout_global(Some(remaining))
                        .build()
                        .call()
                        .map_err(|e| network_error(&e).to_string())?;
                    check()?;
                    let status = response.status().as_u16();
                    if matches!(status, 301 | 302 | 303 | 307 | 308) {
                        let location = response
                            .headers()
                            .get("location")
                            .and_then(|v| v.to_str().ok())
                            .ok_or("network_http")?;
                        url = url.join(location).map_err(|_| "insecure_download")?;
                        continue;
                    }
                    if status != 200 {
                        return Err(http_error(status).into());
                    }
                    if response
                        .headers()
                        .get("content-length")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok())
                        .is_some_and(|n| n > limit)
                    {
                        return Err("download_too_large".into());
                    }
                    let bytes = copy(
                        &mut response.body_mut().as_reader(),
                        &mut file,
                        limit,
                        &|| {
                            check()?;
                            if Instant::now() >= deadline {
                                Err("network_timeout".into())
                            } else {
                                Ok(())
                            }
                        },
                    )?;
                    file.flush().map_err(|_| "model_download_failed")?;
                    log::info!("[translation] download complete bytes={bytes}");
                    return Ok(());
                }
                Err("insecure_download".into())
            })();
            drop(file);
            match result {
                Ok(()) => return Ok(temporary),
                Err(e) => {
                    failure = e;
                    drop(temporary);
                }
            }
            check()?;
            if !retryable(&failure) || Instant::now() >= deadline {
                break;
            }
            log::info!("[translation] retrying download attempt={}", attempt + 2);
        }
        Err(failure)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Cursor;

        fn edges(items: &[(&str, &str)]) -> Catalog {
            items
                .iter()
                .map(|(a, b)| ((a.to_string(), b.to_string()), vec![]))
                .collect()
        }
        fn installed(items: &[(&str, &str)]) -> BTreeSet<Pair> {
            edges(items).into_keys().collect()
        }

        #[test]
        fn planner_direct_then_missing_count_then_english() {
            let mut catalog = edges(&[
                ("pt", "fr"),
                ("pt", "en"),
                ("en", "fr"),
                ("pt", "es"),
                ("es", "fr"),
            ]);
            let have = installed(&[("pt", "es"), ("es", "fr")]);
            assert_eq!(
                route("pt", "fr", &catalog, &have).unwrap(),
                vec![("pt".into(), "fr".into())]
            );
            catalog.remove(&("pt".into(), "fr".into()));
            assert_eq!(route("pt", "fr", &catalog, &have).unwrap()[0].1, "es");
            assert_eq!(
                route("pt", "fr", &catalog, &BTreeSet::new()).unwrap()[0].1,
                "en"
            );
            assert!(route("zz", "fr", &catalog, &have).is_none());
            assert_eq!(route("pt", "fr", &Catalog::new(), &have).unwrap().len(), 2);
        }

        #[test]
        fn outdated_helper_stops_before_any_download_or_install() {
            for value in [json!({}), json!({"version":1}), json!({"version":3})] {
                let mut calls = 0;
                let result = run(
                    &json!({"op":"refresh"}),
                    &mut |request| {
                        calls += 1;
                        assert_eq!(request["op"], "status");
                        Ok(value.clone())
                    },
                    &|| Ok(()),
                );
                assert_eq!(result.unwrap_err(), "worker_outdated");
                assert_eq!(calls, 1);
            }
            assert!(version(&json!({"version":2})).is_ok());
        }

        #[test]
        fn installed_direct_pair_uses_only_status() {
            let models =
                absolute_models(&crate::paths::data_dir(), &std::env::current_dir().unwrap());
            let status = json!({"version":2,"models":"en:pt","catalog_path":models.parent().unwrap().join("translation-index.json")});
            let mut calls = 0;
            let result = run(
                &json!({"op":"install","source":"en","target":"pt"}),
                &mut |request| {
                    calls += 1;
                    assert_eq!(request["op"], "status");
                    Ok(status.clone())
                },
                &|| Ok(()),
            )
            .unwrap();
            assert_eq!(result, status);
            assert_eq!(calls, 1);
        }

        #[test]
        fn strict_hosts_redirects_and_public_addresses() {
            assert!(allowed(INDEX, true, false));
            assert!(!allowed(&format!("{INDEX}?secret=x"), true, false));
            assert!(allowed(
                "https://argos-net.com/v1/model.argosmodel",
                false,
                false
            ));
            assert!(allowed(
                "https://github.com/argosopentech/models/releases/download/v1/a",
                false,
                false
            ));
            assert!(allowed(
                "https://release-assets.githubusercontent.com/a?signature=x",
                false,
                true
            ));
            assert!(!allowed(
                "https://release-assets.githubusercontent.com/a",
                false,
                false
            ));
            for url in [
                "http://argos-net.com/a",
                "https://argos-net.com.evil.test/a",
                "https://127.0.0.1/a",
                "https://localhost/a",
                "https://argos-net.com:444/a",
                "https://u:p@argos-net.com/a",
                "https://github.com/evil/a",
                "file:///tmp/a",
                "https://[::1]/a",
            ] {
                assert!(!allowed(url, false, true), "{url}");
            }
            for ip in [
                "127.0.0.1",
                "10.1.1.1",
                "169.254.169.254",
                "172.16.0.1",
                "192.168.1.1",
                "100.64.0.1",
                "::1",
                "fc00::1",
                "::ffff:127.0.0.1",
            ] {
                assert!(!public_ip(ip.parse().unwrap()));
            }
            assert!(public_ip("8.8.8.8".parse().unwrap()));
        }

        #[test]
        fn streaming_limits_and_generation_cancellation() {
            let mut output = Vec::new();
            assert_eq!(
                copy(&mut Cursor::new(b"1234"), &mut output, 4, &|| Ok(())).unwrap(),
                4
            );
            assert_eq!(
                copy(&mut Cursor::new(b"12345"), &mut Vec::new(), 4, &|| Ok(())).unwrap_err(),
                "download_too_large"
            );
            let checks = std::cell::Cell::new(0);
            let mut output = Vec::new();
            let error = copy(
                &mut Cursor::new(vec![0; 128 * 1024]),
                &mut output,
                MODEL_LIMIT,
                &|| {
                    checks.set(checks.get() + 1);
                    if checks.get() >= 3 {
                        Err("request_cancelled".into())
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();
            assert_eq!(error, "request_cancelled");
            assert_eq!(output.len(), 64 * 1024);
            assert_eq!(CATALOG_LIMIT, 8 * 1024 * 1024);
            assert_eq!(MODEL_LIMIT, 512 * 1024 * 1024);
        }

        #[test]
        fn classification_and_retry_policy_are_bounded_and_private() {
            assert_eq!(http_error(429), "network_rate_limited");
            assert_eq!(http_error(503), "network_server");
            assert_eq!(http_error(403), "network_http_forbidden");
            assert!(retryable(http_error(503)));
            assert!(!retryable(http_error(403)));
            assert!(!retryable("network_tls_certificate"));
            assert!(!retryable("download_too_large"));
            let error =
                std::io::Error::new(std::io::ErrorKind::ConnectionReset, "secret text and token");
            assert_eq!(io_error(&error), "network_interrupted");
            assert_eq!(network_error(&ureq::Error::HostNotFound), "network_dns");
            assert_eq!(network_error(&ureq::Error::Tls("secret")), "network_tls");
        }

        #[test]
        fn catalog_paths_are_not_arbitrary_helper_paths() {
            let models =
                absolute_models(&crate::paths::data_dir(), &std::env::current_dir().unwrap());
            for path in [PathBuf::from("relative.json"), models.join("other.json")] {
                assert_eq!(
                    catalog_path(&json!({"catalog_path":path}), &models).unwrap_err(),
                    "invalid_catalog"
                );
            }
            assert!(
                pairs(&json!({"models":"en:pt,pt:en"}))
                    .unwrap()
                    .contains(&("en".into(), "pt".into()))
            );
            assert!(pairs(&json!({"models":"en:../bad"})).is_err());
        }

        #[test]
        fn catalog_path_accepts_resolved_parent_and_rejects_other_files() {
            let root =
                std::env::temp_dir().join(format!("zapive-catalog-test-{}", std::process::id()));
            let models = root.join("models");
            std::fs::create_dir_all(&models).unwrap();
            let resolved = std::fs::canonicalize(&root).unwrap();
            let path = resolved.join("translation-index.json");
            assert_eq!(
                catalog_path(&json!({"catalog_path":path}), &models).unwrap(),
                path
            );
            std::fs::write(&path, b"[]").unwrap();
            assert!(catalog_path(&json!({"catalog_path":path}), &models).is_ok());
            assert!(
                catalog_path(
                    &json!({"catalog_path":resolved.join("other.json")}),
                    &models
                )
                .is_err()
            );
            std::fs::remove_dir_all(root).unwrap();
        }

        #[test]
        #[ignore = "downloads the live catalog and en:pt model into an isolated temporary directory"]
        fn live_rust_downloader_with_v2_helper() {
            struct Sandbox {
                child: Child,
                root: PathBuf,
            }
            impl Drop for Sandbox {
                fn drop(&mut self) {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    let _ = std::fs::remove_dir_all(&self.root);
                }
            }
            let project = Path::new(env!("CARGO_MANIFEST_DIR"));
            let root = std::env::temp_dir().join(format!(
                "zapive-live-download-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let models = root.join("models");
            std::fs::create_dir_all(&models).unwrap();
            let mut command =
                if let Some(helper) = std::env::var_os("ZAPIVE_TRANSLATION_TEST_HELPER") {
                    Command::new(helper)
                } else {
                    let mut command = Command::new(project.join(if cfg!(windows) {
                        "build/translation/venv/Scripts/python.exe"
                    } else {
                        "build/translation/venv/bin/python"
                    }));
                    command.arg(project.join("packaging/translation/worker.py"));
                    command
                };
            let child = command
                .env("ARGOS_PACKAGES_DIR", &models)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("v2 helper or translation build venv required");
            let mut sandbox = Sandbox { child, root };
            let mut input = sandbox.child.stdin.take().unwrap();
            let output = sandbox.child.stdout.take().unwrap();
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let mut output = BufReader::new(output);
                while let Ok(bytes) = read_response(&mut output) {
                    if tx.send(bytes).is_err() {
                        break;
                    }
                }
            });
            let mut id = 0u64;
            let mut call = |mut request: Value| -> Result<Value, String> {
                id += 1;
                request["id"] = json!(id);
                serde_json::to_writer(&mut input, &request).map_err(|_| "runtime_failed")?;
                input.write_all(b"\n").map_err(|_| "runtime_failed")?;
                input.flush().map_err(|_| "runtime_failed")?;
                let bytes = rx
                    .recv_timeout(request_timeout(request["op"].as_str().unwrap()))
                    .map_err(|_| "worker_timeout")?;
                parse_response(&bytes, id)?
            };
            let status = call(json!({"op":"status"})).unwrap();
            version(&status).expect("stage a protocol v2 helper before running this test");
            assert_eq!(status["catalog_url"], INDEX);
            assert_eq!(status["models"], "");
            let refreshed =
                run_with_models(&json!({"op":"refresh"}), &models, &mut call, &|| Ok(())).unwrap();
            version(&refreshed).unwrap();
            assert!(
                read_catalog(&catalog_path(&refreshed, &models).unwrap(), &|| Ok(()))
                    .unwrap()
                    .unwrap()
                    .contains_key(&("en".into(), "pt".into()))
            );
            let status = run_with_models(
                &json!({"op":"install", "source":"en", "target":"pt"}),
                &models,
                &mut call,
                &|| Ok(()),
            )
            .unwrap();
            assert!(
                pairs(&status)
                    .unwrap()
                    .contains(&("en".into(), "pt".into()))
            );
            assert_eq!(
                std::fs::read_dir(sandbox.root.join("downloads"))
                    .unwrap()
                    .count(),
                0
            );
        }

        #[test]
        fn temporary_files_are_removed_on_error_and_success() {
            let root =
                std::env::temp_dir().join(format!("zapive-download-test-{}", std::process::id()));
            let models = root.join("models");
            std::fs::create_dir_all(&models).unwrap();
            let (temporary, file) = temporary(&models).unwrap();
            let path = temporary.0.clone();
            assert!(path.is_absolute());
            assert_eq!(
                path.parent().unwrap(),
                std::fs::canonicalize(root.join("downloads")).unwrap()
            );
            drop(file);
            drop(temporary);
            assert!(!path.exists());
            let _ = std::fs::remove_dir_all(root);
        }
    }
}
