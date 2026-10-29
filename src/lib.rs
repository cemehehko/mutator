//! mutator: просмотр и правка датников TLL.
//!
//! Модули: [`app`] — запуск (`main`), [`io`] — фасад чтения и записи датников и
//! реестр бэкендов, `tll` — встроенный бэкенд libtll (фича `tll`), [`scheme`] — модель схемы, [`decode`] — разбор тела,
//! [`lua`] — фильтр как у tll-read, [`range`] — границы `-s`, [`dump`] — печать YAML,
//! [`data`] — окно строк, индекс, фильтр и поиск в фоне, [`session`] — сессии по имени файла, [`recent`] — последние файлы,
//! [`spec`] — спека открытия: склейка `+` и ротация `-N`,
//! [`edit`] — правки полей и состава потока, запись по схеме и фиксация,
//! [`lua_highlight`] — подсветка синтаксиса Lua для редактора transform-кода,
//! [`markup`] — разметка ответа модели, общая для окна и TUI,
//! [`browse`] — список линков resolve-сервера для выбора вместо файла,
//! [`send`] — форма отправки сообщения в линк,
//! [`agent`] — диалог с моделью: чтение датника инструментами и сборка Lua-фильтра,
//! `serve` — сервер для тестировщиков: инструменты агента по REST и SSE (фича `rest`).

pub mod agent;
pub mod app;
pub mod browse;
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
pub mod send;
#[cfg(feature = "rest")]
pub mod serve;
pub mod session;
pub mod spec;
#[cfg(feature = "tll")]
pub mod tll;
pub mod ui;
