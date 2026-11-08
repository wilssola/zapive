// Voice and video calls: screen, devices, actions and call events.
use super::*;

// An offer older than this never rings. The server replays queued call
// stanzas on reconnect and the locked-vault backlog replays its own on
// unlock, so without this the app announces calls that ended hours ago.
pub(super) const STALE_CALL_SECS: i64 = 75;

// How long an unanswered offer keeps ringing before it counts as missed.
pub(super) const RING_TIMEOUT_MS: u64 = 45_000;

// How long the call screen stays up after the call ends.
pub(super) const CALL_LINGER_MS: u64 = 2_000;

// Call anti-spam: once a caller's call was dismissed here, the next ones
// from that caller stay quiet for a while (the phone keeps ringing).
pub(super) const SILENCE_CHOICES: [u32; 4] = [2, 5, 10, 30];
pub(super) const SILENCE_DEFAULT_MINUTES: u32 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SilenceSetting {
    pub(super) enabled: bool,
    pub(super) minutes: u32,
}

impl Default for SilenceSetting {
    fn default() -> Self {
        Self {
            enabled: true,
            minutes: SILENCE_DEFAULT_MINUTES,
        }
    }
}

// Only the offered choices are valid; anything else falls back to 10.
pub(super) fn clamp_silence_minutes(minutes: u32) -> u32 {
    if SILENCE_CHOICES.contains(&minutes) {
        minutes
    } else {
        SILENCE_DEFAULT_MINUTES
    }
}

// Whether a new offer must stay quiet: the same caller's last dismissed
// call (unix seconds) is within the configured window. A dismissal in the
// future (clock moved back) never silences.
pub(super) fn should_silence_call(
    now: i64,
    last_dismissed: Option<i64>,
    setting: SilenceSetting,
) -> bool {
    if !setting.enabled {
        return false;
    }
    let Some(at) = last_dismissed else {
        return false;
    };
    let age = now - at;
    (0..i64::from(clamp_silence_minutes(setting.minutes)) * 60).contains(&age)
}

// A call that came in while silenced, shown only inside its conversation.
#[derive(Clone, Debug)]
pub(super) struct SilencedCall {
    pub(super) id: String,
    pub(super) from: String,
    pub(super) video: bool,
    pub(super) group: bool,
    pub(super) at: i64,
}

// Whatever the call screen is currently showing. One at a time: a second
// offer arriving mid-call is turned down rather than queued.
pub(super) struct ActiveCall {
    // Empty until the server hands back the id of an outgoing call.
    pub(super) id: String,
    // Canonical peer jid, for the name and the avatar.
    pub(super) jid: String,
    // The sender jid exactly as it arrived; a reject stanza needs it.
    pub(super) from: String,
    pub(super) outgoing: bool,
    // The call was offered (or placed) as a video call.
    pub(super) video: bool,
    // Our camera is on and sending.
    pub(super) sending_video: bool,
    // The peer's camera is on, or they have asked us to turn ours on.
    pub(super) peer_video: bool,
    pub(super) peer_wants_video: bool,
    // "incoming" | "outgoing" | "connecting" | "active" | "ended"
    pub(super) state: &'static str,
    pub(super) started: Option<Instant>,
}

impl Bridge {
    // ---- calls ----

    pub(super) fn refresh_calls(&mut self) {
        if self.ui.get_sidebar_view() == "calls" {
            self.store.mark_calls_seen();
            self.schedule_save();
        }
        self.ui.set_calls_unseen_count(self.store.unseen_calls());
        let rows: Vec<CallItem> = self
            .store
            .calls
            .iter()
            .take(60)
            .map(|call| {
                let name = self.store.chat_name(&call.jid);
                let avatar = self.avatar_for(&call.jid);
                let label = t(match call.status.as_str() {
                    "accept" => "call.answered",
                    "reject" => "call.declined",
                    "timeout" => {
                        if call.outgoing {
                            "call.no_answer"
                        } else {
                            "call.missed"
                        }
                    }
                    "offer" => "call.ringing",
                    "outgoing" => "call.calling",
                    _ => "call.ended",
                });
                let kind = t(if call.video {
                    "call.video"
                } else {
                    "call.voice"
                });
                let arrow = t(if call.outgoing { "call.out" } else { "call.in" });
                CallItem {
                    id: call.id.clone().into(),
                    from: call.jid.clone().into(),
                    name: name.clone().into(),
                    detail: format!("{arrow} · {kind} · {label}").into(),
                    time: format_time(call.timestamp).into(),
                    hasAvatar: avatar.is_some(),
                    avatar: avatar.unwrap_or_else(empty_image),
                    initial: initial_of(&name).into(),
                    colorIdx: color_idx_of(&call.jid),
                    video: call.video,
                    group: call.group,
                }
            })
            .collect();
        let jids: Vec<String> = self
            .store
            .calls
            .iter()
            .take(60)
            .map(|c| c.jid.clone())
            .collect();
        for jid in jids {
            self.queue_avatar(&jid);
        }
        self.calls_model.set_vec(rows);
    }

    // ---- the call screen ----

    // Created the first time a call needs it, then reused: building a
    // window is expensive and the callbacks only get wired once.
    // The main window's `glass` output also requires the window to be
    // focused (`window-active`), which it is not once a call window takes
    // the focus; the backdrop and the chosen style are what decide the look.
    pub(super) fn main_glass(&self) -> bool {
        self.ui.get_native_backdrop() && self.ui.get_theme_style() == "glass"
    }

    // Keeps an open call window in step with the appearance settings.
    pub(super) fn sync_call_theme(&self) {
        if let Some(win) = self.call_ui.as_ref() {
            win.set_dark_theme(self.ui.get_dark_theme());
            win.set_glass(self.main_glass());
        }
    }

    pub(super) fn call_window(&mut self) -> Option<CallWindow> {
        if let Some(win) = &self.call_ui {
            return Some(win.clone_strong());
        }
        let win = match CallWindow::new() {
            Ok(win) => win,
            Err(e) => {
                eprintln!("[call] cannot open the call window: {e}");
                return None;
            }
        };
        win.on_accept(|| defer(|b| b.accept_call()));
        win.on_decline(|| defer(|b| b.decline_call()));
        win.on_hangup(|| defer(|b| b.hangup_call()));
        win.on_toggle_mute(|| defer(|b| b.toggle_call_mute()));
        win.on_toggle_video(|| defer(|b| b.toggle_call_video()));
        // Closing the window means the same as decline (or hang up, once
        // the call is up), matching WhatsApp's own call window.
        win.window().on_close_requested(|| {
            defer(|b| b.decline_call());
            slint::CloseRequestResponse::HideWindow
        });
        self.call_ui = Some(win.clone_strong());
        Some(win)
    }

    // Pushes the current call onto the screen. Safe to call repeatedly;
    // every state change goes through here.
    pub(super) fn paint_call(&mut self) {
        let Some(call) = self.call.as_ref() else {
            return;
        };
        let (jid, state, video) = (call.jid.clone(), call.state, call.video);
        let name = self.store.chat_name(&jid);
        let avatar = self.avatar_for(&jid);
        let detail = t(match state {
            "incoming" => {
                if video {
                    "call.incoming_video"
                } else {
                    "call.incoming_voice"
                }
            }
            "outgoing" => "call.calling",
            "connecting" => "call.connecting",
            "active" => "call.in_call",
            _ => "call.ended",
        });
        self.queue_avatar(&jid);
        let dark = self.ui.get_dark_theme();
        let Some(win) = self.call_window() else {
            return;
        };
        // The call window carries its own copy of the theme global.
        win.set_dark_theme(dark);
        win.set_glass(self.main_glass());
        win.set_state(state.into());
        win.set_peer_name(name.clone().into());
        win.set_detail(detail.into());
        win.set_has_avatar(avatar.is_some());
        win.set_avatar(avatar.unwrap_or_else(empty_image));
        win.set_initial(initial_of(&name).into());
        win.set_color_idx(color_idx_of(&jid));
        let Some(call) = self.call.as_ref() else {
            return;
        };
        // A voice call is a portrait card; video needs room for a
        // landscape picture, the way WhatsApp resizes its call window.
        let video = call.video || call.sending_video || call.peer_video;
        let wanted = if video {
            slint::LogicalSize::new(760.0, 560.0)
        } else {
            slint::LogicalSize::new(360.0, 540.0)
        };
        if win.window().size().to_logical(win.window().scale_factor()) != wanted {
            win.window().set_size(wanted);
        }
        win.set_video_call(video);
        win.set_video_on(call.sending_video);
        win.set_peer_video(call.peer_video);
        win.set_video_wanted(call.peer_wants_video);
    }

    // ---- video ----

    pub(super) fn toggle_call_video(&mut self) {
        let Some(call) = self.call.as_mut() else {
            return;
        };
        if call.state != "active" {
            return;
        }
        let wanted = !call.sending_video;
        // The camera is only really on once the call side says so; this
        // keeps the button from flapping if it fails to open.
        call.peer_wants_video = false;
        self.wa.send(Cmd::SetCallVideo(wanted));
    }

    // Our own camera came on or went off.
    pub fn on_call_video(&mut self, id: &str, on: bool) {
        let Some(call) = self.call.as_mut() else {
            return;
        };
        if call.id != id {
            return;
        }
        call.sending_video = on;
        if !on {
            self.clear_local_preview();
        }
        self.paint_call();
    }

    // The peer's camera came on or went off.
    pub fn on_call_peer_video(&mut self, id: &str, on: bool) {
        let Some(call) = self.call.as_mut() else {
            return;
        };
        if call.id != id {
            return;
        }
        call.peer_video = on;
        if !on {
            self.clear_remote_video();
        }
        self.paint_call();
    }

    // The peer asked to add video. The camera stays off until the user
    // agrees by pressing the button.
    pub fn on_call_video_requested(&mut self, id: &str) {
        let Some(call) = self.call.as_mut() else {
            return;
        };
        if call.id != id {
            return;
        }
        call.peer_wants_video = true;
        self.paint_call();
    }

    // One frame of the local self-view.
    pub fn on_call_preview(&mut self, picture: crate::media::Decoded) {
        if let Some(win) = self.call_ui.as_ref()
            && self.call.as_ref().is_some_and(|call| call.sending_video)
        {
            win.set_local_video(image_of(&picture));
            win.set_has_local_video(true);
        }
        // The capture thread waits on this before sending the next one.
        crate::camera::PREVIEW_BUSY.store(false, std::sync::atomic::Ordering::Release);
    }

    // One decoded frame from the peer.
    pub fn on_call_remote_video(&mut self, picture: crate::media::Decoded) {
        if let Some(win) = self.call_ui.as_ref()
            && self.call.is_some()
        {
            win.set_remote_video(image_of(&picture));
            win.set_has_remote_video(true);
        }
        crate::camera::REMOTE_BUSY.store(false, std::sync::atomic::Ordering::Release);
    }

    pub(super) fn clear_local_preview(&mut self) {
        // A frame dropped mid-flight would otherwise leave the gate shut
        // and the next call's self-view blank.
        crate::camera::PREVIEW_BUSY.store(false, std::sync::atomic::Ordering::Release);
        if let Some(win) = self.call_ui.as_ref() {
            win.set_has_local_video(false);
            win.set_local_video(empty_image());
        }
    }

    pub(super) fn clear_remote_video(&mut self) {
        crate::camera::REMOTE_BUSY.store(false, std::sync::atomic::Ordering::Release);
        if let Some(win) = self.call_ui.as_ref() {
            win.set_has_remote_video(false);
            win.set_remote_video(empty_image());
        }
    }

    pub(super) fn show_call_window(&mut self) {
        let Some(win) = self.call_window() else {
            return;
        };
        win.set_muted(false);
        win.set_timer("".into());
        if let Err(e) = win.show() {
            eprintln!("[call] cannot show the call window: {e}");
            return;
        }
        // The startup stamp ran before this window existed, so its title
        // bar showed the default icon.
        #[cfg(windows)]
        crate::stamp_window_icons();
        crate::platform::focus_call_window();
    }

    // Dropping the ringer stops the stream; None means silence.
    pub(super) fn set_ring(&mut self, kind: Option<crate::call::Ring>) {
        self.ringer = None;
        if let Some(kind) = kind {
            self.ringer = crate::call::Ringer::start(kind);
        }
    }

    pub(super) fn tick_call(&mut self) {
        let Some(started) = self.call.as_ref().and_then(|call| call.started) else {
            self.call_timer.stop();
            return;
        };
        let secs = started.elapsed().as_secs();
        let text = if secs >= 3600 {
            format!("{}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
        } else {
            format!("{}:{:02}", secs / 60, secs % 60)
        };
        if let Some(win) = self.call_ui.as_ref() {
            win.set_timer(text.into());
        }
    }

    // The peer picked up (or we did): start counting.
    pub(super) fn mark_call_active(&mut self) {
        self.set_ring(None);
        let Some(call) = self.call.as_mut() else {
            return;
        };
        if call.state == "active" {
            return;
        }
        call.state = "active";
        call.started = Some(Instant::now());
        self.paint_call();
        self.tick_call();
        self.call_timer
            .start(slint::TimerMode::Repeated, Duration::from_secs(1), || {
                apply_now(|b| b.tick_call())
            });
    }

    // Leaves the reason on screen for a moment, the way the phone does,
    // then puts the window away.
    pub(super) fn end_call_ui(&mut self, reason: &str) {
        self.set_ring(None);
        self.call_timer.stop();
        let reason = t(reason);
        let Some(call) = self.call.as_mut() else {
            return;
        };
        call.state = "ended";
        call.started = None;
        if let Some(win) = self.call_ui.as_ref() {
            win.set_state("ended".into());
            win.set_timer("".into());
            win.set_detail(reason.into());
            win.set_muted(false);
        }
        self.once(CALL_LINGER_MS, |b| b.close_call_window());
    }

    pub(super) fn close_call_window(&mut self) {
        // A new call may have started while the ended screen lingered.
        if self.call.as_ref().is_some_and(|call| call.state != "ended") {
            return;
        }
        self.call = None;
        self.set_ring(None);
        self.call_timer.stop();
        // Video frames are megabytes each; do not leave one parked in a
        // hidden window for the rest of the session.
        self.clear_local_preview();
        self.clear_remote_video();
        if let Some(win) = self.call_ui.as_ref() {
            let _ = win.hide();
        }
    }

    // ---- devices ----

    // Fills the pickers and applies the saved choices. Enumeration talks
    // to the audio and camera stacks, so it happens when Settings opens
    // rather than on every launch.
    pub fn refresh_devices(&mut self) {
        let system = t("device.system");
        let listed = |mut names: Vec<String>| {
            names.insert(0, system.clone());
            let rows: Vec<SharedString> = names.into_iter().map(SharedString::from).collect();
            ModelRc::from(Rc::new(VecModel::from(rows)))
        };
        self.ui.set_mic_devices(listed(crate::call::input_names()));
        self.ui
            .set_speaker_devices(listed(crate::call::output_names()));
        self.ui.set_camera_devices(listed(
            crate::camera::cameras()
                .into_iter()
                .map(|c| c.name)
                .collect(),
        ));
        let chosen = crate::call::devices();
        let shown = |name: &str| {
            if name.is_empty() {
                system.clone().into()
            } else {
                SharedString::from(name)
            }
        };
        self.ui.set_mic_device(shown(&chosen.mic));
        self.ui.set_speaker_device(shown(&chosen.speaker));
        self.ui.set_camera_device(shown(&chosen.camera));
    }

    pub fn set_device(&mut self, kind: &str, name: &str) {
        // The first entry in every picker means "let the system decide",
        // which is stored as an empty name.
        let name = if name == t("device.system") { "" } else { name };
        let mut chosen = crate::call::devices();
        match kind {
            "mic" => chosen.mic = name.to_string(),
            "speaker" => chosen.speaker = name.to_string(),
            "camera" => chosen.camera = name.to_string(),
            _ => return,
        }
        if let Some(vault) = &self.vault {
            vault.setting_set(&format!("device.{kind}"), name);
        }
        crate::call::set_devices(chosen);
    }

    // Restores the saved devices at boot, before any call can start.
    pub(super) fn load_devices(&mut self) {
        let Some(vault) = &self.vault else { return };
        let get = |key: &str| vault.setting_get(key).unwrap_or_default();
        crate::call::set_devices(crate::call::Devices {
            mic: get("device.mic"),
            speaker: get("device.speaker"),
            camera: get("device.camera"),
        });
    }

    // ---- call actions ----

    pub fn start_call(&mut self, jid: &str) {
        self.place_call(jid, false);
    }

    pub fn start_video_call(&mut self, jid: &str) {
        self.place_call(jid, true);
    }

    pub(super) fn place_call(&mut self, jid: &str, video: bool) {
        let jid = self.store.canon_owned(&normalize_jid(jid));
        if is_group(&jid) || is_channel(&jid) || jid == "status@broadcast" {
            return;
        }
        if self.call.as_ref().is_some_and(|call| call.state != "ended") {
            return;
        }
        self.call = Some(ActiveCall {
            id: String::new(),
            jid: jid.clone(),
            from: jid.clone(),
            outgoing: true,
            video,
            sending_video: video,
            peer_video: false,
            peer_wants_video: false,
            state: "outgoing",
            started: None,
        });
        self.paint_call();
        self.show_call_window();
        self.set_ring(Some(crate::call::Ring::Back));
        self.wa.send(Cmd::StartCall { jid, video });
    }

    pub(super) fn accept_call(&mut self) {
        let Some(call) = self.call.as_mut() else {
            return;
        };
        if call.state != "incoming" {
            return;
        }
        call.state = "connecting";
        // A video offer is answered with the camera on, the way the phone
        // does it; the button in the call window turns it back off.
        let video = call.video;
        call.sending_video = video;
        let id = call.id.clone();
        self.set_ring(None);
        self.paint_call();
        self.wa.send(Cmd::AcceptCall { id, video });
    }

    // Doubles as the hang-up for a call already up, so the window's close
    // button always does the right thing.
    pub(super) fn decline_call(&mut self) {
        let Some(call) = self.call.as_ref() else {
            self.close_call_window();
            return;
        };
        match call.state {
            "incoming" => {
                let (id, from) = (call.id.clone(), call.from.clone());
                let caller = call.jid.clone();
                self.remember_dismissed(&caller);
                self.wa.send(Cmd::RejectCall {
                    id: id.clone(),
                    from: from.clone(),
                });
                self.record_call(&id, &from, "reject", false);
                self.end_call_ui("call.declined");
            }
            "ended" => self.close_call_window(),
            _ => self.hangup_call(),
        }
    }

    pub(super) fn hangup_call(&mut self) {
        if self.call.is_none() {
            return;
        }
        self.wa.send(Cmd::HangupCall);
        self.end_call_ui("call.ended");
    }

    pub(super) fn toggle_call_mute(&mut self) {
        let Some(win) = self.call_ui.as_ref() else {
            return;
        };
        let muted = !win.get_muted();
        win.set_muted(muted);
        self.wa.send(Cmd::SetCallMuted(muted));
    }

    // ---- call events ----

    pub(super) fn record_call(&mut self, id: &str, from: &str, status: &str, outgoing: bool) {
        if id.is_empty() {
            return;
        }
        let known = self.store.calls.iter().find(|c| c.id == id).cloned();
        let (video, group, outgoing) = match &known {
            Some(entry) => (entry.video, entry.group, entry.outgoing),
            None => (false, false, outgoing),
        };
        // A finished call stays "answered" in the log; the terminate that
        // follows it is the hang-up, not a separate outcome.
        if known.as_ref().is_some_and(|entry| entry.status == "accept") && status == "terminate" {
            return;
        }
        let from = known
            .map(|entry| entry.jid)
            .unwrap_or_else(|| from.to_string());
        if from.is_empty() {
            return;
        }
        if self
            .store
            .upsert_call(id, &from, status, video, group, 0, outgoing)
            .is_some()
        {
            self.refresh_calls();
            self.schedule_save();
        }
    }

    // Files a call the log has not seen before, with everything the
    // stanza carried. Later outcomes go through record_call, which keeps
    // what is already known.
    pub(super) fn record_call_new(
        &mut self,
        id: &str,
        from: &str,
        status: &str,
        video: bool,
        group: bool,
        ts: i64,
    ) {
        if self
            .store
            .upsert_call(id, from, status, video, group, ts, false)
            .is_some()
        {
            self.refresh_calls();
            self.schedule_save();
        }
    }

    pub fn on_incoming_call(&mut self, id: &str, from: &str, video: bool, group: bool, ts: i64) {
        // Offers replayed from the offline queue, and offers that waited
        // in the locked-vault backlog while the PIN screen was up, arrive
        // long after they stopped ringing. Log them, never announce them.
        let stale = ts > 0 && now_secs() - ts > STALE_CALL_SECS;
        if stale {
            // A replay of a call already in the log tells us nothing new;
            // recording it again would only overwrite its real outcome.
            if !self.store.calls.iter().any(|c| c.id == id) {
                self.record_call_new(id, from, "timeout", video, group, ts);
            }
            return;
        }
        self.record_call_new(id, from, "offer", video, group, ts);
        // A call can only be taken on the account on screen. One for a
        // background account gets a banner naming it and rings on the
        // phone; the log row settles when the call ends.
        if self.headless {
            if self.notifications {
                let jid = self.store.canon_owned(&normalize_jid(from));
                let name = self.store.chat_name(&jid);
                let account = self.account_label();
                crate::platform::toast(
                    &name,
                    &ta("call.incoming_on_account", &[&account]),
                    crate::platform::ToastPayload::default(),
                );
            }
            return;
        }
        // Already on a call: turn this one down rather than take the
        // screen away from the call in progress.
        if self.call.as_ref().is_some_and(|call| call.state != "ended") {
            self.wa.send(Cmd::RejectCall {
                id: id.to_string(),
                from: from.to_string(),
            });
            self.record_call(id, from, "reject", false);
            return;
        }
        let jid = self.store.canon_owned(&normalize_jid(from));
        if should_silence_call(
            now_secs(),
            self.call_dismissed.get(&jid).copied(),
            self.silence_setting(),
        ) {
            self.silence_incoming(id, from, video, group);
            let id = id.to_string();
            self.once(RING_TIMEOUT_MS, move |b| b.silenced_expired(&id));
            return;
        }
        self.begin_incoming_call(id, from, video);
    }

    // Rings, opens the call window and posts the banner.
    pub(super) fn begin_incoming_call(&mut self, id: &str, from: &str, video: bool) {
        let jid = self.store.canon_owned(&normalize_jid(from));
        self.call = Some(ActiveCall {
            id: id.to_string(),
            jid: jid.clone(),
            from: from.to_string(),
            outgoing: false,
            video,
            sending_video: false,
            peer_video: video,
            peer_wants_video: false,
            state: "incoming",
            started: None,
        });
        self.paint_call();
        self.show_call_window();
        self.set_ring(Some(crate::call::Ring::In));
        let name = self.store.chat_name(&jid);
        let detail = t(if video {
            "call.incoming_video"
        } else {
            "call.incoming_voice"
        });
        // The ring and the call window stay; only the banner obeys the setting.
        if self.notifications {
            crate::platform::toast(&name, &detail, crate::platform::ToastPayload::default());
        }
        // Nobody home: stop ringing and file it as missed.
        let id = id.to_string();
        self.once(RING_TIMEOUT_MS, move |b| b.ring_timeout(&id));
    }

    // ---- call anti-spam ----

    pub(super) fn silence_setting(&self) -> SilenceSetting {
        SilenceSetting {
            enabled: self.call_silence_repeat,
            minutes: self.call_silence_minutes,
        }
    }

    // RAM only; it never outlives the run.
    pub(super) fn remember_dismissed(&mut self, jid: &str) {
        self.call_dismissed.insert(jid.to_string(), now_secs());
    }

    // The offer arrived while its caller is silenced: no ring, no window, no
    // banner. The row waits in the conversation; the server is never told.
    fn silence_incoming(&mut self, id: &str, from: &str, video: bool, group: bool) {
        self.silenced_calls.retain(|call| call.id != id);
        self.silenced_calls.push(SilencedCall {
            id: id.to_string(),
            from: from.to_string(),
            video,
            group,
            at: now_secs(),
        });
        self.refresh_silenced_calls();
        self.schedule_refresh_chats();
    }

    fn caller_of(&self, call: &SilencedCall) -> String {
        self.store.canon_owned(&normalize_jid(&call.from))
    }

    pub(super) fn silenced_for(&self, jid: &str) -> bool {
        self.silenced_calls
            .iter()
            .any(|call| self.caller_of(call) == jid)
    }

    // The banner rows for the open chat.
    pub(super) fn refresh_silenced_calls(&mut self) {
        let keep = (RING_TIMEOUT_MS / 1000) as i64;
        let now = now_secs();
        self.silenced_calls.retain(|call| now - call.at <= keep);
        let current = self.current_jid.clone();
        let rows: Vec<crate::SilencedCall> = self
            .silenced_calls
            .iter()
            .filter(|call| current.as_deref() == Some(self.caller_of(call).as_str()))
            .map(|call| crate::SilencedCall {
                id: call.id.clone().into(),
                name: t("call.silenced.title").into(),
                kind: t(if call.video {
                    "call.silenced.video"
                } else {
                    "call.silenced.voice"
                })
                .into(),
                caller: self.store.chat_name(&self.caller_of(call)).into(),
                video: call.video,
                group: call.group,
                time: format_clock(call.at).into(),
            })
            .collect();
        self.ui
            .set_silenced_calls(ModelRc::new(VecModel::from(rows)));
    }

    // A silenced offer nobody answered becomes a plain missed call.
    pub(super) fn silenced_expired(&mut self, id: &str) {
        let before = self.silenced_calls.len();
        self.silenced_calls.retain(|call| call.id != id);
        if self.silenced_calls.len() != before {
            self.record_call(id, "", "timeout", false);
            self.refresh_silenced_calls();
            self.schedule_refresh_chats();
        }
    }

    // Answer from the banner: it becomes an ordinary incoming call.
    pub(super) fn answer_silenced_call(&mut self, id: &str) {
        let Some(index) = self.silenced_calls.iter().position(|call| call.id == id) else {
            return;
        };
        let call = self.silenced_calls.remove(index);
        self.refresh_silenced_calls();
        if self
            .call
            .as_ref()
            .is_some_and(|active| active.state != "ended")
        {
            return;
        }
        let jid = self.caller_of(&call);
        self.call_dismissed.remove(&jid);
        self.begin_incoming_call(&call.id, &call.from, call.video);
        self.accept_call();
    }

    pub(super) fn decline_silenced_call(&mut self, id: &str) {
        let Some(index) = self.silenced_calls.iter().position(|call| call.id == id) else {
            return;
        };
        let call = self.silenced_calls.remove(index);
        let jid = self.caller_of(&call);
        self.remember_dismissed(&jid);
        self.wa.send(Cmd::RejectCall {
            id: call.id.clone(),
            from: call.from.clone(),
        });
        self.record_call(&call.id, &call.from, "reject", false);
        self.refresh_silenced_calls();
        self.schedule_refresh_chats();
    }

    pub(super) fn handle_call_silence_repeat(&mut self, enabled: bool) {
        self.call_silence_repeat = enabled;
        self.ui.set_call_silence_repeat(enabled);
        if let Some(vault) = &self.vault {
            vault.setting_set("call_silence_repeat", if enabled { "1" } else { "0" });
        }
    }

    pub(super) fn handle_call_silence_minutes(&mut self, minutes: i32) {
        let minutes = clamp_silence_minutes(u32::try_from(minutes).unwrap_or(0));
        self.call_silence_minutes = minutes;
        self.ui.set_call_silence_minutes(minutes as i32);
        if let Some(vault) = &self.vault {
            vault.setting_set("call_silence_minutes", &minutes.to_string());
        }
    }

    pub(super) fn load_call_silence(&mut self, vault: &Vault) {
        self.call_silence_repeat = vault.setting_get("call_silence_repeat").as_deref() != Some("0");
        self.call_silence_minutes = clamp_silence_minutes(
            vault
                .setting_get("call_silence_minutes")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
        );
        self.ui.set_call_silence_repeat(self.call_silence_repeat);
        self.ui
            .set_call_silence_minutes(self.call_silence_minutes as i32);
    }

    pub(super) fn ring_timeout(&mut self, id: &str) {
        if !self
            .call
            .as_ref()
            .is_some_and(|call| call.state == "incoming" && call.id == id)
        {
            return;
        }
        if let Some(jid) = self.call.as_ref().map(|call| call.jid.clone()) {
            self.remember_dismissed(&jid);
        }
        self.record_call(id, "", "timeout", false);
        self.end_call_ui("call.missed");
    }

    // A call that must not ring: an offline-queue replay, or one the peer
    // gave up on before we ever saw the offer.
    pub fn on_call_missed(&mut self, id: &str, from: &str, ts: i64) {
        match self.store.calls.iter().find(|c| c.id == id) {
            // Only a call still shown as ringing becomes a missed one; an
            // outcome already recorded stands.
            Some(entry) if entry.status != "offer" => {}
            Some(_) => self.record_call(id, from, "timeout", false),
            None => self.record_call_new(id, from, "timeout", false, false, ts),
        }
        if self.call.as_ref().is_some_and(|call| call.id == id) {
            self.end_call_ui("call.missed");
        }
    }

    pub fn on_call_status(&mut self, id: &str, status: &str) {
        self.record_call(id, "", status, false);
        // An outgoing call has no id until the server mints one, so an
        // empty id matches whatever is on screen.
        let Some(call) = self.call.as_ref() else {
            return;
        };
        if !call.id.is_empty() && call.id != id {
            return;
        }
        // The screen is already showing the outcome; nothing left to do.
        if call.state == "ended" {
            return;
        }
        match (status, call.state) {
            // Still ringing here, so the accept came from another of our
            // devices: this one stops.
            ("accept", "incoming") => self.end_call_ui("call.elsewhere"),
            ("accept", _) => self.mark_call_active(),
            // Peer-device "busy" rejects never get here, so a reject is
            // always someone turning the call down.
            ("reject", _) => self.end_call_ui("call.declined"),
            (_, "incoming") => self.end_call_ui("call.missed"),
            _ => self.end_call_ui("call.ended"),
        }
    }

    // The outgoing call reached the server; from here on it has an id.
    pub fn on_outgoing_call(&mut self, id: &str) {
        let jid = match self.call.as_mut() {
            Some(call) if call.outgoing && call.id.is_empty() => {
                call.id = id.to_string();
                call.jid.clone()
            }
            _ => return,
        };
        if self
            .store
            .upsert_call(id, &jid, "outgoing", false, false, 0, true)
            .is_some()
        {
            self.refresh_calls();
            self.schedule_save();
        }
    }

    // The relay is up. For the callee that is the whole handshake done;
    // the caller still waits for the peer to pick up.
    pub fn on_call_media_ready(&mut self, id: &str) {
        if self
            .call
            .as_ref()
            .is_some_and(|call| call.id == id && call.state == "connecting")
        {
            self.mark_call_active();
        }
    }

    pub fn on_call_failed(&mut self, reason: &str) {
        self.set_ring(None);
        self.call_timer.stop();
        let Some(call) = self.call.as_mut() else {
            return;
        };
        // A peer that declined, or a call already torn down, has its own
        // outcome on screen; a late failure must not rewrite it.
        if call.state == "ended" {
            return;
        }
        call.state = "ended";
        call.started = None;
        if let Some(win) = self.call_ui.as_ref() {
            win.set_state("ended".into());
            win.set_timer("".into());
            win.set_detail(reason.into());
        }
        self.once(CALL_LINGER_MS * 2, |b| b.close_call_window());
    }
}

#[cfg(test)]
mod silence_tests {
    use super::*;

    const ON: SilenceSetting = SilenceSetting {
        enabled: true,
        minutes: 10,
    };

    #[test]
    fn a_first_call_from_a_caller_always_rings() {
        assert!(!should_silence_call(1_000_000, None, ON));
    }

    #[test]
    fn a_dismissal_inside_the_window_silences() {
        let now = 1_000_000;
        assert!(should_silence_call(now, Some(now), ON));
        assert!(should_silence_call(now, Some(now - 599), ON));
    }

    #[test]
    fn the_window_is_exclusive_at_its_end() {
        let now = 1_000_000;
        assert!(!should_silence_call(now, Some(now - 600), ON));
        assert!(!should_silence_call(now, Some(now - 3_600), ON));
    }

    #[test]
    fn the_setting_off_never_silences() {
        let off = SilenceSetting {
            enabled: false,
            minutes: 10,
        };
        assert!(!should_silence_call(1_000_000, Some(1_000_000), off));
    }

    #[test]
    fn each_offered_window_is_honoured() {
        let now = 1_000_000;
        for minutes in SILENCE_CHOICES {
            let setting = SilenceSetting {
                enabled: true,
                minutes,
            };
            let edge = i64::from(minutes) * 60;
            assert!(
                should_silence_call(now, Some(now - edge + 1), setting),
                "{minutes}"
            );
            assert!(
                !should_silence_call(now, Some(now - edge), setting),
                "{minutes}"
            );
        }
    }

    #[test]
    fn an_unknown_window_falls_back_to_ten_minutes() {
        let now = 1_000_000;
        let odd = SilenceSetting {
            enabled: true,
            minutes: 7,
        };
        assert!(should_silence_call(now, Some(now - 599), odd));
        assert!(!should_silence_call(now, Some(now - 600), odd));
        assert_eq!(clamp_silence_minutes(0), 10);
        assert_eq!(clamp_silence_minutes(30), 30);
    }

    #[test]
    fn a_dismissal_from_the_future_does_not_silence() {
        assert!(!should_silence_call(1_000_000, Some(1_000_100), ON));
    }

    #[test]
    fn defaults_are_on_and_ten_minutes() {
        assert_eq!(SilenceSetting::default(), ON);
    }
}
