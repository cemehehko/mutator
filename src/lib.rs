//! mutator: просмотр и правка датников TLL.
//!
//! Модули: [`app`] — запуск (`main`), [`io`] — фасад чтения и записи датников и
//! реестр бэкендов, `tll` — встроенный бэкенд libtll (фича `tll`), [`scheme`] — модель схемы, [`decode`] — разбор тела,
//! [`lua`] — фильтр как у tll-read, [`range`] — границы `-s`, [`dump`] — печать YAML,
//! [`data`] — окно строк, индекс, фильтр и поиск в фоне, [`session`] — сессии по имени файла, [`recent`] — последние файлы,
//! [`spec`] — спека открытия: склейка `+` и ротация `-N`,
//! [`edit`] — правки полей через tll-patch и их фиксация,
//! [`lua_highlight`] — подсветка синтаксиса Lua для редактора transform-кода,
//! [`markup`] — разметка ответа модели, общая для окна и TUI,
//! [`agent`] — диалог с моделью: чтение датника инструментами и сборка Lua-фильтра.

pub mod agent;
pub mod app;
pub mod columns;
pub mod data;
pub mod decode;
pub mod desk;
pub mod dump;
pub mod edit;
pub mod io;
pub mod lua;
pub mod lua_highlight;
pub mod markup;
pub mod range;
pub mod recent;
pub mod scheme;
pub mod session;
pub mod spec;
#[cfg(feature = "tll")]
pub mod tll;
pub mod ui;
