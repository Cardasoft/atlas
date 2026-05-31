//! Registre d'abonnements d'une connexion (doc 40 §5) — logique pure, testée d'abord.
//! Suit l'ensemble des canaux auxquels une session est abonnée et décide si un
//! événement (par son canal) doit lui être poussé.

use std::collections::HashSet;

#[derive(Debug, Default)]
pub struct Subscriptions {
    channels: HashSet<String>,
}

impl Subscriptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn subscribe(&mut self, channel: &str) {
        self.channels.insert(channel.to_string());
    }

    pub fn unsubscribe(&mut self, channel: &str) {
        self.channels.remove(channel);
    }

    pub fn is_subscribed(&self, channel: &str) -> bool {
        self.channels.contains(channel)
    }

    /// Décide si un événement (par son canal) doit être délivré à cette session.
    pub fn should_deliver(&self, event_channel: &str) -> bool {
        self.is_subscribed(event_channel)
    }

    pub fn len(&self) -> usize {
        self.channels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_then_deliver() {
        let mut s = Subscriptions::new();
        s.subscribe("asset:1");
        assert!(s.should_deliver("asset:1"));
        assert!(!s.should_deliver("asset:2")); // pas abonné → pas délivré
    }

    #[test]
    fn unsubscribe_stops_delivery() {
        let mut s = Subscriptions::new();
        s.subscribe("ingest");
        s.unsubscribe("ingest");
        assert!(!s.should_deliver("ingest"));
        assert!(s.is_empty());
    }

    #[test]
    fn idempotent_subscribe() {
        let mut s = Subscriptions::new();
        s.subscribe("x");
        s.subscribe("x");
        assert_eq!(s.len(), 1);
    }
}
