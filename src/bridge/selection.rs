// Selection mode, confirm dialog, export and drag selection.
use super::*;

// Destructive actions wait for a second click.
pub(super) enum ConfirmAction {
    ExitGroup(String),
    ClearChat(String),
    DeleteSelected { for_everyone: bool },
    DeleteChat(String),
    // Primary button wipes the conversations too; the secondary keeps them.
    Logout,
    RemoveAccount(String),
    // From the login screen: drop what a past session left on disk.
    ForgetConversations,
}

impl Bridge {
    // ---- confirm dialog ----

    pub(super) fn ask_confirm(
        &mut self,
        title: String,
        body: String,
        action: String,
        what: ConfirmAction,
    ) {
        self.ui.set_chat_menu_open(false);
        self.confirm_action = Some(what);
        self.ui.set_confirm_title(title.into());
        self.ui.set_confirm_text(body.into());
        self.ui.set_confirm_action(action.into());
        self.ui.set_confirm_secondary("".into());
        self.ui.set_confirm_open(true);
    }

    pub(super) fn confirm_accepted(&mut self) {
        self.ui.set_confirm_open(false);
        let Some(action) = self.confirm_action.take() else {
            return;
        };
        match action {
            ConfirmAction::ExitGroup(jid) => {
                self.wa.send(Cmd::LeaveGroup(jid));
            }
            ConfirmAction::ClearChat(jid) => self.clear_chat(&jid, true),
            ConfirmAction::DeleteChat(jid) => {
                let newest = self.store.chats.get(&jid).map_or(0, |c| c.timestamp);
                self.store.delete_chat(&jid);
                self.wa.send(Cmd::DeleteChat(jid.clone(), newest));
                if Some(&jid) == self.current_jid.as_ref() {
                    self.close_chat();
                }
                self.schedule_refresh_chats();
            }
            ConfirmAction::DeleteSelected { for_everyone } => self.delete_selected(for_everyone),
            ConfirmAction::Logout => self.logout(true),
            ConfirmAction::RemoveAccount(id) => self.remove_account(&id),
            ConfirmAction::ForgetConversations => {
                self.wipe_conversations();
                self.forget_account_identity();
                self.refresh_accounts();
            }
        }
    }

    // The dialog's second button: "delete for me" when the primary one
    // is "delete for everyone".
    pub(super) fn confirm_secondary(&mut self) {
        self.ui.set_confirm_open(false);
        match self.confirm_action.take() {
            Some(ConfirmAction::DeleteSelected { .. }) => self.delete_selected(false),
            Some(ConfirmAction::Logout) => self.logout(false),
            _ => {}
        }
    }

    // ---- selection mode ----

    pub(super) fn select_start(&mut self, id: &str) {
        self.select_mode = true;
        self.selected.clear();
        self.ui.set_chat_menu_open(false);
        self.ui.set_react_bar_id("".into());
        self.ui.set_select_mode(true);
        if !id.is_empty() {
            self.toggle_select(id);
        } else {
            self.ui.set_select_count(0);
        }
    }

    pub(super) fn toggle_select(&mut self, id: &str) {
        if !self.select_mode {
            self.select_start(id);
            return;
        }
        let on = if let Some(at) = self.selected.iter().position(|s| s == id) {
            self.selected.remove(at);
            false
        } else {
            self.selected.push(id.to_string());
            true
        };
        self.patch_row(id, |row| row.selected = on);
        self.ui.set_select_count(self.selected.len() as i32);
    }

    pub(super) fn select_end(&mut self) {
        if !self.select_mode && self.selected.is_empty() {
            self.ui.set_select_mode(false);
            return;
        }
        self.select_mode = false;
        let ids = std::mem::take(&mut self.selected);
        for id in ids {
            self.patch_row(&id, |row| row.selected = false);
        }
        self.ui.set_select_mode(false);
        self.ui.set_select_count(0);
    }

    // Selected ids in conversation order, so a copy reads top to bottom.
    pub(super) fn selected_in_order(&self) -> Vec<String> {
        let Some(jid) = self.current_jid.as_ref() else {
            return Vec::new();
        };
        self.store
            .messages_for(jid)
            .iter()
            .filter(|m| self.selected.iter().any(|s| s == &m.id))
            .map(|m| m.id.clone())
            .collect()
    }

    // One message copies as its text; several copy the way WhatsApp
    // does, each on a line with its stamp and author.
    pub(super) fn copy_messages(&mut self, ids: &[String]) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        let lines: Vec<String> = ids
            .iter()
            .filter_map(|id| self.store.messages_for(&jid).iter().find(|m| &m.id == id))
            .map(|m| {
                if ids.len() == 1 {
                    return self.store.named_mentions(&m.text);
                }
                let body = self.store.named_mentions(&copy_body(m));
                {
                    let who = if m.from_me {
                        self.store.chat_name(&self.self_jid)
                    } else if m.sender.is_empty() {
                        self.store.chat_name(&m.jid)
                    } else {
                        m.sender.clone()
                    };
                    format!("[{}] {who}: {body}", format_stamp(m.timestamp))
                }
            })
            .collect();
        if lines.is_empty() {
            return;
        }
        let text = lines.join("\n");
        if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
            eprintln!("[clipboard] copy failed: {e}");
        }
    }

    pub(super) fn request_forward_selected(&mut self) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        let ids = self.selected_in_order();
        if ids.is_empty() {
            return;
        }
        let count = ids.len();
        self.reset_forward_picker();
        self.pending_forward = ids.iter().map(|id| (jid.clone(), id.clone())).collect();
        let preview = if count == 1 {
            self.find_message(&ids[0])
                .map(preview_body)
                .unwrap_or_default()
        } else {
            ta("forward.count", &[&count.to_string()])
        };
        let image = if count == 1 {
            self.decoded.get(&ids[0]).map(|(img, _, _)| img.clone())
        } else {
            None
        };
        self.ui.set_forward_preview_text(preview.into());
        self.ui.set_forward_preview_has_image(image.is_some());
        self.ui
            .set_forward_preview_image(image.unwrap_or_else(empty_image));
        self.ui.set_forward_open(true);
        self.select_end();
    }

    pub(super) fn star_selected(&mut self) {
        let ids = self.selected_in_order();
        // Starring the lot when any is unstarred; unstarring otherwise.
        let any_unstarred = ids
            .iter()
            .any(|id| self.find_message(id).is_some_and(|m| !m.starred));
        for id in ids {
            let starred = self.find_message(&id).map(|m| m.starred).unwrap_or(false);
            if starred != any_unstarred {
                self.toggle_star(&id);
            }
        }
        self.select_end();
    }

    pub(super) fn confirm_delete_selected(&mut self) {
        let ids = self.selected_in_order();
        if ids.is_empty() {
            return;
        }
        let all_mine = ids
            .iter()
            .all(|id| self.find_message(id).is_some_and(|m| m.from_me));
        let count = ids.len().to_string();
        self.confirm_action = Some(ConfirmAction::DeleteSelected {
            for_everyone: all_mine,
        });
        self.ui
            .set_confirm_title(ta("confirm.delete_title", &[&count]).into());
        self.ui.set_confirm_text(t("confirm.delete_body").into());
        self.ui.set_confirm_action(
            t(if all_mine {
                "confirm.delete_everyone"
            } else {
                "confirm.delete_me"
            })
            .into(),
        );
        self.ui.set_confirm_secondary(
            if all_mine {
                t("confirm.delete_me")
            } else {
                String::new()
            }
            .into(),
        );
        self.ui.set_confirm_open(true);
    }

    pub(super) fn delete_selected(&mut self, for_everyone: bool) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        let ids = self.selected_in_order();
        if for_everyone {
            for id in &ids {
                self.delete_message(id);
            }
        } else {
            for id in &ids {
                if let Some(m) = self.find_message(id).cloned() {
                    self.wa.send(Cmd::DeleteForMe {
                        jid: jid.clone(),
                        id: id.clone(),
                        from_me: m.from_me,
                        participant: self.key_participant(&m),
                        ts: m.timestamp,
                    });
                }
            }
            if self.store.remove_messages(&jid, &ids) {
                self.rebuild_conversation(&jid);
                self.schedule_refresh_chats();
            }
        }
        self.select_end();
    }

    // ---- export ----

    // Writes the conversation as WhatsApp's "export chat" text file.
    pub(super) fn export_chat(&mut self) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        self.ui.set_chat_menu_open(false);
        self.warm_chat(&jid);
        let name = self.store.chat_name(&jid);
        let me = self.store.chat_name(&self.self_jid);
        let lines: Vec<String> = self
            .store
            .messages_for(&jid)
            .iter()
            .map(|m| {
                let stamp = format_stamp(m.timestamp).replace(' ', ", ");
                if m.kind == MessageKind::System {
                    return format!("{stamp} - {}", m.text);
                }
                let who = if m.from_me {
                    me.clone()
                } else if m.sender.is_empty() {
                    name.clone()
                } else {
                    m.sender.clone()
                };
                format!(
                    "{stamp} - {who}: {}",
                    self.store.named_mentions(&copy_body(m))
                )
            })
            .collect();
        let text = lines.join("\n");
        let file_name = format!(
            "WhatsApp Chat - {}.txt",
            name.chars()
                .filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
                .collect::<String>()
        );
        std::thread::spawn(move || {
            let picked = rfd::FileDialog::new()
                .set_file_name(&file_name)
                .add_filter("Text", &["txt"])
                .save_file();
            if let Some(path) = picked
                && let Err(e) = std::fs::write(&path, text)
            {
                eprintln!("[export] write failed: {e}");
            }
        });
    }

    // ---- drag selection ----

    // The pointer moved while pressed on a row's background: every row
    // between the anchor and the pointer joins the selection.
    pub(super) fn drag_move(&mut self, y: f32) {
        let Some((anchor_id, anchor_y)) = self.drag_anchor.clone() else {
            return;
        };
        if (y - anchor_y).abs() < 8.0 && !self.select_mode {
            return;
        }
        let (lo, hi) = (anchor_y.min(y), anchor_y.max(y));
        let epoch = self.geom_epoch;
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        let mut wanted: Vec<String> = self
            .row_geom
            .iter()
            .filter(|(_, (_, _, e))| *e == epoch)
            .filter(|(_, (ry, rh, _))| *ry <= hi && ry + rh >= lo)
            .map(|(id, _)| id.clone())
            .collect();
        if !wanted.iter().any(|w| w == &anchor_id) {
            wanted.push(anchor_id);
        }
        // Only real messages, in conversation order.
        let ordered: Vec<String> = self
            .store
            .messages_for(&jid)
            .iter()
            .filter(|m| m.kind != MessageKind::System && wanted.iter().any(|w| w == &m.id))
            .map(|m| m.id.clone())
            .collect();
        self.set_selection(ordered);
    }

    pub(super) fn set_selection(&mut self, ids: Vec<String>) {
        if !self.select_mode {
            self.select_mode = true;
            self.ui.set_select_mode(true);
            self.ui.set_chat_menu_open(false);
            self.ui.set_react_bar_id("".into());
        }
        let previous = std::mem::replace(&mut self.selected, ids);
        for id in &previous {
            if !self.selected.contains(id) {
                self.patch_row(id, |row| row.selected = false);
            }
        }
        let now = self.selected.clone();
        for id in &now {
            if !previous.contains(id) {
                self.patch_row(id, |row| row.selected = true);
            }
        }
        self.ui.set_select_count(self.selected.len() as i32);
    }
}
