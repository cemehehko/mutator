//! Линковка с libtll: путь и флаги — от pkg-config, rpath — чтобы бинарник
//! находил библиотеку без `DYLD_LIBRARY_PATH`.
//!
//! Дополнительно добавляем rpath на типичные brew-директории, чтобы dlopen
//! channel-модулей (tll-patch, tll-lua) по короткому имени находил их
//! в /opt/homebrew/lib и /usr/local/lib.

fn main() {
    let lib = pkg_config::Config::new()
        .atleast_version("0.4")
        .probe("tll")
        .expect("libtll не найдена через pkg-config (нужен пакет tll)");
    for dir in &lib.link_paths {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", dir.display());
    }

    // Homebrew на Apple Silicon и Intel, чтобы bare name "tll-patch" и т.п.
    // имели шанс найтись через @rpath при dlopen внутри libtll.
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/opt/homebrew/lib");
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/local/lib");
    }
}
