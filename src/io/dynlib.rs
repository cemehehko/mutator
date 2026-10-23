//! Библиотеки бэкендов — динамической загрузкой (`agr:backend-dlopen`): один
//! бинарник со всеми бэкендами запускается без любой из библиотек, а нет
//! нужной — понятная ошибка при открытии файла её формата.
//!
//! [`dynamic_api!`](crate::dynamic_api) из списка сигнатур C делает таблицу указателей на функции,
//! загружаемую один раз на процесс, и обёртки с теми же именами и сигнатурами —
//! код бэкенда зовёт их, как звал бы слинкованные. Обёртка до успешной загрузки
//! паникует: бэкенд сначала проверяет `api()` (в `Context::try_new`).
//!
//! [`open`] ищет библиотеку: путь из переменной окружения, каталог из
//! pkg-config на сборке, типичные каталоги Homebrew, затем системный поиск.
//! Найденный путь канонизируется: модули бэкенда (Lua, правки) ссылаются на
//! библиотеку по своему пути, и вторая копия той же библиотеки в процессе
//! раздвоила бы её глобальное состояние (журнал, реестр модулей).
//!
//! Наружу: [`open`], [`dynamic_api!`](crate::dynamic_api), `libloading` (для
//! макроса во внешних бэкендах).

use std::path::PathBuf;

/// Для [`dynamic_api!`](crate::dynamic_api) во внешних бэкендах: тот же загрузчик.
pub use libloading;

/// Открыть библиотеку `names` (по порядку предпочтения: `libtll.so.0`,
/// `libtll.so`…). `env` — переменная с полным путём, `libdir` — каталог из
/// pkg-config на сборке.
pub fn open(
    env: &str,
    libdir: Option<&str>,
    names: &[&str],
) -> Result<libloading::Library, String> {
    let mut tried = Vec::new();
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os(env) {
        paths.push(PathBuf::from(p));
    }
    let dirs = libdir
        .into_iter()
        .chain(["/opt/homebrew/lib", "/usr/local/lib"]);
    for d in dirs {
        for n in names {
            paths.push(PathBuf::from(d).join(n));
        }
    }
    for p in paths {
        if !p.exists() {
            tried.push(p.display().to_string());
            continue;
        }
        let p = p.canonicalize().unwrap_or(p);
        match unsafe { load(p.as_os_str()) } {
            Ok(l) => return Ok(l),
            Err(e) => tried.push(format!("{}: {e}", p.display())),
        }
    }
    // Системный поиск по имени: LD_LIBRARY_PATH, ld.so.cache, DYLD_*.
    for n in names {
        match unsafe { load(std::ffi::OsStr::new(n)) } {
            Ok(l) => return Ok(l),
            Err(e) => tried.push(format!("{n}: {e}")),
        }
    }
    Err(format!(
        "не найдена {} (задайте путь в {env}); искал: {}",
        names[0],
        tried.join("; ")
    ))
}

#[cfg(unix)]
unsafe fn load(p: &std::ffi::OsStr) -> Result<libloading::Library, libloading::Error> {
    use libloading::os::unix::{Library, RTLD_GLOBAL, RTLD_NOW};
    // RTLD_GLOBAL: модули бэкенда, которые грузит сама библиотека, видят её
    // символы так же, как при обычной линковке.
    Library::open(Some(p), RTLD_NOW | RTLD_GLOBAL).map(Into::into)
}

#[cfg(not(unix))]
unsafe fn load(p: &std::ffi::OsStr) -> Result<libloading::Library, libloading::Error> {
    libloading::Library::new(p)
}

/// Таблица функций библиотеки и обёртки с именами функций C.
///
/// ```ignore
/// dynamic_api! {
///     loader = || crate::io::dynlib::open("MUTATOR_LIBTLL", None, &["libtll.so.0"]);
///     pub fn tll_channel_free(c: *mut tll_channel_t);
/// }
/// ```
#[macro_export]
macro_rules! dynamic_api {
    (
        loader = $loader:expr;
        $( $(#[$fm:meta])* pub fn $name:ident ( $( $arg:ident : $ty:ty ),* $(,)? ) $( -> $ret:ty )? ; )*
    ) => {
        /// Загруженная библиотека и её функции.
        #[allow(non_snake_case)]
        pub struct Api {
            _lib: $crate::io::dynlib::libloading::Library,
            $( $name: unsafe extern "C" fn( $( $ty ),* ) $( -> $ret )?, )*
        }

        static API: std::sync::OnceLock<Result<Api, String>> = std::sync::OnceLock::new();

        /// Таблица функций; ошибка — библиотеки нет или в ней нет нужного символа.
        pub fn api() -> Result<&'static Api, String> {
            API.get_or_init(|| unsafe { Api::load() }).as_ref().map_err(Clone::clone)
        }

        impl Api {
            unsafe fn load() -> Result<Api, String> {
                let lib: $crate::io::dynlib::libloading::Library = ($loader)()?;
                $(
                    let $name = *lib
                        .get::<unsafe extern "C" fn( $( $ty ),* ) $( -> $ret )?>(
                            concat!(stringify!($name), "\0").as_bytes(),
                        )
                        .map_err(|e| format!("в библиотеке нет {}: {e}", stringify!($name)))?;
                )*
                Ok(Api { _lib: lib, $( $name, )* })
            }
        }

        $(
            $(#[$fm])*
            #[allow(clippy::missing_safety_doc)]
            pub unsafe fn $name( $( $arg : $ty ),* ) $( -> $ret )? {
                (api().expect(concat!("библиотека не загружена: ", stringify!($name))).$name)( $( $arg ),* )
            }
        )*
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `agr:backend-dlopen` — нет библиотеки — ошибка с переменной для пути и
    /// списком мест поиска, а не падение.
    #[test]
    fn missing_library_is_an_error_with_search_list() {
        let e = open(
            "MUTATOR_LIBNOPE",
            Some("/nonexistent"),
            &["libnope-mutator.so"],
        )
        .err()
        .expect("такой библиотеки нет");
        assert!(e.contains("MUTATOR_LIBNOPE"), "{e}");
        assert!(e.contains("/nonexistent/libnope-mutator.so"), "{e}");
    }
}
