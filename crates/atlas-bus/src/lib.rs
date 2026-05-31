//! atlas-bus — bus de messages (doc 02/26).
//!
//! TDD : le **mapping des sujets** (quelle étape publie sur quel sujet) et l'`InMemoryBus`
//! sont testés. Le `NatsBus` (JetStream durable) s'implémentera derrière le trait `Bus`,
//! sans changer les producteurs/consommateurs.

use atlas_ingest::state::Step;
use std::collections::HashMap;
use std::sync::Mutex;

/// Sujet NATS associé à la prochaine étape (doc 26 §3).
pub fn subject_for(step: Step) -> &'static str {
    match step {
        Step::Received => "ingest.received",
        Step::Hashed | Step::Dedup | Step::Extracted | Step::Renditions => "ingest.process",
        Step::Analyzed => "ingest.analyze",
        Step::Indexed => "ingest.index",
        Step::Finalized => "asset.events",
    }
}

/// Contrat d'un bus durable (publish/consume).
pub trait Bus: Send + Sync {
    fn publish(&self, subject: &str, payload: Vec<u8>);
    /// Retire et renvoie les messages en attente d'un sujet (modèle « drain » pour tests).
    fn drain(&self, subject: &str) -> Vec<Vec<u8>>;
}

/// Bus en mémoire (tests/dev) : file FIFO par sujet.
#[derive(Default)]
pub struct InMemoryBus {
    queues: Mutex<HashMap<String, Vec<Vec<u8>>>>,
}

impl InMemoryBus {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Bus for InMemoryBus {
    fn publish(&self, subject: &str, payload: Vec<u8>) {
        self.queues
            .lock()
            .unwrap()
            .entry(subject.to_string())
            .or_default()
            .push(payload);
    }
    fn drain(&self, subject: &str) -> Vec<Vec<u8>> {
        self.queues
            .lock()
            .unwrap()
            .get_mut(subject)
            .map(std::mem::take)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subjects_map_steps() {
        assert_eq!(subject_for(Step::Received), "ingest.received");
        assert_eq!(subject_for(Step::Analyzed), "ingest.analyze");
        assert_eq!(subject_for(Step::Indexed), "ingest.index");
        assert_eq!(subject_for(Step::Finalized), "asset.events");
    }

    #[test]
    fn publish_then_drain_fifo() {
        let bus = InMemoryBus::new();
        bus.publish("ingest.received", b"a".to_vec());
        bus.publish("ingest.received", b"b".to_vec());
        let msgs = bus.drain("ingest.received");
        assert_eq!(msgs, vec![b"a".to_vec(), b"b".to_vec()]);
        // Drainé → vide ensuite (at-least-once consommé une fois).
        assert!(bus.drain("ingest.received").is_empty());
    }

    #[test]
    fn drain_unknown_subject_is_empty() {
        let bus = InMemoryBus::new();
        assert!(bus.drain("nope").is_empty());
    }
}
