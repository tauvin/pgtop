#!/usr/bin/env bash
# Синтетическая нагрузка для разработки pgtop.
#
# Цель — чтобы в pg_stat_activity всегда было что показать:
#   - быстрые SELECT'ы (миллисекунды)
#   - средние с pg_sleep (1-3 с)
#   - длинные изредка (20-50 с)
#   - "забытая" транзакция в idle in transaction изредка
#
# Никаких записей не делаем; работаем поверх pg_catalog и pg_sleep.

set -u

log() { printf '[load %s] %s\n' "$(date +%H:%M:%S)" "$*"; }

# Каждый run_session — отдельный psql-процесс в фоне, поэтому в pg_stat_activity
# он виден как отдельный backend (а не одна общая сессия).
run_session() {
  psql -v ON_ERROR_STOP=1 -At -c "$1" >/dev/null 2>&1 &
}

# Открыть BEGIN, посидеть в idle in transaction $1 секунд, COMMIT.
# Полезно для тестирования предупреждений pgtop про "забытые" транзакции.
idle_in_tx() {
  local secs="$1"
  log "starting idle-in-tx session for ${secs}s"
  (
    psql -v ON_ERROR_STOP=1 <<SQL >/dev/null 2>&1
BEGIN;
SELECT 1;
SELECT pg_sleep(${secs});
COMMIT;
SQL
  ) &
}

log "load generator starting; PGHOST=${PGHOST:-?} DB=${PGDATABASE:-?}"
sleep 2  # дать healthcheck немного запаса

iter=0
while true; do
  iter=$((iter + 1))

  # 5 быстрых селектов
  for _ in 1 2 3 4 5; do
    run_session "SELECT count(*) FROM pg_class"
  done

  # 1 средний (1-3 с)
  mid=$((RANDOM % 3 + 1))
  run_session "SELECT pg_sleep(${mid}), count(*) FROM pg_attribute"

  # 1 длинный каждые 10 итераций (20-49 с)
  if (( iter % 10 == 0 )); then
    long=$((RANDOM % 30 + 20))
    log "starting long query ~${long}s"
    run_session "SELECT pg_sleep(${long})"
  fi

  # idle-in-tx каждые 25 итераций (30-59 с)
  if (( iter % 25 == 0 )); then
    idle_in_tx $((RANDOM % 30 + 30))
  fi

  sleep 1
done
