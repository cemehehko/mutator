//! Временный измеритель раскладки экрана агента: где кончается лента и где
//! начинается поле ввода.
//!
//! Этот тест использует egui-контекст для имитации GUI-отрисовки.
//! При сборке без GUI (--no-default-features) файл не компилируется.

#![cfg(feature = "gui")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui;
use tui_patch::agent::MsgView;
use tui_patch::data::File;
use tui_patch::desk::Desk;
use tui_patch::tll;
use tui_patch::ui::{App, Look};

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

    fn key(&mut self, key: egui::Key) {
        let ev = |pressed| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        self.frame(vec![ev(true), ev(false)]);
        self.frame(vec![]);
    }
}

struct T {
    text: String,
    pos: egui::Pos2,
    size: egui::Vec2,
}

fn collect(shape: &egui::Shape, texts: &mut Vec<T>) {
    match shape {
        egui::Shape::Text(t) => texts.push(T {
            text: t.galley.text().to_string(),
            pos: t.pos,
            size: t.galley.size(),
        }),
        egui::Shape::Vec(v) => {
            for s in v {
                collect(s, texts);
            }
        }
        _ => {}
    }
}

#[test]
fn probe_agent_layout() {
    let mut w = Win::open();
    w.key(egui::Key::F3);
    assert_eq!(w.app.look, Look::Agent);

    // Длинная лента: много сообщений, чтобы лента прокручивалась.
    let mut msgs = Vec::new();
    for i in 0..40 {
        msgs.push(MsgView {
            role: "assistant".into(),
            text: format!("строка ответа номер {i} с некоторым текстом для высоты"),
            index: i,
            name: String::new(),
            is_error: false,
        });
    }
    w.app.agent.messages = msgs;
    for round in 0..3 {
        let mut texts = Vec::new();
        let out = w.frame(vec![]);
        for cs in &out.shapes {
            collect(&cs.shape, &mut texts);
        }
        let mut rows: Vec<(f32, f32, String)> = texts
            .iter()
            .map(|t| (t.pos.y, t.pos.y + t.size.y, t.text.clone()))
            .collect();
        rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let panel = w.ctx.data_mut(|d| {
            d.get_persisted::<egui::PanelState>(egui::Id::new("agent-input"))
                .map(|s| s.outer_rect)
        });
        println!("--- кадр {round} ---");
        let panel_top = panel.map(|r| r.min.y).unwrap_or(f32::MAX);
        let tape: Vec<_> = rows.iter().filter(|(_, _, t)| !t.is_empty()).collect();
        println!("   текстов всего {}", tape.len());
        for (y0, y1, text) in tape.iter().rev().take(3).rev() {
            let short: String = text.chars().take(40).collect();
            println!("   последние: y {y0:7.1}..{y1:7.1}  {short:?}");
        }
        println!("   панель ввода сверху: {panel_top:.1}");
    }
}
