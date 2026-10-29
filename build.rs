//! Библиотеки бэкендов не линкуются — они грузятся при открытии файла
//! (`agr:backend-dlopen`). Здесь только подсказка загрузчику: каталог libtll
//! из pkg-config (`MUTATOR_TLL_LIBDIR`), если она есть на машине сборки.
//!
//! Дополнительно добавляем rpath на типичные brew-директории, чтобы dlopen
//! channel-модулей (tll-lua) по короткому имени находил их в /opt/homebrew/lib
//! и /usr/local/lib.

fn main() {
    if std::env::var_os("CARGO_FEATURE_TLL").is_some() {
        libdir("tll", "MUTATOR_TLL_LIBDIR");
    }

    // Homebrew на Apple Silicon и Intel, чтобы bare name "tll-lua" и т.п.
    // имели шанс найтись через @rpath при dlopen внутри libtll.
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/opt/homebrew/lib");
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/local/lib");
    }
}

/// Каталог библиотеки `name` из pkg-config — в переменную `env` для загрузчика.
/// Библиотеки нет на машине сборки — не ошибка: найдётся при запуске.
fn libdir(name: &str, env: &str) {
    let found = pkg_config::Config::new()
        .cargo_metadata(false)
        .env_metadata(false)
        .probe(name);
    if let Ok(lib) = found {
        if let Some(dir) = lib.link_paths.first() {
            println!("cargo:rustc-env={env}={}", dir.display());
        }
    }
}
