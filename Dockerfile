# Atlas DAM — image du service core (binaire statique, frugal) + front WASM servi en statique.
# Beta web : un binaire unique sert l'UI Leptos (dist/) ET l'API (/v1, /healthz…).
# Build hermétique recommandé : vendorer les deps et builder avec --offline.

# --- Étape 1 : front WASM (Leptos CSR) bâti par trunk (cible wasm32) ---
# Le front est EXCLU du workspace back (toolchain back épinglée 1.78, trop ancienne pour
# l'arbre Leptos) → on le bâtit avec une toolchain stable récente + la cible wasm32.
FROM rust:1-slim AS front
WORKDIR /src
RUN rustup target add wasm32-unknown-unknown \
 && cargo install --locked trunk
COPY crates/atlas-web/ ./crates/atlas-web/
RUN cd crates/atlas-web && trunk build --release
# → /src/crates/atlas-web/dist/

# --- Étape 2 : service Core (back) ---
FROM rust:1.78-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p atlas-core

# --- Étape 3 : image finale distroless non-root ---
FROM gcr.io/distroless/cc-debian12
COPY --from=build /src/target/release/atlas-core /usr/local/bin/atlas-core
COPY --from=front /src/crates/atlas-web/dist/ /srv/atlas-web/
# ATLAS_WEB_DIR : le Core sert ce dossier en statique (UI + API depuis un seul binaire).
ENV ATLAS_WEB_DIR=/srv/atlas-web
EXPOSE 8080
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/atlas-core"]
