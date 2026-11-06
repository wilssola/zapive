// Right-click menu of a row in the conversation list. It acts on the chat
// that was clicked, which need not be the open one.
use super::*;

impl Bridge {
    // Fills the menu's flags from the store, then shows it.
    pub(super) fn open_row_menu(&mut self, jid: &str) {
        let jid = self.store.canon_owned(&normalize_jid(jid));
        let Some(meta) = self.store.chats.get(&jid) else {
            return;
        };
        self.ui.set_row_menu_pinned(meta.pinned > 0);
        self.ui.set_row_menu_muted(meta.is_muted());
        self.ui.set_row_menu_archived(meta.archived);
        self.ui.set_row_menu_favorite(meta.favorite);
        self.ui.set_row_menu_unread(meta.unread > 0);
        self.ui.set_row_menu_jid(jid.into());
        self.ui.set_row_menu_sub("".into());
        self.ui.set_row_menu_open(true);
    }

    // `arg` carries the mute length in hours for "mute".
    pub(super) fn row_menu_action(&mut self, jid: &str, action: &str, arg: i32) {
        let jid = self.store.canon_owned(&normalize_jid(jid));
        match action {
            "info" => {
                self.open_dm(&jid, None);
                self.open_contact_info();
            }
            "pin" => {
                let pinned = self.store.chats.get(&jid).is_some_and(|c| c.pinned > 0);
                let next = !pinned;
                if let Some(meta) = self.store.chats.get_mut(&jid) {
                    meta.pinned = if next { now_secs().max(1) } else { 0 };
                }
                self.wa.send(Cmd::PinChat {
                    jid,
                    pinned: next,
                });
                self.schedule_refresh_chats();
            }
            "mute" => self.mute_chat(&jid, arg),
            "archive" => {
                self.toggle_archive_chat(&jid);
            }
            "favorite" => self.toggle_favorite_for(&jid),
            "unread" => {
                // Only offered for chats without unread messages.
                self.wa.send(Cmd::MarkUnread(jid));
            }
            "delete" => self.ask_confirm(
                t("confirm.delete_chat_title"),
                t("confirm.delete_chat_body"),
                t("confirm.delete"),
                ConfirmAction::DeleteChat(jid),
            ),
            _ => {}
        }
    }
}
