# Atlas DAM — image du service core (binaire statique, frugal).
# Build hermétique recommandé : vendorer les deps et builder avec --offline.
FROM rust:1.78-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p atlas-core

FROM gcr.io/distroless/cc-debian12
COPY --from=build /src/target/release/atlas-core /usr/local/bin/atlas-core
EXPOSE 8080
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/atlas-core"]
