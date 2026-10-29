#!/usr/bin/env bash
# Уровень требований: сценарии ↔ требования ↔ договорённости ↔ тесты и руки.
#   SCENARIOS.md («- `scn:id` — …») держится требованиями REQUIREMENTS.md,
#   REQUIREMENTS.md («- `req:id` — …») — договорённостями ARCHITECTURE.md,
#   а требование обязано иметь хотя бы один тест через свои договорённости
#   (метка agr: над #[test], как в trace.sh).
#   MANUAL.md («- `man:id` — …») — ручные проверки: держатся договорённостями;
#   договорённость, в пункте которой сказано «руками» или «настоящий
#   терминал», обязана иметь там пункт.
# Падает, если:
#   - у сценария нет требований, у требования — договорённостей или тестов,
#     у ручной проверки — договорённостей;
#   - названное требование или договорённость не существует;
#   - требование не держится ни одним сценарием;
#   - «ручная» договорённость без пункта в MANUAL.md;
#   - метка scn:/req:/man: стоит вне своего файла.
set -euo pipefail
cd "$(dirname "$0")/.."

agrs=$(awk '
  /^- `agr:[a-z0-9-]+`/ { match($0, /agr:[a-z0-9-]+/); print substr($0, RSTART + 4, RLENGTH - 4) }
' ARCHITECTURE.md)

# Тесты договорённостей: метка agr: в трёх строках над #[test] или
# #[tokio::test] (как trace.sh).
tests=$(find src tests -name '*.rs' -print0 | sort -z | xargs -0 awk '
  function flush() { for (k = 1; k <= n; k++) if (pend[k] != "") print pend[k]; n = 0 }
  FNR == 1 { flush() }
  {
    for (k = 1; k <= n; k++) {
      if (pend[k] == "") continue
      if ($0 ~ /#\[(tokio::)?test[](]/) { print pend[k]; pend[k] = "" }
      else if (FNR - at[k] >= 3) pend[k] = ""
    }
    line = $0
    while (match(line, /agr:[a-z0-9-]+/)) {
      n++; pend[n] = substr(line, RSTART + 4, RLENGTH - 4); at[n] = FNR
      line = substr(line, RSTART + RLENGTH)
    }
  }
  END { flush() }
')

# Пункт файла: от «- `<pfx:id>`» до следующего пункта или заголовка; ссылки
# `<hold:…>` из «Держат:» собираются в третий столбец.
parse() { # $1 — файл, $2 — pfx, $3 — чей id держат
  awk -v pfx="$2" -v hold="$3" '
    function flush() {
      if (id == "") return
      holds = ""
      s = buf
      while (match(s, hold ":[a-z0-9-]+")) {
        a = substr(s, RSTART + length(hold) + 1, RLENGTH - length(hold) - 1)
        if (index(" " holds " ", " " a " ") == 0) holds = holds (holds == "" ? "" : " ") a
        s = substr(s, RSTART + RLENGTH)
      }
      print id, (buf ~ /Без договорённости|Без требований/ ? 1 : 0), holds
      id = ""
    }
    $0 ~ ("^- `" pfx ":") {
      flush()
      match($0, pfx ":[a-z0-9-]+")
      id = substr($0, RSTART + length(pfx) + 1, RLENGTH - length(pfx) - 1)
      buf = $0; next
    }
    /^(- |#)/ { flush() }
    id != "" { buf = buf "\n" $0 }
    END { flush() }
  ' "$1"
}

fail=0
say() { printf '%s\n' "$*" >&2; }

reqs=$(parse REQUIREMENTS.md req agr)
scns=$(parse SCENARIOS.md scn req)
mans=$(parse MANUAL.md man agr)

req_ids=" "
scn_holds=" "
while read -r id exempt holds; do
  n_agr=$(wc -w <<<"$holds" | tr -d " ")
  t=0
  for a in $holds; do
    grep -qx "$a" <<<"$agrs" || { say "req:$id — договорённость agr:$a не описана в ARCHITECTURE.md"; fail=1; }
    t=$((t + $(grep -cx "$a" <<<"$tests" || true)))
  done
  if [[ -z "$holds" && "$exempt" == 0 ]]; then
    say "req:$id — нет договорённостей (или допиши «Держат: \`agr:…\`», или «Без договорённости: причина»)"
    fail=1
  fi
  if [[ "$t" -eq 0 && "$exempt" == 0 ]]; then
    say "req:$id — ни один тест не держит его договорённости"
    fail=1
  fi
  printf '%-20s договорённостей %2s  тестов %3s\n' "req:$id" "$n_agr" "$t"
  req_ids="$req_ids$id "
done <<<"$reqs"

while read -r id exempt holds; do
  if [[ -z "$holds" ]]; then
    say "scn:$id — нет требований (допиши «Держат: \`req:…\`»)"
    fail=1
  fi
  for r in $holds; do
    if [[ "$req_ids" != *" $r "* ]]; then
      say "scn:$id — требование req:$r не описано в REQUIREMENTS.md"
      fail=1
    fi
    scn_holds="$scn_holds$r "
  done
  printf '%-18s требований %2s\n' "scn:$id" "$(wc -w <<<"$holds" | tr -d " ")"
done <<<"$scns"

man_holds=" "
while read -r id exempt holds; do
  if [[ -z "$holds" ]]; then
    say "man:$id — нет договорённостей (допиши «Держит: \`agr:…\`»)"
    fail=1
  fi
  for a in $holds; do
    grep -qx "$a" <<<"$agrs" || { say "man:$id — договорённость agr:$a не описана в ARCHITECTURE.md"; fail=1; }
    man_holds="$man_holds$a "
  done
  printf '%-16s договорённостей %2s\n' "man:$id" "$(wc -w <<<"$holds" | tr -d " ")"
done <<<"$mans"

# Договорённость «на руки»: в пункте сказано «руками» или «настоящий терминал».
manual_req=$(awk '
  function flush() {
    if (id != "" && buf ~ /руками|настоящий терминал/) print id
    id = ""
  }
  /^- `agr:[a-z0-9-]+`/ { flush(); match($0, /agr:[a-z0-9-]+/); id = substr($0, RSTART + 4, RLENGTH - 4); buf = $0; next }
  /^(- |#)/ { flush() }
  id != "" { buf = buf "\n" $0 }
  END { flush() }
' ARCHITECTURE.md)

while read -r a; do
  [[ -z "$a" ]] && continue
  if [[ "$man_holds" != *" $a "* ]]; then
    say "agr:$a — сказано «руками»/«настоящий терминал», а пункта в MANUAL.md нет"
    fail=1
  fi
done <<<"$manual_req"

for r in $req_ids; do
  [[ -z "$r" ]] && continue
  if [[ "$scn_holds" != *" $r "* ]]; then
    say "req:$r — не держится ни одним сценарием"
    fail=1
  fi
done

# Метки уровней требований живут только в своих файлах: вниз они говорят
# через agr (тесты и руки) и req (сценарии).
for pfx in scn req man; do
  while IFS= read -r where; do
    say "$where: метка $pfx: вне своего файла"
    fail=1
  done < <(grep -rEn "$pfx:[a-z0-9-]+" src tests 2>/dev/null | cut -d: -f1-2 || true)
done

echo "требований $(wc -w <<<"$req_ids" | tr -d ' '), сценариев $(grep -c '^- `scn:' SCENARIOS.md), ручных $(grep -c '^- `man:' MANUAL.md)"
exit $fail
