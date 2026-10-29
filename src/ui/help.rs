//! Справка F1: все возможности и горячие клавиши одним списком — модалка
//! поверх любого вида GUI и рамка на весь экран в TUI. Содержимое — статичные
//! [`sections`]; клавиши в строках помечены обратными кавычками: GUI красит их
//! [`ACCENT`], TUI отбрасывает.
//!
//! Наружу: [`show`], [`sections`] из [`Section`], [`flat`], [`layout`],
//! [`clamp_scroll`].

use egui;
use egui::text::{LayoutJob, TextFormat};

use super::theme::*;
use crate::desk::Desk;

/// Раздел справки: заголовок и строки.
pub(crate) struct Section {
    pub title: &'static str,
    pub lines: &'static [&'static str],
}

/// Разделы справки — один источник для GUI и TUI.
pub(crate) fn sections() -> &'static [Section] {
    &[
        Section {
            title: "Что это",
            lines: &[
                "Просмотр и правка датников TLL. В памяти — окно строк вокруг экрана,",
                "дальнее дочитывается у края; файл не меняется, пока правки не зафиксированы.",
                "Красным — правленое вручную и изменённое Lua-мутатором.",
            ],
        },
        Section {
            title: "Виды",
            lines: &[
                "`F2` — GUI ↔ TUI (тот же файл, те же клавиши)",
                "`F3` — экран агента   `F4` — редактор Lua-transform",
                "`F1`/`Esc` — эта справка",
            ],
        },
        Section {
            title: "Навигация",
            lines: &[
                "`↑`/`↓` или `k`/`j` — по сообщениям   `PgUp`/`PgDn` — страница",
                "`Home`/`End` — начало/конец   колесо — прокрутка без смены выбора",
                "щелчок — выбрать, двойной — открыть деталь; полоса справа — переход по доле",
                "`g` — переход: `SEQ` по seq, `+N` — N-я строка, `-N` — от конца",
                "`r` — перечитать растущий файл; `↓`/`PgDn` на последней строке — то же",
            ],
        },
        Section {
            title: "Линк",
            lines: &[
                "Вместо файла — канал: `mutator resolve://SERVICE/CHANNEL`, `stream+…`, `lua+…`",
                "Только просмотр; новое дописывается в хвост само, курсор стоит на месте",
                "Состояние (Opening, Active, Error…) — в заголовке справа, всегда",
                "`o` — параметры открытия: `mode=online`, `initial`, `seq;seq=N`, `block;block=N`",
                "`r` — переподключить: с историей — с последнего полученного seq",
                "Спул — временный датник, при выходе удаляется; `--keep DIR` — оставить",
            ],
        },
        Section {
            title: "Таблица и колонки",
            lines: &[
                "`c` — список колонок (пробел — вкл/выкл, есть поиск по тексту)",
                "`a` — колонка из поля под курсором (в детали)",
                "`p` — пресет колонок (свои — `~/.config/mutator/presets.toml`)",
                "`←`/`→` — сдвиг колонок   `x` — hex   `i` — типы полей",
                "`{`/`}` — высота панели детали",
            ],
        },
        Section {
            title: "Деталь и правка",
            lines: &[
                "`Enter` — деталь выбранного   `Tab` — фокус таблица ↔ деталь",
                "`e`/`Enter` — править поле; enum — имя, bits — `a|b`, время — строкой",
                "`u` — снять правку поля   `M` — массовая правка поля по всему файлу",
                "Правка проверяется записью по схеме; принятое — красным,",
                "в подсказке поля — «в файле: …». Файл меняется только при фиксации.",
            ],
        },
        Section {
            title: "Состав потока",
            lines: &[
                "`d`/`Del` — удалить строку; `v` — метка, `d` — диапазон от метки:",
                "    со сдвигом (seq дальше −1) или дырой (seq прежние)",
                "`o`/`Ins` — вставить перед строкой её копию   `T` — сменить тип;",
                "    `v` и `T` — тип у диапазона (в фильтре — у найденного) одним телом",
                "`u` в таблице — вернуть удалённое, снять тип, убрать вставку",
                "seq — как в файле; `×5` — удалено, `5→4` и `+5` — seq после записи.",
            ],
        },
        Section {
            title: "Фильтр и поиск",
            lines: &[
                "`f` — фильтр: Lua-выражение, как у `tll-read -f` (нет поля — `nil`):",
                "    name == 'NewOrder' and data.header.status == 'OK'",
                "    `data.qty > 100`   `seq % 2 == 0`",
                "    по списку значений — таблица-множество, без цепочки `or`:",
                "    ({[111]=true, [223232]=true, [34444]=true})[data.person_id]",
                "    кроме списка: not ({[111]=true, [223232]=true, [34444]=true})[data.person_id]",
                "`Shift+F` — снять фильтр   `t` — отбор по типам сообщений",
                "`/` — поиск вперёд, `?` — назад, `n`/`N` — повтор; `Esc` — остановить",
                "`↑`/`↓` в строке ввода — история; найденное растёт по мере скана",
            ],
        },
        Section {
            title: "Lua transform",
            lines: &[
                "`E` — включить/выключить, `F4` — редактор («Применить», «Сбросить»).",
                "Вид показывает тела после мутатора — как уйдут в файл на фиксации;",
                "отличия от файла красные, в подсказке поля — значение из файла.",
                "Сообщение без `tll_callback` выбрасывается — такой seq помечен целиком.",
                "Пример:",
                "    function tll_on_data(seq, name, data)",
                "        tll_callback(seq, name, { u8 = 1 })",
                "    end",
                "Фильтр и поиск работают по сырому потоку, фиксация — полный конвейер.",
            ],
        },
        Section {
            title: "Фиксация",
            lines: &[
                "`w` — диалог: сводка правок и они же yaml-командами tll-patch; первая",
                "строка — сменится ли последний seq (по правкам, без transform).",
                "Режимы: новый файл (x.patched.dat, при занятости -2) или перезапись",
                "с бэкапом (x.orig.dat). Растущий `*.current.dat` перезаписывать нельзя.",
                "Порядок применения: мутатор, затем правки; после записи файл открывается.",
            ],
        },
        Section {
            title: "Агент",
            lines: &[
                "`F3` — диалог с моделью; адрес и ключ — в настройках, хранятся в",
                "`~/.config/mutator/settings.json`.",
                "Агент читает файл инструментами и готовит правки пачкой: одно окно",
                "подтверждения (`y`/`Enter` — принять, `Esc`/`n` — отказ), файл не пишет —",
                "после пачки зафиксируйте `w`. `Ctrl+A` — прервать.",
            ],
        },
        Section {
            title: "Сессия",
            lines: &[
                "Между запусками помнится: фильтр, типы, поиск с историей, колонки,",
                "Lua-код с вкл/выкл, правки полей, выбор, размеры окна и детали.",
                "`--fresh` — открыть, не возвращая сессию.",
            ],
        },
        Section {
            title: "Особенности",
            lines: &[
                "Растущий файл — снимок на момент открытия; `r` — перечитать.",
                "Без tll-lua фильтр и поиск недоступны, просмотр и правки работают.",
                "Поля, скрытые картой присутствия, в дереве не показываются;",
                "сломанное сообщение — ошибка у своего поля, соседние видны.",
                "Строка запуска: `--tui`, `-s НАЧАЛО:КОНЕЦ`, `-f ВЫРАЖЕНИЕ`,",
                "`--message ИМЯ`, `--dump`, `--fresh`. `q` — выход.",
            ],
        },
    ]
}

/// Строки для TUI: (текст, это заголовок раздела). Кавычки клавиш отброшены.
pub(crate) fn flat() -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for s in sections() {
        out.push((s.title.to_string(), true));
        for l in s.lines {
            out.push((l.replace('`', ""), false));
        }
        out.push((String::new(), false));
    }
    out
}

/// Размер бокса справки в TUI: (колонка, строка, ширина, высота, строк тела).
pub(crate) fn layout(cols: usize, rows: usize) -> (usize, usize, usize, usize, usize) {
    let (x, y) = (1.min(cols), 1.min(rows));
    let w = cols.saturating_sub(2).clamp(20, 110);
    let h = rows.saturating_sub(2).max(8);
    // Заголовок и футер — тело листается.
    (x, y, w, h, h.saturating_sub(4))
}

/// Модалка справки поверх текущего вида.
pub(super) fn show(ui: &mut egui::Ui, d: &mut Desk) {
    if !d.help.open {
        return;
    }
    let h = &mut d.help;
    let mut close = false;
    egui::Modal::new(egui::Id::new("help")).show(ui.ctx(), |ui| {
        ui.set_width((ui.ctx().content_rect().width() * 0.8).min(1000.0));
        ui.heading("Справка");
        ui.separator();
        let row = 18.0;
        let height = (ui.ctx().content_rect().height() * 0.75).max(200.0);
        let mut area = egui::ScrollArea::vertical()
            .id_salt("help")
            .auto_shrink([false, true])
            .max_height(height);
        // Позиция живёт в [`Desk::help`] — она переживает смену вида и её
        // видит TUI. Область прокрутки сеется из неё один раз, дальше
        // клавиатура ведёт область через флаг, колесо скроллит само.
        if h.keyboard || !h.seeded {
            area = area.vertical_scroll_offset(h.scroll as f32 * row);
            h.keyboard = false;
            h.seeded = true;
        }
        area.show(ui, |ui| {
            for s in sections() {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(s.title).strong().color(NAME));
                for l in s.lines {
                    ui.label(key_job(l));
                }
            }
            ui.add_space(6.0);
        });
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("F1/Esc — закрыть").color(DIM));
            if ui.button("Закрыть").clicked() {
                close = true;
            }
        });
    });
    if close {
        d.help.open = false;
    }
}

/// Строка с клавишами в кавычках: они — [`ACCENT`], остальное — [`TEXT`].
fn key_job(l: &str) -> LayoutJob {
    let mut job = LayoutJob::default();
    let font = egui::FontId::monospace(13.0);
    let mut key = false;
    for part in l.split('`') {
        if !part.is_empty() {
            let fg = if key { ACCENT } else { TEXT };
            job.append(part, 0.0, TextFormat::simple(font.clone(), fg));
        }
        key = !key;
    }
    job
}

/// Скролл справки не уходит ниже последней строки TUI-экрана.
pub(crate) fn clamp_scroll(d: &mut Desk, cols: usize, rows: usize) {
    let h = &mut d.help;
    let (_, _, _, _, body) = layout(cols, rows);
    let total = flat().len();
    h.scroll = h.scroll.min(total.saturating_sub(body));
    h.keyboard = false;
}
