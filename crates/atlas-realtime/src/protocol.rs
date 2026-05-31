//! Protocole WebSocket (doc 22 §3.8 / doc 40) — messages JSON typés.
//! TDD : les tests figent le format du fil (compatibilité front WASM).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Messages client → serveur.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum ClientMsg {
    Subscribe { channels: Vec<String> },
    Unsubscribe { channels: Vec<String> },
    Ping,
}

/// Messages serveur → client.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum ServerMsg {
    Event {
        channel: String,
        #[serde(rename = "type")]
        kind: String,
        data: Value,
        seq: u64,
    },
    Ack {
        channel: String,
    },
    Denied {
        channel: String,
        reason: String,
    },
    Pong,
    Resync {
        channel: String,
    },
}

impl ClientMsg {
    /// Parse un message client depuis du JSON (tolérant : renvoie None si invalide).
    pub fn parse(s: &str) -> Option<ClientMsg> {
        serde_json::from_str(s).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_subscribe() {
        let m = ClientMsg::parse(r#"{"op":"subscribe","channels":["asset:1","ingest"]}"#).unwrap();
        assert_eq!(
            m,
            ClientMsg::Subscribe {
                channels: vec!["asset:1".into(), "ingest".into()]
            }
        );
    }

    #[test]
    fn parse_ping_unit_variant() {
        assert_eq!(ClientMsg::parse(r#"{"op":"ping"}"#).unwrap(), ClientMsg::Ping);
    }

    #[test]
    fn parse_invalid_is_none() {
        assert!(ClientMsg::parse("not json").is_none());
        assert!(ClientMsg::parse(r#"{"op":"unknown"}"#).is_none());
    }

    #[test]
    fn serialize_event_shape() {
        let ev = ServerMsg::Event {
            channel: "asset:1".into(),
            kind: "asset.ready".into(),
            data: json!({"id": "1"}),
            seq: 7,
        };
        let v: Value = serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
        assert_eq!(v["op"], "event");
        assert_eq!(v["type"], "asset.ready"); // renommé depuis `kind`
        assert_eq!(v["seq"], 7);
    }

    #[test]
    fn serialize_pong() {
        let v: Value = serde_json::from_str(&serde_json::to_string(&ServerMsg::Pong).unwrap()).unwrap();
        assert_eq!(v["op"], "pong");
    }
}
