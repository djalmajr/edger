#!/bin/sh
set -eu
umask 077

# Backup de UM banco do sqld compartilhado (um CronJob por banco; os
# parâmetros por banco vêm do env do CronJob).
#
# Obrigatórios:
#   SQLD_URL     - URL do Service do banco. O primeiro rótulo do Host
#                  escolhe o namespace do libsql; use o FQDN do Service
#                  (http://<banco>.edger.svc.cluster.local:8080).
#   BACKUP_DIR   - diretório dos dumps (PVC sqld-backups, compartilhado)
#   KEEP         - quantos dumps deste banco manter (inteiro >= 1)
#   DB_PREFIX    - prefixo dos arquivos (<banco>-<timestamp>.sql.gz)
# Opcionais:
#   SENTINEL         - tabela que deve existir no dump (ex.: __migrations);
#                      vazio = sem checagem de sentinela
#   SQLD_AUTH_TOKEN  - token de leitura do banco (Bearer no GET /dump)
#   HEALTH_ATTEMPTS  - tentativas do /health (padrão 15)
#   HEALTH_DELAY     - pausa entre tentativas, em segundos (padrão 2)

if [ -z "${SQLD_URL:-}" ]; then
  printf '%s\n' 'backup failed: missing SQLD_URL' >&2
  exit 2
fi
if [ -z "${BACKUP_DIR:-}" ]; then
  printf '%s\n' 'backup failed: missing BACKUP_DIR' >&2
  exit 2
fi
if [ -z "${KEEP:-}" ]; then
  printf '%s\n' 'backup failed: missing KEEP' >&2
  exit 2
fi
if [ -z "${DB_PREFIX:-}" ]; then
  printf '%s\n' 'backup failed: missing DB_PREFIX' >&2
  exit 2
fi

case $KEEP in
  *[!0-9]*)
    printf '%s\n' 'backup failed: invalid KEEP' >&2
    exit 2
    ;;
esac
keep_limit=$KEEP
while [ "${keep_limit#0}" != "$keep_limit" ]; do
  keep_limit=${keep_limit#0}
done
if [ -z "$keep_limit" ] || ! [ "$keep_limit" -ge 1 ] 2>/dev/null; then
  printf '%s\n' 'backup failed: invalid KEEP' >&2
  exit 2
fi

SQLD_AUTH_TOKEN=${SQLD_AUTH_TOKEN:-}
SENTINEL=${SENTINEL:-}
mkdir -p "$BACKUP_DIR"

for stale in "$BACKUP_DIR"/"$DB_PREFIX"-*.sql.gz.partial; do
  if [ -f "$stale" ]; then
    rm -f "$stale"
  fi
done

timestamp=$(date -u +%Y%m%dT%H%M%SZ)
name=$DB_PREFIX-$timestamp.sql.gz
partial=$BACKUP_DIR/$name.partial
final=$BACKUP_DIR/$name
fifo=/tmp/sqld-backup-$DB_PREFIX-$$.fifo

cleanup() {
  if [ -n "${fifo:-}" ]; then
    rm -f "$fifo"
  fi
  if [ -n "${partial:-}" ]; then
    rm -f "$partial"
  fi
}
trap cleanup 0

fail() {
  reason=$1
  printf 'backup failed: %s\n' "$reason" >&2
  exit 1
}

# Um pod novo pode ter as primeiras conexões derrubadas até o controlador
# de rede do cluster admitir o IP dele (visto em produção: duas execuções
# falharam em até 3 s do start). /health não pede token.
attempt=1
until curl -fsS -o /dev/null --max-time 5 "$SQLD_URL/health" 2>/dev/null; do
  if [ "$attempt" -ge "${HEALTH_ATTEMPTS:-15}" ]; then
    fail http
  fi
  attempt=$((attempt + 1))
  sleep "${HEALTH_DELAY:-2}"
done

rm -f "$fifo"
mkfifo "$fifo"
if [ -n "$SQLD_AUTH_TOKEN" ]; then
  (
    printf 'Authorization: Bearer %s\n' "$SQLD_AUTH_TOKEN" |
      curl -fsS -H @- "$SQLD_URL/dump"
  ) > "$fifo" 2>/dev/null &
else
  curl -fsS "$SQLD_URL/dump" > "$fifo" 2>/dev/null &
fi
curl_pid=$!

if gzip -c < "$fifo" > "$partial"; then
  gzip_status=0
else
  gzip_status=$?
fi
if wait "$curl_pid"; then
  curl_status=0
else
  curl_status=$?
fi
rm -f "$fifo"
fifo=

if [ "$curl_status" -ne 0 ]; then
  fail http
fi
if [ "$gzip_status" -ne 0 ] || ! gzip -t "$partial" 2>/dev/null; then
  fail gzip
fi

uncompressed_bytes=$(gzip -dc "$partial" | wc -c)
if [ "$uncompressed_bytes" -eq 0 ]; then
  fail empty
fi

last_nonempty=$(gzip -dc "$partial" | grep -v '^[[:space:]]*$' | tail -n 1)
if [ "$last_nonempty" != 'COMMIT;' ]; then
  fail truncated
fi

# Sentinela opcional por banco: o dump do Cinzel deve carregar a tabela
# __migrations (o restore do Cinzel a confere também); bancos sem
# sentinela (Pinar) pulam a checagem.
if [ -n "$SENTINEL" ]; then
  if ! gzip -dc "$partial" | grep "CREATE TABLE.*$SENTINEL" >/dev/null; then
    fail no-sentinel
  fi
fi

mv "$partial" "$final"
partial=
bytes=$(wc -c < "$final")
bytes=${bytes##* }

files=$(for file in "$BACKUP_DIR"/"$DB_PREFIX"-*.sql.gz; do
  if [ -f "$file" ]; then
    printf '%s\n' "$file"
  fi
done | LC_ALL=C sort -r)
kept=0
if [ -n "$files" ]; then
  while IFS= read -r file; do
    if [ "$kept" -lt "$keep_limit" ]; then
      kept=$((kept + 1))
    else
      rm -f "$file"
    fi
  done <<EOF
$files
EOF
fi

printf 'backup ok: %s %s bytes, kept %s\n' "$name" "$bytes" "$kept"
