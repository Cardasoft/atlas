# atlas-web — front WASM (Leptos CSR)

Première brique du **front Atlas** (le produit pour les utilisateurs métier, jusqu'ici absent) :
une **recherche d'assets** qui appelle `POST /v1/search` (doc 25) et affiche les résultats.

> **Crate EXCLU du workspace back** (`Cargo.toml` racine → `exclude`). Cible **wasm32**, bâti par
> **trunk** (pas par `cargo test --all`) → la CI back (test/clippy/cargo-deny/air-gap) reste verte
> et hermétique.

## Démarrer (dev)
```bash
# API (terminal 1)
cargo run -p atlas-core            # :8080

# Front (terminal 2) — depuis crates/atlas-web/
trunk serve                        # :8081, proxy /v1 -> :8080 (cf. Trunk.toml)
# ouvrir http://localhost:8081
```

## Build
```bash
# depuis crates/atlas-web/
cargo build --target wasm32-unknown-unknown   # compile le wasm
trunk build --release                          # bundle -> dist/
```

## Stack
- **Leptos 0.7 (CSR)** + `view!` ; appels API via **`fetch` (web-sys)** (même origine via le proxy
  trunk) — pas de SDK/CDN, pas de dépendance HTTP lourde (souverain/frugal).
- DTO miroir du contrat OpenAPI (`SearchResultItem`, `SearchResponse`), tolérants (champs optionnels).

## Statut (M1 — démarrage)
- ✅ Écrit : recherche (input + résultats), client API `api::search`, thème sombre.
- ⏳ **À build-vérifier dans un environnement avec accès aux crates** (le sandbox de démarrage a un
  réseau restreint qui empêche de résoudre l'arbre de dépendances Leptos ici). À faire au prochain
  run outillé / en CI front.
- 🔜 Suite : grille d'assets + facettes + visionneuse + **badge provenance IA** (AI Act art. 50),
  upload, recherches sauvegardées, temps réel (WebSocket `/v1/ws`).
