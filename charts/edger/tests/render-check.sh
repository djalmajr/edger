#!/usr/bin/env bash
# Run from any directory; HELM_BIN can name a native Windows helm.exe.
set -eu
chart=${1:-"$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"}
helm_bin=${HELM_BIN:-helm}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
render() { "$helm_bin" template edger "$chart" --set-string rootKey.value=ci-test-root-key "$@"; }
require() {
  if ! grep -F -- "$1" "$2" >/dev/null; then
    echo "FAIL: missing $1" >&2
    exit 1
  fi
}
require_env() {
  if ! grep -E "^[[:space:]]*- name: $1[[:space:]]*$" "$2" >/dev/null; then
    echo "FAIL: missing exact environment name $1" >&2
    exit 1
  fi
}
render >"$tmp/default.yaml"
if grep -E 'EDGER_SQLD_|EDGER_SSE_|EDGER_HTTP_DRAIN_MS' "$tmp/default.yaml"; then
  echo 'FAIL: optional environment present by default' >&2
  exit 1
fi
require 'terminationGracePeriodSeconds: 45' "$tmp/default.yaml"
cat >"$tmp/managed.yaml" <<'YAML'
appSecrets:
  enabled: true
  keySecret:
    name: ci-vault
managedDatabase:
  enabled: true
  adminUrl: http://sqld-admin.invalid:9090
  userUrl: http://sqld-data.invalid:8080
  adminKeySecret:
    name: ci-admin
    key: admin-auth-key
  signerKeySecret:
    name: ci-signer
    key: signer-master
  budgetBytes: "16777216"
YAML
render -f "$tmp/managed.yaml" >"$tmp/managed-rendered.yaml"
for name in ADMIN_URL ADMIN_KEY_FILE USER_URL REGISTRY_DB SIGNER_KEY_FILE MANAGED_BUDGET_BYTES BACKUP_DIR; do
  require_env "EDGER_SQLD_$name" "$tmp/managed-rendered.yaml"
done
for text in 'name: managed-database-directories' 'chmod u=rwx,go=,u-s,g-s,o-t' '/var/lib/edger-managed/registry/registry.db' '/var/lib/edger-managed/backups' '/var/run/secrets/edger-sqld-admin/key' '/var/run/secrets/edger-sqld-signer/key' 'mountPath: /var/lib/edger-managed' 'mountPath: /var/run/secrets/edger-sqld-admin' 'mountPath: /var/run/secrets/edger-sqld-signer' 'secretName: "ci-admin"' 'secretName: "ci-signer"' 'key: "admin-auth-key"' 'key: "signer-master"' 'claimName: edger-managed-database' 'name: edger-managed-database' 'defaultMode: 0440'; do
  require "$text" "$tmp/managed-rendered.yaml"
done
bash "$(dirname -- "$0")/managed-modes-check.sh" "$tmp/managed-rendered.yaml"
render -f "$tmp/managed.yaml" --set managedDatabase.persistence.existingClaim=ci-existing >"$tmp/existing.yaml"
require 'claimName: ci-existing' "$tmp/existing.yaml"
if grep -F 'name: edger-managed-database' "$tmp/existing.yaml"; then
  echo 'FAIL: managed PVC created despite existingClaim' >&2
  exit 1
fi
if render -f "$tmp/managed.yaml" --set-string managedDatabase.adminKeySecret.name= >"$tmp/partial.txt" 2>&1; then
  echo 'FAIL: partial configuration accepted' >&2; exit 1
fi
require 'managedDatabase.adminKeySecret.name is required when enabled' "$tmp/partial.txt"
if render -f "$tmp/managed.yaml" --set appSecrets.enabled=false >"$tmp/no-vault.txt" 2>&1; then
  echo 'FAIL: unavailable vault accepted' >&2; exit 1
fi
require 'managedDatabase requires appSecrets.enabled=true' "$tmp/no-vault.txt"
render --set sse.maxConnections=0 --set sse.maxConnectionsPerApp=128 --set sse.heartbeatMs=1000 --set httpDrainMs=0 --set terminationGracePeriodSeconds=60 >"$tmp/tuning.yaml"
for name in EDGER_SSE_MAX_CONNECTIONS EDGER_SSE_MAX_CONNECTIONS_PER_APP EDGER_SSE_HEARTBEAT_MS EDGER_HTTP_DRAIN_MS; do
  require_env "$name" "$tmp/tuning.yaml"
done
require 'value: "0"' "$tmp/tuning.yaml"
require 'terminationGracePeriodSeconds: 60' "$tmp/tuning.yaml"
echo 'RENDER_CHECK_PASS: default, managed, existingClaim, partial, vault, SSE and shutdown'
