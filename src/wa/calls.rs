// Call state shared between the event pump and the executor, and the
// media pipeline of a live call.
use super::*;

// Offers waiting for the user to pick up. Answering needs the offer
// itself (it carries the encrypted callKey and the relay), and the event
// that delivered it is long gone by then. Only a handful can ring at
// once, so a small vector is plenty.
pub(super) static PENDING_OFFERS: std::sync::LazyLock<
    std::sync::Mutex<Vec<(String, IncomingCall)>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(Vec::new()));

// The call whose media plane is live, if any. Mute and hangup reach it
// from the UI thread through the command channel.
pub(super) static LIVE_CALL: std::sync::LazyLock<std::sync::Mutex<Option<LiveCall>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

pub(super) struct LiveCall {
    pub(super) id: String,
    pub(super) handle: Arc<CallHandle>,
    // Mirrors the handle's mute so the capture side can send silence
    // instead of the room.
    pub(super) muted: Arc<AtomicBool>,
    pub(super) peer: whatsapp_rust::Jid,
    pub(super) creator: whatsapp_rust::Jid,
    // Dropping this closes the camera and stops the encoder.
    pub(super) camera: Option<crate::camera::CameraFeed>,
    // The peer asked to add video and is waiting on an answer; accepting
    // needs this exact token.
    pub(super) upgrade: Option<VideoUpgradeToken>,
}

pub(super) fn stash_offer(id: &str, offer: IncomingCall) {
    let Ok(mut offers) = PENDING_OFFERS.lock() else {
        return;
    };
    offers.retain(|(known, _)| known != id);
    offers.push((id.to_string(), offer));
    if offers.len() > 8 {
        offers.remove(0);
    }
}

pub(super) fn take_offer(id: &str) -> Option<IncomingCall> {
    let mut offers = PENDING_OFFERS.lock().ok()?;
    let at = offers.iter().position(|(known, _)| known == id)?;
    Some(offers.remove(at).1)
}

pub(super) fn take_live() -> Option<LiveCall> {
    LIVE_CALL.lock().ok()?.take()
}

// Hanging up during setup: the handle only exists once accept/call has
// come back, so the request is parked here and the call tears itself
// down the moment it is connected.
pub(super) static CANCEL_PENDING: AtomicBool = AtomicBool::new(false);

// The device opens (cpal and the camera) block until the driver answers,
// which on Windows is routinely hundreds of milliseconds. Doing that on a
// runtime worker steals it from the transport at exactly the moment the
// offer needs to go out, so both run on the blocking pool.
pub(super) async fn open_audio() -> Option<crate::call::CallAudio> {
    tokio::task::spawn_blocking(crate::call::CallAudio::start)
        .await
        .ok()
        .flatten()
}

pub(super) async fn open_camera()
-> Option<(crate::camera::CameraFeed, async_channel::Sender<VideoFrame>)> {
    tokio::task::spawn_blocking(open_video).await.ok().flatten()
}

// Opens the camera and the decode side of a video call. The returned
// sender goes to the voip facade; the receiver is drained by a decode
// thread that pushes finished pictures at the call screen.
pub(super) fn open_video() -> Option<(crate::camera::CameraFeed, async_channel::Sender<VideoFrame>)>
{
    use crate::camera::{PREVIEW_BUSY, REMOTE_BUSY};
    // Deep, unlike the capture side. The engine drops an access unit the
    // sink cannot take, and a dropped unit is not one lost frame: every
    // later frame is built on it, so the picture tears along a slice edge
    // and stays torn until the next IDR. A few seconds of slack rides out
    // a decode hiccup, and access units are kilobytes.
    let (sink_tx, sink_rx) = async_channel::bounded::<VideoFrame>(64);
    let camera = crate::camera::CameraFeed::start(Some(&crate::call::devices().camera), |pic| {
        // One frame in flight at a time. Without this a UI thread that
        // falls behind would queue megabytes of RGBA per second.
        if PREVIEW_BUSY.swap(true, Ordering::AcqRel) {
            return;
        }
        crate::bridge::ui_apply(move |b| b.on_call_preview(pic));
    })?;
    let spawned = std::thread::Builder::new()
        .name("zapive-video-decode".into())
        .spawn(move || {
            let Some(mut remote) = crate::camera::RemoteVideo::new() else {
                log::error!("[call] no H.264 decoder; peer video will not show");
                return;
            };
            // Every access unit must be decoded to keep the reference
            // frames intact; only the handoff to the UI is skippable.
            while let Ok(frame) = sink_rx.recv_blocking() {
                let picture = remote.decode(&frame);
                // The decoder has nothing to build on and cannot fix that
                // by itself: only an IDR from the peer restarts it. The
                // engine throttles, so asking on every loss is the way
                // this is meant to be used.
                if remote.take_lost()
                    && let Ok(live) = LIVE_CALL.lock()
                    && let Some(call) = live.as_ref()
                {
                    call.handle
                        .request_peer_keyframe(KeyframeUrgency::Immediate);
                }
                let Some(pic) = picture else { continue };
                if REMOTE_BUSY.swap(true, Ordering::AcqRel) {
                    continue;
                }
                crate::bridge::ui_apply(move |b| b.on_call_remote_video(pic));
            }
        });
    if let Err(e) = spawned {
        log::error!("[call] cannot start the video decoder: {e}");
        return None;
    }
    Some((camera, sink_tx))
}

// Owns a connected call until it ends: publishes the state the call
// screen renders, drains the engine's diagnostics, and tears the audio
// devices down on the way out.
pub(super) async fn run_call(
    handle: CallHandle,
    audio: crate::call::CallAudio,
    camera: Option<crate::camera::CameraFeed>,
    outgoing: bool,
) {
    use whatsapp_rust::voip::CallEvent;
    let id = handle.call_id().to_string();
    let handle = Arc::new(handle);
    let has_video = camera.is_some();
    if let Ok(mut live) = LIVE_CALL.lock() {
        *live = Some(LiveCall {
            id: id.clone(),
            handle: handle.clone(),
            muted: audio.muted_flag(),
            peer: handle.peer_jid(),
            creator: handle.call_creator().clone(),
            camera,
            upgrade: None,
        });
    }
    if has_video {
        let id = id.clone();
        crate::bridge::ui_apply(move |b| b.on_call_video(&id, true));
    }
    // The event stream is a single shared queue; drain it so the engine
    // never stalls on a full channel, and surface the terminal ones.
    // The user hung up while the handshake was still running.
    if CANCEL_PENDING.swap(false, Ordering::SeqCst) {
        handle.hangup_local().await;
        // The entry above is this call's; leaving it behind would make
        // the next hangup reach for a call that is already gone.
        if let Ok(mut live) = LIVE_CALL.lock()
            && live.as_ref().is_some_and(|call| call.id == id)
        {
            *live = None;
        }
        return;
    }
    // Whether the media plane ever came up. It does not say whether the
    // offer reached the peer -- a call that rang and went unanswered
    // never gets here either -- so it is a diagnostic, not a verdict.
    let connected = Arc::new(AtomicBool::new(false));
    let events = handle.events();
    let watched = id.clone();
    let allocated = connected.clone();
    tokio::spawn(async move {
        // Said once: the peer reports on a cadence, and the fact is
        // whether it ever happened, not how often.
        let mut seen_video_report = false;
        let mut reports = 0u32;
        while let Ok(event) = events.recv().await {
            match event {
                CallEvent::RelayAllocated => {
                    allocated.store(true, Ordering::SeqCst);
                    let id = watched.clone();
                    crate::bridge::ui_apply(move |b| b.on_call_media_ready(&id));
                }
                CallEvent::RelayAllocateFailed(code) => {
                    log::warn!("[call] relay refused the allocate ({code})");
                }
                CallEvent::RelayAllocateTimedOut | CallEvent::RelayReconnectTimedOut => {
                    log::warn!("[call] relay went unresponsive");
                }
                // A reconnect clears the send queue and holds video back
                // until the next IDR, without counting what it discarded.
                CallEvent::MediaSetupFailed(reason) => {
                    log::warn!("[call] media setup failed: {reason}");
                }
                CallEvent::AudioFormatMismatch { expected_rate, .. } => {
                    log::warn!("[call] peer picked a rate other than {expected_rate}");
                }
                // The peer wants to add video, or has answered our own
                // request. A request is parked until the user agrees: the
                // camera must never come on by itself.
                CallEvent::VideoStateChanged {
                    state,
                    upgrade_token,
                    ..
                } => {
                    log::info!("[call] the peer's video state is now {state:?}");
                    if state.is_upgrade_request() {
                        if let Ok(mut live) = LIVE_CALL.lock()
                            && let Some(call) = live.as_mut()
                            && call.id == watched
                        {
                            call.upgrade = upgrade_token;
                        }
                        let id = watched.clone();
                        crate::bridge::ui_apply(move |b| b.on_call_video_requested(&id));
                    } else if state.is_inactive_for_call_mode() {
                        let id = watched.clone();
                        crate::bridge::ui_apply(move |b| b.on_call_peer_video(&id, false));
                    } else {
                        let id = watched.clone();
                        crate::bridge::ui_apply(move |b| b.on_call_peer_video(&id, true));
                    }
                }
                // Our own encoder has to produce an IDR: the peer asked
                // for one over RTCP, or the engine has outbound video it
                // cannot start without one. Until it arrives the engine
                // drops every frame we hand it.
                CallEvent::VideoKeyframeNeeded => {
                    log::info!("[call] the engine wants an IDR from our encoder");
                    if let Ok(live) = LIVE_CALL.lock()
                        && let Some(camera) = live.as_ref().and_then(|c| c.camera.as_ref())
                    {
                        camera.request_keyframe();
                    }
                }
                // The relay could not take what we produced, so the peer
                // sees a gap we caused.
                CallEvent::OutboundMediaDropped {
                    video_access_units,
                    packets,
                } => {
                    log::warn!(
                        "[call] the relay shed {video_access_units} access unit(s) and {packets} packet(s)"
                    );
                }
                // What the peer says back is the only account from outside
                // this process of whether our streams are landing: it names
                // the SSRCs it is reporting on, so a video SSRC that never
                // appears means our picture is not arriving. Logged in full
                // for the first few, then only on the first video mention.
                CallEvent::RtcpReceived {
                    reports_video,
                    reports_audio,
                    referenced_ssrcs,
                    packet_types,
                    ..
                } => {
                    if reports < 15 {
                        reports += 1;
                        log::info!(
                            "[call] peer rtcp {packet_types:?} about {referenced_ssrcs:?} (audio {reports_audio}, video {reports_video})"
                        );
                    }
                    if !seen_video_report && reports_video {
                        seen_video_report = true;
                        log::info!(
                            "[call] the peer reports on our video ({} ssrc(s), rtcp {packet_types:?})",
                            referenced_ssrcs.len()
                        );
                    }
                }
                _ => {}
            }
        }
    });
    // The callee is already talking once accept() returns; the caller
    // waits for the peer's <accept> stanza.
    if !outgoing {
        let id = id.clone();
        crate::bridge::ui_apply(move |b| b.on_call_status(&id, "accept"));
    }
    handle.wait_ended().await;
    drop(audio);
    if let Ok(mut live) = LIVE_CALL.lock()
        && live.as_ref().is_some_and(|call| call.id == id)
    {
        *live = None;
    }
    // A call the peer let ring out ends here too, so this is not a
    // failure to report -- but it is the line that separates "nobody
    // answered" from "the media never connected" when reading the log.
    if !connected.load(Ordering::SeqCst) {
        log::info!("[call] {id} ended without the media plane coming up");
    }
    let ended = id.clone();
    crate::bridge::ui_apply(move |b| b.on_call_status(&ended, "terminate"));
}

// Both call paths fail the same way: tell the user, and make sure the
// call screen does not sit there pretending to connect.
pub(super) fn fail_call(reason: &str) {
    let reason = reason.to_string();
    crate::bridge::ui_apply(move |b| b.on_call_failed(&reason));
}

pub(super) async fn run(ctx: &Ctx, cmd: Cmd) {
    match cmd {
        Cmd::RejectCall { id, from } => {
            let client = ctx.client.clone();
            tokio::spawn(async move {
                // The parked offer carries the creator and the ringing
                // generation, which a bare id cannot reconstruct.
                if let Some(offer) = take_offer(&id) {
                    if let Err(e) = client.voip().reject(&offer).await {
                        log::warn!("[wa] call reject failed: {e}");
                    }
                    return;
                }
                let Some(peer) = parse_jid(&from) else { return };
                if let Err(e) = client.voip().reject_call(&id, &peer, &peer).await {
                    log::warn!("[wa] call reject failed: {e}");
                }
            });
        }
        Cmd::AcceptCall { id, video } => {
            let client = ctx.client.clone();
            CANCEL_PENDING.store(false, Ordering::SeqCst);
            tokio::spawn(async move {
                let Some(offer) = take_offer(&id) else {
                    fail_call(&t("call.gone"));
                    return;
                };
                let Some(audio) = open_audio().await else {
                    // No devices means no call; decline rather than
                    // leave the caller listening to nothing.
                    if let Err(e) = client.voip().reject(&offer).await {
                        log::warn!("[wa] call reject failed: {e}");
                    }
                    fail_call(&t("call.no_devices"));
                    return;
                };
                // Answering an audio-only offer with video is not on
                // the table: the peer never offered to receive it.
                let vision = match video {
                    true => open_camera().await,
                    false => None,
                };
                let voip = client.voip();
                let mut builder = voip.accept(&offer).audio(audio.mic(), audio.speaker());
                if let Some((camera, sink)) = &vision {
                    builder = builder.video(camera.source(), sink.clone());
                }
                // preaccept -> callKey decrypt -> accept -> relay.
                match builder.start().await {
                    Ok(handle) => {
                        run_call(handle, audio, vision.map(|(c, _)| c), false).await;
                    }
                    Err(e) => {
                        log::error!("[wa] call accept failed: {e}");
                        fail_call(&t("call.failed"));
                    }
                }
            });
        }
        Cmd::StartCall { jid, video } => {
            let client = ctx.client.clone();
            CANCEL_PENDING.store(false, Ordering::SeqCst);
            tokio::spawn(async move {
                let Some(peer) = parse_jid(&jid) else {
                    fail_call(&t("call.failed"));
                    return;
                };
                let Some(audio) = open_audio().await else {
                    fail_call(&t("call.no_devices"));
                    return;
                };
                let vision = match video {
                    true => open_camera().await,
                    false => None,
                };
                if video && vision.is_none() {
                    fail_call(&t("call.no_camera"));
                    return;
                }
                let voip = client.voip();
                let mut builder = voip.call(&peer).audio(audio.mic(), audio.speaker());
                if let Some((camera, sink)) = &vision {
                    builder = builder.video(camera.source(), sink.clone());
                }
                log::info!(
                    "[wa] placing a {} call to {peer}",
                    if vision.is_some() { "video" } else { "voice" }
                );
                match builder.start().await {
                    Ok(handle) => {
                        // The call id is minted here, so the screen
                        // only learns it now.
                        let id = handle.call_id().to_string();
                        log::info!("[wa] offer sent for call {id}; waiting for the relay");
                        crate::bridge::ui_apply(move |b| b.on_outgoing_call(&id));
                        run_call(handle, audio, vision.map(|(c, _)| c), true).await;
                    }
                    Err(e) => {
                        log::error!("[wa] outgoing call failed: {e}");
                        fail_call(&t("call.failed"));
                    }
                }
            });
        }
        Cmd::HangupCall => {
            let client = ctx.client.clone();
            tokio::spawn(async move {
                let Some(live) = take_live() else {
                    // Nothing connected yet: leave the note for the
                    // setup task to find.
                    CANCEL_PENDING.store(true, Ordering::SeqCst);
                    return;
                };
                // terminate() tears the local media down as well, but
                // hangup() is what guarantees it when the stanza
                // cannot be sent.
                if let Err(e) = client
                    .voip()
                    .terminate(&live.id, &live.peer, &live.creator)
                    .await
                {
                    log::warn!("[wa] call terminate failed: {e}");
                }
                live.handle.hangup_local().await;
            });
        }
        Cmd::SetCallVideo(on) => {
            let handle = LIVE_CALL.lock().ok().and_then(|live| {
                live.as_ref()
                    .map(|call| (call.handle.clone(), call.id.clone()))
            });
            let Some((handle, id)) = handle else { return };
            tokio::spawn(async move {
                if !on {
                    if let Err(e) = handle.stop_video().await {
                        log::warn!("[wa] stopping video failed: {e}");
                    }
                    if let Ok(mut live) = LIVE_CALL.lock()
                        && let Some(call) = live.as_mut()
                    {
                        // Dropping the feed is what releases the camera.
                        call.camera = None;
                        call.upgrade = None;
                    }
                    crate::bridge::ui_apply(move |b| b.on_call_video(&id, false));
                    return;
                }
                let Some((camera, sink)) = open_video() else {
                    fail_call(&t("call.no_camera"));
                    return;
                };
                // A parked request from the peer has to be answered with
                // its own token; anything else is a fresh upgrade.
                let token = LIVE_CALL
                    .lock()
                    .ok()
                    .and_then(|mut live| live.as_mut().and_then(|call| call.upgrade.take()));
                let started = match token {
                    Some(token) => {
                        handle
                            .accept_video(token, camera.source(), sink.clone())
                            .await
                    }
                    None => handle.start_video(camera.source(), sink.clone()).await,
                };
                if let Err(e) = started {
                    log::error!("[wa] starting video failed: {e}");
                    fail_call(&t("call.no_camera"));
                    return;
                }
                if let Ok(mut live) = LIVE_CALL.lock()
                    && let Some(call) = live.as_mut()
                {
                    call.camera = Some(camera);
                }
                crate::bridge::ui_apply(move |b| b.on_call_video(&id, true));
            });
        }
        Cmd::SetCallMuted(muted) => {
            // set_muted announces <mute_v2> to the peer, so it has to be
            // awaited; the capture side reads the flag either way.
            let live = LIVE_CALL
                .lock()
                .ok()
                .and_then(|live| live.as_ref().map(|c| (c.handle.clone(), c.muted.clone())));
            let Some((handle, flag)) = live else { return };
            flag.store(muted, Ordering::SeqCst);
            tokio::spawn(async move {
                if let Err(e) = handle.set_muted(muted).await {
                    log::warn!("[wa] the peer was not told about the mute: {e}");
                }
            });
        }
        _ => unreachable!("not a calls command"),
    }
}
