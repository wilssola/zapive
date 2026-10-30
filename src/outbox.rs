use serde::{Deserialize, Serialize};
use std::sync::Arc;
use whatsapp_rust::waproto::whatsapp as wa;

#[derive(Clone, Serialize, Deserialize)]
pub struct Preparation {
    pub id: String,
    pub jid: String,
    pub kind: String,
    pub filename: String,
    pub caption: Option<String>,
    pub origin: Option<String>,
    #[serde(with = "source_bytes")]
    pub data: Vec<u8>,
    pub in_rate: u32,
    pub view_once: bool,
}

pub fn new_id() -> String {
    use rand::RngCore as _;
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    format!("3EB0{}", bytes.iter().map(|b| format!("{b:02X}")).collect::<String>())
}

mod source_bytes {
    use super::*;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as B64;

    pub fn serialize<S: serde::Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&B64.encode(bytes))
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        B64.decode(encoded).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    Queued,
    Sending,
    Uncertain,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub jid: String,
    pub created: i64,
    pub state: State,
    #[serde(with = "raw_proto")]
    pub message: Arc<wa::Message>,
    pub error: String,
}

impl Entry {
    pub fn new(id: String, jid: String, message: Arc<wa::Message>, created: i64) -> Self {
        Self {
            id,
            jid,
            created,
            state: State::Queued,
            message,
            error: String::new(),
        }
    }

    pub fn recover_interrupted(&mut self) -> bool {
        if self.state != State::Sending {
            return false;
        }
        self.state = State::Uncertain;
        true
    }
}

mod raw_proto {
    use super::*;
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as B64;
    use whatsapp_rust::waproto::buffa::Message as _;

    pub fn serialize<S: serde::Serializer>(
        message: &Arc<wa::Message>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&B64.encode(message.encode_to_vec()))
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Arc<wa::Message>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        let bytes = B64.decode(encoded).map_err(serde::de::Error::custom)?;
        wa::Message::decode(&mut bytes.as_slice())
            .map(Arc::new)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use whatsapp_rust::waproto::buffa::{Message as _, MessageField};

    #[test]
    fn preparation_roundtrip_source_bytes() {
        for kind in ["image", "document", "audio", "sticker", "gif", "voice"] {
            let data: Vec<u8> = [0.0f32, -0.5, 1.0].iter()
                .flat_map(|sample| sample.to_le_bytes()).collect();
            let preparation = Preparation {
                id: new_id(),
                jid: "123@s.whatsapp.net".into(),
                kind: kind.into(),
                filename: "source.bin".into(),
                caption: Some("caption".into()),
                origin: Some("selected/source.bin".into()),
                data,
                in_rate: 48000,
                view_once: true,
            };
            let value = serde_json::to_value(&preparation).unwrap();
            assert!(value["data"].is_string());
            let restored: Preparation = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(restored.id, preparation.id);
            assert_eq!(restored.jid, preparation.jid);
            assert_eq!(restored.kind, preparation.kind);
            assert_eq!(restored.filename, preparation.filename);
            assert_eq!(restored.caption, preparation.caption);
            assert_eq!(restored.origin, preparation.origin);
            assert_eq!(restored.data, preparation.data);
            assert_eq!(restored.in_rate, preparation.in_rate);
            assert!(restored.view_once);
            let mut invalid = value;
            invalid["data"] = "not base64!".into();
            assert!(serde_json::from_value::<Preparation>(invalid).is_err());
        }
    }

    #[test]
    fn generated_ids_are_safe_and_distinct() {
        let id = new_id();
        assert!(id.starts_with("3EB0"));
        assert_eq!(id.len(), 36);
        assert!(id.bytes().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(id, new_id());
    }

    #[test]
    fn serde_roundtrip_text_and_media() {
        let text = crate::wa::build_text("123@s.whatsapp.net", "hello".into(), &None, vec![], None);
        let media = wa::Message {
            image_message: MessageField::some(wa::message::ImageMessage {
                caption: Some("photo".into()),
                media_key: Some(vec![1, 2, 3].into()),
                direct_path: Some("/media/photo".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        for message in [text, media] {
            let entry = Entry::new(
                "stable-id".into(),
                "123@s.whatsapp.net".into(),
                Arc::new(message),
                42,
            );
            let json = serde_json::to_string(&entry).unwrap();
            let restored: Entry = serde_json::from_str(&json).unwrap();
            assert_eq!(restored.id, entry.id);
            assert_eq!(restored.jid, entry.jid);
            assert_eq!(restored.created, entry.created);
            assert_eq!(restored.state, State::Queued);
            assert_eq!(restored.error, entry.error);
            assert_eq!(
                restored.message.encode_to_vec(),
                entry.message.encode_to_vec()
            );
        }
    }

    #[test]
    fn interrupted_send_becomes_uncertain() {
        for state in [
            State::Queued,
            State::Sending,
            State::Uncertain,
            State::Failed,
        ] {
            let mut entry = Entry::new(
                "id".into(),
                "jid".into(),
                Arc::new(wa::Message::default()),
                0,
            );
            entry.state = state;
            let json = serde_json::to_string(&entry).unwrap();
            let mut restored: Entry = serde_json::from_str(&json).unwrap();
            assert_eq!(restored.recover_interrupted(), state == State::Sending);
            assert_eq!(
                restored.state,
                if state == State::Sending {
                    State::Uncertain
                } else {
                    state
                }
            );
            assert!(!restored.recover_interrupted());
        }
    }

    #[test]
    fn rejects_invalid_payload() {
        let entry = Entry::new(
            "id".into(),
            "jid".into(),
            Arc::new(wa::Message::default()),
            0,
        );
        let mut value = serde_json::to_value(entry).unwrap();
        for invalid in ["not base64!", "/w=="] {
            value["message"] = invalid.into();
            assert!(serde_json::from_value::<Entry>(value.clone()).is_err());
        }
    }
}
