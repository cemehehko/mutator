//! Окно без экрана: кадры egui через `Context::run_ui` над эталонным файлом.
//!
//! Сборка без GUI (--no-default-features) не тянет eframe.
//! Поэтому весь файл подключается только при фиче "gui".

#![cfg(feature = "gui")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui;
use tui_patch::data::File;
use tui_patch::desk::Desk;
use tui_patch::tll;
use tui_patch::ui::{App, Look};

/// Корень крейта во время запуска: сборка могла идти в другом каталоге.
fn root() -> std::path::PathBuf {
    std::env::var_os("CARGO_MANIFEST_DIR")
        .map(Into::into)
        .unwrap_or_else(|| env!("CARGO_MANIFEST_DIR").into())
}

struct Win {
    ctx: egui::Context,
    app: App,
    t: f64,
}

impl Win {
    fn open() -> Win {
        tll::quiet();
        let path = root().join("tests/data/lz4.dat");
        let mut desk = Desk::open(File::open(&path).unwrap(), Arc::new(|| {}));
        let t = Instant::now();
        while desk.index.is_none() || !desk.idle() {
            desk.wait(Duration::from_millis(50));
            assert!(t.elapsed() < Duration::from_secs(20));
        }
        let (cmd, events) = {
            let (ct, _cr) = tokio::sync::mpsc::unbounded_channel();
            let (_et, er) = std::sync::mpsc::channel();
            (ct, er)
        };
        let mut w = Win {
            ctx: egui::Context::default(),
            app: App::new(desk, cmd, events),
            t: 0.0,
        };
        w.frame(vec![]);
        w.frame(vec![]);
        w
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(1200.0, 800.0),
            )),
            time: Some(self.t),
            events,
            ..Default::default()
        };
        self.t += 1.0 / 60.0;
        self.app.filter_input(&self.ctx, &mut input);
        let app = &mut self.app;
        self.ctx.run_ui(input, |ui| app.draw(ui))
    }

    fn settle(&mut self) {
        let t = Instant::now();
        loop {
            self.frame(vec![]);
            if self.app.desk.idle() {
                self.frame(vec![]);
                return;
            }
            self.app.desk.wait(Duration::from_millis(20));
            assert!(t.elapsed() < Duration::from_secs(20));
        }
    }

    fn key(&mut self, key: egui::Key) {
        let ev = |pressed| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        self.frame(vec![ev(true), ev(false)]);
        self.settle();
    }

    fn text(&mut self, s: &str) {
        self.frame(vec![egui::Event::Text(s.into())]);
        self.settle();
    }

    fn click(&mut self, at: egui::Pos2, double: bool) {
        self.frame(vec![egui::Event::PointerMoved(at)]);
        for _ in 0..if double { 2 } else { 1 } {
            for pressed in [true, false] {
                self.frame(vec![egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }]);
            }
        }
        self.settle();
    }

    /// Сколько сейчас занимает панель детали (её запомненный прямоугольник).
    fn panel_height(&self) -> f32 {
        self.ctx
            .data_mut(|d| {
                d.get_persisted::<egui::PanelState>(egui::Id::new("detail"))
                    .map(|s| s.outer_rect.height())
            })
            .unwrap_or(0.0)
    }

    /// Верх панели детали — по нему тянется граница.
    fn panel_top(&self) -> f32 {
        self.ctx
            .data_mut(|d| {
                d.get_persisted::<egui::PanelState>(egui::Id::new("detail"))
                    .map(|s| s.outer_rect.min.y)
            })
            .unwrap_or(0.0)
    }

    /// Высота под панелью детали: панель идёт от доли именно её.
    fn detail_room(&self) -> f32 {
        self.panel_height() / self.app.desk.detail_ratio
    }

    /// Тексты кадра и центры мелких фигур — галочек и радиокнопок.
    fn shapes(out: &egui::FullOutput) -> (Vec<Text>, Vec<egui::Pos2>) {
        let (mut texts, mut marks) = (Vec::new(), Vec::new());
        for cs in &out.shapes {
            collect(&cs.shape, &mut texts, &mut marks);
        }
        (texts, marks)
    }

    /// Кадр с открытой деталью и курсором на поле `path`.
    fn at_detail_field(&mut self, path: &str) {
        if !self.app.desk.detail_open {
            self.key(egui::Key::Enter);
        }
        assert!(self.app.desk.detail_open, "деталь");
        let det = self.app.desk.detail.as_mut().unwrap();
        det.cursor = det
            .lines()
            .iter()
            .position(|(_, n)| n.path == path)
            .unwrap_or_else(|| panic!("нет поля {path}"));
        self.app.desk.focus = tui_patch::desk::Focus::Detail;
    }

    fn idle(&mut self) {
        for _ in 0..4 {
            self.frame(vec![]);
        }
    }
}

/// Текст кадра: содержимое, где нарисован и каков размер.
struct Text {
    text: String,
    pos: egui::Pos2,
    size: egui::Vec2,
}

fn collect(shape: &egui::Shape, texts: &mut Vec<Text>, marks: &mut Vec<egui::Pos2>) {
    match shape {
        egui::Shape::Text(t) => texts.push(Text {
            text: t.galley.text().to_string(),
            pos: t.pos,
            size: t.galley.size(),
        }),
        egui::Shape::Circle(c) => marks.push(c.center),
        egui::Shape::Rect(r) if r.rect.width() < 30.0 && r.rect.height() < 30.0 => {
            marks.push(r.rect.center())
        }
        egui::Shape::Vec(v) => {
            for s in v {
                collect(s, texts, marks);
            }
        }
        _ => {}
    }
}

/// Таблица окна знает свою высоту, клавиши двигают выбор, двойной щелчок по
/// строке открывает деталь, F2 — тот же экран сеткой.
#[test]
fn window_table_keys_clicks_and_tui_look() {
    let mut w = Win::open();
    let h = w.app.desk.height;
    assert!(h > 20, "высота таблицы по окну: {h}");
    w.key(egui::Key::ArrowDown);
    w.key(egui::Key::ArrowDown);
    assert_eq!(w.app.desk.sel, 2);
    w.key(egui::Key::End);
    assert_eq!(w.app.desk.selected().unwrap().seq, 1599);
    assert!(
        w.app.desk.sel < w.app.desk.top + w.app.desk.height,
        "выбор на экране"
    );
    w.key(egui::Key::Home);

    // Строка 5 таблицы: ниже двух строк панели и шапки таблицы.
    let row_y = (100..300).map(|y| y as f32).find(|&y| {
        w.click(egui::pos2(300.0, y), false);
        w.app.desk.sel == 5
    });
    assert!(row_y.is_some(), "щелчок выбирает строку");
    w.click(egui::pos2(300.0, row_y.unwrap()), true);
    assert!(w.app.desk.detail_open, "двойной щелчок — деталь");
    w.key(egui::Key::Tab);
    w.key(egui::Key::ArrowDown);
    assert_eq!(w.app.desk.detail.as_ref().unwrap().cursor, 1);

    w.key(egui::Key::F2);
    assert_eq!(w.app.look, Look::Tui);
    let s = w.app.snapshot(&w.ctx);
    assert_eq!(s.look.as_deref(), Some("tui"), "вид — в сессию");
    assert!(
        s.detail_ratio.is_some_and(|r| r > 0.0 && r < 1.0),
        "доля детали — в сессию"
    );
    assert!(s.detail_open);
    w.key(egui::Key::Escape);
    assert!(!w.app.desk.detail_open);
    w.text("t");
    assert!(
        w.app.desk.prompt.is_some(),
        "в TUI-виде клавиши идут в Desk"
    );
    w.key(egui::Key::Escape);
    w.key(egui::Key::F2);
    assert_eq!(w.app.look, Look::Gui);
}

/// `f` ставит фокус в поле фильтра окна, Enter применяет выражение.
#[test]
fn window_filter_field() {
    if !tll::Context::new().ensure_lua() {
        eprintln!("нет tll-lua — пропуск");
        return;
    }
    let mut w = Win::open();
    w.text("f");
    w.text("name == 'Ping'");
    w.key(egui::Key::Enter);
    let m = w.app.desk.matches.clone().expect("фильтр применён");
    while !m.is_done() {
        w.settle();
    }
    w.settle();
    assert_eq!(m.len(), 300);
    assert_eq!(w.app.desk.selected().unwrap().name, "Ping");
    // Выбор типов — окно поверх таблицы.
    w.text("t");
    assert!(w.app.desk.prompt.is_some());
}

/// `agr:detail-share` — высота детали в окне — доля высоты окна: `{`/`}`
/// меняют её сразу, пустая деталь («нет выбранной строки») и сброс детали
/// (как после правки поля) панель не схлопывают.
#[test]
fn detail_height_follows_ratio() {
    let mut w = Win::open();
    w.key(egui::Key::Enter);
    let room = w.detail_room();
    assert!(room > 100.0, "под деталь есть место: {room}");
    assert!(
        (w.panel_height() / room - w.app.desk.detail_ratio).abs() < 0.02,
        "панель по доле: {} при доле {}",
        w.panel_height(),
        w.app.desk.detail_ratio
    );

    // Пустая деталь — высота та же, а не строка «нет выбранной строки».
    w.app.desk.detail = None;
    w.idle();
    assert!(
        (w.panel_height() / room - w.app.desk.detail_ratio).abs() < 0.02,
        "пустая деталь не схлопывает панель: {}",
        w.panel_height()
    );

    // `{` и `}` — доля вниз и вверх, панель следом.
    w.app.desk.detail_ratio = 0.5;
    w.idle();
    let half = w.panel_height();
    w.text("{");
    assert_eq!(w.app.desk.detail_ratio, 0.45);
    assert!(
        w.panel_height() < half,
        "«{{» ниже: {} против {half}",
        w.panel_height()
    );
    w.text("}");
    w.text("}");
    assert_eq!(w.app.desk.detail_ratio, 0.55);
    assert!(
        w.panel_height() > half,
        "«}}» выше: {} против {half}",
        w.panel_height()
    );
}

/// `agr:detail-share` — потянули границу детали — доля берётся из неё, и
/// дальше высота держится по доле.
#[test]
fn detail_edge_drag_sets_ratio() {
    let mut w = Win::open();
    w.key(egui::Key::Enter);
    w.app.desk.detail_ratio = 0.3;
    w.idle();
    let room = w.detail_room();
    let top = w.panel_top();
    w.frame(vec![egui::Event::PointerMoved(egui::pos2(600.0, top))]);
    w.frame(vec![egui::Event::PointerButton {
        pos: egui::pos2(600.0, top),
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    }]);
    let mut seen = None;
    for dy in [10.0, 30.0, 60.0] {
        w.frame(vec![egui::Event::PointerMoved(egui::pos2(600.0, top - dy))]);
        seen = Some(w.app.desk.detail_ratio);
    }
    w.frame(vec![egui::Event::PointerButton {
        pos: egui::pos2(600.0, top - 60.0),
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    }]);
    w.idle();
    let ratio = w.app.desk.detail_ratio;
    assert!(ratio > 0.3, "доля выросла за пальцем: {ratio}");
    assert_eq!(seen, Some(ratio), "доля уже при перетаскивании");
    assert!(
        (w.panel_height() / room - ratio).abs() < 0.02,
        "панель по новой доле: {} при {ratio}",
        w.panel_height()
    );
}

/// Галочка (Bits) и радиокнопка (Enum) — на одной строке со своей подписью:
/// значок слева, подпись сразу за ним по той же высоте.
#[test]
fn picker_mark_and_label_share_a_row() {
    for (path, label) in [("flags", "fok (0x2)"), ("side", "Buy")] {
        let mut w = Win::open();
        w.at_detail_field(path);
        w.app.desk.edit_field();
        let mut out = w.frame(vec![]);
        for _ in 0..3 {
            out = w.frame(vec![]);
        }
        let (texts, marks) = Win::shapes(&out);
        // Подпись пункта — в окне правки (левая колонка поверх таблицы), а не
        // в ячейке таблицы с тем же текстом.
        let t = texts
            .iter()
            .filter(|t| t.text.starts_with(label) && t.pos.x < 500.0 && t.pos.y > 300.0)
            .find(|t| {
                marks
                    .iter()
                    .any(|m| m.x < t.pos.x && m.y >= t.pos.y - 2.0 && m.y <= t.pos.y + t.size.y)
            })
            .unwrap_or_else(|| panic!("нет подписи {label:?} со значком"));
        // Значок — ближайшая слева фигура в полосе подписи.
        let mark = marks
            .iter()
            .filter(|m| m.x < t.pos.x && m.y >= t.pos.y - 2.0 && m.y <= t.pos.y + t.size.y)
            .max_by(|a, b| a.x.partial_cmp(&b.x).unwrap())
            .unwrap();
        assert!(
            t.pos.x - mark.x < t.size.y + 8.0,
            "{label}: подпись сразу за значком, а не строкой ниже (зазор {})",
            t.pos.x - mark.x
        );
    }
}

/// Щелчок по подписи пункта — выбор, как по значку (Bits — переключение,
/// Enum — один из).
#[test]
fn picker_click_on_label_chooses() {
    let mut w = Win::open();
    w.at_detail_field("flags");
    w.app.desk.edit_field();
    w.idle();
    let p = w.app.desk.prompt.as_ref().unwrap();
    assert_eq!(p.items.iter().filter(|i| i.on).count(), 1, "fok включён");
    assert_eq!(p.text, "0x2");
    // Подпись «lvl (0x70)»: значок около x=280, подпись — сразу за ним.
    w.click(egui::pos2(320.0, 424.0), false);
    let p = w.app.desk.prompt.as_ref().unwrap();
    assert!(
        p.items.iter().any(|i| i.label == "lvl" && i.on),
        "щелчок по подписи включил флаг"
    );
    assert!(p.text.contains('7'), "hex пересчитан: {}", p.text);
}

/// `agr:agent-one-turn` — F3 открывает экран агента, лента рисует ответ
/// модели (общая разметка `agr:markup-shared`), подтверждение идёт командой
/// воркеру. Сети нет: воркер жив, но сообщения кладутся в состояние напрямую.
#[test]
fn agent_screen_lists_and_confirms() {
    let mut w = Win::open();
    w.key(egui::Key::F3);
    assert_eq!(w.app.look, Look::Agent, "F3 — экран агента");

    // Воркер при старте шлёт список диалогов и настройки.
    w.settle();
    assert!(
        !w.app.agent.settings.model.is_empty(),
        "настройки пришли в состояние"
    );

    // Лента с разметкой: заголовок, абзац и код из ответа модели.
    w.app.agent.messages = vec![tui_patch::agent::MsgView {
        role: "assistant".into(),
        text: "## Итог\n\nнашёл **три** строки:\n\n```lua\nseq > 5\n```".into(),
        index: 0,
        name: String::new(),
        is_error: false,
    }];
    let out = w.frame(vec![]);
    let (texts, _) = Win::shapes(&out);
    assert!(
        texts.iter().any(|t| t.text == "Итог"),
        "заголовок ответа нарисован"
    );
    assert!(
        texts.iter().any(|t| t.text.contains("три")),
        "жирный спан ответа нарисован"
    );
    assert!(
        texts.iter().any(|t| t.text.contains("seq > 5")),
        "блок кода ответа нарисован"
    );

    // Запрос подтверждения пачки — модальное окно; Confirm уходит воркеру.
    w.app.agent.confirm = Some(tui_patch::ui::agent::Confirm {
        tool: "edit_messages".into(),
        summary: tui_patch::edit::Summary {
            lines: vec![
                "5 Order header.order_id: 1000 → 777".into(),
                "6 Order header.order_id: 1000 → 778".into(),
            ],
            totals: vec![
                ("сообщений".into(), "2".into()),
                ("полей".into(), "2".into()),
            ],
        },
    });
    let _ = w.frame(vec![]);
    let out = w.frame(vec![]);
    let (texts, _) = Win::shapes(&out);
    let all: Vec<&str> = texts.iter().map(|t| t.text.as_str()).collect();
    assert!(
        texts.iter().any(|t| t.text == "Подтвердить правки"),
        "окно подтверждения открыто; тексты: {all:?}"
    );
    assert!(
        texts.iter().any(|t| t.text.contains("сообщений: 2")),
        "количество сообщений видно; тексты: {all:?}"
    );
    assert!(
        texts.iter().any(|t| t.text.contains("edit_messages")),
        "имя инструмента видно"
    );

    w.key(egui::Key::F3);
    assert_eq!(w.app.look, Look::Gui, "F3 — назад к таблице");
}

/// Экран Lua-редактора (F4) рисует сам код: текст полей ввода виден, а не
/// вытеснен нулевой шириной панелью номеров строк (`agr:gui-lua-text`).
#[test]
fn lua_editor_shows_code_in_gui() {
    let mut w = Win::open();
    w.app.look = Look::LuaEditor;
    w.app.desk.lua_code = tui_patch::lua::transform_template();
    for _ in 0..3 {
        w.frame(vec![]);
    }
    let out = w.frame(vec![]);
    let (texts, _) = Win::shapes(&out);
    let all: String = texts.iter().map(|t| t.text.as_str()).collect();
    assert!(
        all.contains("function tll_on_data"),
        "код трансформа нарисован; тексты: {:?}",
        texts.iter().map(|t| &t.text).collect::<Vec<_>>()
    );
    assert!(
        all.contains("tll_callback(seq, name, data)"),
        "тело шаблона нарисовано"
    );
}
