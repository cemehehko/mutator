#!/usr/bin/env bash
# Трассировка договорённостей: ARCHITECTURE.md ↔ тесты ↔ код.
#
# Договорённость — пункт ARCHITECTURE.md вида «- `agr:id` — …». Метка
# `agr:id` в doc-комментарии над `#[test]` (в `src/` или `tests/`) — тест,
# который её держит; в другом месте `src/` — код, который её реализует.
# Скрипт падает, если:
#   - у договорённости нет места в коде;
#   - у договорённости нет теста, а в её пункте не сказано «Без теста»;
#   - в коде стоит метка, которой в ARCHITECTURE.md нет.
# Перенесено из black-d; отличие — тесты ищутся и в `tests/`.
set -euo pipefail
cd "$(dirname "$0")/.."

agreements=$(awk '
  /^- `agr:[a-z0-9-]+`/ { flush(); match($0, /agr:[a-z0-9-]+/); id = substr($0, RSTART + 4, RLENGTH - 4); notest = 0 }
  /^(- |#)/ && !/^- `agr:/ { flush() }
  id != "" && /Без теста/ { notest = 1 }
  function flush() { if (id != "") print id, notest; id = "" }
  END { flush() }
' ARCHITECTURE.md)

# «файл:строка id вид»: test — если в ближайших трёх строках ниже #[test].
marks=$(find src tests -name '*.rs' -print0 | sort -z | xargs -0 awk '
  function flush() { for (k = 1; k <= n; k++) if (pend[k] != "") print pend[k], "code"; n = 0 }
  FNR == 1 { flush() }
  {
    for (k = 1; k <= n; k++) {
      if (pend[k] == "") continue
      if ($0 ~ /#\[test\]/) { print pend[k], "test"; pend[k] = "" }
      else if (FNR - at[k] >= 3) { print pend[k], "code"; pend[k] = "" }
    }
    line = $0
    while (match(line, /agr:[a-z0-9-]+/)) {
      n++; pend[n] = FILENAME ":" FNR " " substr(line, RSTART + 4, RLENGTH - 4); at[n] = FNR
      line = substr(line, RSTART + RLENGTH)
    }
  }
  END { flush() }
' | awk '$3 == "test" || $1 ~ /^src\//')

fail=0
say() { printf '%s\n' "$*" >&2; }

while read -r id notest; do
  tests=$(awk -v id="$id" '$2 == id && $3 == "test"' <<<"$marks" | wc -l | tr -d ' ')
  code=$(awk -v id="$id" '$2 == id && $3 == "code"' <<<"$marks" | wc -l | tr -d ' ')
  if [ "$code" -eq 0 ]; then say "agr:$id — нет места в коде"; fail=1; fi
  if [ "$tests" -eq 0 ] && [ "$notest" -eq 0 ]; then say "agr:$id — нет теста (или допиши «Без теста: причина»)"; fail=1; fi
  if [ "$tests" -gt 0 ] && [ "$notest" -eq 1 ]; then say "agr:$id — тест есть, а пункт говорит «Без теста»"; fail=1; fi
  printf '%-24s тестов %2s  в коде %s%s\n' "agr:$id" "$tests" "$code" "$([ "$notest" -eq 1 ] && echo '  (без теста)')"
done <<<"$agreements"

known=$(awk '{ print $1 }' <<<"$agreements")
while read -r where id _; do
  [ -z "$where" ] && continue
  grep -qx "$id" <<<"$known" || { say "$where: метка agr:$id не описана в ARCHITECTURE.md"; fail=1; }
done <<<"$marks"

exit $fail
