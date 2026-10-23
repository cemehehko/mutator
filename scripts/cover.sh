#!/usr/bin/env bash
# Покрытие по модулям карты: единица — строка таблицы модулей ARCHITECTURE.md,
# а не файл. Как модуль нарезан внутри, покрытие не меняет.
#
#   cargo llvm-cov --summary-only | scripts/cover.sh
#
# Перенесено из black-d; покрытие считают и интеграционные тесты tests/.
#
# У каждого модуля либо порог в FLOOR, либо «Без теста: причина» в `//!` (у
# каталога — в его `mod.rs`, у строки из нескольких файлов — в каждом). Порог
# только поднимается; модуль ниже порога, без порога или с порогом при «Без
# теста» — ошибка. Помощники тестов (`#[cfg(test)] mod …;`) не в счёт.
set -euo pipefail
export LC_ALL=C
cd "$(dirname "$0")/.."

# Храповик: поднимать вслед за покрытием, не опускать.
FLOOR="
tll/ 93
scheme.rs 80
decode/ 94
lua.rs 96
lua_highlight.rs 91
range.rs 96
dump.rs 88
data/ 94
columns.rs 96
desk.rs 84
session.rs 86
edit/ 95
agent/ 62
markup.rs 99
ui/ 75
"

table=$(cat)
test_only=$(grep -rhA1 '^#\[cfg(test)\]' src --include='*.rs' | awk '/^mod [a-z_]+;/ { sub(/;/, "", $2); print $2 }')
module_doc() { awk '/^\/\/!/ { print; next } /^[^\/]/ && NF { exit }' "$1"; }

fail=0
say() { printf '%s\n' "$*" >&2; fail=1; }

while read -r row; do
  names=$(grep -oE '`[^`]+`' <<<"$row" | tr -d '`')
  unit=$(tr '\n' ' ' <<<"$names" | sed 's/ $//')
  exempt=1
  for name in $names; do
    head=src/$name
    [ -d "$head" ] && head=$head/mod.rs
    module_doc "$head" | grep -q 'Без теста:' || exempt=0
  done
  read -r lines missed < <(awk -v names="$unit" -v skip="$(tr '\n' ' ' <<<"$test_only")" '
    BEGIN { n = split(names, want, " "); split(skip, s, " "); for (k in s) test[s[k] ".rs"] = 1 }
    $2 ~ /^[0-9]+$/ {
      file = $1; base = file; sub(/.*\//, "", base)
      if (base in test) next
      for (k = 1; k <= n; k++)
        if (file == want[k] || (want[k] ~ /\/$/ && index(file, want[k]) == 1)) { l += $8; m += $9 }
    }
    END { print l + 0, m + 0 }
  ' <<<"$table")
  cover=$(awk -v l="$lines" -v m="$missed" 'BEGIN { if (l == 0) print "-"; else printf "%.1f", 100 * (l - m) / l }')
  floor=$(awk -v u="$unit" '$1 == u { print $2 }' <<<"$FLOOR")

  if [ "$exempt" -eq 1 ]; then
    printf '%-58s %6s  (без теста)\n' "$unit" "$cover"
    [ -n "$floor" ] && say "$unit: «Без теста» в //!, а порог задан — убери одно из двух"
    continue
  fi
  printf '%-58s %6s  порог %s\n' "$unit" "$cover" "${floor:--}"
  if [ -z "$floor" ]; then
    say "$unit: нет порога — задай в scripts/cover.sh или напиши в //! «Без теста: причина»"
  elif [ "$floor" -lt 1 ]; then
    say "$unit: порог 0 — модулю без тестов нужно «Без теста: причина» в //!"
  elif [ "$cover" = - ] || awk -v c="$cover" -v f="$floor" 'BEGIN { exit !(c < f) }'; then
    say "$unit: покрытие $cover% ниже порога $floor%"
  fi
done < <(awk '/^\| `/' ARCHITECTURE.md | cut -d'|' -f2)

exit $fail
