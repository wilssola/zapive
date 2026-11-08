// Opening a whatsapp-rust client for one account.
use super::*;

pub(super) struct Session {
    // Keeps the bot's background workers (history sync intake) alive.
    pub(super) _handle: BotHandle,
    pub(super) client: Arc<Client>,
    // Whose session this is (the id in the vault's account registry).
    pub(super) account: String,
}

// How this device shows up under "Linked devices" on the phone. The
// library's default is platform UNKNOWN with the os "rust", which the
// phone renders as "Other device" with a question mark. DESKTOP plus a
// real OS name is what the desktop apps send, and gets the OS's own
// name and icon. Spelled the way WhatsApp's UA parser spells them, since
// the pair-code handshake rejects an os it does not know. Read by the
// server at pairing only: a session paired before this shows the old
// name until it is linked again.
pub(super) fn device_identity() -> whatsapp_rust::wacore::store::DevicePropsOverride {
    let os = if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "Mac OS"
    } else {
        "Linux"
    };
    whatsapp_rust::wacore::store::DevicePropsOverride::new()
        .with_os(os)
        .with_platform_type(wa::device_props::PlatformType::DESKTOP)
}

pub(super) async fn build_session(
    cmd_tx: mpsc::UnboundedSender<Cmd>,
    account: &str,
    epoch: u64,
) -> Result<Session, Box<dyn std::error::Error + Send + Sync>> {
    let db_url = wa_session_path(account).to_string_lossy().into_owned();
    let store = SqliteStore::new(&db_url).await?;
    let bot = Bot::builder()
        .with_backend(store)
        .with_device_props(device_identity())
        .build()
        .await?;
    let client = bot.client();
    client
        .subscribe_handler(Arc::new(Pump {
            client: Arc::downgrade(&client),
            cmd_tx,
            epoch,
            rt: tokio::runtime::Handle::current(),
            failures: AtomicU32::new(0),
            stopped: AtomicBool::new(false),
        }))
        .detach();
    // spawn() wires the sync worker and starts client.run(); the run loop
    // itself ends on disconnect, but the workers stay up, so later resumes
    // just spawn client.run() again.
    let handle = bot.spawn();
    Ok(Session {
        _handle: handle,
        client,
        account: account.to_string(),
    })
}

pub(super) fn parse_jid(jid: &str) -> Option<whatsapp_rust::Jid> {
    jid.parse().ok()
}
