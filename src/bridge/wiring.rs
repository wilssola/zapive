// Window wiring: installs the bridge and connects every Slint callback.
use super::*;

// WhatsApp-style thumbnail box: fit within 330x380, never upscale.
// Plays a press that landed on a link's hand-cursor box again, with the
// boxes out of the way, so it reaches the selectable text under them.
// `pass_through` and `released` reach the window's link-press-through and
// link-release-seen properties (the selftest has a window of its own).
pub fn replay_link_press(
    window: &slint::Window,
    x: f32,
    y: f32,
    pass_through: &dyn Fn(bool),
    released: &dyn Fn() -> bool,
) {
    use slint::platform::{PointerEventButton, WindowEvent};
    thread_local! {
        // The last press replayed: when, where, and its place in a run of
        // clicks (1 = single, 2 = double, 3 = triple).
        static LAST: std::cell::Cell<Option<(Instant, f32, f32, u8)>> = const { std::cell::Cell::new(None) };
    }
    // Slint counts clicks from the presses it sees, and it has now seen
    // this one twice. Count them here instead and spell the run out.
    let run = LAST.with(|last| {
        let run = match last.get() {
            Some((at, lx, ly, n))
                if at.elapsed() < Duration::from_millis(450)
                    && (lx - x).abs() < 6.0
                    && (ly - y).abs() < 6.0
                    && n < 3 =>
            {
                n + 1
            }
            _ => 1,
        };
        last.set(Some((Instant::now(), x, y, run)));
        run
    });
    let position = slint::LogicalPosition::new(x, y);
    pass_through(true);
    // The box still holds the mouse grab from the real press. A press of
    // another button is refused by it (it is disabled now), which frees
    // the grab, and it also restarts Slint's own click counter.
    window.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Other,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Other,
    });
    for click in 1..=run {
        window.dispatch_event(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
        if click < run {
            window.dispatch_event(WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            });
        }
    }
    // A click quicker than this replay: the button is already up, and
    // the release went to the box instead of the text.
    if released() {
        window.dispatch_event(WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        });
    }
    pass_through(false);
}

pub fn install(ui: &AppWindow, wa: WaService) {
    let chats_model = Rc::new(VecModel::<ChatItem>::default());
    let messages_model = Rc::new(VecModel::<MessageItem>::default());
    let reactions_model = Rc::new(VecModel::<ReactionItem>::default());
    let forward_model = Rc::new(VecModel::<ChatItem>::default());
    let status_model = Rc::new(VecModel::<ChatItem>::default());
    let calls_model = Rc::new(VecModel::<CallItem>::default());
    let info_media_model = Rc::new(VecModel::<ModelRc<StickerCell>>::default());
    let sticker_model = Rc::new(VecModel::<ModelRc<StickerCell>>::default());
    let fav_model = Rc::new(VecModel::<ModelRc<StickerCell>>::default());
    let gif_model = Rc::new(VecModel::<ModelRc<StickerCell>>::default());
    let mention_model = Rc::new(VecModel::<MemberItem>::default());
    let search_model = Rc::new(VecModel::<SearchHit>::default());
    let chat_search_model = Rc::new(VecModel::<SearchHit>::default());
    let member_model = Rc::new(VecModel::<MemberItem>::default());
    let info_member_model = Rc::new(VecModel::<MemberItem>::default());
    let label_model = Rc::new(VecModel::<LabelItem>::default());
    let label_chip_model = Rc::new(VecModel::<LabelItem>::default());
    let pdf_model = Rc::new(VecModel::<PdfPage>::default());
    let accounts_model = Rc::new(VecModel::<AccountItem>::default());
    ui.set_accounts(ModelRc::from(accounts_model.clone()));
    ui.set_pdf_pages(ModelRc::from(pdf_model.clone()));
    ui.set_mention_rows(ModelRc::from(mention_model.clone()));
    ui.set_search_hits(ModelRc::from(search_model.clone()));
    ui.set_chat_search_rows(ModelRc::from(chat_search_model.clone()));
    ui.set_member_rows(ModelRc::from(member_model.clone()));
    ui.set_info_member_rows(ModelRc::from(info_member_model.clone()));
    ui.set_labels(ModelRc::from(label_model.clone()));
    ui.set_label_chips(ModelRc::from(label_chip_model.clone()));
    ui.set_sticker_rows(ModelRc::from(sticker_model.clone()));
    ui.set_fav_rows(ModelRc::from(fav_model.clone()));
    ui.set_gif_rows(ModelRc::from(gif_model.clone()));
    ui.set_chats(ModelRc::from(chats_model.clone()));
    ui.set_messages(ModelRc::from(messages_model.clone()));
    ui.set_reaction_rows(ModelRc::from(reactions_model.clone()));
    ui.set_forward_rows(ModelRc::from(forward_model.clone()));
    ui.set_statuses(ModelRc::from(status_model.clone()));
    ui.set_calls(ModelRc::from(calls_model.clone()));
    ui.set_info_media(ModelRc::from(info_media_model.clone()));
    ui.set_emoji_rows(crate::emoji::palette_rows());
    wire_callbacks(ui);
    let bridge = Bridge {
        ui: ui.clone_strong(),
        wa,
        vault: None,
        store: Store::default(),
        translation: crate::translation::Worker::new(),
        translation_gen: 0,
        translation_request: 0,
        translation_monitor: None,
        translation_cache: HashMap::new(),
        translation_settings: TranslationSettings::default(),
        translation_pairs: TranslationPairs::default(),
        chats_model,
        messages_model,
        current_jid: None,
        unread_boundary: None,
        self_jid: String::new(),
        search_text: String::new(),
        tab: "all".into(),
        view: "chats".into(),
        groups_fetched: false,
        reply_to: None,
        truncated: HashSet::new(),
        scroll_pos: HashMap::new(),
        history_batches: 0,
        history_pending: false,
        scroll_up_fetch: false,
        last_scroll_load: None,
        refresh_queued: false,
        save_queued: false,
        pending_registered: false,
        account: String::new(),
        epoch: 0,
        registered: false,
        switching: false,
        wipe_on_logout: false,
        accounts_model: accounts_model.clone(),
        peer_online: false,
        typing_gen: 0,
        chat_typing: HashMap::new(),
        presence_subscribed: HashSet::new(),
        draft_sequence: 0,
        locked_backlog: Vec::new(),
        avatars: HashMap::new(),
        requested_avatars: HashSet::new(),
        avatar_tries: HashMap::new(),
        media_inflight: HashSet::new(),
        media_previews: HashMap::new(),
        preview_requested: HashSet::new(),
        media_failed: HashSet::new(),
        media_path: HashMap::new(),
        decoded: HashMap::new(),
        decoded_order: std::collections::VecDeque::new(),
        decoded_bytes: 0,
        warm_order: std::collections::VecDeque::new(),
        hover_jid: String::new(),
        animated: HashMap::new(),
        anim_order: std::collections::VecDeque::new(),
        anim_bytes: 0,
        anim_timer: slint::Timer::default(),
        stick_requeued: false,
        media_key: crate::vault::KeyHandle::default(),
        audio: None,
        audio_rate_idx: 0,
        audio_buffers: HashMap::new(),
        audio_timer: slint::Timer::default(),
        waves: HashMap::new(),
        recorder: None,
        rec_timer: slint::Timer::default(),
        rec_started: None,
        reactions_model,
        forward_model,
        react_pick_for: None,
        pending_forward: Vec::new(),
        sync_batches: 0,
        sync_messages: 0,
        select_mode: false,
        selected: Vec::new(),
        mention_at: None,
        mentions: Vec::new(),
        mention_model,
        group_meta_at: HashMap::new(),
        common_groups_cache: HashMap::new(),
        common_groups_pending: HashMap::new(),
        search_model,
        chat_search_model,
        chat_search_query: String::new(),
        chat_search_pos: 0,
        stickers: HashMap::new(),
        sticker_order: std::collections::VecDeque::new(),
        sticker_bytes: 0,
        member_model,
        member_picked: HashSet::new(),
        info_member_model,
        label_model,
        label_chip_model,
        confirm_action: None,
        pdf: crate::pdf::PdfWorker::start(),
        pdf_gen: 0,
        pdf_id: String::new(),
        pdf_path: None,
        pdf_sizes: Vec::new(),
        pdf_model,
        pdf_zoom: 1.0,
        pdf_page_w: 0.0,
        pdf_rendered: HashMap::new(),
        pdf_pending: HashSet::new(),
        drag_anchor: None,
        row_geom: HashMap::new(),
        geom_epoch: 0,
        row_gc_queued: false,
        share_target: None,
        pending_image: None,
        original_image: None,
        preview_jid: None,
        preview_generation: 0,
        preview_busy: false,
        last_paste: None,
        notify_queue: Vec::new(),
        notify_queued: false,
        notifications: true,
        notify_previews: false,
        notify_tokens: HashMap::new(),
        notification_thumbnails: HashMap::new(),
        preparation_states: HashMap::new(),
        preparations_active: HashSet::new(),
        outbox: Vec::new(),
        outbox_connected: false,
        mcp: None,
        update_manual: false,
        status_model,
        calls_model,
        info_media_model,
        viewer: None,
        call_ui: None,
        call: None,
        call_timer: slint::Timer::default(),
        ringer: None,
        requested_channels: HashSet::new(),
        sticker_model,
        fav_model,
        gif_model,
        gif_url_by_id: HashMap::new(),
        zoom_frames: Vec::new(),
        zoom_idx: 0,
        zoom_timer: slint::Timer::default(),
        video: None,
        video_audio: None,
        video_id: None,
        video_gen: 0,
        video_playing: false,
        video_pos: 0.0,
        video_duration: 0.0,
        video_volume: 1.0,
        video_muted: false,
        oneshots: Vec::new(),
    };
    BRIDGE.with(|cell| *cell.borrow_mut() = Some(bridge));
    // Update checks start here, not at boot: the banner must also reach
    // the lock and login screens.
    apply_now(|b| b.once(10_000, |b| b.update_tick()));
    // Safety net: whatever goes wrong during startup, the splash goes away.
    apply_now(|b| b.once(8_000, |b| b.finish_loading()));
}

pub(super) fn wire_callbacks(ui: &AppWindow) {
    ui.on_translation_action(|action| defer(move |b| b.translation_action(&action)));
    ui.on_translation_install(|source| defer(move |b| b.install_translation_model(&source)));
    ui.on_translation_remove(|source, target| defer(move |b| b.remove_translation_model(&source, &target)));
    ui.on_translation_model_selected(|source| defer(move |b| b.translation_model_selected(&source)));
    ui.on_translation_language(|kind, value| defer(move |b| b.translation_language(&kind, &value)));
    ui.set_status_text(t("status.connecting").into());

    ui.on_request_pairing_code(|phone| {
        let phone = phone.to_string();
        defer(move |b| b.handle_pairing(&phone));
    });
    ui.on_alert_retry(|| {
        defer(|b| {
            b.ui.set_alert_open(false);
            b.ui.set_status_text(t("status.connecting").into());
            b.wa.send(Cmd::Resume);
        });
    });
    ui.on_alert_dismiss(|| defer(|b| b.ui.set_alert_open(false)));
    ui.on_logout(|| {
        defer(|b| {
            b.ui.set_settings_open(false);
            b.ask_confirm(
                t("logout.title"),
                t("logout.body"),
                t("logout.wipe"),
                ConfirmAction::Logout,
            );
            b.ui.set_confirm_secondary(t("logout.keep").into());
        });
    });
    ui.on_switch_account(|id| {
        let id = id.to_string();
        defer(move |b| b.switch_account(&id));
    });
    ui.on_add_account(|| defer(|b| b.add_account()));
    ui.on_remove_account(|id| {
        let id = id.to_string();
        defer(move |b| b.ask_remove_account(&id));
    });
    ui.on_forget_conversations(|| {
        defer(|b| {
            b.ask_confirm(
                t("forget.title"),
                t("forget.body"),
                t("forget.action"),
                ConfirmAction::ForgetConversations,
            );
        });
    });
    ui.on_unlock(|pin| {
        let pin = pin.to_string();
        defer(move |b| b.handle_unlock(&pin));
    });
    ui.on_theme_changed(|mode| {
        let mode = mode.to_string();
        defer(move |b| b.handle_theme(&mode));
    });
    ui.on_style_changed(|style| {
        let style = style.to_string();
        defer(move |b| b.handle_style(&style));
    });
    ui.on_notifications_changed(|enabled| defer(move |b| b.handle_notifications(enabled)));
    ui.on_notify_previews_changed(|enabled| defer(move |b| b.handle_notify_previews(enabled)));
    ui.on_privacy_blur_changed(|category, enabled| {
        let category = category.to_string();
        defer(move |b| b.handle_privacy_blur(&category, enabled));
    });
    ui.on_mcp_changed(|enabled| defer(move |b| b.handle_mcp(enabled)));
    ui.on_mcp_copy_config(|| defer(|b| b.mcp_copy_config()));
    ui.on_mcp_regenerate_token(|| defer(|b| b.mcp_regenerate_token()));
    ui.on_autostart_changed(|on| {
        defer(move |b| b.handle_autostart(on));
    });
    ui.on_language_changed(|mode| {
        let mode = mode.to_string();
        defer(move |b| {
            if let Some(vault) = &b.vault {
                vault.setting_set("language", &mode);
            }
            b.ui.set_settings_status(t("lang.restart").into());
        });
    });
    ui.on_send_receipts_changed(|on| {
        defer(move |b| {
            if let Some(vault) = &b.vault {
                vault.setting_set("send_receipts", if on { "1" } else { "0" });
            }
            b.ui.set_send_receipts(on);
        });
    });
    ui.on_auto_download_photos_changed(|on| {
        defer(move |b| {
            if let Some(vault) = &b.vault {
                vault.setting_set("auto_download_photos", if on { "1" } else { "0" });
            }
            b.ui.set_auto_download_photos(on);
        });
    });
    ui.on_auto_download_audio_changed(|on| {
        defer(move |b| {
            if let Some(vault) = &b.vault {
                vault.setting_set("auto_download_audio", if on { "1" } else { "0" });
            }
            b.ui.set_auto_download_audio(on);
        });
    });
    ui.on_auto_download_video_changed(|on| {
        defer(move |b| {
            if let Some(vault) = &b.vault {
                vault.setting_set("auto_download_video", if on { "1" } else { "0" });
            }
            b.ui.set_auto_download_video(on);
        });
    });
    ui.on_auto_download_documents_changed(|on| {
        defer(move |b| {
            if let Some(vault) = &b.vault {
                vault.setting_set("auto_download_documents", if on { "1" } else { "0" });
            }
            b.ui.set_auto_download_documents(on);
        });
    });
    ui.on_auto_download_stickers_changed(|on| {
        defer(move |b| {
            if let Some(vault) = &b.vault {
                vault.setting_set("auto_download_stickers", if on { "1" } else { "0" });
            }
            b.ui.set_auto_download_stickers(on);
        });
    });
    ui.on_apply_deletions_changed(|on| {
        defer(move |b| {
            if let Some(vault) = &b.vault {
                vault.setting_set("apply_deletions", if on { "1" } else { "0" });
            }
            b.ui.set_apply_deletions(on);
        });
    });

    ui.on_open_chat(|jid| {
        let jid = jid.to_string();
        defer(move |b| b.open_dm(&jid, None));
    });
    ui.on_chat_hover(|jid| {
        let jid = jid.to_string();
        defer(move |b| b.chat_hover(&jid));
    });
    ui.set_app_version(crate::update::current_version().into());
    ui.on_changelog_load(|refresh| {
        defer(move |b| b.load_changelog(refresh));
    });
    ui.on_update_check(|| {
        defer(|b| {
            b.update_manual = true;
            b.ui.set_settings_status(t("update.checking").into());
            b.wa.send(Cmd::CheckUpdate);
        });
    });
    ui.on_update_apply(|| {
        defer(|b| {
            b.ui.set_update_state(2);
            b.wa.send(Cmd::ApplyUpdate);
        });
    });
    ui.on_update_restart(|| {
        defer(|_| {
            crate::update::request_restart();
            let _ = slint::quit_event_loop();
        });
    });
    ui.on_open_dm(|target| {
        let target = target.to_string();
        defer(move |b| {
            // Styled-text links carry either a jid (mention) or a real URL.
            if target.starts_with("http://") || target.starts_with("https://") {
                crate::platform::open_path(&target);
            } else {
                b.open_dm(&target, None);
            }
        });
    });
    ui.on_search_changed(|text| {
        let text = text.to_string();
        defer(move |b| {
            b.search_text = text.trim().to_lowercase();
            b.refresh_chats();
        });
    });
    ui.on_tab_changed(|tab| {
        let tab = tab.to_string();
        defer(move |b| {
            b.tab = tab;
            b.refresh_chats();
        });
    });
    ui.on_view_changed(|view| {
        let view = view.to_string();
        defer(move |b| {
            if view == "calls" {
                b.store.mark_calls_seen();
                b.refresh_calls();
            }
            b.view = view;
            b.refresh_chats();
        });
    });
    ui.on_send_message(|text| {
        let text = text.to_string();
        let context = BRIDGE.with(|cell| cell.borrow().as_ref().map(|b| (b.account.clone(), b.current_jid.clone(), b.reply_to.clone(), b.mentions.clone())));
        defer(move |b| {
            if let Some((account, jid, reply, mentions)) = context {
                if b.account != account || b.current_jid != jid { return; }
                if b.reply_to != reply || b.mentions != mentions || b.ui.get_composer_text().as_str() != text { return; }
                b.handle_send_text(&text);
            }
        });
    });
    ui.on_load_older(|| defer(|b| b.handle_scroll_up_load()));
    ui.on_jump_latest(|| {
        defer(|b| {
            if let Some(jid) = b.current_jid.clone() {
                if let Some(vault) = &b.vault {
                    if let Err(error) = b.store.save_checked(vault) {
                        eprintln!("[history] jump latest save failed: {error}");
                        b.message_action_failed(&t("history.unavailable"));
                        return;
                    }
                    b.store.evict(&jid);
                    b.store.hydrate(vault, &jid);
                    if !b.store.hydrated.contains(&jid) {
                        b.message_action_failed(&t("history.unavailable"));
                        return;
                    }
                }
                b.scroll_pos.remove(&jid);
                b.truncated.remove(&jid);
                b.rebuild_conversation(&jid);
                b.load_media_for_chat(&jid);
                b.queue_row_avatars(&jid);
            }
            b.scroll_to_end();
        });
    });
    ui.on_request_reply(|id| {
        let id = id.to_string();
        defer(move |b| b.start_reply(&id));
    });
    ui.on_message_action(|id, action| {
        let (id, action) = (id.to_string(), action.to_string());
        defer(move |b| b.message_action(&id, &action));
    });
    ui.on_submit_edit(|id, text| {
        let (id, text) = (id.to_string(), text.to_string());
        defer(move |b| {
            if b.find_message(&id).is_some_and(message_editable) && !text.trim().is_empty() {
                if let Some(jid) = b.current_jid.clone() {
                    b.wa.send(Cmd::Edit { jid, id, text: text.trim().to_string() });
                    b.ui.set_message_dialog_open(false);
                }
            }
        });
    });
    ui.on_cancel_reply(|| defer(|b| b.clear_reply()));
    ui.on_open_quote(|jid, id| {
        let (jid, id) = (jid.to_string(), id.to_string());
        defer(move |b| {
            if !jid.is_empty() && !id.is_empty() {
                b.open_dm(&jid, Some(&id));
            }
        });
    });
    ui.on_react_to(|id, emoji| {
        let (id, emoji) = (id.to_string(), emoji.to_string());
        defer(move |b| {
            b.ui.set_react_bar_id("".into());
            b.send_reaction(&id, &emoji);
        });
    });
    ui.on_react_pick(|id| {
        let id = id.to_string();
        defer(move |b| {
            // The full picker doubles as the reaction picker.
            b.react_pick_for = Some(id.clone());
            b.ui.set_picker_open(true);
        });
    });
    ui.on_emoji_pick(|emoji| {
        let emoji = emoji.to_string();
        defer(move |b| {
            if let Some(target) = b.react_pick_for.take() {
                b.ui.set_picker_open(false);
                b.send_reaction(&target, &emoji);
            } else {
                b.ui.invoke_append_composer(emoji.into());
                b.save_composer(false);
            }
        });
    });
    ui.on_show_reactions(|id| {
        let id = id.to_string();
        defer(move |b| b.show_reactions(&id));
    });
    ui.on_toggle_star(|id| {
        let id = id.to_string();
        defer(move |b| b.toggle_star(&id));
    });
    ui.on_delete_message(|id| {
        let id = id.to_string();
        defer(move |b| b.delete_message(&id));
    });
    ui.on_request_forward(|id| {
        let id = id.to_string();
        defer(move |b| b.request_forward(&id));
    });
    ui.on_forward_search(|query| {
        let query = query.to_string();
        defer(move |b| b.fill_forward_rows(&query));
    });
    ui.on_forward_to(|jid| {
        let jid = jid.to_string();
        defer(move |b| b.handle_forward_to(&jid));
    });
    ui.on_attach_image(|| defer(|b| b.handle_attach("image")));
    ui.on_attach_audio(|| defer(|b| b.handle_attach("audio")));
    ui.on_attach_doc(|| defer(|b| b.handle_attach("doc")));
    ui.on_confirm_send_image(|caption| {
        let caption = caption.to_string();
        defer(move |b| b.confirm_send_image(&caption));
    });
    ui.on_cancel_send_image(|| defer(|b| b.cancel_preview()));
    ui.on_crop_preview(|x, y, w, h| {
        defer(move |b| b.edit_preview(crate::media::PreviewEdit::Crop(x, y, w, h)));
    });
    ui.on_text_preview(|text, x, y| {
        let text = text.to_string();
        defer(move |b| b.edit_preview(crate::media::PreviewEdit::Text(text, x, y)));
    });
    ui.on_draw_preview(|points, color, thickness| {
        let points = points.to_string();
        let rgba = [color.red(), color.green(), color.blue(), color.alpha()];
        defer(move |b| b.edit_preview(crate::media::PreviewEdit::Draw(points, rgba, thickness)));
    });
    ui.on_reset_preview(|| defer(|b| b.reset_preview()));
    ui.on_paste_clipboard(|| defer(|b| b.handle_paste()));
    ui.on_picker_opened(|| defer_after(Duration::from_millis(200), |b| b.load_sticker_panel()));
    ui.on_picker_closed(|| defer(|_| {}));
    ui.on_sticker_send(|id| {
        let id = id.to_string();
        defer(move |b| b.send_sticker_by_id(&id));
    });
    ui.on_attach_sticker(|| {
        defer(|b| {
            if b.current_jid.is_none() {
                return;
            }
            std::thread::spawn(|| {
                let picked = rfd::FileDialog::new()
                    .add_filter(t("picker.images"), &["jpg", "jpeg", "png", "webp"])
                    .pick_file();
                if let Some(path) = picked {
                    ui_apply(move |b| {
                        if let Some(jid) = b.current_jid.clone() {
                            b.wa.send(Cmd::SendSticker { jid, path });
                        }
                    });
                }
            });
        });
    });
    ui.on_gif_search(|query| {
        let query = query.to_string();
        defer(move |b| {
            b.ui.set_gif_hint("".into());
            b.wa.send(Cmd::GifSearch(query));
        });
    });
    ui.on_gif_send(|id| {
        let id = id.to_string();
        defer(move |b| b.send_gif_by_id(&id));
    });
    ui.on_open_video(|id| {
        let id = id.to_string();
        defer(move |b| b.open_video(&id));
    });
    ui.on_close_video(|| defer(|b| b.close_video()));
    ui.on_video_toggle(|| defer(|b| b.video_toggle()));
    ui.on_video_seek(|fraction| {
        defer(move |b| {
            let to = fraction as f64 * b.video_duration;
            b.video_seek(to);
        });
    });
    ui.on_video_seek_by(|secs| {
        defer(move |b| {
            let to = b.video_pos + secs as f64;
            b.video_seek(to);
        });
    });
    ui.on_video_set_volume(|volume| defer(move |b| b.video_set_volume(volume, volume <= 0.0)));
    ui.on_video_mute_toggle(|| {
        defer(|b| {
            let (volume, muted) = (b.video_volume, !b.video_muted);
            // Unmuting a slider dragged to zero would stay silent.
            b.video_set_volume(if !muted && volume <= 0.0 { 1.0 } else { volume }, muted);
        });
    });
    ui.on_video_save(|| defer(|b| b.video_save()));
    ui.on_status_open(|jid| {
        let jid = jid.to_string();
        defer(move |b| b.open_status_viewer(&jid));
    });
    ui.on_status_next(|| defer(|b| b.step_status(1)));
    ui.on_status_prev(|| defer(|b| b.step_status(-1)));
    ui.on_status_close(|| {
        defer(|b| {
            b.viewer = None;
            b.ui.set_sv_open(false);
        });
    });
    ui.on_start_call(|jid| {
        let jid = jid.to_string();
        defer(move |b| b.start_call(&jid));
    });
    // Enumerating devices touches the audio and camera stacks, so it waits
    // until the user actually opens Settings.
    ui.on_settings_opened(|| defer(|b| b.refresh_devices()));
    ui.on_settings_search(|text, query| {
        text.to_lowercase().contains(&query.to_lowercase())
    });
    ui.on_start_video_call(|jid| {
        let jid = jid.to_string();
        defer(move |b| b.start_video_call(&jid));
    });
    ui.on_device_changed(|kind, name| {
        let (kind, name) = (kind.to_string(), name.to_string());
        defer(move |b| b.set_device(&kind, &name));
    });
    ui.on_open_info(|| defer(|b| b.open_contact_info()));
    ui.on_close_info(|| defer(|b| b.ui.set_info_open(false)));
    ui.on_toggle_archive(|| defer(|b| b.toggle_archive()));
    ui.on_clear_chat(|| defer(|b| b.clear_current_chat()));
    ui.on_save_pin(|current, next| {
        let (current, next) = (current.to_string(), next.to_string());
        defer(move |b| b.handle_save_pin(&current, &next));
    });
    ui.on_remove_pin(|current| {
        let current = current.to_string();
        defer(move |b| b.handle_remove_pin(&current));
    });
    ui.set_audio_rate_label("1x".into());
    ui.on_audio_toggle(|id| {
        let id = id.to_string();
        defer(move |b| b.toggle_audio(&id));
    });
    ui.on_audio_seek(|id, frac| {
        let id = id.to_string();
        defer(move |b| b.seek_audio(&id, frac));
    });
    ui.on_audio_cycle_rate(|| defer(|b| b.cycle_audio_rate()));
    ui.on_mini_audio_toggle(|| {
        defer(|b| {
            if let Some(id) = b.audio.as_ref().map(|a| a.id.clone()) {
                b.toggle_audio(&id);
            }
        });
    });
    ui.on_mini_audio_open(|| {
        defer(|b| {
            if let Some((jid, id)) = b.audio.as_ref().map(|a| (a.jid.clone(), a.id.clone())) {
                b.open_dm(&jid, Some(&id));
            }
        });
    });
    ui.on_mini_audio_close(|| defer(|b| b.stop_audio()));
    ui.on_rec_start(|| defer(|b| b.start_recording()));
    ui.on_rec_stop(|| defer(|b| b.stop_recording(true)));
    ui.on_rec_cancel(|| defer(|b| b.stop_recording(false)));
    ui.on_play_audio(|path| {
        // Documents (and, until phase 4, audio files) open with whatever
        // the OS associates; the handler needs a decrypted copy.
        let path = path.to_string();
        defer(move |b| {
            let cached = std::path::PathBuf::from(&path);
            if let Some(plain) = crate::media::temp_plain(&b.media_key, &cached) {
                crate::platform::open_path(&plain.to_string_lossy());
            }
        });
    });
    // Documents: view in-app (PDF) or with the system, and save a copy.
    ui.on_doc_view(|id| {
        let id = id.to_string();
        defer(move |b| b.open_document(&id));
    });
    ui.on_doc_save(|id| {
        let id = id.to_string();
        defer(move |b| b.save_document(&id));
    });
    ui.on_pdf_close(|| defer(|b| b.close_pdf()));
    ui.on_pdf_scrolled(|y| defer(move |b| b.pdf_scrolled(y)));
    ui.on_pdf_zoom(|step| defer(move |b| b.pdf_zoom_step(step)));
    ui.on_pdf_goto(|delta| defer(move |b| b.pdf_goto(delta)));
    ui.on_pdf_view_resized(|| defer(|b| b.pdf_layout(true)));
    ui.on_pdf_open_external(|| {
        defer(|b| {
            if let Some(path) = b.pdf_path.clone() {
                crate::platform::open_path(&path.to_string_lossy());
            }
        });
    });
    ui.on_pdf_save_as(|| {
        defer(|b| {
            let id = b.pdf_id.clone();
            b.save_document(&id);
        });
    });
    ui.on_pdf_install_engine(|| {
        defer(|b| {
            b.ui.set_pdf_installing(true);
            b.ui.set_pdf_status(t("pdf.installing").into());
            b.wa.send(Cmd::PdfEngineInstall);
        });
    });
    // Status viewer: play a video update in the player.
    ui.on_status_play(|| defer(|b| b.play_status_video()));
    // Drag selection over the conversation.
    ui.on_row_geom(|id, y, h| {
        let id = id.to_string();
        defer(move |b| {
            let epoch = b.geom_epoch;
            b.row_geom.insert(id.clone(), (y, h, epoch));
            b.media_for_visible_row(&id);
        });
    });
    ui.on_conv_scrolled(|| {
        defer(|b| {
            b.geom_epoch = b.geom_epoch.wrapping_add(1);
            b.schedule_row_gc();
        });
    });
    ui.on_download_media(|id| {
        let id = id.to_string();
        defer(move |b| b.request_manual_media(&id));
    });
    ui.on_open_lightbox(|id| {
        let id = id.to_string();
        defer(move |b| b.open_lightbox(&id));
    });
    ui.on_share_accept(|| defer(|b| b.share_history_send()));
    ui.on_share_cancel(|| {
        defer(|b| {
            b.share_target = None;
            b.ui.set_share_open(false);
        });
    });
    ui.on_drag_start(|id, y| {
        let id = id.to_string();
        defer(move |b| b.drag_anchor = Some((id, y)));
    });
    ui.on_drag_move(|y| defer(move |b| b.drag_move(y)));
    ui.on_drag_end(|| defer(|b| b.drag_anchor = None));
    // A press on a link's hand-cursor box, to be handed to the text
    // underneath. Not from inside the callback: that runs in the middle
    // of Slint dispatching the very press being replaced.
    ui.on_link_pressed({
        let weak = ui.as_weak();
        move |x, y| {
            let weak = weak.clone();
            slint::Timer::single_shot(Duration::ZERO, move || {
                if let Some(ui) = weak.upgrade() {
                    replay_link_press(
                        ui.window(),
                        x,
                        y,
                        &|on| ui.set_link_press_through(on),
                        &|| ui.get_link_release_seen(),
                    );
                }
            });
        }
    });
    ui.on_open_link_at(|id, offset| {
        let id = id.to_string();
        defer(move |b| b.open_link_at(&id, offset.max(0) as usize));
    });
    ui.on_copy_image(|id| {
        let id = id.to_string();
        defer(move |b| {
            let Some(cached) = b.media_path.get(&id) else { return };
            let Some(path) = crate::media::temp_plain(&b.media_key, &std::path::PathBuf::from(cached)) else { return };
            std::thread::spawn(move || {
                let Ok(image) = image::open(path) else { return };
                let image = image.to_rgba8();
                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                    let _ = clipboard.set_image(arboard::ImageData {
                        width: image.width() as usize,
                        height: image.height() as usize,
                        bytes: std::borrow::Cow::Owned(image.into_raw()),
                    });
                }
            });
        });
    });
    ui.on_copy_text(|id| {
        let id = id.to_string();
        defer(move |b| b.copy_messages(&[id]));
    });
    // Sync banner and its progress dialog.
    ui.on_sync_clicked(|| defer(|b| b.show_sync_dialog()));
    ui.on_day_at_top(|day| {
        let day = day.to_string();
        defer(move |b| {
            if b.ui.get_floating_day() != day.as_str() {
                b.ui.set_floating_day(day.into());
            }
        });
    });
    // Composer: mentions.
    ui.on_composer_changed(|text, cursor| {
        let text = text.to_string();
        defer(move |b| b.composer_changed(&text, cursor.max(0) as usize));
    });
    ui.on_mention_pick(|idx| defer(move |b| b.mention_pick(idx.max(0) as usize)));
    ui.on_mention_close(|| defer(|b| b.ui.set_mention_open(false)));
    // Selection mode.
    ui.on_select_start(|id| {
        let id = id.to_string();
        defer(move |b| b.select_start(&id));
    });
    ui.on_toggle_select(|id| {
        let id = id.to_string();
        defer(move |b| b.toggle_select(&id));
    });
    ui.on_select_cancel(|| defer(|b| b.select_end()));
    ui.on_select_copy(|| {
        defer(|b| {
            let ids = b.selected_in_order();
            b.copy_messages(&ids);
            b.select_end();
        });
    });
    ui.on_select_forward(|| defer(|b| b.request_forward_selected()));
    ui.on_select_star(|| defer(|b| b.star_selected()));
    ui.on_select_delete(|| defer(|b| b.confirm_delete_selected()));
    // Chat menu.
    ui.on_menu_add_member(|| defer(|b| b.open_member_picker()));
    ui.on_menu_search(|| defer(|b| b.open_chat_search()));
    ui.on_menu_mute(|hours| defer(move |b| b.mute_current(hours)));
    ui.on_menu_ephemeral(|secs| defer(move |b| b.set_ephemeral_current(secs.max(0) as u32)));
    ui.on_menu_favorite(|| defer(|b| b.toggle_favorite()));
    ui.on_menu_list(|id| {
        let id = id.to_string();
        defer(move |b| b.toggle_list(&id));
    });
    ui.on_menu_new_list(|name| {
        let name = name.to_string();
        defer(move |b| b.create_list(&name));
    });
    ui.on_menu_delete_list(|id| {
        let id = id.to_string();
        defer(move |b| b.delete_list(&id));
    });
    ui.on_menu_export(|| defer(|b| b.export_chat()));
    ui.on_menu_close_chat(|| defer(|b| b.close_chat()));
    ui.on_menu_clear(|| {
        defer(|b| {
            if let Some(jid) = b.current_jid.clone() {
                b.ask_confirm(
                    t("confirm.clear_title"),
                    t("confirm.clear_body"),
                    t("confirm.clear"),
                    ConfirmAction::ClearChat(jid),
                );
            }
        });
    });
    ui.on_menu_delete_chat(|| {
        defer(|b| {
            if let Some(jid) = b.current_jid.clone() {
                b.ask_confirm(
                    t("confirm.delete_chat_title"),
                    t("confirm.delete_chat_body"),
                    t("confirm.delete"),
                    ConfirmAction::DeleteChat(jid),
                );
            }
        });
    });
    ui.on_menu_exit_group(|| {
        defer(|b| {
            if let Some(jid) = b.current_jid.clone() {
                let name = b.store.chat_name(&jid);
                b.ask_confirm(
                    ta("confirm.exit_title", &[&name]),
                    t("confirm.exit_body"),
                    t("confirm.exit"),
                    ConfirmAction::ExitGroup(jid),
                );
            }
        });
    });
    ui.on_confirm_accept(|| defer(|b| b.confirm_accepted()));
    ui.on_confirm_other(|| defer(|b| b.confirm_secondary()));
    // Member picker.
    ui.on_member_search(|query| {
        let query = query.to_string();
        defer(move |b| b.fill_member_rows(&query));
    });
    ui.on_member_toggle(|jid| {
        let jid = jid.to_string();
        defer(move |b| {
            if !b.member_picked.remove(&jid) {
                b.member_picked.insert(jid);
            }
            let count = b.member_picked.len() as i32;
            b.ui.set_member_count(count);
            for i in 0..b.member_model.row_count() {
                if let Some(mut row) = b.member_model.row_data(i) {
                    let checked = b.member_picked.contains(row.jid.as_str());
                    if row.checked != checked {
                        row.checked = checked;
                        b.member_model.set_row_data(i, row);
                    }
                }
            }
        });
    });
    ui.on_member_confirm(|| defer(|b| b.confirm_add_members()));
    // Search.
    ui.on_open_hit(|jid, id| {
        let (jid, id) = (jid.to_string(), id.to_string());
        defer(move |b| b.open_hit(&jid, &id));
    });
    ui.on_chat_search_changed(|query| {
        let query = query.to_string();
        defer(move |b| b.chat_search(&query));
    });
    ui.on_chat_search_step(|delta| defer(move |b| b.chat_search_step(delta)));
    ui.on_chat_search_close(|| {
        defer(|b| {
            b.ui.set_chat_search_open(false);
            b.ui.set_highlight_id("".into());
        });
    });
    ui.on_notice_clicked(|id| {
        let id = id.to_string();
        defer(move |b| {
            if let Some(rest) = id.strip_prefix("sys:invite:")
                && let Some((_, member)) = rest.split_once(':')
                && let Some(group) = b.current_jid.clone()
            {
                b.open_share_dialog(&group, member);
            } else {
                b.open_contact_info();
            }
        });
    });
    ui.on_open_member(|jid| {
        let jid = jid.to_string();
        defer(move |b| {
            b.ui.set_info_open(false);
            b.open_dm(&jid, None);
        });
    });
}
