# Contribuer à Atlas DAM

Merci de contribuer. Ce guide décrit la méthode (TDD), les conventions non négociables,
et **comment brancher chaque seam** (les interfaces prêtes à recevoir leur implémentation réelle).

Référence produit complète : suite documentaire `docs 00–40` (hors dépôt). Architecture : doc 02/23.

---

## 1. Prérequis

- **Rust** 1.78 (épinglé par `rust-toolchain.toml`) avec `rustfmt` et `clippy`.
- **Docker** (édition Solo : Postgres+pgvector, NATS, SeaweedFS).
- `cargo-deny` pour l'audit de licences : `cargo install cargo-deny`.

```bash
rustup show               # installe la toolchain épinglée
cargo build               # compile le workspace
cargo test --all          # exécute tous les tests unitaires
```

---

## 2. Méthode : TDD (obligatoire)

Cycle **rouge → vert → refactor** pour **toute** implémentation :

1. **Rouge** — écrire d'abord les tests qui décrivent le contrat attendu (ils échouent).
2. **Vert** — écrire le minimum de code pour les faire passer.
3. **Refactor** — nettoyer en gardant les tests verts.

Règles appliquées dans ce dépôt :

- **Extraire la logique pure** (sans I/O) et la couvrir en tests unitaires. Exemples existants :
  `rrf::fuse`, `understanding::interpret`, `hash::sha256_hex`/`average_hash`, `state::should_run`,
  `prepare::prepare`, `vector::pgvector_literal`, `store::storage_key`, `render::crop_box_for_ratio`,
  `bus::subject_for`, `auth::Channel::parse` / `DefaultPdp`, `protocol` (sérialisation).
- **Les chemins avec I/O** (PostgreSQL, FS, réseau) → tests d'**intégration `#[ignore]`** pilotés par
  variable d'environnement, plus un **stub/fake** pour les tests rapides
  (`InMemoryIndex`, `FakeEmbedder`, `InMemoryBus`, `NoopProcessor`, `FsObjectStore`).
- **Tests de sécurité de premier ordre** à écrire avec la feature : non-fuite de permissions (RLS),
  idempotence, air-gap. (Écrire ces tests a déjà révélé 2 bugs RLS — voir doc 27/`repo.rs`.)
- `cargo test --all` doit rester **vert** ; toute régression bloque la PR.

---

## 3. Invariants non négociables

Toute contribution doit les respecter (vérifiés en CI) :

- **Rust** au socle, **front WASM** ; **100 % open-source** (licences en liste blanche, `deny.toml`).
- **Zéro dépendance externe runtime** (hors API LLM, opt-in). **IA locale par défaut.**
- **API-first** : le contrat `openapi/atlas.v1.yaml` est la source de vérité ; UI/SDK/CLI/MCP en dérivent.
- **Multi-tenant par RLS** (`set_config('atlas.tenant', …, true)` + `FORCE ROW LEVEL SECURITY`).
- **Temps réel des interfaces via WebSocket** (`/v1/ws`), abonnements scopés par permissions.
- **Air-gap** : tout doit fonctionner sans réseau externe (test CI dédié).

---

## 4. Avant d'ouvrir une PR

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all
cargo deny check                 # licences / bans / sources
./scripts/airgap-test.sh         # après cargo build --release
```

Tests d'intégration base (optionnels en local, requis en CI) :

```bash
docker compose up -d postgres
ATLAS_TEST_DATABASE_URL="postgres://atlas:atlas@localhost:5432/atlas" \
  cargo test -p atlas-db -- --ignored
```

Checklist PR (extrait de la Definition of Done, doc 24) :
- [ ] Tests d'abord ; couverture de la logique pure.
- [ ] Permissions (RLS/PDP) testées — aucune fuite.
- [ ] Contrat OpenAPI à jour si l'API change.
- [ ] Fonctionne en air-gap ; licences en liste blanche.
- [ ] Documentation (commentaires de module + README/feature) à jour.

---

## 5. Structure des crates

| Crate | Rôle | Doc |
|-------|------|-----|
| `atlas-types` | DTO partagés back↔front | 22 |
| `atlas-config` | configuration souveraine (env) | 02 |
| `atlas-embed` | embeddings (`Embedder` + `FakeEmbedder` ; seam SigLIP) | 25 |
| `atlas-search` | recherche hybride (RRF, understanding, `/v1/search`) | 04/25 |
| `atlas-ingest` | hash, pHash, machine d'états, `prepare` | 26 |
| `atlas-store` | stockage objet (`ObjectStore` + `FsObjectStore`) | 02/23 |
| `atlas-render` | renditions/crop (géométrie pure + `ImageProcessor`) | 06/34 |
| `atlas-bus` | bus de messages (`Bus` + `InMemoryBus`, sujets) | 26 |
| `atlas-realtime` | WebSocket `/v1/ws` (protocole, abonnements, auth/PDP) | 40 |
| `atlas-db` | PostgreSQL (pool, RLS, FTS, kNN pgvector) | 25/27 |
| `atlas-core` | service Axum, montage `/v1`, endpoints | 22 |

---

## 6. Brancher un seam (procédure type)

Chaque intégration native est isolée derrière un **trait** avec un stub/fake testé. Pour la rendre réelle :

1. **Garder le trait et ses tests inchangés** (ils définissent le contrat).
2. Ajouter la dépendance (mirrorée en interne, licence en liste blanche).
3. Implémenter le trait dans une struct dédiée.
4. **Écrire d'abord un test d'intégration `#[ignore]`** qui prouve le comportement réel.
5. Sélectionner l'implémentation par configuration (env/feature), le **fake restant le défaut de test**.

### 6.1 Stockage objet → SeaweedFS (S3)
- Trait : `atlas_store::ObjectStore`. Défaut : `FsObjectStore` (réel, Solo).
- À faire : `S3ObjectStore` (crate S3 open-source), même clé `storage_key`.
- Test d'intégration `#[ignore]` : put/get/delete contre un SeaweedFS de test (`ATLAS_S3_*`).

### 6.2 Renditions/crop → libvips / FFmpeg
- Trait : `atlas_render::ImageProcessor`. Défaut tests : `NoopProcessor`.
- La **géométrie** (`fit_within`, `crop_box_for_ratio`) reste pure et testée — ne pas la dupliquer.
- À faire : `VipsProcessor` (bindings libvips) pour `thumbnail`/resize ; FFmpeg pour A/V.
- Test `#[ignore]` : encoder une image de fixture, vérifier dimensions de sortie = `fit_within(...)`.

### 6.3 Bus/workers → NATS JetStream
- Trait : `atlas_bus::Bus`. Défaut tests : `InMemoryBus`.
- À faire : `NatsBus` (client NATS) avec streams durables ; le worker consomme `subject_for(step)`
  et exécute l'étape via `state::should_run` (idempotence).
- Test `#[ignore]` : publier→consommer contre un NATS de test ; rejeu = exactly-once effectif.

### 6.4 Embeddings → SigLIP (ONNX Runtime)
- Trait : `atlas_embed::Embedder`. Défaut : `FakeEmbedder`. Seam : `SiglipEmbedder` (feature `ml`).
- À faire : ajouter `ort` (ONNX Runtime, MIT) + chargement d'un **modèle local mirroré**
  (jamais de pull runtime) ; `EMBED_DIM` doit rester aligné avec `embedding.vec vector(1152)`.
- Sélection : `FakeEmbedder` par défaut ; `SiglipEmbedder` activé en prod (config + feature `ml`).
- Test : golden set de similarité (nDCG@10 ≥ 0,85, doc 25) en non-régression.

### 6.5 Auth/PDP WebSocket → OIDC + droits
- Traits : `atlas_realtime::auth::{Authenticator, Pdp}`. Défauts : `DevAuthenticator`, `DefaultPdp`.
- À faire : `OidcAuthenticator` (vérif. JWT via IdP client) ; PDP fin reliant `Channel::Asset(id)`
  au tenant/droits (doc 27/38). Injecter via `Hub` (champs `authenticator`/`pdp`).
- Test : un abonnement à un asset hors tenant est `Denied` ; jeton invalide → 401 avant upgrade.

---

## 7. Style de code

- `rustfmt` (défaut) + `clippy` sans warning.
- Modules documentés (`//!`) avec le « pourquoi » et la référence doc.
- Pas de `unwrap()` sur des chemins faillibles en production (ok en tests).
- Erreurs typées (`thiserror`) ; réponses API en Problem Details (RFC 9457).
- Commentaires et identifiants en français pour le domaine, anglais pour les termes techniques établis.

---

## 8. Commits & branches

- Trunk-based ; branches courtes ; PR petites et revues.
- Messages de commit impératifs et descriptifs (ex. « ajoute le PDP d'abonnement WebSocket »).
- Feature flags pour activer progressivement (pas de big-bang).

---

*Bon code — et toujours : un test d'abord.*
