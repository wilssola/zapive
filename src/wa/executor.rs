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
    let mut ctx = Ctx {
        client: session.client.clone(),
        epoch: 0,
        media_key: crate::vault::KeyHandle::default(),
        media_sem: Arc::new(tokio::sync::Semaphore::new(3)),
        avatar_sem: Arc::new(tokio::sync::Semaphore::new(8)),
    };
    while let Some(cmd) = cmd_rx.recv().await {
        let epoch = SESSION_EPOCH.load(Ordering::SeqCst);
        ctx.epoch = epoch;
        ctx.client = session.client.clone();
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
                ctx.media_key = key;
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
                drop(session);
                crate::paths::remove_db(&wa_session_path(&account));
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
