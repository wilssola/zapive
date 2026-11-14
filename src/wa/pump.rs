// Inbound events: the library's event handler, turned into UI work.
use super::*;

// What a receipt kind means for the ticks: 2 = sent to the server only
// (a downgraded delivery ack, never past that), 3 delivered, 4 read,
// 5 played; 0 = not a delivery state.
pub(crate) fn receipt_status(kind: &ReceiptType) -> u32 {
    match kind {
        ReceiptType::Sent => 2,
        ReceiptType::Delivered => 3,
        ReceiptType::Read => 4,
        ReceiptType::Played => 5,
        _ => 0,
    }
}

// Sync event tap on the library's dispatch thread: converts each event
// into UI work (via ui_apply) or a command back to the executor.
pub(super) struct Pump {
    // Weak on purpose: the pump is registered on this very client's event
    // bus, so a strong handle would be a cycle that keeps the client (and
    // its session database open) alive for the rest of the process.
    pub(super) client: std::sync::Weak<Client>,
    pub(super) cmd_tx: mpsc::UnboundedSender<Cmd>,
    // The epoch this client was built under; see SESSION_EPOCH.
    pub(super) epoch: u64,
    pub(super) rt: tokio::runtime::Handle,
    pub(super) failures: AtomicU32,
    pub(super) stopped: AtomicBool,
}

impl EventHandler for Pump {
    fn handle_event(&self, event: Arc<Event>) {
        // A client that was switched away from may still be draining.
        if self.epoch != SESSION_EPOCH.load(Ordering::SeqCst) {
            return;
        }
        match &*event {
            Event::PairingQrCode(qr) => {
                let code = qr.code.clone();
                ui_apply(move |b| b.on_qr(&code));
            }
            Event::PairSuccess(_) => {
                ui_apply(|b| b.on_status(&t("status.connecting")));
            }
            Event::Connected(_) => {
                log::info!("[wa] connected ts={}", crate::store::now_secs());
                self.failures.store(0, Ordering::Relaxed);
                self.stopped.store(false, Ordering::Relaxed);
                let Some(client) = self.client.upgrade() else {
                    return;
                };
                let pn = client
                    .pn()
                    .map(|j| j.to_non_ad_string())
                    .unwrap_or_default();
                let lid = client
                    .lid()
                    .map(|j| j.to_non_ad_string())
                    .unwrap_or_default();
                // What the account switcher calls this account.
                let name = client.push_name();
                ui_apply(move |b| b.on_open(&pn, &lid, &name));
            }
            Event::StreamReplaced(_) => {
                // WhatsApp keeps one desktop session per account: the
                // library already stopped reconnecting; ask the user.
                self.stopped.store(true, Ordering::Relaxed);
                ui_apply(|b| b.on_fatal("conflict"));
            }
            Event::LoggedOut(_) => {
                self.stopped.store(true, Ordering::Relaxed);
                ui_apply(|b| b.on_status(&t("status.session_ended")));
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
                log::info!(
                    "[wa] disconnected ts={} kind={} clean={}",
                    crate::store::now_secs(),
                    disconnect_kind(&d.reason),
                    d.reason.is_clean_shutdown()
                );
                if !d.reason.is_clean_shutdown() && !self.stopped.load(Ordering::Relaxed) {
                    self.count_failure();
                }
            }
            Event::UndecryptableMessage(u) => {
                log::info!(
                    "[wa] undecryptable ts={} msg_ts={} group={} unavailable={} chat={}",
                    crate::store::now_secs(),
                    u.info.timestamp.timestamp(),
                    u.info.source.is_group,
                    u.is_unavailable,
                    chat_tag(&u.info.source.chat.to_non_ad_string())
                );
            }
            Event::OfflineSyncPreview(p) => {
                log::info!(
                    "[wa] offline preview ts={} total={} messages={}",
                    crate::store::now_secs(),
                    p.total,
                    p.messages
                );
            }
            Event::OfflineSyncCompleted(c) => {
                log::info!(
                    "[wa] offline completed ts={} count={}",
                    crate::store::now_secs(),
                    c.count
                );
            }
            Event::OfflineSyncInterrupted(i) => {
                log::info!(
                    "[wa] offline interrupted ts={} total={} delivered={}",
                    crate::store::now_secs(),
                    i.total,
                    i.delivered
                );
            }
            Event::Messages(_) => {
                let event = event.clone();
                ui_apply(move |b| {
                    if let Event::Messages(batch) = &*event {
                        b.on_messages(batch);
                    }
                });
            }
            Event::ServerAck(ack) if ack.class.as_deref() == Some("message") => {
                let id = ack.id.clone();
                let error = ack.error.clone();
                crate::bridge::wa_apply_at(self.epoch, move |b| b.on_outbox_ack(&id, error));
            }
            Event::Receipt(receipt) => {
                match receipt.r#type {
                    ReceiptType::Retry | ReceiptType::EncRekeyRetry | ReceiptType::ServerError => {
                        eprintln!(
                            "[receipt] {:?} for {} message(s)",
                            receipt.r#type,
                            receipt.message_ids.len()
                        );
                    }
                    ReceiptType::ReadSelf | ReceiptType::PlayedSelf => {
                        // Read on another device of ours: the local unread goes away.
                        let chat = receipt.source.chat.to_non_ad_string();
                        let ids: Vec<String> =
                            receipt.message_ids.iter().map(|i| i.to_string()).collect();
                        ui_apply(move |b| b.on_read_elsewhere(&chat, &ids));
                        return;
                    }
                    _ => {}
                }
                let status = receipt_status(&receipt.r#type);
                if status == 0 {
                    return;
                }
                let chat = receipt.source.chat.to_non_ad_string();
                let ids: Vec<String> = receipt.message_ids.iter().map(|i| i.to_string()).collect();
                let participant = receipt.source.sender.to_non_ad_string();
                let timestamp = receipt.timestamp.timestamp();
                let alternate = receipt
                    .source
                    .sender_alt
                    .as_ref()
                    .map(|jid| jid.to_non_ad_string());
                ui_apply(move |b| {
                    b.on_receipt(
                        &chat,
                        &ids,
                        &participant,
                        alternate.as_deref(),
                        status,
                        timestamp,
                    )
                });
            }
            Event::HistorySync(_lazy) => {
                // Decoding inflates megabytes of protobuf: keep it off the
                // event bus thread and off the UI.
                let event = event.clone();
                let epoch = self.epoch;
                self.rt.spawn(async move {
                    let Event::HistorySync(lazy) = &*event else {
                        return;
                    };
                    let Some(hs) = lazy.get() else {
                        eprintln!("[history] chunk failed to decode");
                        return;
                    };
                    let on_demand = lazy.peer_data_request_session_id().is_some()
                        || hs.sync_type == wa::history_sync::HistorySyncType::OnDemand;
                    let mut chunk = HistoryChunk {
                        on_demand,
                        progress: hs.progress,
                        chats: Vec::new(),
                        messages: Vec::new(),
                        pushnames: Vec::new(),
                    };
                    for conv in &hs.conversations {
                        chunk.chats.push(HistChat {
                            jid: conv.id.clone(),
                            name: conv
                                .name
                                .clone()
                                .or_else(|| conv.display_name.clone())
                                .filter(|n| !n.is_empty()),
                            timestamp: conv.conversation_timestamp.unwrap_or(0) as i64,
                            unread: conv.unread_count,
                            pinned: history_pin(conv.pinned),
                            muted: history_mute(conv.mute_end_time),
                            archived: conv.archived,
                        });
                        for hm in &conv.messages {
                            if let Some(web) = hm.message.as_option() {
                                chunk.messages.push(web.clone());
                            }
                        }
                    }
                    for pn in &hs.pushnames {
                        if let (Some(id), Some(name)) = (pn.id.clone(), pn.pushname.clone()) {
                            chunk.pushnames.push((id, name));
                        }
                    }
                    // Decoding takes long enough for an account switch
                    // to slip in, so the chunk carries the epoch of the
                    // client it came from, not of whoever is current by
                    // the time it is ready.
                    crate::bridge::wa_apply_at(epoch, move |b| b.on_history_chunk(chunk));
                });
            }
            // Every <call> stanza arrives as IncomingCall, not just the
            // offers: preaccept, accept, reject, terminate and the
            // transport chatter all land here. Only an <offer> may ring.
            Event::IncomingCall(call) => {
                let from = call.from.to_non_ad_string();
                let id = call.action.call_id().to_string();
                let ts = call.timestamp.timestamp();
                match &call.action {
                    CallAction::Offer { is_video, .. } => {
                        // An offer replayed from the offline queue is long
                        // dead. The library turns most of those into
                        // MissedCall already; this is the belt to that
                        // brace, since ringing for one is the worst
                        // possible outcome.
                        if call.offline {
                            ui_apply(move |b| b.on_call_missed(&id, &from, ts));
                            return;
                        }
                        let video = *is_video
                            || call.notify.as_deref().is_some_and(|n| n.contains("video"));
                        let group = call.group.is_some();
                        // Answering needs the offer itself (the encrypted
                        // callKey and the relay live on it), so park it
                        // until the user picks up or it goes stale.
                        stash_offer(&id, (**call).clone());
                        ui_apply(move |b| b.on_incoming_call(&id, &from, video, group, ts));
                    }
                    CallAction::Accept { .. } => {
                        ui_apply(move |b| b.on_call_status(&id, "accept"));
                    }
                    // A reason means one of the peer's devices bowed out
                    // (`busy`), not that the person declined: the rest of
                    // their devices keep ringing, so the call is still on.
                    CallAction::Reject { reason: None, .. } => {
                        ui_apply(move |b| b.on_call_status(&id, "reject"));
                    }
                    CallAction::Terminate { .. } => {
                        ui_apply(move |b| b.on_call_status(&id, "terminate"));
                    }
                    _ => {}
                }
            }
            Event::MissedCall(missed) => {
                let id = missed.call_id.clone();
                let from = missed.from.to_non_ad_string();
                let ts = missed.timestamp.timestamp();
                ui_apply(move |b| b.on_call_missed(&id, &from, ts));
            }
            // Another of our devices picked the call up (or turned it
            // down); either way this one stops ringing.
            Event::CallEndedElsewhere(elsewhere) => {
                let id = elsewhere.call_id.clone();
                let status = match elsewhere.outcome {
                    ElsewhereOutcome::Rejected => "reject",
                    // Accepted, and whatever outcome the server adds next.
                    _ => "accept",
                };
                ui_apply(move |b| b.on_call_status(&id, status));
            }
            Event::ChatPresence(update) => {
                let chat = update.source.chat.to_non_ad_string();
                let sender = update.source.sender.to_non_ad_string();
                let state = format!("{:?}", update.state);
                let media = format!("{:?}", update.media);
                ui_apply(move |b| b.on_chat_presence(&chat, &sender, &state, &media));
            }
            Event::Presence(update) => {
                let from = update.from.to_non_ad_string();
                let available = !update.unavailable;
                ui_apply(move |b| b.on_presence(&from, available));
            }
            Event::PinUpdate(u) => {
                let jid = u.jid.to_non_ad_string();
                let pinned = u.action.pinned.unwrap_or(false);
                let stamp = pin_stamp(pinned, u.timestamp.timestamp());
                ui_apply(move |b| b.on_chat_flag(&jid, Some(stamp), None));
            }
            Event::ArchiveUpdate(u) => {
                let jid = u.jid.to_non_ad_string();
                let archived = u.action.archived.unwrap_or(false);
                ui_apply(move |b| b.on_chat_flag(&jid, None, Some(archived)));
            }
            Event::StarUpdate(u) => {
                let jid = u.chat_jid.to_non_ad_string();
                let id = u.message_id.clone();
                let starred = u.action.starred.unwrap_or(false);
                ui_apply(move |b| b.on_star(&jid, &id, starred));
            }
            Event::MarkChatAsReadUpdate(u) => {
                let jid = u.jid.to_non_ad_string();
                let read = u.action.read.unwrap_or(true);
                ui_apply(move |b| b.on_mark_read(&jid, read));
            }
            Event::MuteUpdate(u) => {
                let jid = u.jid.to_non_ad_string();
                let muted = u.action.muted.unwrap_or(false);
                // -1 = indefinite, otherwise an expiry in ms.
                let until = u.action.mute_end_timestamp.unwrap_or(0);
                let muted = if !muted {
                    0
                } else if until <= 0 {
                    -1
                } else {
                    until / 1000
                };
                ui_apply(move |b| b.on_mute(&jid, muted));
            }
            Event::ClearChatUpdate(u) => {
                let jid = u.jid.to_non_ad_string();
                let cutoff = u.action.message_range.as_option().and_then(|r| {
                    range_cutoff(r.last_message_timestamp, r.last_system_message_timestamp)
                });
                ui_apply(move |b| b.on_chat_cleared(&jid, false, cutoff));
            }
            Event::DeleteChatUpdate(u) => {
                let jid = u.jid.to_non_ad_string();
                let cutoff = u.action.message_range.as_option().and_then(|r| {
                    range_cutoff(r.last_message_timestamp, r.last_system_message_timestamp)
                });
                ui_apply(move |b| b.on_chat_cleared(&jid, true, cutoff));
            }
            Event::LabelEditUpdate(u) => {
                let id = u.label_id.clone();
                let name = u.action.name.clone().unwrap_or_default();
                let color = u.action.color.unwrap_or(0);
                let order = u.action.order_index.unwrap_or(0);
                let deleted = u.action.deleted.unwrap_or(false);
                let list_type = u.action.r#type.map(|kind| kind as i32);
                ui_apply(move |b| b.on_label(&id, &name, color, order, deleted, list_type));
            }
            Event::LabelAssociationUpdate(u) => {
                let id = u.label_id.clone();
                let jid = u.chat_jid.to_non_ad_string();
                let on = u.action.labeled.unwrap_or(false);
                ui_apply(move |b| b.on_label_chat(&id, &jid, on));
            }
            Event::GroupUpdate(u) => {
                use whatsapp_rust::wacore::stanza::groups::GroupNotificationAction as A;
                let jid = u.group_jid.to_non_ad_string();
                let actor = u.participant.as_ref().map(|j| j.to_non_ad_string());
                let actor_pn = u.participant_pn.as_ref().map(|j| j.to_non_ad_string());
                let ts = u.timestamp.timestamp();
                let people =
                    |list: &[whatsapp_rust::wacore::stanza::groups::GroupParticipantInfo]| {
                        list.iter()
                            .map(|p| {
                                (
                                    p.jid.to_non_ad_string(),
                                    p.phone_number.as_ref().map(|j| j.to_non_ad_string()),
                                )
                            })
                            .collect::<Vec<_>>()
                    };
                let mut change = GroupChange {
                    jid,
                    actor,
                    actor_pn,
                    ts,
                    kind: "",
                    who: Vec::new(),
                    text: String::new(),
                    number: 0,
                    reason: String::new(),
                };
                match &*u.action {
                    A::Add {
                        participants,
                        reason,
                    } => {
                        change.kind = "add";
                        change.who = people(participants);
                        change.reason = reason.clone().unwrap_or_default();
                    }
                    A::Remove { participants, .. } => {
                        change.kind = "remove";
                        change.who = people(participants);
                    }
                    A::Promote { participants } => {
                        change.kind = "promote";
                        change.who = people(participants);
                    }
                    A::Demote { participants } => {
                        change.kind = "demote";
                        change.who = people(participants);
                    }
                    A::Subject { subject, .. } => {
                        change.kind = "subject";
                        change.text = subject.clone();
                    }
                    A::Description { description, .. } => {
                        change.kind = "description";
                        change.text = description.clone().unwrap_or_default();
                    }
                    A::Announce => change.kind = "announce",
                    A::NotAnnounce => change.kind = "not_announce",
                    A::Ephemeral { expiration, .. } => {
                        change.kind = "ephemeral";
                        change.number = *expiration;
                    }
                    A::Locked { .. } => change.kind = "locked",
                    A::Unlocked => change.kind = "unlocked",
                    A::Delete { .. } => change.kind = "delete",
                    A::Invite { .. } => change.kind = "invite",
                    _ => return,
                }
                ui_apply(move |b| b.on_group_update(change));
            }
            Event::ContactUpdate(u) => {
                let jid = u.jid.to_non_ad_string();
                let name = u
                    .action
                    .full_name
                    .clone()
                    .or_else(|| u.action.first_name.clone())
                    .unwrap_or_default();
                if !name.is_empty() {
                    ui_apply(move |b| b.on_contact(&jid, &name));
                }
            }
            Event::BusinessStatusUpdate(u) => {
                let Some(name) = u.verified_name.clone().filter(|n| !n.trim().is_empty()) else {
                    return;
                };
                let jid = u.jid.to_non_ad_string();
                ui_apply(move |b| b.on_verified_name(&jid, &name));
            }
            _ => {}
        }
    }
}

// Pin order for a chat: the time it was pinned, never 0 while pinned. The
// sync action carries no usable timestamp on some pins (it decodes to the
// epoch), and 0 means "not pinned" everywhere else.
pub(super) fn pin_stamp(pinned: bool, ts: i64) -> i64 {
    if !pinned {
        0
    } else if ts > 0 {
        ts
    } else {
        1
    }
}

// History sync writes 0 for every chat that is not pinned there, which
// would wipe pins already delivered by app-state sync. Only a positive
// value is news; unpinning arrives as a PinUpdate.
pub(super) fn history_pin(pinned: Option<u32>) -> Option<i64> {
    pinned.filter(|p| *p > 0).map(i64::from)
}

fn tag_salt() -> &'static [u8; 16] {
    static SALT: std::sync::OnceLock<[u8; 16]> = std::sync::OnceLock::new();
    SALT.get_or_init(|| {
        use rand::RngCore as _;
        let mut salt = [0u8; 16];
        rand::rng().fill_bytes(&mut salt);
        salt
    })
}

pub(super) fn salted_tag(salt: &[u8; 16], chat: &str) -> String {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    hasher.update(salt);
    hasher.update(chat.as_bytes());
    hasher
        .finalize()
        .iter()
        .take(3)
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn chat_tag(chat: &str) -> String {
    salted_tag(tag_salt(), chat)
}

pub fn delay_secs(now: i64, message_ts: i64) -> i64 {
    now.saturating_sub(message_ts)
}

pub(super) fn disconnect_kind(reason: &whatsapp_rust::wacore::net::DisconnectReason) -> String {
    use whatsapp_rust::wacore::net::DisconnectReason as R;
    match reason {
        R::ServerClose { code, .. } => format!("server_close:{}", code.unwrap_or(0)),
        R::StreamEnded => "stream_ended".into(),
        R::ReadError(_) => "read_error".into(),
        R::Unknown => "unknown".into(),
    }
}

const MUTE_FOREVER_HORIZON_SECS: i64 = 100 * 365 * 24 * 3600;
const MUTE_MILLIS_THRESHOLD: u64 = 100_000_000_000;

pub(super) fn history_mute(mute_end_time: Option<u64>) -> Option<i64> {
    let until = mute_end_time.filter(|until| *until > 0)?;
    let until = if until > MUTE_MILLIS_THRESHOLD {
        until / 1000
    } else {
        until
    };
    let horizon = crate::store::now_secs().saturating_add(MUTE_FOREVER_HORIZON_SECS);
    Some(match i64::try_from(until) {
        Ok(until) if until <= horizon => until,
        _ => -1,
    })
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

#[cfg(test)]
mod receipt_tests {
    use super::*;

    #[test]
    fn a_pin_without_timestamp_stays_pinned() {
        assert_eq!(pin_stamp(true, 0), 1);
        assert_eq!(pin_stamp(true, -5), 1);
        assert_eq!(pin_stamp(true, 1_700_000_000), 1_700_000_000);
        assert_eq!(pin_stamp(false, 1_700_000_000), 0);
    }

    #[test]
    fn history_sync_never_unpins() {
        assert_eq!(history_pin(None), None);
        assert_eq!(history_pin(Some(0)), None);
        assert_eq!(history_pin(Some(3)), Some(3));
    }

    #[test]
    fn history_chunk_keeps_pins_from_app_state() {
        let mut store = crate::store::Store::default();
        let jid = "5511999999999@s.whatsapp.net";
        store.upsert_chat(jid, None, 0, None, Some(pin_stamp(true, 0)), None);
        store.upsert_chat(
            jid,
            Some("Ana"),
            10,
            Some(0),
            history_pin(Some(0)),
            Some(false),
        );
        assert!(store.chats[jid].pinned > 0);
        assert_eq!(store.sorted_chats()[0].jid, jid);
    }

    #[test]
    fn diagnostic_chat_tag_is_short_stable_and_hides_the_jid() {
        let jid = "120363000000000001@g.us";
        let tag = chat_tag(jid);
        assert_eq!(tag.len(), 6);
        assert_eq!(tag, chat_tag(jid));
        assert_ne!(tag, chat_tag("120363000000000002@g.us"));
        let first: [u8; 16] = rand::random();
        let mut second: [u8; 16] = rand::random();
        second[0] = !first[0];
        assert_ne!(salted_tag(&first, jid), salted_tag(&second, jid));
        assert_eq!(salted_tag(&first, jid), salted_tag(&first, jid));
        assert_eq!(delay_secs(1_000, 940), 60);
    }

    #[test]
    fn history_sync_carries_the_previous_mute() {
        assert_eq!(history_mute(None), None);
        assert_eq!(history_mute(Some(0)), None);
        assert_eq!(history_mute(Some(4_102_444_800)), Some(4_102_444_800));
        assert_eq!(history_mute(Some(u64::MAX)), Some(-1));
        assert_eq!(history_mute(Some(i64::MAX as u64 + 1)), Some(-1));
        assert_eq!(history_mute(Some(40_000_000_000)), Some(-1));
        let in_an_hour = crate::store::now_secs() + 3600;
        assert_eq!(
            history_mute(Some(in_an_hour as u64 * 1000)),
            Some(in_an_hour),
            "a millisecond expiry must stay finite"
        );
    }

    #[test]
    fn receipt_kinds_map_to_tick_levels() {
        assert_eq!(receipt_status(&ReceiptType::Sent), 2);
        assert_eq!(receipt_status(&ReceiptType::Delivered), 3);
        assert_eq!(receipt_status(&ReceiptType::Read), 4);
        assert_eq!(receipt_status(&ReceiptType::Played), 5);
    }

    #[test]
    fn other_kinds_never_touch_the_ticks() {
        for kind in [
            ReceiptType::Retry,
            ReceiptType::ServerError,
            ReceiptType::ReadSelf,
            ReceiptType::PlayedSelf,
            ReceiptType::Sender,
            ReceiptType::Inactive,
            ReceiptType::PeerMsg,
            ReceiptType::HistorySync,
            ReceiptType::Other("x".into()),
        ] {
            assert_eq!(receipt_status(&kind), 0, "{kind:?}");
        }
    }
}
