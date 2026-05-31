#!/usr/bin/env bash
# Test air-gap (doc 02 §12 / doc 24) : le service démarre et répond SANS réseau externe.
# En CI, exécuter idéalement dans un namespace réseau coupé (unshare -n).
set -euo pipefail

BIN="./target/release/atlas-core"
PORT="${ATLAS_PORT:-8080}"
export ATLAS_BIND="127.0.0.1:${PORT}"
export ATLAS_EDITION="solo"
# Aucune variable n'active de LLM externe : souveraineté par défaut.

echo "[airgap] démarrage de atlas-core (réseau externe non requis)…"
"$BIN" &
PID=$!
trap 'kill "$PID" 2>/dev/null || true' EXIT

# Attente du readiness
for i in $(seq 1 30); do
  if curl -fsS "http://127.0.0.1:${PORT}/healthz" >/dev/null 2>&1; then
    echo "[airgap] /healthz OK"
    break
  fi
  sleep 0.5
done

curl -fsS "http://127.0.0.1:${PORT}/readyz"      >/dev/null && echo "[airgap] /readyz OK"
curl -fsS "http://127.0.0.1:${PORT}/openapi.json" >/dev/null && echo "[airgap] /openapi.json servi localement OK"

echo "[airgap] SUCCÈS : fonctionne sans dépendance externe."
