//! Цвета — общие для окна, TUI в окне и терминала. Тёмная тема.
//!
//! Наружу: [`BG`], [`PANEL`], [`TEXT`], [`DIM`], [`ACCENT`], [`NAME`], [`OK`],
//! [`ERROR`], [`SELECT`], [`SELECT_DIM`], [`STRIPE`], [`HILITE`], [`EDITED`].

use egui::Color32;

pub const BG: Color32 = Color32::from_rgb(0x16, 0x18, 0x1c);
pub const PANEL: Color32 = Color32::from_rgb(0x1f, 0x22, 0x28);
pub const TEXT: Color32 = Color32::from_rgb(0xd8, 0xdb, 0xe0);
pub const DIM: Color32 = Color32::from_rgb(0x80, 0x86, 0x90);
pub const ACCENT: Color32 = Color32::from_rgb(0x6c, 0xb6, 0xff);
pub const NAME: Color32 = Color32::from_rgb(0xe5, 0xc0, 0x7b);
pub const OK: Color32 = Color32::from_rgb(0x98, 0xc3, 0x79);
pub const ERROR: Color32 = Color32::from_rgb(0xe0, 0x6c, 0x75);
/// Подложка выбранной строки и поля.
pub const SELECT: Color32 = Color32::from_rgb(0x2c, 0x3e, 0x5a);
/// Подложка выбранной строки, когда фокус в другой панели.
pub const SELECT_DIM: Color32 = Color32::from_rgb(0x2a, 0x2e, 0x36);
/// Чётные строки таблицы.
pub const STRIPE: Color32 = Color32::from_rgb(0x1b, 0x1d, 0x22);
/// Байты выбранного поля в hex.
pub const HILITE: Color32 = Color32::from_rgb(0x4a, 0x3b, 0x1a);
/// Правленое значение (наложено правкой, в файле ещё старое).
pub const EDITED: Color32 = Color32::from_rgb(0xff, 0x5f, 0x5f);
