#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${EDGER_BASE_URL:-http://127.0.0.1:19080}"
ROOT_KEY="${ROOT_API_KEY:-test-root}"
SCENARIO_NAME="cpanel-scenario"
VERSIONS=("1.0.0" "1.1.0" "1.2.0")

api() {
  curl --fail --silent --show-error -H "x-api-key: ${ROOT_KEY}" "$@"
}

worker_exists() {
  api "${BASE_URL}/api/admin/workers" | jq -e \
    --arg name "${SCENARIO_NAME}" --arg version "$1" \
    '.workers | any(.name == $name and .version == $version)' >/dev/null
}

package_version() {
  local version="$1"
  local dir="/tmp/edger-${SCENARIO_NAME}-${version}"
  local archive="/tmp/${SCENARIO_NAME}-${version}.zip"
  rm -rf "${dir}" "${archive}"
  mkdir -p "${dir}"
  printf '%s\n' \
    "name: ${SCENARIO_NAME}" \
    "version: \"${version}\"" \
    'entrypoint: index.ts' \
    'kind: fetch' \
    'ttl: 60s' \
    'maxProcesses: 2' \
    'queueLimit: 1' \
    'queueTimeout: 120ms' >"${dir}/manifest.yaml"
  printf '%s\n' \
    'export default async function fetch(request) {' \
    '  const url = new URL(request.url);' \
    '  const delay = Number(url.searchParams.get("delay") || "0");' \
    '  if (delay > 0) await new Promise((resolve) => setTimeout(resolve, delay));' \
    '  if (url.pathname.endsWith("/fail")) throw new Error("cpanel scenario failure");' \
    "  return Response.json({ version: \"${version}\", delay });" \
    '}' >"${dir}/index.ts"
  (cd "${dir}" && zip -q "${archive}" manifest.yaml index.ts)
  printf '%s' "${archive}"
}

deploy_versions() {
  local version archive
  for version in "${VERSIONS[@]}"; do
    if worker_exists "${version}"; then
      continue
    fi
    archive="$(package_version "${version}")"
    api -X POST -H 'content-type: application/zip' --data-binary "@${archive}" \
      "${BASE_URL}/api/admin/workers/install" >/dev/null
  done
}

generate_traffic() {
  local request
  for request in {1..8}; do
    curl --silent --output /dev/null "${BASE_URL}/${SCENARIO_NAME}@1.1.0?sample=${request}"
  done
  for request in {1..16}; do
    curl --silent --output /dev/null "${BASE_URL}/${SCENARIO_NAME}?sample=${request}"
  done
  curl --silent --output /dev/null "${BASE_URL}/${SCENARIO_NAME}/fail" || true
  curl --silent --output /dev/null "${BASE_URL}/${SCENARIO_NAME}?delay=450" &
  curl --silent --output /dev/null "${BASE_URL}/${SCENARIO_NAME}?delay=450" &
  curl --silent --output /dev/null "${BASE_URL}/${SCENARIO_NAME}?delay=450" &
  curl --silent --output /dev/null "${BASE_URL}/${SCENARIO_NAME}?delay=450" &
  wait || true
}

setup() {
  deploy_versions
  api -X POST "${BASE_URL}/api/admin/workers/${SCENARIO_NAME}/disable?version=1.0.0" >/dev/null
  generate_traffic
  api "${BASE_URL}/metrics/stats" | rg -o "\{[^{}]*\"name\":\"${SCENARIO_NAME}\"[^{}]*\}" || true
}

cleanup() {
  local version
  for version in "${VERSIONS[@]}"; do
    rm -rf "workers/${SCENARIO_NAME}@${version}"
  done
  rm -rf "workers/${SCENARIO_NAME}"
  api -X POST -H 'content-type: application/json' --data '{"dryRun":false}' \
    "${BASE_URL}/api/admin/workers/rescan" >/dev/null
}

case "${1:-setup}" in
  setup) setup ;;
  traffic) generate_traffic ;;
  cleanup) cleanup ;;
  *) echo "usage: $0 [setup|traffic|cleanup]" >&2; exit 2 ;;
esac
