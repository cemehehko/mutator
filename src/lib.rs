//! mutator: просмотр и правка датников TLL.
//!
//! Модули: [`tll`] — libtll, [`scheme`] — модель схемы, [`decode`] — разбор тела,
//! [`lua`] — фильтр как у tll-read, [`range`] — границы `-s`, [`dump`] — печать YAML,
//! [`data`] — окно строк, индекс, фильтр и поиск в фоне, [`session`] — сессии по имени файла, [`recent`] — последние файлы,
//! [`edit`] — правки полей через tll-patch и их фиксация,
//! [`lua_highlight`] — подсветка синтаксиса Lua для редактора transform-кода,
//! [`markup`] — разметка ответа модели, общая для окна и TUI,
//! [`agent`] — диалог с моделью: чтение датника инструментами и сборка Lua-фильтра.

pub mod agent;
pub mod columns;
pub mod data;
pub mod decode;
pub mod desk;
pub mod dump;
pub mod edit;
pub mod lua;
pub mod lua_highlight;
pub mod markup;
pub mod range;
pub mod recent;
pub mod scheme;
pub mod session;
pub mod tll;
pub mod ui;
