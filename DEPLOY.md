# DEPLOY — Atlas DAM (beta web)

Ce document explique comment **lancer la beta web Atlas en local** et **la déployer**
(Docker ou binaire). La « beta web » = le service `atlas-core` (API Axum) qui **sert aussi
le front WASM Leptos** bâti par trunk → un **binaire unique** rend l'UI **et** l'API.

> Souveraineté/frugalité : aucune dépendance externe runtime (hors LLM opt-in), pas de TLS
> côté Postgres, IA locale par défaut, build hermétique.

---

## 1. Ce qu'est la beta web aujourd'hui

- **Front** (`crates/atlas-web`, Leptos 0.7 CSR, cible `wasm32`) : page de **recherche
  d'assets** (champ + résultats + statut), thème sombre. Appelle `POST /v1/search` via
  `fetch` (même origine).
- **Back** (`crates/atlas-core`, Axum) : routes système (`/healthz`, `/readyz`, `/version`,
  `/openapi.json`) + API `/v1` (recherche, ingestion si Postgres, WebSocket temps réel).
- **Nouveau (cet incrément)** : `atlas-core` sert le front statique (`dist/`) **en repli**
  quand la variable `ATLAS_WEB_DIR` pointe sur un dossier `dist/` trunk. Repli SPA : toute
  route inconnue (hors `/v1`, `/healthz`…) renvoie `index.html` en **200** (routage CSR).
  Sans `ATLAS_WEB_DIR` → comportement inchangé (API seule, front via `trunk serve` en dev).

Sans Postgres, la recherche tourne en **mode dégradé** (in-memory, 0 résultat mais contrat
respecté : `"degraded":true`). Avec Postgres (compose), la recherche FTS+kNN est réelle.

---

## 2. Lancer en local

### 2.a — Dev avec hot-reload (deux terminaux)

```bash
# Terminal 1 — API (:8080)
cargo run -p atlas-core

# Terminal 2 — front (depuis crates/atlas-web/, toolchain stable + cible wasm32)
rustup target add wasm32-unknown-unknown          # une fois
cargo install --locked trunk                       # une fois
cd crates/atlas-web && trunk serve                 # :8081, proxy /v1 -> :8080
# ouvrir http://localhost:8081
```

### 2.b — Binaire unique (UI + API depuis un seul process) — recommandé pour démo

```bash
# 1) Bâtir le front (toolchain stable récente — l'arbre Leptos exige > 1.78)
cd crates/atlas-web
rustup run stable trunk build --release            # -> crates/atlas-web/dist/
cd ../..

# 2) Lancer le Core en lui désignant le dist/
ATLAS_WEB_DIR="$PWD/crates/atlas-web/dist" \
  cargo run -p atlas-core --release
# ouvrir http://localhost:8080  → l'UI Atlas s'affiche, servie par atlas-core
```

Vérifications rapides (port 8080) :

```bash
curl -s -o /dev/null -w "%{http_code} %{content_type}\n" http://localhost:8080/         # 200 text/html
curl -s -o /dev/null -w "%{http_code} %{content_type}\n" http://localhost:8080/*.wasm    # 200 application/wasm
curl -s http://localhost:8080/healthz                                                    # {"status":"ok"}
curl -s -X POST http://localhost:8080/v1/search -H 'content-type: application/json' \
  -d '{"query":"plage","page_size":5}'                                                   # {"results":[...],"degraded":true}
```

> **Toolchain** : le back est épinglé **1.78** (`rust-toolchain.toml`) ; le front WASM se
> bâtit avec **stable** (`rustup run stable trunk build`) car l'arbre Leptos requiert un
> rustc plus récent. C'est aussi ce que fait la CI front.

### 2.c — Édition Solo complète (infra incluse : Postgres+pgvector, NATS, SeaweedFS)

```bash
docker compose up --build
# ouvrir http://localhost:8080  (UI + API + recherche FTS/kNN réelle)
```

L'image bâtit le front (étape `front` du `Dockerfile`) et le sert via `ATLAS_WEB_DIR`
(défini dans `docker-compose.yml` et dans l'image).

---

## 3. Déploiement

### 3.a — Image Docker (recommandé)

Le `Dockerfile` est multi-étapes :
1. **`front`** — `rust:1-slim` + `wasm32` + `trunk build --release` → `dist/`.
2. **`build`** — `rust:1.78-slim` → `cargo build --release -p atlas-core` (binaire).
3. **final** — `gcr.io/distroless/cc-debian12`, **non-root** (`65532`), copie le binaire +
   `dist/`, fixe `ENV ATLAS_WEB_DIR=/srv/atlas-web`, expose `8080`.

```bash
docker build -t atlas-core:beta .
docker run --rm -p 8080:8080 atlas-core:beta            # UI + API (mode dégradé sans DB)
# avec une base externe :
docker run --rm -p 8080:8080 \
  -e ATLAS_DATABASE_URL="postgres://atlas:atlas@db:5432/atlas" atlas-core:beta
```

> Build hermétique/air-gap : vendorer les deps (`cargo vendor`) et builder `--offline` (cf.
> `scripts/airgap-test.sh`). Le front, lui, télécharge `trunk` + `wasm-bindgen` au build de
> l'étape `front` (à vendorer aussi pour un build 100 % hors-ligne — chantier suivant).

### 3.b — Binaire seul (sans Docker)

```bash
cargo build --release -p atlas-core               # -> target/release/atlas-core
cd crates/atlas-web && rustup run stable trunk build --release && cd ../..
ATLAS_BIND="0.0.0.0:8080" \
ATLAS_WEB_DIR="/chemin/vers/atlas-web/dist" \
  ./target/release/atlas-core
```

Déposer le binaire + le `dist/` sur la cible, exporter `ATLAS_WEB_DIR`, lancer (systemd,
supervisord…). Aucun secret requis pour démarrer (build hermétique, IA locale par défaut).

---

## 4. Variables d'environnement (Core)

| Variable | Défaut | Rôle |
|----------|--------|------|
| `ATLAS_BIND` | `0.0.0.0:8080` | adresse d'écoute |
| `ATLAS_DATABASE_URL` | `postgres://atlas:atlas@localhost:5432/atlas` | Postgres (sinon mode dégradé in-memory) |
| `ATLAS_NATS_URL` | `nats://localhost:4222` | bus NATS (jalons suivants) |
| `ATLAS_EDITION` | `solo` | `solo` · `team` · `enterprise` |
| `ATLAS_ALLOW_EXTERNAL_LLM` | (off) | `1`/`true` pour autoriser un LLM externe (sinon IA locale) |
| **`ATLAS_WEB_DIR`** | (vide → API seule) | **dossier `dist/` du front WASM servi en statique** |

---

## 5. État vérifié (2026-06-25, toolchain back 1.78 / front stable 1.96)

- `cargo build --workspace` → **vert**.
- `cargo test --workspace` → **130 tests verts, 8 ignored** (intégration Postgres `#[ignore]`).
- `cargo clippy --all-targets -- -D warnings` → **0 warning**.
- `trunk build --release` (front) → **OK** (`dist/` : index.html + JS + WASM).
- Lancement binaire avec `ATLAS_WEB_DIR` → `/` sert l'UI (200 text/html), `*.wasm` en
  `application/wasm`, repli SPA en 200, `/healthz` et `/v1/search` opérationnels.
