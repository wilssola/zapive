// Local translation. CTranslate2 runs in-process on a dedicated thread with
// the Argos OPUS-MT packages (see engine.rs); models are downloaded and
// unpacked by downloads.rs and models.rs. Requests are JSON objects and
// replies are JSON values, so the bridge never touches the engine directly.
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread;
use std::time::{Duration, Instant};

mod detect;
mod downloads;
mod engine;
mod models;

use models::{Catalog, Pair, code, plan_route};

const QUEUE_CAPACITY: usize = 8;
const MAX_REQUEST: usize = 128 * 1024;
const MAX_TEXT: usize = 16_000;
const MAX_ENGINES: usize = 2;
const TRANSLATE_TIMEOUT: Duration = Duration::from_secs(180);
type Done = Box<dyn FnOnce(Result<Value, String>) + Send>;
type Progress = Box<dyn Fn(Value) + Send>;

#[derive(Clone)]
pub struct Worker {
    inner: Arc<Inner>,
}

struct Inner {
    sender: Mutex<Option<mpsc::SyncSender<Task>>>,
    shared: Arc<Mutex<State>>,
    models: PathBuf,
}

struct State {
    generation: u64,
    shutdown: bool,
    running: bool,
}

struct Task {
    request: Value,
    generation: u64,
    done: Done,
    progress: Option<Progress>,
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
        let cwd = std::env::current_dir().unwrap_or_default();
        Self::with_models(absolute_models(&crate::paths::data_dir(), &cwd))
    }

    fn with_models(models: PathBuf) -> Self {
        Self {
            inner: Arc::new(Inner {
                sender: Mutex::new(None),
                shared: Arc::new(Mutex::new(State {
                    generation: 0,
                    shutdown: false,
                    running: false,
                })),
                models,
            }),
        }
    }

    /// Queues a request; `done` runs on the engine thread with the reply.
    /// `stop`, `restart` and `cancel` cancel everything queued or running before them.
    pub fn request(
        &self,
        request: Value,
        done: impl FnOnce(Result<Value, String>) + Send + 'static,
    ) -> Result<(), String> {
        self.send(request, None, Box::new(done))
    }

    /// Like `request`; `progress` runs on the engine thread with install progress.
    pub fn request_with_progress(
        &self,
        request: Value,
        progress: impl Fn(Value) + Send + 'static,
        done: impl FnOnce(Result<Value, String>) + Send + 'static,
    ) -> Result<(), String> {
        self.send(request, Some(Box::new(progress)), Box::new(done))
    }

    fn send(&self, request: Value, progress: Option<Progress>, done: Done) -> Result<(), String> {
        validate_request(&request)?;
        let mut sender = lock(&self.inner.sender);
        if sender.is_none() {
            let (tx, rx) = mpsc::sync_channel(QUEUE_CAPACITY);
            let shared = self.inner.shared.clone();
            let models = self.inner.models.clone();
            thread::Builder::new()
                .name("translation-engine".into())
                .spawn(move || serve(rx, shared, models))
                .map_err(|_| "worker_unavailable".to_string())?;
            *sender = Some(tx);
        }
        let mut state = lock(&self.inner.shared);
        let control = matches!(request["op"].as_str(), Some("stop" | "restart" | "cancel"));
        let generation = state.generation.wrapping_add(u64::from(control));
        let task = Task {
            request,
            generation,
            done,
            progress,
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
        }
        Ok(())
    }

    pub fn status(&self) -> Value {
        json!({ "running": lock(&self.inner.shared).running })
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let mut state = lock(&self.shared);
        state.shutdown = true;
        state.generation = state.generation.wrapping_add(1);
        state.running = false;
        drop(state);
        lock(&self.sender).take();
    }
}

fn absolute_models(data: &Path, cwd: &Path) -> PathBuf {
    let path = data.join("translation").join("models");
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

fn cancelled(shared: &Mutex<State>, generation: u64) -> Result<(), String> {
    let state = lock(shared);
    if state.shutdown || state.generation != generation {
        Err("request_cancelled".into())
    } else {
        Ok(())
    }
}

/// Everything the engine thread keeps between requests.
#[derive(Default)]
struct Core {
    engines: Vec<(PathBuf, engine::Engine)>,
    detector: Option<lingua::LanguageDetector>,
}

impl Core {
    fn reset(&mut self) {
        self.engines.clear();
        self.detector = None;
    }

    /// The loaded engine for a package directory, most recently used last.
    fn engine(&mut self, dir: &Path) -> Result<&engine::Engine, String> {
        if let Some(index) = self.engines.iter().position(|(path, _)| path == dir) {
            let entry = self.engines.remove(index);
            self.engines.push(entry);
        } else {
            let meta = models::read_meta(dir).map_err(|_| "invalid_model_file")?;
            let loaded = engine::Engine::load(dir, meta.target_prefix)?;
            if self.engines.len() >= MAX_ENGINES {
                self.engines.remove(0);
            }
            self.engines.push((dir.to_path_buf(), loaded));
        }
        Ok(&self.engines.last().expect("engine was just pushed").1)
    }
}

fn serve(receiver: mpsc::Receiver<Task>, shared: Arc<Mutex<State>>, models: PathBuf) {
    let mut core = Core::default();
    while let Ok(task) = receiver.recv() {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            execute(&task, &shared, &models, &mut core)
        }));
        let result = outcome.unwrap_or_else(|_| {
            log::error!("[translation] engine panicked");
            core.reset();
            Err("runtime_failed".into())
        });
        finish(task.done, result);
    }
    lock(&shared).running = false;
}

fn execute(
    task: &Task,
    shared: &Mutex<State>,
    models: &Path,
    core: &mut Core,
) -> Result<Value, String> {
    let generation = task.generation;
    let op = task.request["op"].as_str().unwrap_or_default();
    let deadline = (op == "translate").then(|| Instant::now() + TRANSLATE_TIMEOUT);
    let check = || {
        cancelled(shared, generation)?;
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err("worker_timeout".into());
        }
        Ok(())
    };
    check()?;
    if op == "stop" {
        core.reset();
        lock(shared).running = false;
        return Ok(json!({ "running": false }));
    }
    std::fs::create_dir_all(models).map_err(|_| "models_directory_required")?;
    if op == "restart" {
        core.reset();
    }
    lock(shared).running = true;
    match op {
        "status" | "restart" | "cancel" => {}
        "refresh" => {
            downloads::refresh(models, &check)?;
        }
        "install" => {
            let source = task.request["source"].as_str().unwrap_or_default();
            let target = task.request["target"].as_str().unwrap_or_default();
            core.engines.clear();
            let progress = |value: Value| {
                if let Some(progress) = &task.progress {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| progress(value)));
                }
            };
            downloads::install(models, source, target, &check, &progress)?;
        }
        "remove" => {
            let source = task.request["source"].as_str().unwrap_or_default();
            let target = task.request["target"].as_str().unwrap_or_default();
            core.engines.clear();
            models::remove(models, source, target)?;
        }
        "translate" => return translate(&task.request, models, core, &check),
        _ => return Err("invalid_operation".into()),
    }
    check()?;
    Ok(status(models, &check))
}

fn status(models: &Path, check: models::Check<'_>) -> Value {
    let catalog = downloads::read_catalog(models, check)
        .unwrap_or_else(|error| {
            log::warn!("[translation] cached catalog ignored: {error}");
            None
        })
        .unwrap_or_default();
    let mut value = models::status(models, &catalog);
    value["running"] = json!(true);
    value
}

fn translate(
    request: &Value,
    models: &Path,
    core: &mut Core,
    check: models::Check<'_>,
) -> Result<Value, String> {
    let source = request["source"].as_str().unwrap_or_default();
    let target = request["target"].as_str().unwrap_or_default();
    let text = request["text"].as_str().unwrap_or_default();
    let source = if matches!(source, "" | "auto") {
        detect::detect(text, &mut core.detector)?
    } else {
        source.to_string()
    };
    check()?;
    let reply = |text: &str| json!({ "text": text, "source": source, "target": target });
    if source == target {
        return Ok(reply(text));
    }
    let installed = models::installed(models);
    let have: BTreeSet<Pair> = installed.keys().cloned().collect();
    let route =
        plan_route(&source, target, &Catalog::new(), &have).ok_or("model_not_installed")?;
    let mut text = text.to_string();
    for edge in route {
        check()?;
        let dir = installed.get(&edge).ok_or("model_not_installed")?;
        text = core.engine(dir)?.translate(&text, check)?;
    }
    Ok(reply(&text))
}

/// Models still missing to translate `source -> target`: `Some(0)` when it
/// already works with what is installed, `None` when no route is known.
pub fn missing_models(
    source: &str,
    target: &str,
    catalog: &BTreeSet<Pair>,
    installed: &BTreeSet<Pair>,
) -> Option<usize> {
    let catalog: Catalog = catalog.iter().map(|pair| (pair.clone(), Vec::new())).collect();
    plan_route(source, target, &catalog, installed)
        .map(|route| route.iter().filter(|pair| !installed.contains(*pair)).count())
}

fn validate_request(request: &Value) -> Result<(), String> {
    if !request.is_object() {
        return Err("invalid_request".into());
    }
    let op = request["op"].as_str();
    if !matches!(
        op,
        Some("status" | "refresh" | "install" | "remove" | "translate" | "stop" | "restart" | "cancel")
    ) {
        return Err("invalid_operation".into());
    }
    if serde_json::to_vec(request)
        .map_err(|_| "invalid_request")?
        .len()
        > MAX_REQUEST
    {
        return Err("request_too_large".into());
    }
    let field = |key: &str| match &request[key] {
        Value::Null => Ok(""),
        Value::String(value) => Ok(value.as_str()),
        _ => Err("invalid_request"),
    };
    let (source, target, text) = (field("source")?, field("target")?, field("text")?);
    if text.chars().count() > MAX_TEXT || source.len() > 16 || target.len() > 16 {
        return Err("request_too_large".into());
    }
    match op {
        Some("translate") => {
            if !code(target) || !(matches!(source, "" | "auto") || code(source)) {
                return Err("invalid_language".into());
            }
            if text.trim().is_empty() {
                return Err("empty_text".into());
            }
        }
        Some("install" | "remove") => {
            if !code(source) || !code(target) || source == target {
                return Err("invalid_language".into());
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "zapive-translation-{name}-{}-{}",
            std::process::id(),
            models::random_hex()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn call(worker: &Worker, request: Value) -> Result<Value, String> {
        let (tx, rx) = channel();
        worker
            .request(request, move |result| {
                let _ = tx.send(result);
            })
            .map(|()| rx.recv_timeout(Duration::from_secs(30)).unwrap())
            .and_then(|result| result)
    }

    #[test]
    fn validates_requests() {
        assert!(validate_request(&json!({"op":"stop"})).is_ok());
        assert!(validate_request(&json!({"op":"restart"})).is_ok());
        assert!(validate_request(&json!({"op":"cancel"})).is_ok());
        assert!(validate_request(&json!({"op":"remove","source":"en","target":"pt"})).is_ok());
        assert_eq!(
            validate_request(&json!({"op":"remove","source":"..","target":"pt"})).unwrap_err(),
            "invalid_language"
        );
        assert!(validate_request(&json!({"op":"status"})).is_ok());
        assert_eq!(validate_request(&json!({"op":"unknown"})).unwrap_err(), "invalid_operation");
        assert_eq!(validate_request(&json!([])).unwrap_err(), "invalid_request");
        assert_eq!(
            validate_request(&json!({"op":"translate","target":"pt","text":"x".repeat(MAX_REQUEST)}))
                .unwrap_err(),
            "request_too_large"
        );
        let translate = |source: &str, target: &str, text: &str| {
            validate_request(&json!({"op":"translate","source":source,"target":target,"text":text}))
        };
        assert!(translate("auto", "pt", "hello").is_ok());
        assert!(translate("", "pt", "hello").is_ok());
        assert!(translate("en", "pt", "hello").is_ok());
        assert_eq!(translate("en", "PT", "hello").unwrap_err(), "invalid_language");
        assert_eq!(translate("../", "pt", "hello").unwrap_err(), "invalid_language");
        assert_eq!(translate("en", "pt", "  \n").unwrap_err(), "empty_text");
        assert_eq!(
            translate("en", "pt", &"x".repeat(MAX_TEXT + 1)).unwrap_err(),
            "request_too_large"
        );
        assert!(translate("en", "pt", &"é".repeat(MAX_TEXT)).is_ok());
        assert_eq!(
            validate_request(&json!({"op":"translate","target":5,"text":"x"})).unwrap_err(),
            "invalid_request"
        );
        let install = |source: &str, target: &str| {
            validate_request(&json!({"op":"install","source":source,"target":target}))
        };
        assert!(install("en", "pt").is_ok());
        assert_eq!(install("en", "en").unwrap_err(), "invalid_language");
        assert_eq!(install("auto", "pt").unwrap_err(), "invalid_language");
    }

    #[test]
    fn durable_absolute_paths() {
        let cwd = std::env::current_dir().unwrap();
        let expected = cwd.join("data/translation/models");
        assert_eq!(absolute_models(Path::new("data"), &cwd), expected);
        assert!(absolute_models(&crate::paths::data_dir(), &cwd).is_absolute());
    }

    #[test]
    fn new_and_clones_are_lazy() {
        let worker = Worker::with_models(scratch("lazy").join("models"));
        let clone = worker.clone();
        assert!(lock(&worker.inner.sender).is_none());
        assert_eq!(clone.status(), json!({"running":false}));
        drop(worker);
        assert!(!lock(&clone.inner.shared).shutdown);
    }

    #[test]
    fn lifecycle_status_stop_and_restart() {
        let root = scratch("lifecycle");
        let worker = Worker::with_models(root.join("models"));
        let status = call(&worker, json!({"op":"status"})).unwrap();
        assert_eq!(status["models"], "");
        assert_eq!(status["pairs"], "");
        assert_eq!(worker.status(), json!({"running":true}));
        assert_eq!(call(&worker, json!({"op":"stop"})).unwrap(), json!({"running":false}));
        assert_eq!(worker.status(), json!({"running":false}));
        assert_eq!(call(&worker, json!({"op":"restart"})).unwrap()["running"], true);
        assert_eq!(call(&worker, json!({"op":"cancel"})).unwrap()["installed"], json!([]));
        assert_eq!(
            call(&worker, json!({"op":"remove","source":"en","target":"pt"})).unwrap_err(),
            "model_not_installed"
        );
        assert_eq!(worker.status(), json!({"running":true}));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn translate_reports_domain_errors_without_models() {
        let root = scratch("domain");
        let worker = Worker::with_models(root.join("models"));
        let same = call(
            &worker,
            json!({"op":"translate","source":"pt","target":"pt","text":"olá"}),
        )
        .unwrap();
        assert_eq!(same, json!({"text":"olá","source":"pt","target":"pt"}));
        assert_eq!(
            call(&worker, json!({"op":"translate","source":"en","target":"pt","text":"hello"}))
                .unwrap_err(),
            "model_not_installed"
        );
        assert_eq!(
            call(&worker, json!({"op":"translate","source":"auto","target":"pt","text":"hi"}))
                .unwrap_err(),
            "source_required"
        );
        let japanese = call(
            &worker,
            json!({"op":"translate","source":"auto","target":"ja","text":"こんにちは"}),
        )
        .unwrap();
        assert_eq!(japanese["source"], "ja");
        assert_eq!(worker.status(), json!({"running":true}));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stop_cancels_requests_queued_before_it() {
        let root = scratch("cancel");
        let worker = Worker::with_models(root.join("models"));
        let (tx, rx) = channel();
        let (release_tx, release_rx) = channel::<()>();
        let blocker = tx.clone();
        worker
            .request(json!({"op":"status"}), move |_| {
                let _ = release_rx.recv_timeout(Duration::from_secs(10));
                let _ = blocker.send("status");
            })
            .unwrap();
        let queued = tx.clone();
        worker
            .request(json!({"op":"translate","source":"en","target":"pt","text":"hello"}), move |r| {
                let _ = queued.send(if r == Err("request_cancelled".into()) { "cancelled" } else { "ran" });
            })
            .unwrap();
        let stopped = tx.clone();
        worker
            .request(json!({"op":"stop"}), move |r| {
                let _ = stopped.send(if r.is_ok() { "stopped" } else { "stop failed" });
            })
            .unwrap();
        release_tx.send(()).unwrap();
        let order: Vec<_> = (0..3).map(|_| rx.recv_timeout(Duration::from_secs(10)).unwrap()).collect();
        assert_eq!(order, ["status", "cancelled", "stopped"]);
        assert_eq!(worker.status(), json!({"running":false}));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn full_queue_is_reported_as_busy() {
        let root = scratch("busy");
        let worker = Worker::with_models(root.join("models"));
        let (release_tx, release_rx) = channel::<()>();
        worker
            .request(json!({"op":"status"}), move |_| {
                let _ = release_rx.recv_timeout(Duration::from_secs(10));
            })
            .unwrap();
        let mut accepted = 0;
        let mut busy = false;
        for _ in 0..QUEUE_CAPACITY + 4 {
            match worker.request(json!({"op":"status"}), |_| {}) {
                Ok(()) => accepted += 1,
                Err(error) => {
                    assert_eq!(error, "worker_busy");
                    busy = true;
                    break;
                }
            }
        }
        assert!(busy && accepted <= QUEUE_CAPACITY + 1);
        release_tx.send(()).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn dropping_the_worker_cancels_queued_work() {
        let root = scratch("drop");
        let worker = Worker::with_models(root.join("models"));
        let shared = worker.inner.shared.clone();
        call(&worker, json!({"op":"status"})).unwrap();
        drop(worker);
        let state = lock(&shared);
        assert!(state.shutdown);
        assert!(!state.running);
        let _ = std::fs::remove_dir_all(root);
    }
}
