# Atlas DAM — monorepo (M0 fondations)

DAM IA-first, **souverain**, **open-source**, **sans dépendance externe** (hors LLM optionnel),
**Rust** au socle, front **WASM**. Ce dépôt est le **scaffolding M0** (chemin critique du doc 24).

## Méthode : TDD (obligatoire)
Toute implémentation suit le cycle **rouge → vert → refactor** :
1. Écrire d'abord les tests qui décrivent le contrat attendu (ils échouent).
2. Écrire le minimum de code pour les faire passer.
3. Refactorer en gardant les tests verts.

Conventions appliquées dans ce dépôt :
- Extraire la **logique pure** (testable sans I/O) et la couvrir en tests unitaires :
  `rrf::fuse`, `understanding::interpret`, `vector::pgvector_literal`, `FakeEmbedder`.
- Les chemins avec I/O (PostgreSQL) sont testés en **intégration `#[ignore]`** (exécutés
  avec une base de test) ; le contrat est d'abord validé par les fonctions pures + stubs.
- La CI (`cargo test --all`) doit rester verte ; toute régression bloque.
- Tests de sécurité de premier ordre : **aucune fuite de permissions** (RLS), idempotence,
  air-gap — à écrire avec chaque feature (doc 24 DoD).

## Invariants (non négociables)
- Rust socle (back) + WASM (front) ; 100 % open-source (liste blanche de licences, `deny.toml`).
- Zéro dépendance externe runtime ; **IA locale par défaut** (`ATLAS_ALLOW_EXTERNAL_LLM` off).
- API-first : le contrat OpenAPI (`openapi/atlas.v1.yaml`) est la source de vérité, servi localement (`/openapi.json`).
- Temps réel des interfaces via WebSocket (doc 40) — branché aux jalons suivants.

## Structure
```
Cargo.toml                 workspace
rust-toolchain.toml        toolchain épinglée
deny.toml                  gouvernance licences (cargo-deny)
openapi/atlas.v1.yaml      contrat d'API (source de vérité)
migrations/0001_init.sql   tenant, asset, embedding (pgvector), search_text (FTS), audit + RLS
crates/
  atlas-types/             DTO partagés back↔front (Serde)
  atlas-config/            configuration souveraine (env)
  atlas-embed/             embeddings : trait Embedder + FakeEmbedder ; seam SigLIP/ort (feature `ml`)
  atlas-search/            recherche hybride : traits d'index, fusion RRF, query understanding, /v1/search
  atlas-ingest/            ingestion : hash (SHA-256), pHash/Hamming, machine d'états, prepare
  atlas-store/             stockage objet : ObjectStore + FsObjectStore (SeaweedFS S3 derrière le trait)
  atlas-render/            renditions/crop : géométrie pure (fit, crop par focal) + ImageProcessor (libvips)
  atlas-bus/               bus de messages : Bus + InMemoryBus + mapping de sujets (NATS derrière le trait)
  atlas-realtime/          Realtime Gateway WebSocket /v1/ws : protocole, abonnements, Hub, auth + PDP
  atlas-db/                PostgreSQL (pool sqlx), RLS par tenant, FTS + kNN pgvector, ping readiness
  atlas-core/              service Core API (Axum) : /healthz /readyz (ping DB) /version /openapi.json + /v1
docker-compose.yml         édition Solo (Postgres+pgvector, NATS, SeaweedFS, atlas-core)
scripts/airgap-test.sh     vérifie le démarrage sans réseau externe
.github/workflows/ci.yml   fmt + clippy + tests + licences + build offline + air-gap
```

## Démarrer (dev)
```bash
# 1) Tests unitaires (rapides, sans infra)
cargo test --all

# 2) Lancer le service seul
cargo run -p atlas-core
curl -s localhost:8080/healthz
curl -s localhost:8080/openapi.json | head
# ingestion (nécessite PostgreSQL) → l'asset devient immédiatement cherchable
curl -s -X POST localhost:8080/v1/assets \
  -H 'content-type: application/json' \
  -d '{"title":"Plage au coucher de soleil","text":"mer sable paysage"}'
# ingestion d'un contenu généré par IA → étiquetage de transparence (AI Act art. 50)
curl -s -X POST localhost:8080/v1/assets \
  -H 'content-type: application/json' \
  -d '{"title":"Affiche","text":"montagne","provenance":"ai_generated","generator":"Firefly"}'
# → la réponse porte "transparency_label":"Contenu généré par IA" et la facette ai_provenance
# upload d'un FICHIER BINAIRE réel (AT-004, geste DAM n°1) → content_sha256 sur les octets réels
curl -s -X POST localhost:8080/v1/assets \
  -F 'file=@photo.jpg;type=image/jpeg' \
  -F 'title=Plage' -F 'text=mer sable' -F 'provenance=human'
# recherche (in-memory si pas de DB ; FTS+kNN réels si DB)
curl -s -X POST localhost:8080/v1/search \
  -H 'content-type: application/json' \
  -d '{"query":"plage paysage sans personne","page_size":10}'

# 3) Beta web — binaire unique servant l'UI (front WASM) + l'API
cd crates/atlas-web && rustup run stable trunk build --release && cd ../..   # -> dist/
ATLAS_WEB_DIR="$PWD/crates/atlas-web/dist" cargo run -p atlas-core --release
# ouvrir http://localhost:8080  → l'UI Atlas est servie par atlas-core (repli SPA inclus)

# 4) Édition Solo complète (infra incluse)
docker compose up --build
```

> **Déploiement / lancement de la beta web** (Docker, binaire, variables d'env, état
> vérifié) : voir **[`DEPLOY.md`](DEPLOY.md)**. Le Core sert le front statique dès que
> `ATLAS_WEB_DIR` pointe sur un `dist/` trunk (sinon API seule — comportement historique).

## Tests d'intégration (base réelle)
Les tests touchant PostgreSQL sont `#[ignore]` et pilotés par une variable d'env.
Ils prouvent la chaîne **insert asset → search_text/embedding → FTS + kNN**, et surtout
l'**isolation RLS inter-tenant** (aucune fuite). La RLS est en `FORCE` (s'applique même au
propriétaire), donc l'isolation est réellement vérifiée.

```bash
# Démarrer la base seule (migrations appliquées au 1er boot)
docker compose up -d postgres
# Lancer uniquement les tests d'intégration
ATLAS_TEST_DATABASE_URL="postgres://atlas:atlas@localhost:5432/atlas" \
  cargo test -p atlas-db -- --ignored
```

## Vérifications de conformité
```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo deny check          # licences/bans/sources
./scripts/airgap-test.sh  # après cargo build --release
```

## Transparence IA & provenance (AI Act art. 50 / C2PA)
À l'ingestion, Atlas enregistre la **provenance** de chaque asset pour répondre au règlement
européen **AI Act, article 50** (marquage/étiquetage des contenus générés ou manipulés par IA,
applicable au **2 août 2026** ; *Code de bonnes pratiques* finalisé le 10 juin 2026 citant les
**Content Credentials C2PA** comme mécanisme de référence) :
- `ai_provenance` : `human` · `ai_generated` · `ai_edited` · `unknown`.
- `c2pa_present` : un manifeste C2PA signé est présent dans le binaire.
- `generator` : outil/modèle générateur déclaré (ex. « Firefly »).

La détection est **pure et hermétique** (`atlas-ingest::provenance`, testée sans I/O) : elle
inspecte les octets pour repérer la boîte **JUMBF/C2PA** et les marqueurs **IPTC
`digitalSourceType`** (`trainedAlgorithmicMedia` → généré ; `compositeWith…` → modifié). Une
**déclaration** explicite de l'éditeur (champ `provenance` de l'upload) **prime** sur la
détection. La provenance est exposée dans la réponse d'ingestion (avec un
`transparency_label` prêt à afficher), poussée en temps réel (événement `asset.ingested`),
persistée (migration `0006`) et offerte en **facette de recherche** `ai_provenance` (pour
filtrer/auditer les contenus IA). Le parsing cryptographique complet d'un manifeste C2PA signé
(crate `c2pa`, dépendances natives) viendra à un jalon ultérieur, sans changer ce contrat.

## Prochaines étapes (doc 24, M1 — MVP Solo)
1. ✅ Recherche hybride — squelette (`atlas-search`, doc 25) : `/v1/search`, fusion RRF, query understanding.
2. ✅ PostgreSQL (`atlas-db`) : pool sqlx, RLS par tenant (transaction + `set_config('atlas.tenant')`), `PgLexicalIndex` (FTS), readiness DB. Le service bascule sur Pg si joignable, sinon in-memory.
3. ✅ Embedding de requête (`atlas-embed`, trait `Embedder` + `FakeEmbedder` testé) + `PgVectorIndex` kNN pgvector (`vec <=> $qvec`, filtres + RLS, `pgvector_literal` testé). SigLIP réel = remplacement de `FakeEmbedder` sans toucher l'aval.
4. ✅ Ingestion `POST /v1/assets` (`atlas-ingest::prepare` pur + persistance repo) : hash → `search_text` → `embedding`, asset immédiatement cherchable. **Upload `multipart/form-data` de fichier binaire réel** (AT-004) : `content_sha256` + détection C2PA sur les **octets réels** ; JSON M1 conservé en compat. Reste : pHash perceptuel réel sur image décodée (AT-004b), stockage objet (AT-005), renditions, workers NATS.
5. ✅ Realtime Gateway (`atlas-realtime`, doc 40) : WebSocket `/v1/ws` (protocole subscribe/event/ping, Hub broadcast, reprise par `seq`, heartbeat). `POST /v1/assets` publie `asset.ingested` → l'UI abonnée se met à jour **sans rafraîchissement**. Reste : auth à l'upgrade + pont NATS multi-nœuds.
6. Seams posés (TDD, logique pure testée) : **stockage objet** (`atlas-store`, FS réel + SeaweedFS à brancher), **renditions/crop** (`atlas-render`, géométrie testée + libvips à brancher), **bus/workers** (`atlas-bus`, InMemoryBus + NATS à brancher), **auth/PDP WebSocket** (`atlas-realtime::auth`), **SigLIP** (`atlas-embed` feature `ml`). Reste à brancher les libs natives (libvips/FFmpeg, ort+modèle, client NATS) dans un environnement avec toolchain + miroirs.

> Note build : `sqlx` est configuré **sans macros** (pas de DB requise à la compilation) et **sans TLS** (Postgres local/interne) → build hermétique, licences propres.

Référence complète : voir la suite documentaire (docs 00–40).
