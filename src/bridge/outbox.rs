// Attachments staged for sending and the outbox that delivers them.
use super::*;

impl Bridge {
    // The just-sent message flows through the normal pipeline; the later
    // echo from the server is deduplicated by id in the store.
    pub fn stage_attachment(&mut self, preparation: crate::outbox::Preparation) -> bool {
        let key = format!("store:preparation:{}", preparation.id);
        let result = serde_json::to_string(&preparation)
            .map_err(|e| e.to_string())
            .and_then(|json| {
                self.vault
                    .as_ref()
                    .ok_or("vault unavailable".to_string())?
                    .set_checked(&key, &json)
            });
        if result.is_err() {
            self.message_action_failed(&t("outbox.storage_failed"));
            return false;
        }
        self.show_preparation(&preparation, "outbox.preparing");
        if self.outbox_connected && self.preparations_active.insert(preparation.id.clone()) {
            self.wa.send(Cmd::PrepareAttachment(preparation));
        }
        true
    }

    pub(super) fn show_preparation(
        &mut self,
        preparation: &crate::outbox::Preparation,
        state: &str,
    ) {
        use whatsapp_rust::proto_helpers::MessageBuilderExt as _;
        self.preparation_states
            .insert(preparation.id.clone(), state == "outbox.failed");
        self.echo_sent(
            &preparation.jid,
            &preparation.id,
            wa::Message::text(format!("{}: {}", t(state), preparation.filename)),
        );
        self.patch_row(&preparation.id, |row| {
            row.sendPending = true;
            row.sendRetry = state == "outbox.failed";
            row.sendState = t(state).into();
            row.ticks = "pending".into();
        });
    }

    pub(super) fn resume_preparations(&mut self) {
        let preparations: Vec<_> = self
            .vault
            .as_ref()
            .map(|vault| {
                vault
                    .keys("store:preparation:")
                    .into_iter()
                    .filter_map(|key| vault.get(&key))
                    .filter_map(|json| {
                        serde_json::from_str::<crate::outbox::Preparation>(&json).ok()
                    })
                    .collect()
            })
            .unwrap_or_default();
        for preparation in preparations {
            self.show_preparation(&preparation, "outbox.preparing");
            if self.outbox_connected && self.preparations_active.insert(preparation.id.clone()) {
                self.wa.send(Cmd::PrepareAttachment(preparation));
            }
        }
    }

    pub fn on_attachment_prepared(&mut self, id: &str, result: Result<wa::Message, String>) {
        self.preparations_active.remove(id);
        let key = format!("store:preparation:{id}");
        let preparation = self
            .vault
            .as_ref()
            .and_then(|vault| vault.get(&key))
            .and_then(|json| serde_json::from_str::<crate::outbox::Preparation>(&json).ok());
        let Some(preparation) = preparation else {
            return;
        };
        let message = match result {
            Ok(message) => message,
            Err(_) => {
                self.show_preparation(&preparation, "outbox.failed");
                return;
            }
        };
        let mut entries = self.outbox.clone();
        entries.push(crate::outbox::Entry::new(
            id.to_string(),
            preparation.jid.clone(),
            std::sync::Arc::new(message.clone()),
            now_secs(),
        ));
        let json = match serde_json::to_string(&entries) {
            Ok(json) => json,
            Err(_) => return,
        };
        let saved = self.vault.as_ref().is_some_and(|vault| {
            vault
                .batch_checked(&[("store:outbox", &json)], &[&key])
                .is_ok()
        });
        if !saved {
            self.message_action_failed(&t("outbox.storage_failed"));
            return;
        }
        self.outbox = entries;
        self.preparation_states.remove(id);
        if let Some(messages) = self.store.messages.get_mut(&preparation.jid) {
            messages.retain(|message| message.id != id);
        }
        self.echo_sent_from(&preparation.jid, id, message, preparation.origin);
        if self.current_jid.as_deref() == Some(&preparation.jid) {
            self.rebuild_conversation(&preparation.jid);
        }
        self.dispatch_outbox();
    }

    pub(super) fn persist_outbox(&self, entries: &[crate::outbox::Entry]) -> Result<(), String> {
        let vault = self.vault.as_ref().ok_or("vault unavailable")?;
        let json = serde_json::to_string(entries).map_err(|e| e.to_string())?;
        vault.set_checked("store:outbox", &json)
    }

    pub(super) fn restore_outbox(&mut self) {
        self.outbox_connected = false;
        self.resume_preparations();
        let cached = self.vault.as_ref().and_then(|v| v.get("store:outbox"));
        self.outbox = cached
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        for entry in &mut self.outbox {
            entry.recover_interrupted();
        }
        let _ = self.persist_outbox(&self.outbox);
        for entry in self.outbox.clone() {
            let found = self
                .vault
                .as_ref()
                .is_some_and(|v| self.store.hydrate_id(v, &entry.jid, &entry.id));
            if found
                && self
                    .store
                    .messages_for(&entry.jid)
                    .iter()
                    .any(|message| message.id == entry.id && message.status >= 1)
            {
                self.on_outbox_ack(&entry.id, None);
                continue;
            }
            if !found {
                self.echo_sent(&entry.jid, &entry.id, (*entry.message).clone());
                if let Some(message) = self
                    .store
                    .messages
                    .get_mut(&entry.jid)
                    .and_then(|messages| messages.iter_mut().find(|m| m.id == entry.id))
                {
                    message.timestamp = entry.created;
                }
            }
        }
    }

    pub fn enqueue_prepared(
        &mut self,
        jid: &str,
        message: wa::Message,
        origin: Option<String>,
    ) -> bool {
        use rand::RngCore as _;
        let mut bytes = [0u8; 16];
        rand::rng().fill_bytes(&mut bytes);
        let id = format!(
            "3EB0{}",
            bytes.iter().map(|b| format!("{b:02X}")).collect::<String>()
        );
        let jid = self.store.canon_owned(jid);
        let entry = crate::outbox::Entry::new(
            id.clone(),
            jid.clone(),
            std::sync::Arc::new(message.clone()),
            now_secs(),
        );
        let mut entries = self.outbox.clone();
        entries.push(entry);
        if self.persist_outbox(&entries).is_err() {
            self.message_action_failed(&t("outbox.storage_failed"));
            return false;
        }
        self.outbox = entries;
        self.echo_sent_from(&jid, &id, message, origin);
        self.dispatch_outbox();
        true
    }

    pub(super) fn dispatch_outbox(&mut self) {
        use crate::outbox::State;
        if !self.outbox_connected || self.switching {
            return;
        }
        if self
            .outbox
            .iter()
            .any(|entry| entry.state == State::Sending)
        {
            return;
        }
        let Some(index) = self
            .outbox
            .iter()
            .position(|entry| entry.state == State::Queued)
        else {
            return;
        };
        let mut entries = self.outbox.clone();
        entries[index].state = State::Sending;
        if self.persist_outbox(&entries).is_err() {
            self.message_action_failed(&t("outbox.storage_failed"));
            return;
        }
        self.outbox = entries;
        let entry = self.outbox[index].clone();
        self.refresh_outbox_row(&entry.id);
        self.wa.send(Cmd::SendQueued {
            id: entry.id.clone(),
            jid: entry.jid,
            message: entry.message,
        });
        let epoch = self.epoch;
        self.once(45_000, move |b| {
            if b.epoch != epoch {
                return;
            }
            if let Some(pending) = b
                .outbox
                .iter_mut()
                .find(|e| e.id == entry.id && e.state == State::Sending)
            {
                pending.state = State::Uncertain;
                let _ = b.persist_outbox(&b.outbox);
                b.refresh_outbox_row(&entry.id);
                b.dispatch_outbox();
            }
        });
    }

    pub(super) fn refresh_outbox_row(&self, id: &str) {
        let state = self
            .outbox
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.state);
        let label = match state {
            Some(crate::outbox::State::Queued) => t("outbox.queued"),
            Some(crate::outbox::State::Sending) => t("outbox.sending"),
            Some(crate::outbox::State::Uncertain) => t("outbox.uncertain"),
            Some(crate::outbox::State::Failed) => t("outbox.failed"),
            None => String::new(),
        };
        self.patch_row(id, |row| {
            row.sendState = label.into();
            row.sendRetry = matches!(
                state,
                Some(crate::outbox::State::Failed | crate::outbox::State::Uncertain)
            );
            row.sendPending = state.is_some();
            if state.is_some() {
                row.ticks = "pending".into();
            }
        });
    }

    pub fn on_outbox_result(&mut self, _jid: &str, id: &str, result: Result<(), String>) {
        if let Some(entry) = self.outbox.iter_mut().find(|entry| entry.id == id)
            && result.is_err()
        {
            entry.state = crate::outbox::State::Uncertain;
            entry.error = t("outbox.uncertain");
            let _ = self.persist_outbox(&self.outbox);
            self.refresh_outbox_row(id);
            self.dispatch_outbox();
        }
    }

    pub fn on_outbox_ack(&mut self, id: &str, error: Option<String>) {
        if !self.outbox.iter().any(|entry| entry.id == id) {
            return;
        }
        if let Some(error) = error {
            if let Some(entry) = self.outbox.iter_mut().find(|entry| entry.id == id) {
                entry.state = crate::outbox::State::Failed;
                entry.error = error;
            }
        } else {
            let entry = self
                .outbox
                .iter()
                .find(|entry| entry.id == id)
                .cloned()
                .unwrap();
            if let Some(vault) = &self.vault {
                self.store.hydrate_id(vault, &entry.jid, id);
            }
            self.store.set_status(&entry.jid, id, 1);
            if let Some(vault) = &self.vault
                && self.store.save_checked(vault).is_err()
            {
                return;
            }
            let remaining: Vec<_> = self
                .outbox
                .iter()
                .filter(|entry| entry.id != id)
                .cloned()
                .collect();
            if self.persist_outbox(&remaining).is_err() {
                return;
            }
            self.outbox = remaining;
        }
        let _ = self.persist_outbox(&self.outbox);
        self.refresh_outbox_row(id);
        if !self.outbox.iter().any(|entry| entry.id == id) {
            self.patch_row(id, |row| row.ticks = "✓".into());
        }
        self.dispatch_outbox();
    }

    pub fn echo_sent_from(
        &mut self,
        jid: &str,
        id: &str,
        message: wa::Message,
        origin: Option<String>,
    ) {
        if let Some(origin) = origin
            && let Some(vault) = &self.vault
        {
            vault.setting_set(&format!("media-source:{id}"), &origin);
        }
        self.echo_sent(jid, id, message);
    }

    pub fn echo_sent(&mut self, jid: &str, id: &str, message: wa::Message) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let meta = crate::wa_map::LiveMeta {
            chat: jid.to_string(),
            sender: self.self_jid.clone(),
            sender_alt: None,
            recipient_alt: None,
            id,
            from_me: true,
            push_name: "",
            timestamp: now,
        };
        let Some(stored) =
            crate::wa_map::normalize(&mut self.store, &meta, &message, &HashMap::new(), 0)
        else {
            return;
        };
        let jid = stored.jid.clone();
        let stored_id = stored.id.clone();
        if self.store.add_message(stored) && Some(&jid) == self.current_jid.as_ref() {
            self.push_message_row(&jid, &stored_id);
        }
        self.schedule_refresh_chats();
    }

    // ---- outgoing media ----

    pub(super) fn handle_attach(&mut self, kind: &'static str) {
        if self.current_jid.is_none() {
            return;
        }
        let jid = self.current_jid.clone();
        let epoch = self.epoch;
        let generation = self.preview_generation;
        std::thread::spawn(move || {
            let dialog = rfd::FileDialog::new();
            let dialog = match kind {
                "image" => {
                    dialog.add_filter(t("picker.images"), &["jpg", "jpeg", "png", "webp", "bmp"])
                }
                "audio" => dialog.add_filter(
                    t("picker.audio"),
                    &["mp3", "ogg", "opus", "m4a", "aac", "wav"],
                ),
                _ => dialog.add_filter(t("picker.all"), &["*"]),
            };
            if let Some(path) = dialog.pick_file() {
                ui_apply(move |b| {
                    if b.current_jid == jid
                        && b.epoch == epoch
                        && b.preview_generation == generation
                    {
                        b.on_picked(kind, path);
                    }
                });
            }
        });
    }

    pub(super) fn on_picked(&mut self, kind: &str, path: std::path::PathBuf) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        match kind {
            // WhatsApp-style confirmation: caption field before sending.
            "image" => self.start_preview(path),
            "audio" => self.wa.send(Cmd::SendAudioFile {
                jid,
                origin: Some(path.to_string_lossy().into_owned()),
                path,
            }),
            _ => self.wa.send(Cmd::SendDocument {
                jid,
                origin: Some(path.to_string_lossy().into_owned()),
                path,
            }),
        }
    }

    pub(super) fn cancel_preview(&mut self) {
        self.preview_generation = self.preview_generation.wrapping_add(1);
        if let Some(path) = self.pending_image.take()
            && self.original_image.as_ref() != Some(&path)
        {
            let _ = std::fs::remove_file(path);
        }
        self.original_image = None;
        self.staged_document = None;
        self.preview_jid = None;
        self.preview_busy = false;
        self.ui.set_preview_busy(false);
        self.ui.set_preview_open(false);
    }

    pub(super) fn start_preview(&mut self, path: std::path::PathBuf) {
        self.cancel_preview();
        self.preview_jid = self.current_jid.clone();
        self.original_image = Some(path.clone());
        self.load_preview(path);
    }

    pub(super) fn load_preview(&mut self, path: std::path::PathBuf) {
        self.preview_generation = self.preview_generation.wrapping_add(1);
        let generation = self.preview_generation;
        let epoch = self.epoch;
        self.preview_busy = true;
        self.ui.set_preview_busy(true);
        std::thread::spawn(move || {
            let image = std::fs::read(&path)
                .ok()
                .and_then(|bytes| crate::media::decode_bytes(&bytes, 1280));
            ui_apply(move |b| {
                if b.preview_generation != generation
                    || b.epoch != epoch
                    || b.current_jid != b.preview_jid
                {
                    return;
                }
                b.preview_busy = false;
                b.ui.set_preview_busy(false);
                if let Some(image) = image {
                    b.on_preview_ready(&path, image);
                }
            });
        });
    }

    pub fn on_preview_ready(&mut self, path: &std::path::Path, img: crate::media::Decoded) {
        if self.current_jid.is_none()
            || self.current_jid != self.preview_jid
            || self.original_image.as_deref() != Some(path)
        {
            return;
        }
        self.pending_image = Some(path.to_path_buf());
        self.ui.set_preview_image(image_of(&img));
        self.ui.set_preview_open(true);
        self.ui.invoke_focus_preview_caption();
    }

    pub(super) fn reset_preview(&mut self) {
        let Some(path) = self.original_image.clone() else {
            return;
        };
        if let Some(edited) = self.pending_image.take()
            && edited != path
        {
            let _ = std::fs::remove_file(edited);
        }
        self.load_preview(path);
    }

    pub(super) fn edit_preview(&mut self, edit: crate::media::PreviewEdit) {
        if self.preview_busy || !self.ui.get_preview_open() || self.current_jid != self.preview_jid
        {
            return;
        }
        let Some(path) = self.pending_image.clone() else {
            return;
        };
        self.preview_generation = self.preview_generation.wrapping_add(1);
        let generation = self.preview_generation;
        let epoch = self.epoch;
        self.preview_busy = true;
        self.ui.set_preview_busy(true);
        std::thread::spawn(move || {
            let result = crate::media::edit_preview(&path, edit);
            ui_apply(move |b| {
                if b.preview_generation != generation
                    || b.epoch != epoch
                    || b.current_jid != b.preview_jid
                {
                    if let Some((path, _)) = result {
                        let _ = std::fs::remove_file(path);
                    }
                    return;
                }
                b.preview_busy = false;
                b.ui.set_preview_busy(false);
                if let Some((path, image)) = result {
                    if let Some(previous) = b.pending_image.replace(path)
                        && b.original_image.as_ref() != Some(&previous)
                    {
                        let _ = std::fs::remove_file(previous);
                    }
                    b.ui.set_preview_image(image_of(&image));
                }
            });
        });
    }

    pub(super) fn confirm_send_image(&mut self, caption: &str) {
        if self.preview_busy || self.current_jid != self.preview_jid {
            return;
        }
        if self.confirm_staged_document() {
            return;
        }
        let path = self.pending_image.take();
        let origin = self
            .original_image
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        self.cancel_preview();
        let (Some(jid), Some(path)) = (self.current_jid.clone(), path) else {
            return;
        };
        let caption = caption.trim();
        self.wa.send(Cmd::SendImage {
            jid,
            origin,
            path,
            caption: (!caption.is_empty()).then(|| caption.to_string()),
        });
        self.next_staged();
    }

    // Fired on any Ctrl+V while a chat is open; only acts when the
    // clipboard holds an image (text pastes normally into the composer).
    pub(super) fn handle_paste(&mut self) {
        if self.current_jid.is_none() || self.ui.get_preview_open() {
            return;
        }
        if let Some(last) = self.last_paste
            && last.elapsed() < Duration::from_millis(400)
        {
            return;
        }
        self.last_paste = Some(Instant::now());
        let Ok(mut clipboard) = arboard::Clipboard::new() else {
            return;
        };
        let Ok(img) = clipboard.get_image() else {
            return;
        };
        let (w, h) = (img.width as u32, img.height as u32);
        let Some(buffer) = image::RgbaImage::from_raw(w, h, img.bytes.into_owned()) else {
            return;
        };
        let path = std::env::temp_dir().join(format!(
            "zapive_paste_{}.png",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        if image::DynamicImage::ImageRgba8(buffer).save(&path).is_ok() {
            self.start_preview(path);
        }
    }
}
