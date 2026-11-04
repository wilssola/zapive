// Polls: creating them, voting, collecting other people's votes and
// turning the stored votes into what the bubble shows.
use super::*;
use crate::store::{PollTally, next_poll_selection, poll_voter_count, tally_poll};
use base64::Engine as _;

const MAX_QUESTION_CHARS: usize = 255;
const MAX_OPTION_CHARS: usize = 100;
const MIN_OPTIONS: usize = 2;
const MAX_OPTIONS: usize = 12;

// The question and options as they will be sent: trimmed, control
// characters dropped, duplicates removed, limits applied. None when the
// poll would not be valid (empty question, fewer than two options).
pub(super) fn sanitize_poll(question: &str, options: &[String]) -> Option<(String, Vec<String>)> {
    let clean = |text: &str, limit: usize| -> String {
        text.chars()
            .filter(|c| !c.is_control() || *c == ' ')
            .take(limit)
            .collect::<String>()
            .trim()
            .to_string()
    };
    let question = clean(question, MAX_QUESTION_CHARS);
    let mut seen = HashSet::new();
    let options: Vec<String> = options
        .iter()
        .map(|option| clean(option, MAX_OPTION_CHARS))
        .filter(|option| !option.is_empty() && seen.insert(option.clone()))
        .take(MAX_OPTIONS)
        .collect();
    (!question.is_empty() && options.len() >= MIN_OPTIONS).then_some((question, options))
}

// What the bubble needs, as plain data.
#[derive(Debug, Default, PartialEq)]
pub(super) struct PollView {
    pub options: Vec<PollTally>,
    pub multi: bool,
    pub total: usize,
    pub can_vote: bool,
}

pub(super) fn poll_view(
    m: &StoredMessage,
    me: &str,
    name_of: impl Fn(&str) -> String,
) -> Option<PollView> {
    let raw = m.raw.as_ref()?;
    let (_, options, selectable) = crate::wa_map::poll_definition(raw)?;
    if options.is_empty() {
        return None;
    }
    Some(PollView {
        options: tally_poll(&options, &m.poll_votes, me, name_of),
        multi: selectable != 1,
        total: poll_voter_count(&m.poll_votes),
        can_vote: secret_of(m).is_some() && !m.deleted && !m.revoked,
    })
}

fn secret_of(m: &StoredMessage) -> Option<Vec<u8>> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&m.poll_secret)
        .ok()?;
    (bytes.len() == 32).then_some(bytes)
}

impl Bridge {
    pub(super) fn poll_rows(&self, m: &StoredMessage) -> (Vec<crate::PollOption>, bool, i32, bool) {
        let name_of = |jid: &str| {
            if jid == "me" {
                return t("reactions.you");
            }
            let canon = self.store.canon_owned(&normalize_jid(jid));
            let name = self.store.chat_name(&canon);
            if name.is_empty() {
                display_id(&canon)
            } else {
                name
            }
        };
        let Some(view) = poll_view(m, "me", name_of) else {
            return (Vec::new(), false, 0, false);
        };
        let rows = view
            .options
            .into_iter()
            .map(|tally| crate::PollOption {
                name: tally.name.into(),
                votes: tally.votes.min(i32::MAX as usize) as i32,
                voted: tally.voted,
                voters: tally.voters.join("\n").into(),
            })
            .collect();
        (
            rows,
            view.multi,
            view.total.min(i32::MAX as usize) as i32,
            view.can_vote,
        )
    }

    // The poll dialog's "create". Nothing is sent when the chat is closed,
    // the chat cannot be written to, or the poll is not valid.
    pub(super) fn create_poll(&mut self, question: &str, options: &[String], multi: bool) {
        let Some(jid) = self.current_jid.clone() else {
            return;
        };
        if !self
            .store
            .chats
            .get(&jid)
            .is_some_and(|chat| chat.can_send())
            || is_channel(&jid)
        {
            return;
        }
        let Some((question, options)) = sanitize_poll(question, options) else {
            return;
        };
        let selectable = if multi { 0 } else { 1 };
        self.ui.set_poll_composer_open(false);
        self.wa.send(Cmd::CreatePoll {
            jid,
            question,
            options,
            selectable,
        });
    }

    // Confirmation from the network: the poll is now a message of ours.
    pub fn on_poll_created(&mut self, jid: &str, id: &str, message: wa::Message, secret: &[u8]) {
        let jid = self.store.canon_owned(&normalize_jid(jid));
        let now = now_secs();
        let meta = crate::wa_map::LiveMeta {
            chat: jid.clone(),
            sender: self.self_jid.clone(),
            sender_alt: None,
            recipient_alt: None,
            id,
            from_me: true,
            push_name: "",
            timestamp: now,
        };
        let Some(mut stored) =
            crate::wa_map::normalize(&mut self.store, &meta, &message, &HashMap::new(), 0)
        else {
            return;
        };
        stored.poll_secret = base64::engine::general_purpose::STANDARD.encode(secret);
        let target = stored.jid.clone();
        let stored_id = stored.id.clone();
        if self.store.add_message(stored) && Some(&target) == self.current_jid.as_ref() {
            self.push_message_row(&target, &stored_id);
        }
        self.schedule_refresh_chats();
        self.schedule_save();
    }

    // The user clicked option `index` of poll `id`.
    pub(super) fn poll_vote(&mut self, id: &str, index: i32) {
        let Some(m) = self.find_message(id).cloned() else {
            return;
        };
        let (Some(raw), Some(secret)) = (m.raw.as_ref(), secret_of(&m)) else {
            return;
        };
        let Some((_, options, selectable)) = crate::wa_map::poll_definition(raw) else {
            return;
        };
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        let current: Vec<String> = m
            .poll_votes
            .get("me")
            .map(|vote| {
                options
                    .iter()
                    .filter(|name| vote.options.contains(&crate::store::poll_option_hash(name)))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let Some(next) = next_poll_selection(&options, &current, index, selectable != 1) else {
            return;
        };
        let creator = if m.from_me {
            self.self_jid.clone()
        } else if m.sender_jid.is_empty() {
            m.jid.clone()
        } else {
            m.sender_jid.clone()
        };
        self.wa.send(Cmd::PollVote {
            jid: m.jid.clone(),
            id: m.id.clone(),
            creator,
            secret,
            options: next,
        });
    }

    // My own vote went out: show it at once instead of waiting for the echo.
    pub fn on_poll_vote_sent(&mut self, jid: &str, id: &str, picked: &[String]) {
        let jid = self.store.canon_owned(&normalize_jid(jid));
        let hashes = picked
            .iter()
            .map(|name| crate::store::poll_option_hash(name))
            .collect();
        let stamp = now_secs() * 1000;
        if self.store.apply_poll_vote(&jid, id, "me", hashes, stamp) {
            self.refresh_poll_row(&jid, id);
            self.schedule_save();
        }
    }

    // Another person's (or another device's) vote, decrypted.
    pub fn on_poll_vote(
        &mut self,
        jid: &str,
        id: &str,
        voter: &str,
        hashes: Vec<String>,
        timestamp_ms: i64,
    ) {
        let jid = self.store.canon_owned(&normalize_jid(jid));
        let voter_canon = self.store.canon_owned(&normalize_jid(voter));
        let key = if self.store.self_jids.contains(&voter_canon) {
            "me".to_string()
        } else {
            voter_canon
        };
        if self
            .store
            .apply_poll_vote(&jid, id, &key, hashes, timestamp_ms)
        {
            self.refresh_poll_row(&jid, id);
            self.schedule_save();
        }
    }

    fn refresh_poll_row(&mut self, jid: &str, id: &str) {
        if Some(jid) != self.current_jid.as_deref() {
            return;
        }
        let Some(m) = self
            .store
            .messages_for(jid)
            .iter()
            .find(|m| m.id == id)
            .cloned()
        else {
            return;
        };
        let (options, multi, total, can_vote) = self.poll_rows(&m);
        self.patch_row(id, |row| {
            row.poll_options = slint::ModelRc::new(VecModel::from(options));
            row.poll_multi = multi;
            row.poll_total = total;
            row.poll_can_vote = can_vote;
        });
    }

    // A vote arrived inside a message batch. Find its poll (hydrating the
    // chat), and hand the ciphertext to the client to open.
    pub(super) fn receive_poll_vote(
        &mut self,
        chat: &str,
        voter: &str,
        vote_message: &wa::message::PollUpdateMessage,
        timestamp_ms: i64,
    ) {
        let Some(key) = vote_message.poll_creation_message_key.as_option() else {
            return;
        };
        let Some(poll_id) = key.id.as_deref() else {
            return;
        };
        let Some(enc) = vote_message.vote.as_option() else {
            return;
        };
        let (Some(payload), Some(iv)) = (enc.enc_payload.clone(), enc.enc_iv.clone()) else {
            return;
        };
        let jid = self.store.canon_owned(&normalize_jid(chat));
        if let Some(vault) = &self.vault {
            self.store.hydrate_id(vault, &jid, poll_id);
        }
        let Some(poll) = self
            .store
            .messages_for(&jid)
            .iter()
            .find(|m| m.id == poll_id)
            .cloned()
        else {
            return;
        };
        let Some(secret) = secret_of(&poll) else {
            return;
        };
        let creator = if poll.from_me {
            self.self_jid.clone()
        } else if poll.sender_jid.is_empty() {
            poll.jid.clone()
        } else {
            poll.sender_jid.clone()
        };
        self.wa.send(Cmd::DecryptVote {
            jid,
            poll_id: poll_id.to_string(),
            creator,
            voter: voter.to_string(),
            secret,
            payload,
            iv,
            timestamp_ms,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn sanitize_trims_dedupes_and_limits() {
        let (question, opts) = sanitize_poll(
            "  Lunch? ",
            &options(&[" Pizza ", "Pizza", "", "Sushi", "\u{7}Tacos"]),
        )
        .unwrap();
        assert_eq!(question, "Lunch?");
        assert_eq!(opts, options(&["Pizza", "Sushi", "Tacos"]));
    }

    #[test]
    fn sanitize_rejects_invalid_polls() {
        assert!(sanitize_poll("", &options(&["A", "B"])).is_none());
        assert!(sanitize_poll("   ", &options(&["A", "B"])).is_none());
        assert!(sanitize_poll("Q", &options(&["A"])).is_none());
        assert!(sanitize_poll("Q", &options(&["A", "a "])).is_some());
        assert!(sanitize_poll("Q", &options(&["A", " A "])).is_none());
    }

    #[test]
    fn sanitize_caps_option_count_and_lengths() {
        let many: Vec<String> = (0..20).map(|i| format!("opt{i}")).collect();
        let (_, opts) = sanitize_poll("Q", &many).unwrap();
        assert_eq!(opts.len(), MAX_OPTIONS);
        let long = "x".repeat(500);
        let (question, opts) = sanitize_poll(&long, &[long.clone(), "y".into()]).unwrap();
        assert_eq!(question.chars().count(), MAX_QUESTION_CHARS);
        assert_eq!(opts[0].chars().count(), MAX_OPTION_CHARS);
    }
}
