// BUG-010: chats of unsaved numbers must stay in the list. Owned by Edsger.
// Synthetic data only: no account, no network, in-memory vault.
use super::*;
use crate::wa::HistChat;

const LID: &str = "1000000000001@lid";
const PN: &str = "5500000000001@s.whatsapp.net";
const OTHER_PN: &str = "5500000000002@s.whatsapp.net";

fn text_message(jid: &str, id: &str, timestamp: i64) -> StoredMessage {
    let mut message = crate::wa_map::system_message(jid, id, timestamp, format!("body {id}"));
    message.kind = MessageKind::Text;
    message
}

fn history_web(remote: &str, id: &str, timestamp: u64) -> wa::WebMessageInfo {
    wa::WebMessageInfo {
        key: Some(wa::MessageKey {
            id: Some(id.into()),
            remote_jid: Some(remote.into()),
            from_me: Some(false),
            ..Default::default()
        })
        .into(),
        message: Some(wa::Message {
            conversation: Some(format!("body {id}")),
            ..Default::default()
        })
        .into(),
        message_timestamp: Some(timestamp),
        ..Default::default()
    }
}

fn hist_chat(jid: &str, name: Option<&str>, timestamp: i64) -> HistChat {
    HistChat {
        jid: jid.into(),
        name: name.map(str::to_string),
        timestamp,
        unread: None,
        pinned: None,
        muted: None,
        archived: None,
    }
}

// Mirrors Bridge::on_history_chunk (incoming.rs), minus the UI work.
fn apply_chunk(store: &mut Store, chunk: &HistoryChunk) -> usize {
    for (jid, name) in &chunk.pushnames {
        store.upsert_contact(jid, None, Some(name));
    }
    for c in &chunk.chats {
        store.upsert_chat(
            &c.jid,
            c.name.as_deref(),
            c.timestamp,
            c.unread,
            c.pinned,
            c.archived,
        );
    }
    let mut added = 0;
    for web in &chunk.messages {
        if let Some(stored) = crate::wa_map::from_history(store, web)
            && store.add_message(stored)
        {
            added += 1;
        }
    }
    added
}

fn chunk(chats: Vec<HistChat>, messages: Vec<wa::WebMessageInfo>) -> HistoryChunk {
    HistoryChunk {
        on_demand: false,
        progress: None,
        chats,
        messages,
        pushnames: Vec::new(),
    }
}

fn listed(store: &Store) -> Vec<String> {
    store
        .sorted_chats()
        .into_iter()
        .map(|c| c.jid.clone())
        .collect()
}

fn ids(store: &Store, jid: &str) -> Vec<String> {
    let mut out: Vec<String> = store
        .messages_for(jid)
        .iter()
        .map(|m| m.id.clone())
        .collect();
    out.sort();
    out
}

fn s(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}

fn open_vault() -> Vault {
    let mut vault = Vault::open_at(":memory:").unwrap();
    vault.open().unwrap();
    vault
}

// ---- H1: @lid chat without an alias ----

#[test]
fn h1_lid_chat_without_alias_is_listed() {
    let mut store = Store::default();
    store.upsert_chat(LID, Some("Unsaved"), 100, Some(2), None, None);
    assert_eq!(listed(&store), s(&[LID]));
    assert_eq!(store.chats[LID].unread, 2);
}

#[test]
fn h1_lid_history_message_creates_a_listed_chat_and_keeps_the_message() {
    let mut store = Store::default();
    let added = apply_chunk(
        &mut store,
        &chunk(Vec::new(), vec![history_web(LID, "m1", 500)]),
    );
    assert_eq!(added, 1);
    assert_eq!(listed(&store), s(&[LID]));
    assert_eq!(ids(&store, LID), s(&["m1"]));
    assert_eq!(store.chats[LID].timestamp, 500);
}

#[test]
fn h1_lid_name_falls_back_to_something_readable_not_empty() {
    let mut store = Store::default();
    store.upsert_chat(LID, None, 100, None, None, None);
    assert!(!store.chat_name(LID).trim().is_empty());
    store.upsert_contact(LID, None, Some("Push Name"));
    assert_eq!(store.chat_name(LID), "Push Name");
}

#[test]
fn h1_lid_chat_survives_vault_roundtrip_without_alias() {
    let vault = open_vault();
    let mut store = Store::default();
    store.upsert_chat(LID, Some("Unsaved"), 100, None, None, None);
    store.hydrated.insert(LID.into());
    store.add_message(text_message(LID, "m1", 100));
    store.save_checked(&vault).unwrap();
    let mut restarted = Store::default();
    restarted.load_from(&vault);
    assert_eq!(listed(&restarted), s(&[LID]));
    restarted.hydrate(&vault, LID);
    assert_eq!(ids(&restarted, LID), s(&["m1"]));
}

#[test]
fn h1_lid_and_pn_chats_with_no_alias_stay_separate_and_both_listed() {
    let mut store = Store::default();
    store.upsert_chat(LID, None, 200, None, None, None);
    store.upsert_chat(OTHER_PN, None, 100, None, None, None);
    assert_eq!(listed(&store), s(&[LID, OTHER_PN]));
}

// ---- H2: HistChat without a name ----

#[test]
fn h2_histchat_without_name_is_still_listed_for_pn_and_lid() {
    let mut store = Store::default();
    let c = chunk(
        vec![
            hist_chat(PN, None, 300),
            hist_chat(LID, None, 200),
            hist_chat(OTHER_PN, Some(""), 100),
        ],
        Vec::new(),
    );
    apply_chunk(&mut store, &c);
    assert_eq!(listed(&store), s(&[PN, LID, OTHER_PN]));
}

#[test]
fn h2_unnamed_pn_chat_shows_the_number() {
    let mut store = Store::default();
    apply_chunk(
        &mut store,
        &chunk(vec![hist_chat(PN, None, 300)], Vec::new()),
    );
    assert_eq!(store.chat_name(PN), "+5500000000001");
}

#[test]
fn h2_pushname_names_an_unnamed_histchat() {
    let mut store = Store::default();
    let mut c = chunk(vec![hist_chat(PN, None, 300)], Vec::new());
    c.pushnames.push((PN.to_string(), "Pushed".to_string()));
    apply_chunk(&mut store, &c);
    assert_eq!(store.chat_name(PN), "Pushed");
}

#[test]
fn h2_unnamed_histchat_does_not_erase_an_existing_name() {
    let mut store = Store::default();
    store.upsert_chat(PN, Some("Known"), 100, None, None, None);
    apply_chunk(
        &mut store,
        &chunk(
            vec![hist_chat(PN, None, 300), hist_chat(PN, Some(""), 400)],
            Vec::new(),
        ),
    );
    assert_eq!(store.chats[PN].name, "Known");
    assert_eq!(store.chats[PN].timestamp, 400);
}

#[test]
fn h2_unnamed_histchat_with_messages_is_listed_and_keeps_messages() {
    let mut store = Store::default();
    let c = chunk(
        vec![hist_chat(PN, None, 0)],
        vec![history_web(PN, "m1", 700), history_web(PN, "m2", 710)],
    );
    assert_eq!(apply_chunk(&mut store, &c), 2);
    assert_eq!(listed(&store), s(&[PN]));
    assert_eq!(store.chats[PN].timestamp, 710);
    assert_eq!(ids(&store, PN), s(&["m1", "m2"]));
}

#[test]
fn h2_unnamed_histchat_with_zero_timestamp_is_still_listed() {
    let mut store = Store::default();
    apply_chunk(&mut store, &chunk(vec![hist_chat(PN, None, 0)], Vec::new()));
    assert_eq!(listed(&store), s(&[PN]));
}

#[test]
fn h2_histchat_with_non_displayable_jid_is_dropped() {
    let mut store = Store::default();
    apply_chunk(
        &mut store,
        &chunk(
            vec![
                hist_chat("status@broadcast", None, 1),
                hist_chat("123@broadcast", None, 1),
            ],
            Vec::new(),
        ),
    );
    assert!(store.chats.is_empty());
}

#[test]
fn h2_unnamed_histchat_survives_vault_roundtrip() {
    let vault = open_vault();
    let mut store = Store::default();
    apply_chunk(
        &mut store,
        &chunk(
            vec![hist_chat(PN, None, 300)],
            vec![history_web(PN, "m1", 300)],
        ),
    );
    store.save_checked(&vault).unwrap();
    let mut restarted = Store::default();
    restarted.load_from(&vault);
    assert_eq!(listed(&restarted), s(&[PN]));
    restarted.hydrate(&vault, PN);
    assert_eq!(ids(&restarted, PN), s(&["m1"]));
}

// ---- H3: late @lid <-> phone alias ----

fn lid_and_pn_chats() -> Store {
    let mut store = Store::default();
    store.upsert_chat(LID, Some("Lid Name"), 200, Some(2), None, None);
    store.add_message(text_message(LID, "lid1", 150));
    store.add_message(text_message(LID, "lid2", 200));
    store.upsert_chat(PN, None, 100, Some(1), Some(50), None);
    store.add_message(text_message(PN, "pn1", 100));
    store
}

#[test]
fn h3_late_alias_merges_into_one_chat_and_keeps_every_message() {
    let mut store = lid_and_pn_chats();
    assert!(store.learn_alias(LID, PN));
    assert_eq!(listed(&store), s(&[PN]));
    assert!(!store.chats.contains_key(LID));
    assert_eq!(ids(&store, PN), s(&["lid1", "lid2", "pn1"]));
    assert!(store.messages_for(LID).is_empty());
    assert!(store.messages_for(PN).iter().all(|m| m.jid == PN));
}

#[test]
fn h3_late_alias_merges_metadata() {
    let mut store = lid_and_pn_chats();
    store.learn_alias(LID, PN);
    let meta = &store.chats[PN];
    assert_eq!(
        meta.name, "Lid Name",
        "the only name must survive the merge"
    );
    assert_eq!(meta.unread, 3);
    assert_eq!(meta.timestamp, 200);
    assert_eq!(meta.pinned, 50);
    assert_eq!(meta.jid, PN);
}

#[test]
fn h3_late_alias_without_existing_pn_chat_renames_the_chat() {
    let mut store = Store::default();
    store.upsert_chat(LID, Some("Only Lid"), 200, None, None, None);
    store.add_message(text_message(LID, "lid1", 200));
    store.learn_alias(LID, PN);
    assert_eq!(listed(&store), s(&[PN]));
    assert_eq!(store.chats[PN].name, "Only Lid");
    assert_eq!(ids(&store, PN), s(&["lid1"]));
}

#[test]
fn h3_alias_is_idempotent_and_rejects_noops() {
    let mut store = lid_and_pn_chats();
    assert!(store.learn_alias(LID, PN));
    assert!(!store.learn_alias(LID, PN));
    assert!(!store.learn_alias(PN, PN));
    assert_eq!(ids(&store, PN).len(), 3);
    assert_eq!(store.chats[PN].unread, 3);
}

#[test]
fn h3_events_for_the_lid_after_alias_land_in_the_pn_chat() {
    let mut store = lid_and_pn_chats();
    store.learn_alias(LID, PN);
    store.upsert_chat(LID, None, 900, None, None, None);
    let added = apply_chunk(
        &mut store,
        &chunk(Vec::new(), vec![history_web(LID, "late", 900)]),
    );
    assert_eq!(added, 1);
    assert_eq!(listed(&store), s(&[PN]));
    assert!(ids(&store, PN).contains(&"late".to_string()));
    assert_eq!(store.chats[PN].timestamp, 900);
}

#[test]
fn h3_sorted_chats_orders_pin_then_recency_after_merge() {
    let mut store = lid_and_pn_chats();
    store.upsert_chat(OTHER_PN, Some("Other"), 9_000, None, None, None);
    store.learn_alias(LID, PN);
    assert_eq!(
        listed(&store),
        s(&[PN, OTHER_PN]),
        "pinned merged chat leads"
    );
    store.chats.get_mut(PN).unwrap().pinned = 0;
    assert_eq!(listed(&store), s(&[OTHER_PN, PN]));
}

#[test]
fn h3_duplicate_message_ids_are_not_doubled_by_the_merge() {
    let mut store = Store::default();
    store.add_message(text_message(LID, "same", 100));
    store.add_message(text_message(PN, "same", 100));
    store.learn_alias(LID, PN);
    assert_eq!(ids(&store, PN), s(&["same"]));
}

#[test]
fn h3_draft_follows_the_merged_chat() {
    let mut store = lid_and_pn_chats();
    store.drafts.insert(
        LID.into(),
        crate::store::Draft {
            text: "typed".into(),
            mentions: Vec::new(),
            promoted: 5,
        },
    );
    store.learn_alias(LID, PN);
    assert_eq!(store.drafts.get(PN).map(|d| d.text.as_str()), Some("typed"));
    assert!(!store.drafts.contains_key(LID));
}

#[test]
fn h3_persisted_lid_chat_then_late_alias_is_merged_after_restart() {
    let vault = open_vault();
    let mut store = Store::default();
    store.hydrated.insert(LID.into());
    store.hydrated.insert(PN.into());
    store.upsert_chat(LID, Some("Lid Name"), 200, None, None, None);
    store.add_message(text_message(LID, "lid1", 200));
    store.upsert_chat(PN, None, 100, None, None, None);
    store.add_message(text_message(PN, "pn1", 100));
    store.save_checked(&vault).unwrap();

    store.learn_alias(LID, PN);
    store.save_checked(&vault).unwrap();

    let mut restarted = Store::default();
    restarted.load_from(&vault);
    assert_eq!(
        listed(&restarted),
        s(&[PN]),
        "no ghost @lid row after restart"
    );
    assert_eq!(restarted.canon(LID), PN);
    restarted.hydrate(&vault, PN);
    assert_eq!(ids(&restarted, PN), s(&["lid1", "pn1"]));
    restarted.hydrate(&vault, LID);
    assert!(
        restarted.messages_for(LID).is_empty(),
        "vault must not keep history under the old @lid"
    );
    assert_eq!(restarted.chats[PN].name, "Lid Name");
}

#[test]
fn h3_alias_before_first_save_persists_only_the_pn_chat() {
    let vault = open_vault();
    let mut store = lid_and_pn_chats();
    store.learn_alias(LID, PN);
    store.save_checked(&vault).unwrap();
    let mut restarted = Store::default();
    restarted.load_from(&vault);
    assert_eq!(listed(&restarted), s(&[PN]));
    restarted.hydrate(&vault, PN);
    assert_eq!(ids(&restarted, PN).len(), 3);
}
