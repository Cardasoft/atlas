//! atlas-ingest — pipeline d'ingestion (doc 26).
//!
//! M1 (TDD) : ce crate démarre par sa **logique pure**, entièrement testée —
//! empreintes de contenu/perceptuelles (`hash`) et machine d'états (`state`).
//! Les workers asynchrones (réception, extraction, renditions, analyse IA) et la
//! persistance NATS/Postgres viendront ensuite, en réutilisant ces briques validées.

pub mod hash;
pub mod prepare;
pub mod state;
