#!/usr/bin/env bash
# Связи между уровнями описания, которые не видит компилятор.
#
#   - Карта называет модули: каждый файл и каталог `src/` (кроме корней крейта
#     `lib.rs` и `main.rs`) стоит в таблице модулей ARCHITECTURE.md.
#   - Родитель называет детей: каждый файл и подкаталог папки назван в `//!` её
#     `mod.rs`. Нечего сказать о файле одной фразой — значит, это не отдельная
#     вещь, и её место в соседнем файле.
#   - `//!` называет поверхность: всё, что файл открывает наружу (`pub…` на
#     верхнем уровне и `pub use`), названо в его `//!`. Старый долг записан в
#     DEBT и только убывает: больше — назови или закрой, меньше — опусти запись.
#
# Названо — значит, имя стоит в обратных кавычках: `имя`, [`имя`],
# [`Тип::имя`], `имя.rs`, `имя/`.
#
#   scripts/levels.sh        — проверить
#   scripts/levels.sh debt   — напечатать нынешний долг в формате DEBT
set -euo pipefail
export LC_ALL=C
cd "$(dirname "$0")/.."

# Безымянная поверхность, которая была до правила: файл и сколько имён.
# Перенесено из black-d. tll/sys.rs — сырые объявления C API, имена один в
# один как в заголовках libtll: называть их в //! — пересказ заголовков.
DEBT="
tll/sys.rs 30
"

mode=${1:-check}
fail=0
say() { printf '%s\n' "$*" >&2; fail=1; }

# Строки `//!` файла одной строкой.
module_doc() { awk '/^\/\/!/ { print; next } /^[^\/]/ && NF { exit }' "$1"; }

# Названо ли имя в тексте.
named() { grep -qE "\`(\[?[A-Za-z_:]*::)?$2(\.rs|/)?\`" <<<"$1"; }

# Карта называет модули.
map=$(awk '/^\| `/' ARCHITECTURE.md)
for entry in src/*; do
  name=${entry#src/}
  case $name in lib.rs | main.rs) continue ;; esac
  [ -d "$entry" ] && name="$name/"
  grep -qF "\`$name\`" <<<"$map" || say "ARCHITECTURE.md: модуль $name не назван в таблице модулей"
done

# Родитель называет детей.
while read -r parent; do
  dir=$(dirname "$parent")
  doc=$(module_doc "$parent")
  for child in "$dir"/*; do
    [ "$child" = "$parent" ] && continue
    stem=$(basename "$child" .rs)
    named "$doc" "$stem" || say "$parent: в //! не назван ${child#src/}"
  done
done < <(find src -mindepth 2 -name mod.rs | sort)

# `//!` называет поверхность. Тестовые модули и помощники тестов не в счёт.
test_only=$(grep -rhA1 '^#\[cfg(test)\]' src --include='*.rs' | awk '/^mod [a-z_]+;/ { sub(/;/, "", $2); print $2 }')
while read -r file; do
  rel=${file#src/}
  grep -qx "$(basename "$file" .rs)" <<<"$test_only" && continue
  doc=$(module_doc "$file")
  surface=$(awk '
    /^#\[cfg\(test\)\]/ { t = 1; next }
    t && /^mod tests/ { exit }
    { t = 0 }
    /^pub(\([^)]*\))? (fn|struct|enum|const|static|type|trait) / {
      sub(/^pub(\([^)]*\))? /, ""); sub(/^(fn|struct|enum|const|static|type|trait) /, "")
      match($0, /^[A-Za-z_][A-Za-z0-9_]*/); print substr($0, 1, RLENGTH); next
    }
    /^pub(\([^)]*\))? use / && !/\*/ {
      s = $0; sub(/^pub(\([^)]*\))? use /, "", s); sub(/;.*/, "", s)
      if (s ~ /\{/) { sub(/.*\{/, "", s); sub(/\}.*/, "", s) } else sub(/.*::/, "", s)
      n = split(s, part, /, */); for (k = 1; k <= n; k++) if (part[k] != "self") print part[k]
    }
  ' "$file")
  unnamed=0
  while read -r name; do
    [ -z "$name" ] && continue
    named "$doc" "$name" || unnamed=$((unnamed + 1))
  done <<<"$surface"
  if [ "$mode" = debt ]; then
    [ "$unnamed" -gt 0 ] && echo "$rel $unnamed"
    continue
  fi
  debt=$(awk -v f="$rel" '$1 == f { print $2 }' <<<"$DEBT")
  debt=${debt:-0}
  if [ "$unnamed" -gt "$debt" ]; then
    say "$rel: поверхность без имени в //! — $unnamed, долг $debt: назови новое в //! или закрой его"
  elif [ "$unnamed" -lt "$debt" ]; then
    say "$rel: безымянной поверхности стало $unnamed — опусти долг в scripts/levels.sh с $debt"
  fi
done < <(find src -name '*.rs' | sort)

exit $fail
