#!/usr/bin/env bash
# Execute the rendered init command on a real filesystem, never the PVC.
set -eu
rendered=${1:?pass the rendered managed Deployment YAML}
command=$(tr -d '\r' <"$rendered" | sed -n 's/^ *- "\(umask 077;.*\)"$/\1/p')
[ -n "$command" ] || { echo 'FAIL: rendered init command missing' >&2; exit 1; }
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
chmod 2770 "$tmp"
if [ "$(stat -c %a "$tmp")" != 2770 ]; then
  echo 'MODES_CHECK_ENV: filesystem cannot represent 02770; rerun on a Linux filesystem'
  # CI must prove the modes; only a local non-Linux filesystem may skip.
  [ "${CI:-}" = true ] && { echo 'FAIL: modes check skipped under CI' >&2; exit 1; }
  exit 0
fi
check_mode() {
  actual=$(stat -c %a "$2")
  [ "$actual" = "$1" ] || { echo "FAIL: $2 mode $actual, expected $1" >&2; exit 1; }
}
for scenario in fresh remount; do
  root="$tmp/$scenario"
  mkdir "$root"
  chmod 2770 "$root"
  if [ "$scenario" = remount ]; then
    mkdir "$root/registry" "$root/backups"
    chmod 2770 "$root/registry" "$root/backups"
    for file in registry/registry.db registry/registry.db-journal registry/registry.db-wal registry/registry.db-shm backups/previous.sql backups/.tmp-staging; do
      printf synthetic >"$root/$file"
      chmod 0660 "$root/$file"
    done
    printf untouched >"$tmp/outside"
    chmod 0664 "$tmp/outside"
    ln -s "$tmp/outside" "$root/backups/link"
    ln -s "$tmp" "$root/backups/directory-link"
    mkfifo "$root/backups/fifo"
    chmod 0660 "$root/backups/fifo"
    link_before=$(stat -c '%i:%a' "$root/backups/link")
  fi
  # Substitute only the mount root; execute the actual rendered shell program.
  EDGER_TEST_ROOT="$root" /bin/sh -ec "${command//\/var\/lib\/edger-managed/\"\$EDGER_TEST_ROOT\"}"
  check_mode 700 "$root/registry"
  check_mode 700 "$root/backups"
  if [ "$scenario" = remount ]; then
    for file in registry/registry.db registry/registry.db-journal registry/registry.db-wal registry/registry.db-shm backups/previous.sql backups/.tmp-staging; do
      check_mode 600 "$root/$file"
    done
    [ -L "$root/backups/link" ] && [ "$(readlink "$root/backups/link")" = "$tmp/outside" ]
    [ "$(stat -c '%i:%a' "$root/backups/link")" = "$link_before" ]
    [ -L "$root/backups/directory-link" ] && [ "$(readlink "$root/backups/directory-link")" = "$tmp" ]
    check_mode 664 "$tmp/outside"
    [ -p "$root/backups/fifo" ]
    check_mode 660 "$root/backups/fifo"
  fi
  echo "MODES_CHECK_PASS: $scenario"
done
