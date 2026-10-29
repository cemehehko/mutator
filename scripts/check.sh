#!/usr/bin/env bash
# Все проверки разом, в Docker: на macOS-хосте тяжёлое не гоняем.
#
#   scripts/check.sh            — всё
#   scripts/check.sh test doc   — только названные шаги
#
# Шаги: fmt (rustfmt), clippy, test (юнит, интеграционные на эталонах, doctests),
# doc (битые ссылки на код — ошибка), req (сценарии ↔ требования ↔
# договорённости ↔ тесты и руки), trace (договорённости ↔ тесты ↔ код), levels
# (карта
# и `//!` называют модули, файлы и поверхность), cov (покрытие по модулям карты,
# пороги — в scripts/cover.sh), budget (файлы уровней — в бюджете строк).
#
# Образ с libtll и tll-lua собирается из соседних исходников `../tll`,
# `../tll-lua` (docker/Dockerfile). Кэш реестра и
# сборки — в именованных томах.
set -euo pipefail
cd "$(dirname "$0")/.."

steps=${*:-fmt clippy test doc req trace levels cov budget}

# req, trace, levels и budget — grep и wc, Docker им не нужен.
for step in req trace levels budget; do
  if [[ " $steps " == *" $step "* ]]; then
    echo "== $step"
    "scripts/$step.sh"
    steps=${steps//$step/}
  fi
done
[[ -z "${steps// /}" ]] && exit 0

image=tui-patch-check
ctx=$(mktemp -d)
trap 'rm -rf "$ctx"' EXIT
cp docker/Dockerfile "$ctx/"
for src in "${TLL_SRC:-../tll}:tll" \
  "${TLL_LUA_SRC:-../tll-lua}:tll-lua"; do
  rsync -a --exclude .git --exclude build --exclude b --exclude .cache "${src%%:*}/" "$ctx/${src##*:}/"
done
docker build -q -t "$image" "$ctx" >/dev/null

docker run --rm \
  -v "$PWD":/src:ro \
  -v tui-patch-cargo:/usr/local/cargo/registry \
  -v tui-patch-target:/target \
  -e CARGO_TARGET_DIR=/target \
  -e STEPS="$steps" \
  "$image" bash -euo pipefail -c '
    mkdir /work && cd /src
    tar cf - --exclude=./target --exclude=./.git . | tar xf - -C /work
    cd /work
    # Образцы со стендов в корне — не для проверок: только эталоны tests/data.
    rm -f ./*.dat
    for step in $STEPS; do
      echo "== $step"
      case $step in
        fmt)    rustup component add rustfmt >/dev/null 2>&1; cargo fmt --check ;;
        clippy) rustup component add clippy >/dev/null 2>&1; cargo clippy --all-targets --features mcp,wui -q -- -D warnings ;;
        test)   cargo test -q -j 1 --features mcp,wui ;;
        doc)    RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" \
                  cargo doc --lib --no-deps --document-private-items --features mcp,wui -q ;;
        cov)    rustup component add llvm-tools-preview >/dev/null 2>&1
                command -v cargo-llvm-cov >/dev/null || curl -sSfL \
                  "https://github.com/taiki-e/cargo-llvm-cov/releases/latest/download/cargo-llvm-cov-$(uname -m)-unknown-linux-gnu.tar.gz" \
                  | tar xz -C /usr/local/cargo/bin
                # По одной сборке за раз: параллельная линковка инструментированного
                # окна съедает память VM и линкер гибнет по OOM.
                CARGO_BUILD_JOBS=1 cargo llvm-cov --features mcp,wui --summary-only 2>/dev/null | scripts/cover.sh ;;
        *)      echo "неизвестный шаг: $step" >&2; exit 2 ;;
      esac
    done
  '
