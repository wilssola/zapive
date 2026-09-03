// The WhatsApp side of the app. Owns the whatsapp-rust client on the
// tokio runtime; the UI talks to it through the Cmd channel, and events
// flow back through bridge::ui_apply. Mirrors src/whatsapp.ts on master.
use crate::bridge::ui_apply;
use crate::i18n::t;
use crate::paths::wa_session_path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use tokio::sync::mpsc;
use whatsapp_rust::bot::{Bot, BotHandle};
use whatsapp_rust::client::Client;
use whatsapp_rust::pair_code::PairCodeOptions;
use whatsapp_rust::store::SqliteStore;
use whatsapp_rust::types::events::{Event, EventHandler};

// After this many unclean drops in a row the retry loop stops and the
// user gets the "offline" modal instead.
const MAX_FAILURES: u32 = 5;

#[derive(Debug)]
pub enum Cmd {
    // Spawns the connection loop; sent once the bridge is installed so no
    // early event is lost.
    Start,
    PairWithCode(String),
    // From the fatal modal's button: reconnect after conflict/offline.
    Resume,
    Logout,
    Shutdown,
    // Internal: wipe the session store and restart with a fresh QR.
    ResetSession,
    // Internal: too many failures — stop retrying and tell the user.
    HaltRetries,
}

#[derive(Clone)]
pub struct WaService {
    cmd_tx: mpsc::UnboundedSender<Cmd>,
}

impl WaService {
    // Builds the client (blocking briefly on the runtime to open the
    // store) and reports whether a paired session already exists.
    pub fn start(rt: &tokio::runtime::Runtime) -> Result<(Self, bool), String> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let session = rt
            .block_on(build_session(cmd_tx.clone()))
            .map_err(|e| e.to_string())?;
        let registered = session.client.pn().is_some();
        rt.spawn(executor(session, cmd_rx, cmd_tx.clone()));
        Ok((Self { cmd_tx }, registered))
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.cmd_tx.send(cmd);
    }
}

struct Session {
    // Keeps the bot's background workers (history sync intake) alive.
    _handle: BotHandle,
    client: Arc<Client>,
}

async fn build_session(
    cmd_tx: mpsc::UnboundedSender<Cmd>,
) -> Result<Session, Box<dyn std::error::Error + Send + Sync>> {
    let db_url = wa_session_path().to_string_lossy().into_owned();
    let store = SqliteStore::new(&db_url).await?;
    let bot = Bot::builder().with_backend(store).build().await?;
    let client = bot.client();
    client
        .subscribe_handler(Arc::new(Pump {
            client: client.clone(),
            cmd_tx,
            failures: AtomicU32::new(0),
            stopped: AtomicBool::new(false),
        }))
        .detach();
    // spawn() wires the sync worker and starts client.run(); the run loop
    // itself ends on disconnect, but the workers stay up, so later resumes
    // just spawn client.run() again.
    let handle = bot.spawn();
    Ok(Session { _handle: handle, client })
}

// Sync event tap on the library's dispatch thread: converts each event
// into UI work (via ui_apply) or a command back to the executor.
struct Pump {
    client: Arc<Client>,
    cmd_tx: mpsc::UnboundedSender<Cmd>,
    failures: AtomicU32,
    stopped: AtomicBool,
}

impl EventHandler for Pump {
    fn handle_event(&self, event: Arc<Event>) {
        match &*event {
            Event::PairingQrCode(qr) => {
                let code = qr.code.clone();
                ui_apply(move |b| b.on_qr(&code));
            }
            Event::PairSuccess(_) => {
                ui_apply(|b| b.on_status(&t("status.connecting")));
            }
            Event::Connected(_) => {
                self.failures.store(0, Ordering::Relaxed);
                self.stopped.store(false, Ordering::Relaxed);
                let pn = self.client.pn().map(|j| j.to_string()).unwrap_or_default();
                let lid = self.client.lid().map(|j| j.to_string()).unwrap_or_default();
                ui_apply(move |b| b.on_open(&pn, &lid));
            }
            Event::StreamReplaced(_) => {
                // WhatsApp keeps one desktop session per account: the
                // library already stopped reconnecting; ask the user.
                self.stopped.store(true, Ordering::Relaxed);
                ui_apply(|b| b.on_fatal("conflict"));
            }
            Event::LoggedOut(_) => {
                self.stopped.store(true, Ordering::Relaxed);
                ui_apply(|b| b.on_status(&t("status.sessionEnded")));
                let _ = self.cmd_tx.send(Cmd::ResetSession);
            }
            Event::ConnectFailure(f) => {
                if f.reason.is_logged_out() {
                    self.stopped.store(true, Ordering::Relaxed);
                    let _ = self.cmd_tx.send(Cmd::ResetSession);
                } else {
                    self.count_failure();
                }
            }
            Event::Disconnected(d) => {
                if !d.reason.is_clean_shutdown() && !self.stopped.load(Ordering::Relaxed) {
                    self.count_failure();
                }
            }
            _ => {}
        }
    }
}

impl Pump {
    fn count_failure(&self) {
        let n = self.failures.fetch_add(1, Ordering::Relaxed) + 1;
        if n >= MAX_FAILURES {
            self.stopped.store(true, Ordering::Relaxed);
            let _ = self.cmd_tx.send(Cmd::HaltRetries);
        } else {
            ui_apply(|b| b.on_status(&t("status.reconnecting")));
        }
    }
}

async fn executor(
    mut session: Session,
    mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    cmd_tx: mpsc::UnboundedSender<Cmd>,
) {
    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            Cmd::Start | Cmd::Resume => {
                let client = session.client.clone();
                tokio::spawn(async move { client.run().await });
            }
            Cmd::PairWithCode(digits) => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let options = PairCodeOptions { phone_number: digits, ..Default::default() };
                    match client.pair_with_code(options).await {
                        Ok(code) => {
                            let pretty = format_pair_code(&code);
                            ui_apply(move |b| b.on_pairing_code(&pretty));
                        }
                        Err(e) => {
                            eprintln!("[wa] pair code request failed: {e}");
                            ui_apply(|b| b.on_pairing_failed());
                        }
                    }
                });
            }
            Cmd::Logout => {
                // Dispatches Event::LoggedOut, which routes back here as
                // ResetSession.
                session.client.logout().await;
            }
            Cmd::HaltRetries => {
                session.client.disconnect().await;
                ui_apply(|b| b.on_fatal("offline"));
            }
            Cmd::ResetSession => {
                session.client.disconnect().await;
                drop(session);
                let path = wa_session_path();
                for suffix in ["", "-wal", "-shm"] {
                    let mut p = path.as_os_str().to_owned();
                    p.push(suffix);
                    let _ = std::fs::remove_file(std::path::PathBuf::from(p));
                }
                match build_session(cmd_tx.clone()).await {
                    Ok(next) => {
                        session = next;
                        ui_apply(|b| b.on_logged_out());
                        let client = session.client.clone();
                        tokio::spawn(async move { client.run().await });
                    }
                    Err(e) => {
                        eprintln!("[wa] session rebuild failed: {e}");
                        let msg = t("status.connectFailed");
                        ui_apply(move |b| b.on_status(&msg));
                        return;
                    }
                }
            }
            Cmd::Shutdown => {
                session.client.disconnect().await;
                return;
            }
        }
    }
}

fn format_pair_code(code: &str) -> String {
    if code.len() == 8 { format!("{}-{}", &code[..4], &code[4..]) } else { code.to_string() }
}
