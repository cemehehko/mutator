#!/usr/bin/env bash
# Бюджет строк файлов уровней: чем ниже уровень, тем длиннее файл — вверх
# короткие «зачем», вниз длинные «как». Правка, выводящая файл за бюджет,
# сначала сокращает: переносит деталь уровнем ниже или удаляет устаревшее.
# Назначение бюджета меняется только вниз — как пороги покрытия в cover.sh,
# только вверх. Превышение или пропавший файл — ошибка.
set -euo pipefail
cd "$(dirname "$0")/.."

budgets="SCENARIOS.md 20
MANUAL.md 30
REQUIREMENTS.md 105
README.md 70
ARCHITECTURE.md 280
AGENTS.md 70"

fail=0
while read -r f n; do
  [[ -z $f ]] && continue
  if [[ ! -f $f ]]; then
    printf '%s: файла нет\n' "$f" >&2
    fail=1
    continue
  fi
  got=$(wc -l <"$f" | tr -d '[:space:]')
  if (( got > n )); then
    printf '%s: %s строк — бюджет %s: сократи деталь или осознанно снизь бюджет\n' \
      "$f" "$got" "$n" >&2
    fail=1
  fi
  printf '%-16s %3s / %3s\n' "$f" "$got" "$n"
done <<<"$budgets"

exit $fail
