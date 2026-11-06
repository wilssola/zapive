// The command loop: runs every Cmd against the live session.
use super::*;

// What every command handler may need from the loop.
pub(super) struct Ctx {
    pub(super) client: Arc<Client>,
    // The epoch the command was received under; see SESSION_EPOCH.
    pub(super) epoch: u64,
    pub(super) media_key: crate::vault::KeyHandle,
    pub(super) media_sem: Arc<tokio::sync::Semaphore>,
    pub(super) avatar_sem: Arc<tokio::sync::Semaphore>,
}

pub(super) async fn executor(
    mut session: Session,
    mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
    cmd_tx: mpsc::UnboundedSender<Cmd>,
) {
    let mut media_key = crate::vault::KeyHandle::default();
    let media_sem = Arc::new(tokio::sync::Semaphore::new(3));
    let avatar_sem = Arc::new(tokio::sync::Semaphore::new(8));
    while let Some(cmd) = cmd_rx.recv().await {
        let epoch = SESSION_EPOCH.load(Ordering::SeqCst);
        // Built per command, not kept across them: a Ctx that outlives the
        // command would hold the client of a session that was just replaced
        // (logout, account switch) and keep its database open.
        let ctx = Ctx {
            client: session.client.clone(),
            epoch,
            media_key: media_key.clone(),
            media_sem: media_sem.clone(),
            avatar_sem: avatar_sem.clone(),
        };
        match cmd {
            cmd @ (Cmd::PrepareAttachment { .. }
            | Cmd::ResendMedia { .. }
            | Cmd::SendFile { .. }
            | Cmd::SendGifUrl { .. }) => attachments::run(&ctx, cmd).await,
            cmd @ (Cmd::Media { .. } | Cmd::LinkThumb { .. }) => media::run(&ctx, cmd).await,
            cmd @ (Cmd::React { .. }
            | Cmd::Revoke { .. }
            | Cmd::Edit { .. }
            | Cmd::PinMessage { .. }
            | Cmd::MarkUnread { .. }
            | Cmd::Star { .. }
            | Cmd::MarkRead { .. }
            | Cmd::MarkPlayed { .. }
            | Cmd::Forward { .. }
            | Cmd::DeleteForMe { .. }
            | Cmd::SendVoice { .. }
            | Cmd::SendQueued { .. }) => messaging::run(&ctx, cmd).await,
            cmd @ (Cmd::CreatePoll { .. } | Cmd::DecryptVote { .. } | Cmd::PollVote { .. }) => {
                polls::run(&ctx, cmd).await
            }
            cmd @ (Cmd::RejectCall { .. }
            | Cmd::AcceptCall { .. }
            | Cmd::StartCall { .. }
            | Cmd::HangupCall
            | Cmd::SetCallVideo { .. }
            | Cmd::SetCallMuted { .. }) => calls::run(&ctx, cmd).await,
            cmd @ (Cmd::FetchChannel { .. }
            | Cmd::FetchChatInfo { .. }
            | Cmd::FetchAvatar { .. }
            | Cmd::StatusPoster { .. }
            | Cmd::AvatarLarge { .. }
            | Cmd::SubscribePresence { .. }) => profile::run(&ctx, cmd).await,
            cmd @ (Cmd::Archive { .. }
            | Cmd::PinChat { .. }
            | Cmd::Mute { .. }
            | Cmd::Unmute { .. }
            | Cmd::SetEphemeral { .. }
            | Cmd::ClearChat { .. }
            | Cmd::DeleteChat { .. }
            | Cmd::LabelCreate { .. }
            | Cmd::LabelDelete { .. }
            | Cmd::LabelChat { .. }
            | Cmd::Favorites { .. }
            | Cmd::ShareHistory { .. }
            | Cmd::FetchHistory { .. }) => chats::run(&ctx, cmd).await,
            cmd @ (Cmd::FetchGroupMeta { .. }
            | Cmd::LeaveGroup { .. }
            | Cmd::AddMembers { .. }
            | Cmd::FetchCommonGroups { .. }
            | Cmd::FetchGroups) => groups::run(&ctx, cmd).await,
            Cmd::MediaKey(key) => {
                media_key = key;
            }
            Cmd::Start | Cmd::Resume => {
                let client = session.client.clone();
                tokio::spawn(async move { client.run().await });
            }
            Cmd::PairWithCode(digits) => {
                let client = session.client.clone();
                tokio::spawn(async move {
                    let options = PairCodeOptions {
                        phone_number: digits,
                        ..Default::default()
                    };
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
            Cmd::SwitchAccount(account) => {
                // A call cannot follow the user to another account.
                if let Some(live) = take_live() {
                    let _ = session
                        .client
                        .voip()
                        .terminate(&live.id, &live.peer, &live.creator)
                        .await;
                    live.handle.hangup_local().await;
                }
                session.client.disconnect().await;
                let previous = session.account.clone();
                drop(ctx);
                drop(session);
                SESSION_EPOCH.fetch_add(1, Ordering::SeqCst);
                let opened = match build_session(cmd_tx.clone(), &account).await {
                    Ok(next) => Ok(next),
                    Err(e) => {
                        eprintln!("[wa] cannot open account {account:?}: {e}");
                        build_session(cmd_tx.clone(), &previous).await
                    }
                };
                match opened {
                    Ok(next) => {
                        session = next;
                        let account = session.account.clone();
                        let registered = session.client.pn().is_some();
                        let epoch = SESSION_EPOCH.load(Ordering::SeqCst);
                        // Not through the epoch-stamped path on purpose:
                        // this is the message that opens the new epoch.
                        crate::bridge::ui_apply(move |b| {
                            b.on_account_ready(&account, registered, epoch)
                        });
                    }
                    Err(e) => {
                        eprintln!("[wa] session rebuild failed: {e}");
                        let msg = t("status.connect_failed");
                        crate::bridge::ui_apply(move |b| b.on_status(&msg));
                        return;
                    }
                }
            }
            Cmd::ResetSession => {
                session.client.disconnect().await;
                let account = session.account.clone();
                drop(ctx);
                drop(session);
                if !discard_session_store(&wa_session_path(&account), 40).await {
                    // The old credentials are still on disk, so the client
                    // built below logs in as the removed account, gets 401
                    // and asks for another reset instead of showing a QR.
                    eprintln!("[wa] could not remove the logged-out session of {account:?}");
                }
                match build_session(cmd_tx.clone(), &account).await {
                    Ok(next) => {
                        session = next;
                        ui_apply(|b| b.on_logged_out());
                        let client = session.client.clone();
                        tokio::spawn(async move { client.run().await });
                    }
                    Err(e) => {
                        eprintln!("[wa] session rebuild failed: {e}");
                        let msg = t("status.connect_failed");
                        ui_apply(move |b| b.on_status(&msg));
                        return;
                    }
                }
            }
            Cmd::Shutdown => {
                // Leaving a call ringing on the peer's phone after the app
                // is gone would be the worst kind of goodbye.
                if let Some(live) = take_live() {
                    let _ = session
                        .client
                        .voip()
                        .terminate(&live.id, &live.peer, &live.creator)
                        .await;
                    live.handle.hangup_local().await;
                }
                session.client.disconnect().await;
                return;
            }
        }
    }
}

// Deletes a logged-out account's whatsapp-rust database so the next client
// starts unpaired and shows a QR code. The old client's tasks may still be
// winding down and Windows will not delete a database that is open, so the
// removal is retried (250 ms apart) before giving up.
async fn discard_session_store(path: &std::path::Path, attempts: u32) -> bool {
    for _ in 0..attempts {
        if crate::paths::remove_db_checked(path) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    crate::paths::remove_db_checked(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("zapive-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn logout_discards_the_session_database_and_its_sidecars() {
        let dir = scratch("discard");
        let db = dir.join("wa-test.db");
        for suffix in ["", "-wal", "-shm"] {
            std::fs::write(dir.join(format!("wa-test.db{suffix}")), b"x").unwrap();
        }
        assert!(discard_session_store(&db, 1).await);
        for suffix in ["", "-wal", "-shm"] {
            assert!(!dir.join(format!("wa-test.db{suffix}")).exists());
        }
        // Nothing left is not a failure either: a second reset is harmless.
        assert!(discard_session_store(&db, 1).await);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_session_that_cannot_be_removed_is_reported() {
        let dir = scratch("stuck");
        let db = dir.join("wa-stuck.db");
        // A directory cannot be unlinked like a file, which stands in for a
        // database another handle still holds open.
        std::fs::create_dir_all(&db).unwrap();
        assert!(!discard_session_store(&db, 1).await);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
