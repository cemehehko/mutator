# Образ проверок: Rust + libtll, tll-lua и tll-patch, собранные из соседних
# исходников.
#
# Контекст сборки собирает scripts/check.sh: этот файл и копии `../tll`,
# `../tll-lua`, `../tll-patch` без `.git` и каталогов сборки.
#
# Так проверки идут против того же tll, с которым работают стенды, а не
# против пакета из дистрибутива.
FROM rust:1

RUN apt-get update && apt-get install -y --no-install-recommends \
      meson ninja-build pkg-config cmake \
      libfmt-dev libyaml-dev zlib1g-dev liblz4-dev liblua5.4-dev libgtest-dev \
      python3 python3-yaml \
    && rm -rf /var/lib/apt/lists/*

COPY tll /build/tll
RUN cd /build/tll && meson setup b --prefix=/usr/local -Dwith_gtest=disabled -Dwerror=false \
    && ninja -C b && ninja -C b install && ldconfig

COPY tll-lua /build/tll-lua
RUN cd /build/tll-lua && rm -rf build b && meson setup b --prefix=/usr/local -Dwerror=false \
    && ninja -C b libtll-lua.so && install -m 644 b/libtll-lua.so /usr/local/lib/ && ldconfig

COPY tll-patch /build/tll-patch
RUN cd /build/tll-patch && rm -rf build b && meson setup b --prefix=/usr/local -Dwerror=false \
    && ninja -C b && install -m 644 b/src/libtll-patch.so /usr/local/lib/ && ldconfig

ENV TUI_PATCH_LUA=/usr/local/lib/tll-lua
ENV TUI_PATCH_PATCH=/usr/local/lib/tll-patch
