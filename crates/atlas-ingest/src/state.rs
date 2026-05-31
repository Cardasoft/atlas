//! Machine d'états du pipeline d'ingestion (doc 26 §4) — pur, testé d'abord (TDD).
//! Les étapes sont ordonnées, idempotentes et rejouables ; ce module encode l'ordre
//! et les transitions légales, indépendamment de l'exécution (workers).

/// Étapes du pipeline (doc 26 §4.2), dans l'ordre.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    Received,
    Hashed,
    Dedup,
    Extracted,
    Renditions,
    Analyzed,
    Indexed,
    Finalized,
}

/// Ordre canonique des étapes.
pub const ORDER: [Step; 8] = [
    Step::Received,
    Step::Hashed,
    Step::Dedup,
    Step::Extracted,
    Step::Renditions,
    Step::Analyzed,
    Step::Indexed,
    Step::Finalized,
];

impl Step {
    /// Étape suivante, ou `None` si l'ingestion est terminée.
    pub fn next(self) -> Option<Step> {
        let i = ORDER.iter().position(|&s| s == self).unwrap();
        ORDER.get(i + 1).copied()
    }

    /// Vrai si `self` précède strictement `other` dans le pipeline.
    pub fn is_before(self, other: Step) -> bool {
        self < other
    }
}

/// Décision d'exécution d'une étape selon l'avancement déjà persisté (idempotence, doc 26 §4.3).
/// `last_done` = dernière étape terminée (None si rien n'a encore été fait).
pub fn should_run(step: Step, last_done: Option<Step>) -> bool {
    match last_done {
        None => step == Step::Received,
        Some(done) => done.is_before(step) && step.next_of(done),
    }
}

impl Step {
    /// Vrai si `self` est l'étape immédiatement après `prev` (exécution séquentielle).
    fn next_of(self, prev: Step) -> bool {
        prev.next() == Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_is_monotonic() {
        for w in ORDER.windows(2) {
            assert!(w[0].is_before(w[1]));
        }
    }

    #[test]
    fn next_walks_the_pipeline() {
        assert_eq!(Step::Received.next(), Some(Step::Hashed));
        assert_eq!(Step::Indexed.next(), Some(Step::Finalized));
        assert_eq!(Step::Finalized.next(), None);
    }

    #[test]
    fn first_step_runs_only_from_scratch() {
        assert!(should_run(Step::Received, None));
        assert!(!should_run(Step::Hashed, None));
    }

    #[test]
    fn only_the_immediate_next_step_runs() {
        // Après Hashed, seule Dedup doit s'exécuter (pas de saut, pas de retour).
        assert!(should_run(Step::Dedup, Some(Step::Hashed)));
        assert!(!should_run(Step::Extracted, Some(Step::Hashed))); // saut interdit
        assert!(!should_run(Step::Hashed, Some(Step::Hashed))); // déjà fait → idempotent
        assert!(!should_run(Step::Received, Some(Step::Hashed))); // retour interdit
    }
}
