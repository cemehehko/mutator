//! Состояние экрана агента: события воркера ([`crate::agent::Ev`])
//! складываются в то, что рисует вид. Окно и TUI держат одно и то же
//! [`State`] и опрашивают канал событий каждый кадр; рисует его окно —
//! `show`, ответ модели разбирается общим [`crate::markup`]
//! (`agr:markup-shared`). Действия уходят воркеру командами
//! ([`crate::agent::Cmd`]) — агент работает на том же `Desk`, что и человек
//! (`agr:agent-same-desk`).
//!
//! Файлы: `chat.rs` — лента (строки для TUI, виджеты для окна), `confirm.rs` —
//! окно подтверждения пачки правок, `settings.rs` — настройки
//! подключения, `dialogs.rs` — список диалогов, `progress.rs` — индикатор
//! фазы хода (`agr:agent-progress`).
//!
//! Наружу: [`State`], [`Confirm`], [`INPUT_ROWS`], [`INPUT_GROW`], `show`,
//! [`frame`], [`Act`], [`apply`], [`key`].

mod chat;
mod confirm;
mod dialogs;
mod progress;
mod settings;

use egui;

use super::theme::*;
use crate::agent::{Cmd, CmdTx, Ev, MsgView, Settings};
use crate::desk::Key;
use crate::ui::tui::grid::Grid;

/// Запрос подтверждения пачки правок: сводка и итоги.
#[derive(Clone, Debug)]
pub struct Confirm {
    pub tool: String,
    pub summary: crate::edit::Summary,
}

/// Снимок того, что вид знает об агенте.
#[derive(Clone, Debug, Default)]
pub struct State {
    /// Список диалогов и активный из них.
    pub dialogs: Vec<crate::agent::DialogView>,
    pub active: Option<String>,
    /// Лента открытого диалога.
    pub messages: Vec<MsgView>,
    pub msg_dialog: Option<String>,
    /// Настройки подключения.
    pub settings: Settings,
    /// Кусок ответа, который модель ещё стримит.
    pub streaming: String,
    pub streaming_dialog: Option<String>,
    /// Ход идёт: модель отвечает или ждёт подтверждения.
    pub busy: bool,
    /// Фаза хода для индикатора и когда она началась (`agr:agent-progress`).
    pub phase: Option<progress::Wait>,
    pub since: Option<std::time::Instant>,
    /// Когда пришёл последний кусок ответа.
    pub last_delta: Option<std::time::Instant>,
    /// Итог последнего инструмента хода: `schema_fields → 25 строк`.
    pub last_tool: Option<String>,
    /// Последняя ошибка.
    pub error: Option<String>,
    /// Вопрос принят в очередь: уйдёт, когда модель закончит.
    pub notice: Option<String>,
    /// Ждём подтверждения инструмента.
    pub confirm: Option<Confirm>,
    /// Ответ запроса списка моделей.
    pub models: Option<Result<Vec<String>, String>>,
    /// Ответ проверки подключения.
    pub checked: Option<Result<String, String>>,
    /// Размер контекста активного диалога (`tokens`, `exact`).
    pub context: Option<(u64, bool)>,
    /// Поддерживает ли сервер поле `tools` (`None` — не проверяли).
    pub tools_ok: Option<bool>,
    /// Журнал хода агента (ограничен 1000 записями: старые вытесняются).
    pub trace: std::collections::VecDeque<String>,
    /// Показывать журнал хода в ленте.
    pub show_trace: bool,
    /// Набранный, но ещё не отправленный вопрос.
    pub input: String,
    /// Позиция курсора в `input`, в знаках.
    pub cursor: usize,
    /// Окно настроек открыто; `Some` — черновик полей.
    pub settings_draft: Option<Settings>,
    /// Какое поле окна настроек в TUI правится (номер в [`settings::FIELDS`]).
    pub settings_field: usize,
    /// Лента прокручена от низа на столько строк (только TUI).
    pub scroll: usize,
}

/// Сколько строк занимает поле ввода в окне; расти оно может не дальше
/// [`INPUT_GROW`] высоты экрана.
pub const INPUT_ROWS: usize = 2;
/// Доля высоты экрана, выше которой поле ввода не растёт.
pub const INPUT_GROW: f32 = 0.75;

impl State {
    pub fn new() -> State {
        State::default()
    }

    /// Применить одно событие воркера.
    pub fn apply(&mut self, ev: Ev) {
        match ev {
            Ev::Dialogs { active, dialogs } => {
                self.active = active;
                self.dialogs = dialogs;
            }
            Ev::Messages { dialog, messages } => {
                self.msg_dialog = Some(dialog);
                self.messages = messages;
                self.streaming.clear();
                self.streaming_dialog = None;
                self.scroll = 0;
            }
            Ev::Settings(s) => self.settings = s,
            Ev::Started { dialog } => {
                // Ход начался: индикатор сразу, не дожидаясь первого куска.
                self.streaming_dialog = Some(dialog);
                self.streaming.clear();
                self.busy = true;
                self.scroll = 0;
                self.enter(progress::Wait::Thinking);
                self.last_delta = None;
            }
            Ev::Delta { dialog, text } => {
                self.streaming_dialog = Some(dialog);
                self.streaming.push_str(&text);
                self.busy = true;
                self.scroll = 0;
                if self.phase != Some(progress::Wait::Writing) {
                    self.enter(progress::Wait::Writing);
                }
                self.last_delta = Some(std::time::Instant::now());
            }
            Ev::Tool { name, .. } => self.enter(progress::Wait::Tool(name)),
            Ev::ToolDone {
                name,
                lines,
                is_error,
                ..
            } => self.last_tool = Some(tool_summary(&name, lines, is_error)),
            Ev::Title { dialog, title } => {
                if let Some(d) = self.dialogs.iter_mut().find(|d| d.id == dialog) {
                    d.title = title;
                }
            }
            Ev::Done { .. } => {
                self.busy = false;
                self.notice = None;
                self.phase = None;
                self.last_tool = None;
            }
            Ev::Error { text, .. } => {
                self.error = Some(text);
                self.busy = false;
                self.notice = None;
                self.phase = None;
                self.last_tool = None;
            }
            Ev::Models(r) => self.models = Some(r),
            Ev::Checked(r) => self.checked = Some(r),
            Ev::Context { tokens, exact, .. } => self.context = Some((tokens, exact)),
            Ev::Trace { text } => {
                self.trace.push_back(text);
                if self.trace.len() > 1000 {
                    self.trace.pop_front();
                }
                self.scroll = 0;
            }
            Ev::Notice { text, .. } => self.notice = Some(text),
            Ev::Retry { attempt, .. } => {
                self.notice = Some(format!("запрос повторяется (попытка {})…", attempt + 1));
            }
            Ev::ConfirmRequest { tool, summary } => {
                // Ждём человека, а не модель: индикатор молчит.
                self.confirm = Some(Confirm { tool, summary });
                self.phase = None;
            }
            Ev::ToolsProbed(ok) => self.tools_ok = Some(ok),
        }
    }

    /// Новая фаза хода — с этого мгновения.
    fn enter(&mut self, phase: progress::Wait) {
        self.phase = Some(phase);
        self.since = Some(std::time::Instant::now());
    }

    /// Индикатор хода на момент `now`; хода нет — `None`.
    fn indicator(&self, now: std::time::Instant) -> Option<progress::Indicator> {
        let phase = self.phase.as_ref().filter(|_| self.busy)?;
        let ago = |t: Option<std::time::Instant>| t.map(|t| now.saturating_duration_since(t));
        Some(progress::indicator(
            phase,
            ago(self.since).unwrap_or_default(),
            ago(self.last_delta),
            self.streaming.chars().count(),
            self.last_tool.as_deref(),
        ))
    }

    /// Курсор в конце потока: виден, пока модель пишет, и мигает.
    fn caret(&self, now: std::time::Instant) -> bool {
        self.busy
            && self.phase == Some(progress::Wait::Writing)
            && self
                .since
                .is_some_and(|t| progress::caret_on(now.saturating_duration_since(t)))
    }

    /// Выбрать всё, что воркер успел прислать.
    pub fn poll(&mut self, events: &std::sync::mpsc::Receiver<Ev>) {
        for ev in events.try_iter() {
            self.apply(ev);
        }
    }

    /// Прокрутить ленту на `lines` строк; вверх — отрицательное. Точную границу
    /// знает [`frame`]: там видно, сколько строк в ленте.
    pub fn scroll_by(&mut self, lines: isize) {
        let s = self.scroll as isize + lines;
        self.scroll = s.max(0) as usize;
    }
}

/// Действие по щелчку в сетке экрана агента.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Act {
    /// Переключить активный диалог (номер в [`State::dialogs`]).
    Pick(usize),
    /// Открыть настройки.
    Settings,
    /// Подтвердить ждущий вызов.
    Confirm,
    /// Отклонить ждущий вызов.
    Deny,
    /// Стереть знак ввода или закрыть настройки.
    Back,
    /// Отослать набранный вопрос.
    Send,
    /// Клавиша — по обычным правилам (навигация и ввод).
    Key(Key),
}

/// Высота шапки TUI-экрана: строка диалогов и строка настроек.
const HEAD: usize = 2;
/// Подвал: строка ввода и строка состояния.
const FOOTER: usize = 3;

/// Подсказка в пустом поле ввода.
const PLACEHOLDER: &str = "Введите вопрос агенту или опишите, что хотите получить";

/// Экран агента в TUI: сетка знаков `cols`×`rows`. Пока открыто окно
/// подтверждения или настроек, ввод вопроса неактивен. Лента — снизу вверх:
/// видно столько последних строк, сколько влезло, [`State::scroll`] листает
/// назад.
/// Ввод, перенесённый по строкам ширины `w`: сами строки и позиция курсора
/// (строка, столбец) в этом разложении. Перенос по знакам: слова рвутся,
/// зато курсор всегда виден и не зависит от ширины слов.
fn wrap_input(input: &str, cursor: usize, w: usize) -> (Vec<String>, usize, usize) {
    let w = w.max(1);
    let chars: Vec<char> = input.chars().collect();
    let mut lines = vec![String::new()];
    // Курсор в координатах разложения: строка и столбец последней строки.
    let at = |lines: &Vec<String>| (lines.len() - 1, lines.last().unwrap().chars().count());
    let mut crow = 0;
    let mut ccol = 0;
    for (i, c) in chars.iter().enumerate() {
        if i == cursor {
            (crow, ccol) = at(&lines);
        }
        let row = lines.last_mut().expect("строка всегда есть");
        row.push(*c);
        if row.chars().count() >= w {
            lines.push(String::new());
        }
    }
    if chars.len() == cursor {
        (crow, ccol) = at(&lines);
    }
    // Пустых хвостовых строк не оставляем: перенос после последнего знака
    // лишний, кроме случая «курсор стоит в новой строке».
    if lines.last().is_some_and(|l| l.is_empty()) && chars.len() != cursor {
        lines.pop();
    }
    (lines, crow, ccol)
}

/// Сообщение в ленте: вопрос или ответ. Результаты инструментов и ходы
/// модели из одних вызовов — работа хода, а не разговор: их итог виден в
/// строке состояния, пока ход идёт (`agr:agent-loop`).
fn in_tape(m: &MsgView) -> bool {
    match m.role.as_str() {
        "tool" => false,
        "assistant" => !m.text.trim().is_empty(),
        _ => true,
    }
}

/// Итог инструмента одной строкой: `schema_fields → 25 строк`.
fn tool_summary(name: &str, lines: usize, is_error: bool) -> String {
    if is_error {
        format!("{name} — ошибка")
    } else {
        format!("{name} → {lines} строк")
    }
}

pub fn frame(state: &mut State, cols: usize, rows: usize) -> Grid<Act> {
    let mut g: Grid<Act> = Grid::new(cols);
    let rows = rows.max(HEAD + FOOTER + 1);
    let avail = rows - FOOTER - HEAD;

    // Шапка: лента диалогов, режим и контекст.
    g.shade(0, 0, cols, PANEL);
    dialogs::row(&mut g, state, cols);
    dialogs::settings_row(&mut g, state, cols);

    // Лента: разобранный общий markup, перенесённый по ширине экрана; видно
    // столько последних строк, сколько влезло, [`State::scroll`] листает назад.
    let tape: Vec<_> = chat::tape(state)
        .iter()
        .flat_map(|l| chat::wrap_line(l, cols))
        .collect();
    state.scroll = state.scroll.min(tape.len().saturating_sub(avail));
    let end = tape.len() - state.scroll;
    let start = end.saturating_sub(avail);
    let mut y = HEAD;
    for line in &tape[start..end] {
        let mut x = 0;
        for (t, fg) in line {
            x = g.put(y, x, t, *fg);
            if x >= cols {
                break;
            }
        }
        y += 1;
    }
    if state.scroll == 0 && y < rows - FOOTER {
        g.put(y, 0, "  (Enter — отправить вопрос)", DIM);
    }

    // Поле ввода: [`INPUT_ROWS`] строки с переносом, курсор — где ввод.
    let input_y = rows - 1 - INPUT_ROWS;
    for dy in 0..INPUT_ROWS {
        g.shade(input_y + dy, 0, cols, PANEL);
    }
    let waiting = state.confirm.is_some() || state.settings_draft.is_some();
    let (head, fg) = if waiting {
        (" ожидание: ", DIM)
    } else {
        ("", ACCENT)
    };
    let x = g.put(input_y, 0, head, fg);
    let rest = cols.saturating_sub(x);
    let (lines, crow, ccol) = wrap_input(&state.input, state.cursor, rest);
    let base = lines.len().saturating_sub(INPUT_ROWS);
    for (dy, line) in lines.iter().skip(base).enumerate() {
        let tx = if base == 0 && dy == 0 { x } else { 0 };
        g.put(input_y + dy, tx, line, TEXT);
    }
    if !waiting && state.input.is_empty() {
        g.put(input_y, x, PLACEHOLDER, DIM);
    }
    if !waiting && crow >= base {
        // Первая строка начинается правее приглашения — курсор сдвигаем так же.
        let shift = if base == 0 && crow == 0 { x } else { 0 };
        g.shade(input_y + (crow - base), shift + ccol, 1, ACCENT);
    }

    // Строка состояния: идёт ход — индикатор фазы (`agr:agent-progress`).
    // Знаки индикатора — первыми, текст состояния — следом за ними.
    let mut sx = 0;
    let (status, sf) = if let Some(ind) = state.indicator(std::time::Instant::now()) {
        match &ind.look {
            progress::Look::Dots(b) => {
                for b in b {
                    sx = g.put(rows - 1, sx, "•", progress::dot_color(*b));
                }
            }
            progress::Look::Spin(c) => sx = g.put(rows - 1, sx, &c.to_string(), ACCENT),
        }
        sx += 1;
        (format!("{} (Ctrl+A — прервать)", ind.text), OK)
    } else if state.busy {
        ("модель отвечает… (Ctrl+A — прервать)".to_string(), OK)
    } else if state.confirm.is_some() {
        (
            "ожидание подтверждения (y/n, Ctrl+A — прервать)".to_string(),
            ACCENT,
        )
    } else if state.settings_draft.is_some() {
        (
            "настройки: Tab — поле · Enter — сохранить · Esc — отмена".to_string(),
            DIM,
        )
    } else {
        ("Enter — отправить · Ctrl+N — диалог".to_string(), DIM)
    };
    g.put(rows - 1, sx, &status, sf);
    if let Some(n) = &state.notice {
        let room = cols.saturating_sub(sx + status.chars().count() + 2);
        let shown = crate::ui::cols::fit(n, room, false);
        g.put_right(rows - 1, cols, &shown, ACCENT);
    }

    g.reach_rows(rows);

    if state.confirm.is_some() {
        confirm::box_tui(&mut g, state, cols, rows);
    }
    if state.settings_draft.is_some() {
        settings::box_tui(&mut g, state, cols, rows);
    }
    g
}

/// Выполнить действие щелчка.
pub fn apply(state: &mut State, cmd: &CmdTx, a: Act) {
    match a {
        Act::Pick(i) => {
            let id = state.dialogs.get(i).map(|d| d.id.clone());
            if let Some(id) = id {
                let _ = cmd.send(Cmd::PickDialog { id });
            }
        }
        Act::Settings => {
            if state.settings_draft.is_none() {
                state.settings_draft = Some(state.settings.clone());
                state.settings_field = 0;
                state.checked = None;
                state.models = None;
            }
        }
        Act::Confirm => {
            let _ = cmd.send(Cmd::Confirm);
            state.confirm = None;
        }
        Act::Deny => {
            let _ = cmd.send(Cmd::Deny);
            state.confirm = None;
        }
        Act::Back => {
            state.settings_draft = None;
        }
        Act::Send => {
            let text = state.input.trim().to_string();
            if !text.is_empty() {
                let _ = cmd.send(Cmd::Send { text });
                state.input.clear();
                state.cursor = 0;
            }
        }
        Act::Key(k) => key(state, cmd, k),
    }
}

/// Одна клавиша на экране агента.
pub fn key(state: &mut State, cmd: &CmdTx, k: Key) {
    // Окно настроек модальное: клавиши идут в его поля.
    if state.settings_draft.is_some() && settings::key_tui(state, cmd, k) {
        return;
    }
    match k {
        Key::Ctrl('n') => {
            let _ = cmd.send(Cmd::NewDialog);
        }
        Key::Ctrl('a') => {
            let _ = cmd.send(Cmd::Abort);
            state.confirm = None;
        }
        Key::Ctrl(_) => {}
        Key::Esc => {
            if state.settings_draft.is_some() {
                state.settings_draft = None;
            } else if state.confirm.is_some() {
                let _ = cmd.send(Cmd::Deny);
                state.confirm = None;
            }
        }
        Key::Enter => {
            if state.confirm.is_some() {
                let _ = cmd.send(Cmd::Confirm);
                state.confirm = None;
            } else {
                apply(state, cmd, Act::Send);
            }
        }
        Key::Char('y') if state.confirm.is_some() => {
            let _ = cmd.send(Cmd::Confirm);
            state.confirm = None;
        }
        Key::Char('n') if state.confirm.is_some() => {
            let _ = cmd.send(Cmd::Deny);
            state.confirm = None;
        }
        Key::Backspace => {
            if state.cursor > 0 {
                let mut ch: Vec<char> = state.input.chars().collect();
                ch.remove(state.cursor - 1);
                state.input = ch.into_iter().collect();
                state.cursor -= 1;
            }
        }
        Key::Delete => {
            let mut ch: Vec<char> = state.input.chars().collect();
            if state.cursor < ch.len() {
                ch.remove(state.cursor);
                state.input = ch.into_iter().collect();
            }
        }
        Key::PageUp => state.scroll_by(20),
        Key::PageDown => state.scroll_by(-20),
        Key::Left => state.cursor = state.cursor.saturating_sub(1),
        Key::Right => state.cursor = (state.cursor + 1).min(state.input.chars().count()),
        Key::Home => state.cursor = 0,
        Key::End => state.cursor = state.input.chars().count(),
        Key::Up | Key::Down | Key::Insert => {}
        Key::Char(c) => {
            if state.confirm.is_none() && state.settings_draft.is_none() {
                let mut ch: Vec<char> = state.input.chars().collect();
                ch.insert(state.cursor.min(ch.len()), c);
                state.input = ch.into_iter().collect();
                state.cursor += 1;
            }
        }
        Key::Tab | Key::BackTab => {}
    }
}

/// Экран агента: сверху — диалоги и режим, по центру — лента, снизу — вопрос.
pub(super) fn show(ui: &mut egui::Ui, state: &mut State, cmd: &CmdTx) {
    dialogs::panels(ui, state, cmd);

    // Поле ввода: [`INPUT_ROWS`] строк, растёт с текстом, но не выше
    // [`INPUT_GROW`] экрана; Enter отправляет, Shift+Enter переносит.
    egui::Panel::bottom("agent-input").show(ui, |ui| {
        ui.horizontal(|ui| {
            let max_h = ui.ctx().input(|i| i.viewport_rect().height()) * INPUT_GROW;
            // Поле занимает всё, кроме кнопки; иначе она уезжает за край.
            let width = (ui.available_width() - 110.0).max(200.0);
            let mut r = None;
            egui::ScrollArea::vertical()
                .id_salt("agent-input")
                .auto_shrink([true, true])
                .max_height(max_h)
                .show(ui, |ui| {
                    r = Some(
                        ui.add(
                            egui::TextEdit::multiline(&mut state.input)
                                .hint_text("вопрос модели… (Enter — отправить)")
                                .desired_rows(INPUT_ROWS)
                                .desired_width(width)
                                .font(egui::TextStyle::Monospace),
                        ),
                    );
                });
            // Многострочное поле само вставляет перенос: Enter без Shift ловим
            // по сырому вводу, пока фокус в поле, и отправляем (перенос от
            // этого Enter пропадает вместе со всей строкой — её чистим).
            let enter = r.is_some_and(|r| {
                r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift)
            });
            let send = enter || ui.button("Отправить").clicked();
            if send && !state.input.trim().is_empty() {
                let _ = cmd.send(Cmd::Send {
                    text: state.input.clone(),
                });
                state.input.clear();
            }
            if (state.busy || state.confirm.is_some()) && ui.button("Прервать").clicked() {
                let _ = cmd.send(Cmd::Abort);
                state.confirm = None;
            }
        });
        ui.label(
            egui::RichText::new(
                "F3 — назад к таблице · Enter — отправить · Shift+Enter — новая строка",
            )
            .small()
            .color(DIM),
        );
    });

    egui::CentralPanel::default().show(ui, |ui| {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                // Вопросы и ответы; инструменты — в строке состояния
                // (`agr:agent-loop`).
                for m in state.messages.iter().filter(|m| in_tape(m)) {
                    chat::message(ui, m);
                }
                if !state.streaming.is_empty() {
                    chat::message(
                        ui,
                        &MsgView {
                            role: "assistant".into(),
                            text: state.streaming.clone(),
                            index: state.messages.len(),
                            name: String::new(),
                            is_error: false,
                        },
                    );
                }
                if let Some(ind) = state.indicator(std::time::Instant::now()) {
                    progress::show(ui, &ind);
                    // Анимация живёт, только пока окно перерисовывается.
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(50));
                }
                if let Some(e) = &state.error {
                    ui.colored_label(ERROR, e);
                }
            });
    });

    settings::window(ui, state, cmd);
    confirm::window(ui, state, cmd);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `agr:agent-progress`
    /// Фаза хода идёт за событиями воркера: начало — ждём, кусок — пишет,
    /// инструмент — его имя, конец — индикатора нет.
    #[test]
    fn phase_follows_worker_events() {
        let mut s = State::new();
        let d = || "d".to_string();
        s.apply(Ev::Started { dialog: d() });
        assert_eq!(s.phase, Some(progress::Wait::Thinking));
        s.apply(Ev::Delta {
            dialog: d(),
            text: "а".into(),
        });
        assert_eq!(s.phase, Some(progress::Wait::Writing));
        s.apply(Ev::Tool {
            dialog: d(),
            name: "goto".into(),
        });
        assert_eq!(s.phase, Some(progress::Wait::Tool("goto".into())));
        s.apply(Ev::Done { dialog: d() });
        assert_eq!(s.phase, None);
        assert!(s.indicator(std::time::Instant::now()).is_none());
    }

    /// `agr:agent-progress`
    /// В TUI строка состояния хода — точки и «модель думает»; пока модель
    /// пишет, в конце потока стоит курсор.
    #[test]
    fn tui_status_shows_phase() {
        let mut s = State::new();
        s.apply(Ev::Started { dialog: "d".into() });
        let g = frame(&mut s, 80, 12);
        assert!(
            g.text(11).starts_with("••• модель думает"),
            "{}",
            g.text(11)
        );
        s.apply(Ev::Delta {
            dialog: "d".into(),
            text: "привет".into(),
        });
        let g = frame(&mut s, 80, 12);
        assert!(
            g.text(11).contains("модель пишет · 6 знаков"),
            "{}",
            g.text(11)
        );
        let tape = chat::tape(&s);
        assert_eq!(tape.last().unwrap().last().unwrap().0, "▍", "курсор потока");
    }

    /// `agr:agent-progress`
    /// Результат инструмента посреди хода — итогом в строке состояния; конец
    /// хода его снимает.
    #[test]
    fn tool_result_goes_to_status_line() {
        let mut s = State::new();
        s.apply(Ev::Started { dialog: "d".into() });
        s.apply(Ev::ToolDone {
            dialog: "d".into(),
            name: "schema_fields".into(),
            lines: 3,
            is_error: false,
        });
        s.apply(Ev::Started { dialog: "d".into() });
        let g = frame(&mut s, 100, 12);
        assert!(
            g.text(11)
                .contains("модель думает · schema_fields → 3 строк"),
            "{}",
            g.text(11)
        );
        s.apply(Ev::Done { dialog: "d".into() });
        assert_eq!(s.last_tool, None);
    }

    /// `agr:agent-empty`
    /// Ошибка хода видна и гасит индикатор.
    #[test]
    fn error_is_shown_and_stops_indicator() {
        let mut s = State::new();
        s.apply(Ev::Started { dialog: "d".into() });
        s.apply(Ev::Error {
            dialog: "d".into(),
            text: "модель не ответила (finish: content_filter)".into(),
        });
        assert!(s.error.as_deref().unwrap().contains("не ответила"));
        assert!(s.indicator(std::time::Instant::now()).is_none());
    }

    /// Пустое поле ввода подсказывает, что писать; с первым знаком подсказка
    /// исчезает.
    #[test]
    fn empty_input_shows_placeholder() {
        let mut s = State::new();
        let y = 12 - 1 - INPUT_ROWS;
        let g = frame(&mut s, 80, 12);
        assert!(g.text(y).starts_with("Введите вопрос агенту"));
        s.input = "а".into();
        s.cursor = 1;
        let g = frame(&mut s, 80, 12);
        assert_eq!(g.text(y), "а");
    }

    /// Окно настроек в TUI правится с клавиатуры: Tab — следующее поле,
    /// знаки и забой — текст, Enter — сохранить, ввод вопроса не затронут.
    #[test]
    fn tui_settings_edit_and_save() {
        let (cmd, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut s = State::new();
        apply(&mut s, &cmd, Act::Settings);
        assert!(s.settings_draft.is_some());
        // Адрес — первое поле: стереть и набрать свой.
        key(&mut s, &cmd, Key::Ctrl('u'));
        for c in "http://x/v1".chars() {
            key(&mut s, &cmd, Key::Char(c));
        }
        key(&mut s, &cmd, Key::Backspace);
        key(&mut s, &cmd, Key::Char('1'));
        // Tab ×2 — модель.
        key(&mut s, &cmd, Key::Tab);
        key(&mut s, &cmd, Key::Tab);
        key(&mut s, &cmd, Key::Ctrl('u'));
        key(&mut s, &cmd, Key::Char('m'));
        assert!(s.input.is_empty(), "текст ушёл в поля, а не в вопрос");
        key(&mut s, &cmd, Key::Enter);
        assert!(s.settings_draft.is_none());
        match rx.try_recv() {
            Ok(Cmd::SaveSettings { settings }) => {
                assert_eq!(settings.base_url, "http://x/v1");
                assert_eq!(settings.model, "m");
            }
            other => panic!("ждали SaveSettings, пришло {other:?}"),
        }
    }

    /// Esc закрывает окно настроек без сохранения; Ctrl+T просит проверку
    /// подключения с текущим черновиком.
    #[test]
    fn tui_settings_escape_discards_and_ctrl_t_checks() {
        let (cmd, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut s = State::new();
        apply(&mut s, &cmd, Act::Settings);
        key(&mut s, &cmd, Key::Char('x'));
        key(&mut s, &cmd, Key::Ctrl('t'));
        assert!(matches!(rx.try_recv(), Ok(Cmd::CheckSettings { .. })));
        key(&mut s, &cmd, Key::Esc);
        assert!(s.settings_draft.is_none());
        assert!(rx.try_recv().is_err(), "отмена ничего не сохраняет");
    }

    /// В окне настроек ключ скрыт: виден только хвост, а фокус показан
    /// стрелкой у поля.
    #[test]
    fn tui_settings_box_masks_key_and_marks_focus() {
        let (cmd, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut s = State::new();
        s.settings.api_key = "sk_secretsecretsecret1234".into();
        apply(&mut s, &cmd, Act::Settings);
        let g = frame(&mut s, 100, 24);
        let all: String = (0..g.rows())
            .map(|r| g.text(r))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("1234"));
        assert!(!all.contains("secretsecret"));
        assert!(all.contains("▸ адрес"));
    }

    /// Курсор стоит на знаке, с которым совпадает позиция в строке ввода.
    #[test]
    fn input_cursor_sits_after_the_prompt() {
        let mut s = State::new();
        s.input = "привет".into();
        s.cursor = 3;
        let g = frame(&mut s, 40, 12);
        let y = 12 - 1 - INPUT_ROWS;
        // Приглашения нет: три знака «при» — до курсора, он на «в».
        assert_eq!(g.bg(y, 3), Some(ACCENT));
        assert_eq!(g.bg(y, 2), Some(PANEL));
        // Курсор в конце ввода — на первой клетке после текста.
        s.cursor = 6;
        let g = frame(&mut s, 40, 12);
        assert_eq!(g.bg(y, 6), Some(ACCENT));
    }

    /// Ввод текста по знакам: буква, забой, отправка.
    #[test]
    fn input_takes_chars_and_backspace_sends_on_enter() {
        let (cmd, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut s = State::new();
        key(&mut s, &cmd, Key::Char('п'));
        key(&mut s, &cmd, Key::Char('р'));
        key(&mut s, &cmd, Key::Backspace);
        assert_eq!(s.input, "п");
        key(&mut s, &cmd, Key::Enter);
        assert!(s.input.is_empty());
    }

    /// Ctrl+N просит новый диалог.
    #[test]
    fn ctrl_n_new_dialog() {
        let (cmd, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut s = State::new();
        s.dialogs.push(crate::agent::DialogView {
            id: "d1".into(),
            title: "т".into(),
            closed: false,
        });
        s.active = Some("d1".into());
        key(&mut s, &cmd, Key::Ctrl('n'));
        assert!(matches!(rx.try_recv(), Ok(Cmd::NewDialog)));
    }

    /// y/n отвечают на подтверждение, не попадая в строку ввода.
    #[test]
    fn y_and_n_answer_confirmation() {
        let (cmd, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut s = State::new();
        let confirm = || Confirm {
            tool: "edit_messages".into(),
            summary: crate::edit::Summary {
                lines: vec!["5 Order header.order_id: 1000 → 777".into()],
                totals: vec![("сообщений".into(), "1".into())],
            },
        };
        s.confirm = Some(confirm());
        key(&mut s, &cmd, Key::Char('n'));
        assert!(s.confirm.is_none(), "n отклоняет");
        assert!(s.input.is_empty(), "n не печатается в ввод");
        assert!(matches!(rx.try_recv(), Ok(Cmd::Deny)));
        s.confirm = Some(confirm());
        key(&mut s, &cmd, Key::Char('y'));
        assert!(matches!(rx.try_recv(), Ok(Cmd::Confirm)));
    }

    /// Поле ввода — 2 строки и растёт не выше 3/4 высоты экрана.
    #[test]
    fn input_is_two_rows_and_grows_at_most_three_quarters() {
        assert_eq!(INPUT_ROWS, 2);
        assert!((INPUT_GROW - 0.75).abs() < 1e-9);
    }

    /// Лента прокручивается и не уходит за начало.
    #[test]
    fn scroll_moves_and_clamps_at_top() {
        let mut s = State::new();
        s.scroll_by(5);
        assert_eq!(s.scroll, 5);
        s.scroll_by(-10);
        assert_eq!(s.scroll, 0);
    }

    /// Журнал трейсов ограничен 1000: старые вытесняются.
    #[test]
    fn trace_capped_at_1000() {
        let mut s = State::new();
        for i in 0..1001 {
            s.apply(Ev::Trace {
                text: format!("t{i}"),
            });
        }
        assert_eq!(s.trace.len(), 1000);
        assert_eq!(s.trace.front().unwrap(), "t1");
        assert_eq!(s.trace.back().unwrap(), "t1000");
    }
}
