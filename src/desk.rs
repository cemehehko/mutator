//! Состояние просмотрщика — одно на все виды: окно, TUI в окне и терминал
//! читают и меняют один [`Desk`] (`agr:tui-same-data`).
//!
//! Таблица — окно строк вокруг видимой части с запасом в экран с каждой
//! стороны; у края запаса воркер дочитывает ещё, дальний край обрезается.
//! Положение задаёт сообщение, а не номер строки (`agr:anchor`): номер
//! появляется, когда готов индекс или в виде фильтра. Видимые строки
//! разбираются в ячейки при приходе; деталь — только у выбранной, и
//! перечитывается, когда у неё сменилось тело (правки, мутатор).
//!
//! Растущий файл (`*.current.dat`) — снимок на момент открытия; `r` (или `↓`/
//! `PgDn` на последней строке) перечитывает его хвост на том же месте, а `End`
//! с последней строки — перечитывает и встаёт на новую последнюю (`agr:reload`).
//!
//! Ввод — [`Key`] от любого вида; правки строк фильтра, поиска, перехода и
//! выбора типов/колонок — [`Prompt`] здесь же, чтобы оба вида вели себя одинаково.
//! Открытая справка F1 ([`Help`]) глотает клавиши, пока её не закрыли.
//!
//! Правка поля: `Enter`/`e` в детали — новое значение, воркер прогоняет его
//! через tll-patch на этом сообщении (`agr:edit-checked`); принятое ложится в
//! [`Edits`], и вид перечитывается через новый yaml (`agr:patch-overlay`).
//! `u` снимает правку поля, `w` — диалог фиксации [`Commit`]; записанный файл
//! открывается вместо прежнего ([`Desk::reopen`]).
//!
//! Наружу: [`Desk`], [`Key`], [`RowView`], [`Detail`], [`Prompt`], [`PromptKind`],
//! [`Focus`], [`Pick`], [`EditKind`], [`Commit`], [`CommitMode`], [`MassEdit`],
//! [`Help`], [`editable`].

use std::path::PathBuf;
use std::sync::Arc;

use crate::columns::{self, Preset};
use crate::data::{At, Cmd, Edge, Ev, File, Index, Matches, Mutations, Place, View, Worker};
use crate::decode::{self, Node};
use crate::edit::commit::{self, Target};
use crate::edit::{Edits, Store, Summary};
use crate::io::{Context, LuaMode, Msg, Open, Reader};
use crate::lua::Filter;
use crate::range::Border;
use crate::scheme::{Kind, Sub};
use crate::session::Session;

/// Клавиша от любого вида.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    /// Ctrl+буква: у экрана агента это команды, вводу не буква.
    Ctrl(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
}

/// Строка таблицы: сообщение и готовые ячейки колонок.
#[derive(Debug, Clone)]
pub struct RowView {
    pub seq: i64,
    pub msgid: i32,
    pub name: String,
    pub data: Vec<u8>,
    /// Текст по колонкам [`Desk::columns`]; `None` — поля в этом типе нет.
    pub cells: Vec<Option<String>>,
}

/// Разобранное выбранное сообщение.
#[derive(Debug, Clone)]
pub struct Detail {
    pub seq: i64,
    /// Тело, из которого разобрано дерево: то же seq с новым телом — перечитать.
    pub data: Vec<u8>,
    pub root: Node,
    /// Курсор по строкам [`Node::walk`].
    pub cursor: usize,
    /// Первая видимая строка дерева.
    pub top: usize,
}

impl Detail {
    pub fn lines(&self) -> Vec<(usize, &Node)> {
        self.root.walk()
    }
    pub fn current(&self) -> Option<&Node> {
        self.root.walk().get(self.cursor).map(|(_, n)| *n)
    }
}

/// Где фокус клавиатуры.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Table,
    Detail,
}

/// Что вводится в строке внизу.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    Filter,
    Search {
        back: bool,
    },
    Goto,
    Types,
    Columns,
    /// Новое значение поля [`Prompt::path`] сообщения `seq`.
    Edit {
        seq: i64,
        kind: EditKind,
    },
    /// Массовая правка поля — один prompt на всю пачку, дальше подтверждение.
    MassEdit {
        kind: EditKind,
    },
}

/// Тип правки поля.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    /// Обычный скаляр — текст.
    Text,
    /// Перечисление: пикер имён и десятичное число.
    Enum,
    /// Битовые флаги: пикер имён и hex-число.
    Bits,
}

/// Пункт списка выбора (типы, колонки).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    pub label: String,
    pub hint: String,
    pub on: bool,
    /// Для Enum: код перечисления (i64, может быть отрицательным).
    /// Для Bits: маска битов этой именованной группы (как i64 от u64).
    pub value: Option<i64>,
}

/// Строка ввода или список выбора.
#[derive(Debug, Clone)]
pub struct Prompt {
    pub kind: PromptKind,
    pub text: String,
    /// Курсор в символах.
    pub cursor: usize,
    /// Для списков: пункты, текущий, первый видимый.
    pub items: Vec<Pick>,
    pub sel: usize,
    pub top: usize,
    /// Для правки: путь поля.
    pub path: String,
}

impl Prompt {
    fn line(kind: PromptKind, text: String) -> Prompt {
        let cursor = text.chars().count();
        Prompt {
            kind,
            text,
            cursor,
            items: Vec::new(),
            sel: 0,
            top: 0,
            path: String::new(),
        }
    }

    /// Пункты списка, подходящие под набранный текст: (номер в `items`, пункт).
    /// Для правки Enum/Bits список не фильтруется по текстовому полю (там число/hex),
    /// чтобы известное текущее значение всегда было видно в списке.
    pub fn visible(&self) -> Vec<(usize, &Pick)> {
        let filter_by_q = !matches!(
            self.kind,
            PromptKind::Edit {
                kind: EditKind::Enum | EditKind::Bits,
                ..
            } | PromptKind::MassEdit {
                kind: EditKind::Enum | EditKind::Bits,
                ..
            }
        );
        let q = self.text.to_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                if !filter_by_q {
                    true
                } else {
                    q.is_empty() || p.label.to_lowercase().contains(&q)
                }
            })
            .collect()
    }

    pub fn title(&self) -> &'static str {
        match self.kind {
            PromptKind::Filter => "фильтр (Lua, как tll-read -f)",
            PromptKind::Search { back: false } => "поиск вперёд",
            PromptKind::Search { back: true } => "поиск назад",
            PromptKind::Goto => "переход: seq | +N строка | -N от конца",
            PromptKind::Types => "типы сообщений",
            PromptKind::Columns => "колонки",
            PromptKind::Edit { .. } => "новое значение поля",
            PromptKind::MassEdit { .. } => "массовая правка поля",
        }
    }

    fn edit(&mut self, k: Key) {
        let mut chars: Vec<char> = self.text.chars().collect();
        match k {
            Key::Char(c) => {
                chars.insert(self.cursor, c);
                self.cursor += 1;
            }
            Key::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                chars.remove(self.cursor);
            }
            Key::Delete if self.cursor < chars.len() => {
                chars.remove(self.cursor);
            }
            Key::Left => self.cursor = self.cursor.saturating_sub(1),
            Key::Right => self.cursor = (self.cursor + 1).min(chars.len()),
            Key::Home => self.cursor = 0,
            Key::End => self.cursor = chars.len(),
            _ => {}
        }
        self.text = chars.into_iter().collect();
    }
}

/// Куда фиксировать правки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitMode {
    /// Новый файл рядом, исходник как был.
    NewFile,
    /// На место исходника, исходник — в бэкап.
    Overwrite,
}

/// Справка F1: живёт на [`Desk`] постоянно, `open` — показана ли. Позиция
/// `scroll` — истина для TUI и клавиатуры и переживает закрытие; `keyboard` —
/// скроллили с клавиатуры или колесом, GUI тогда ведёт область прокрутки,
/// иначе она живёт своим состоянием; `seeded` — GUI уже взял стартовое
/// смещение из `scroll`.
#[derive(Debug, Default)]
pub struct Help {
    pub open: bool,
    pub scroll: usize,
    pub keyboard: bool,
    pub seeded: bool,
}

/// Диалог фиксации правок: сводка, yaml, куда писать.
#[derive(Debug, Clone)]
pub struct Commit {
    pub mode: CommitMode,
    /// Имя нового файла и имя бэкапа — по умолчанию свободные.
    pub new_name: String,
    pub backup_name: String,
    pub summary: Summary,
    pub yaml: String,
    /// Идёт запись: доля пройденного.
    pub running: Option<f32>,
    pub error: Option<String>,
    /// Прокрутка сводки и yaml (TUI).
    pub scroll: usize,
}

impl Commit {
    /// Имя, которое сейчас вводится.
    pub fn name(&self) -> &str {
        match self.mode {
            CommitMode::NewFile => &self.new_name,
            CommitMode::Overwrite => &self.backup_name,
        }
    }

    pub fn name_mut(&mut self) -> &mut String {
        match self.mode {
            CommitMode::NewFile => &mut self.new_name,
            CommitMode::Overwrite => &mut self.backup_name,
        }
    }

    pub fn target(&self) -> Target {
        let p = PathBuf::from(self.name().trim());
        match self.mode {
            CommitMode::NewFile => Target::NewFile(p),
            CommitMode::Overwrite => Target::Overwrite { backup: p },
        }
    }
}

/// Правка, ждущая ответа tll-patch.
struct Pending {
    id: u64,
    edits: Edits,
    patch: PathBuf,
    /// Строка ввода — вернуть, если правку не примут.
    prompt: Prompt,
}

/// Массовая правка поля: состояние подтверждения с подсчётом сообщений.
#[derive(Debug, Clone)]
pub struct MassEdit {
    pub path: String,
    pub new_value: String,
    pub old_value: String,
    pub name: String,
    pub kind: EditKind,
    pub count: usize,
    /// Сообщения, которые будут правиться: (seq, имя типа).
    pub pairs: Vec<(i64, String)>,
}

/// Сколько строк читать за раз сверх экрана.
const MIN_FETCH: usize = 64;

/// Состояние просмотрщика.
pub struct Desk {
    pub file: File,
    worker: Worker,
    gen: u64,

    pub index: Option<Arc<Index>>,
    pub index_progress: f32,

    /// Применённый фильтр и его результат.
    pub filter: Filter,
    filter_id: u64,
    pub matches: Option<Arc<Matches>>,
    /// Фильтр отправлен, результата ещё нет.
    filter_pending: bool,
    /// Где стоял выбор при смене фильтра: скан растёт от начала файла, и как
    /// только он дойдёт до этого seq, вид вернётся сюда.
    filter_anchor: Option<i64>,

    /// Код Lua для transform-режима (F4): полный код, не выражение.
    pub lua_code: String,
    /// Режим Lua-канала.
    pub lua_mode: LuaMode,
    /// Включён ли transform-код: `false` — код написан, но не применяется
    /// (тумблер в основном окне отключает его без потери текста).
    pub lua_enabled: bool,
    /// Курсор в редакторе Lua (TUI): позиция по символам UTF-8 в [`Desk::lua_code`].
    pub lua_cursor: usize,
    /// Первая видимая строка кода в TUI-редакторе (вертикальная прокрутка).
    pub lua_scroll: usize,
    /// Видимая высота тела TUI-редактора: страница PgUp/PgDn.
    pub lua_page: usize,
    /// Подтверждение сброса кода в дефолт (GUI-модалка, TUI-приглашение).
    pub lua_reset_confirm: bool,
    /// Transform-код отправлен, результата (переоткрытия) ещё нет.
    transform_pending: bool,

    pub rows: Vec<RowView>,
    pub first_row: Option<usize>,
    pub at_start: bool,
    pub at_end: bool,
    /// Первая видимая строка и выбранная — номера в [`Desk::rows`].
    pub top: usize,
    pub sel: usize,
    /// Видимых строк таблицы — задаёт вид.
    pub height: usize,
    pending_replace: bool,
    pending_before: bool,
    pending_after: bool,
    /// После замены окна встать на этот seq (или в конец — [`i64::MAX`]).
    land: Option<i64>,
    /// После замены окна встать на строку с этим номером.
    land_row: Option<usize>,
    /// Фильтр ещё ищет, а хвост окна упёрся в найденное.
    tail_wait: bool,

    pub columns: Vec<String>,
    /// Сколько колонок полей прокручено влево (←/→).
    pub hscroll: usize,
    pub presets: Vec<Preset>,
    pub preset: usize,

    pub detail_open: bool,
    pub detail: Option<Detail>,
    pub detail_height: usize,
    /// Доля высоты под деталь (0..1); одна для TUI и GUI, сохраняется в сессию.
    pub detail_ratio: f32,
    /// Hex сообщения в детали (`x`); по умолчанию выключен.
    pub show_hex: bool,
    /// Тип, смещение и размер в строке поля детали (`i`); по умолчанию выключены.
    pub show_types: bool,
    /// Путь поля для курсора детали, пока детали ещё нет (из сессии).
    detail_hint: Option<String>,
    /// Курсор детали сдвинут клавишами — вид окна докручивает к нему дерево.
    pub detail_follow: bool,
    pub focus: Focus,

    pub prompt: Option<Prompt>,
    last_search: Option<(Filter, bool)>,
    search_id: u64,
    pub searching: bool,

    /// Строка состояния: последняя ошибка или итог действия.
    pub status: String,
    pub quit: bool,
    history_filter: Vec<String>,
    history_search: Vec<String>,

    /// Правки, наложенные на вид.
    pub edits: Edits,
    /// Что мутатор изменил в видимом окне: seq → путь → значение в файле, до
    /// него (`agr:transform-shadow`). Тела строк в виде — прогоные.
    pub mutations: Mutations,
    store: Store,
    check_id: u64,
    pending: Option<Pending>,
    pub commit: Option<Commit>,
    /// Массовая правка поля — диалог подтверждения.
    pub mass_edit: Option<MassEdit>,
    /// Справка F1: глотает клавиши, пока [`Help::open`].
    pub help: Help,
    /// seq, готовые для массовой правки — результат MassScan воркера.
    pub mass_pairs: Option<Vec<(i64, String)>>,
    scan_id: u64,
    pub scan_pending: bool,
    /// Записанный файл: вид откроет его вместо этого ([`Desk::reopen`]).
    pub reopen: Option<PathBuf>,
    notify: Arc<dyn Fn() + Send + Sync>,
    /// Канал действий агента: агент не трогает состояние сам, а просит
    /// (`agr:agent-same-desk`). `None` — вид уже отдал приёмник другому `Desk`
    /// (после фиксации правок окно переезжает на новый файл).
    acts: Option<std::sync::mpsc::Receiver<crate::agent::Act>>,
    act_tx: crate::agent::ActTx,
    /// Что агент уже знает о файле: сколько сообщений и правок. Вид делится
    /// этим через [`crate::agent::Cmd`], чтобы агент читал то же, что человек.
    synced_edits: Edits,
    /// Поколение файла: растёт при каждом переоткрытии. Агент кладёт его в
    /// правку, и устаревшая правка отбрасывается, а не ложится в чужой файл
    /// (`agr:agent-gen`).
    file_gen: Arc<std::sync::atomic::AtomicU64>,
}

impl Desk {
    /// Открыть файл и запросить первую страницу. `notify` будит вид.
    pub fn open(file: File, notify: Arc<dyn Fn() + Send + Sync>) -> Desk {
        let (act_tx, acts) = std::sync::mpsc::sync_channel(256);
        Desk::open_with(
            file,
            notify,
            act_tx,
            Some(acts),
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
        )
    }

    /// Счётчик поколений файла для агента ([`crate::agent::Act::Sync`]).
    pub fn file_gen(&self) -> Arc<std::sync::atomic::AtomicU64> {
        self.file_gen.clone()
    }

    /// Канал действий для воркера агента: [`crate::agent::start`] получает его
    /// копию, а [`Desk::poll`] применяет пришедшее к себе.
    pub fn act_tx(&self) -> crate::agent::ActTx {
        self.act_tx.clone()
    }

    /// Правки, о которых агенту ещё не сказали.
    pub fn unsynced_edits(&self) -> Option<Edits> {
        (self.edits != self.synced_edits).then(|| self.edits.clone())
    }

    /// Агент узнал про эти правки — второй раз не посылаем.
    pub fn mark_synced(&mut self) {
        self.synced_edits = self.edits.clone();
    }

    /// Снимок текущего состояния вида для агента (`agr:agent-snap`).
    pub fn make_snap(&self) -> crate::agent::Snap {
        let sel_row = self.first_row.map(|f| f + self.sel);
        let sel_seq = self.rows.get(self.sel).map(|r| r.seq);
        crate::agent::Snap {
            total: self.index.as_ref().map(|idx| idx.total),
            index: self.index.clone(),
            filter: self.filter.clone(),
            columns: self.columns.clone(),
            sel_seq,
            sel_row,
        }
    }

    fn open_with(
        file: File,
        notify: Arc<dyn Fn() + Send + Sync>,
        act_tx: crate::agent::ActTx,
        acts: Option<std::sync::mpsc::Receiver<crate::agent::Act>>,
        file_gen: Arc<std::sync::atomic::AtomicU64>,
    ) -> Desk {
        let presets = columns::load(&file.scheme);
        let columns = presets
            .first()
            .map(|p| p.columns.clone())
            .unwrap_or_default();
        let worker = Worker::start(file.clone(), notify.clone());
        let mut d = Desk {
            file,
            worker,
            gen: 0,
            index: None,
            index_progress: 0.0,
            filter: Filter::default(),
            filter_id: 0,
            matches: None,
            filter_pending: false,
            filter_anchor: None,
            lua_code: String::new(),
            lua_mode: LuaMode::Filter,
            lua_enabled: true,
            lua_cursor: 0,
            lua_page: 20,
            lua_scroll: 0,
            lua_reset_confirm: false,
            transform_pending: false,
            rows: Vec::new(),
            first_row: None,
            at_start: true,
            at_end: false,
            top: 0,
            sel: 0,
            height: 30,
            pending_replace: false,
            pending_before: false,
            pending_after: false,
            land: None,
            land_row: None,
            tail_wait: false,
            columns,
            hscroll: 0,
            presets,
            preset: 0,
            detail_open: false,
            detail: None,
            detail_height: 12,
            detail_ratio: 0.3,
            show_hex: false,
            show_types: false,
            detail_hint: None,
            detail_follow: false,
            focus: Focus::Table,
            prompt: None,
            last_search: None,
            search_id: 0,
            searching: false,
            status: String::new(),
            quit: false,
            history_filter: Vec::new(),
            history_search: Vec::new(),
            edits: Edits::default(),
            mutations: Mutations::default(),
            store: Store::new(),
            check_id: 0,
            pending: None,
            commit: None,
            mass_edit: None,
            help: Help::default(),
            mass_pairs: None,
            scan_id: 0,
            scan_pending: false,
            reopen: None,
            notify,
            acts,
            act_tx,
            synced_edits: Edits::default(),
            file_gen,
        };
        d.jump(At::Seq(i64::MIN), None);
        d
    }

    /// Применить действия агента: он просит, меняет — тот же `Desk`, что и
    /// человек (`agr:agent-same-desk`). Фильтр, поиск, переход, колонки — сразу;
    /// правки — как свои: yaml на диск, файл перечитывается через `patch+`.
    fn act(&mut self, a: crate::agent::Act) {
        match a {
            crate::agent::Act::Filter(f) => self.set_filter(f),
            crate::agent::Act::Search { filter, back } => self.search(filter, back),
            crate::agent::Act::Goto(where_) => self.goto(&where_),
            crate::agent::Act::SetColumns(cols) => {
                self.columns = cols;
                self.status = "колонки (агент)".into();
            }
            crate::agent::Act::MarkSynced => {
                self.mark_synced();
            }
            crate::agent::Act::LuaTransform { code, enabled } => {
                self.lua_code = code.clone();
                self.lua_enabled = enabled;
                if code.is_empty() {
                    self.clear_transform();
                } else if enabled {
                    self.set_transform(code);
                } else {
                    self.lua_mode = LuaMode::Filter;
                    self.worker.send(Cmd::Lua {
                        lua: None,
                        lua_mode: None,
                    });
                    self.transform_pending = false;
                    let land = self.selected().map(|r| r.seq);
                    let top = self.rows.get(self.top).map(|r| r.seq).unwrap_or(i64::MIN);
                    self.jump(At::Seq(top), land);
                    self.status = "transform выключен (агент)".into();
                }
            }
            crate::agent::Act::Sync { edits, gen } => {
                if gen != self.file_gen.load(std::sync::atomic::Ordering::SeqCst) {
                    // Человек переоткрыл файл, пока агент думал: правка по
                    // устаревшей картине — не применяем (`agr:agent-gen`).
                    self.status = "файл переоткрыт — правка агента отброшена".into();
                    return;
                }
                self.sync_edits(edits)
            }
        }
    }

    /// Правки агента в вид: yaml на диск, окно перечитывается через `patch+`
    /// (`agr:patch-overlay`). Незафиксированные правки агента и человека —
    /// одни и те же.
    fn sync_edits(&mut self, e: Edits) {
        let patch = match self.store.write(&e) {
            Ok(p) => p,
            Err(x) => {
                self.status = format!("правки агента: {x}");
                return;
            }
        };
        self.status = format!(
            "правок агента: {} в {} сообщ. — w зафиксировать",
            e.fields(),
            e.messages()
        );
        self.apply(e, patch);
    }

    fn view(&self) -> View {
        match &self.matches {
            Some(m) => View::Filtered(m.clone()),
            None => View::All,
        }
    }

    fn fetch_size(&self) -> usize {
        (self.height * 2).max(MIN_FETCH)
    }

    /// Новое окно: всё в полёте забыто.
    fn jump(&mut self, at: At, land: Option<i64>) {
        self.gen += 1;
        self.pending_replace = true;
        self.pending_before = false;
        self.pending_after = false;
        self.tail_wait = false;
        self.land = land;
        self.land_row = None;
        let n = self.fetch_size() + self.height;
        self.worker.send(Cmd::Window {
            gen: self.gen,
            view: self.view(),
            at,
            n,
        });
    }

    /// Всего строк в виде, если известно, и признак «ещё растёт».
    pub fn total(&self) -> Option<(usize, bool)> {
        match &self.matches {
            Some(m) => Some((m.len(), !m.is_done())),
            None => self.index.as_ref().map(|i| (i.total, false)),
        }
    }

    /// Номер выбранной строки, если известен.
    pub fn sel_row(&self) -> Option<usize> {
        self.first_row.map(|f| f + self.sel)
    }

    /// Доля для полосы прокрутки: (начало видимого, его доля).
    pub fn scroll_fraction(&self) -> Option<(f32, f32)> {
        let h = self.height.max(1) as f32;
        if let (Some((total, _)), Some(first)) = (self.total(), self.first_row) {
            let t = total.max(1) as f32;
            return Some((((first + self.top) as f32 / t).min(1.0), (h / t).min(1.0)));
        }
        let (b, l) = (self.file.info.seq_begin?, self.file.info.seq_last?);
        let span = (l - b).max(1) as f32;
        let seq = self.rows.get(self.top)?.seq;
        Some((
            ((seq - b) as f32 / span).clamp(0.0, 1.0),
            (h / span).min(1.0),
        ))
    }

    /// Перейти по доле полосы прокрутки.
    pub fn scroll_to(&mut self, f: f32) {
        let f = f.clamp(0.0, 1.0);
        let at = match self.total() {
            Some((total, _)) if self.matches.is_some() || self.index.is_some() => {
                At::Row(((total as f32 * f) as usize).min(total.saturating_sub(self.height)))
            }
            _ => {
                let (Some(b), Some(l)) = (self.file.info.seq_begin, self.file.info.seq_last) else {
                    return;
                };
                At::Seq(b + ((l - b) as f64 * f as f64) as i64)
            }
        };
        self.jump(at, None);
    }

    /// Разобрать события воркера и просьбы агента. `true` — что-то поменялось.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        let acts: Vec<crate::agent::Act> = self.acts.iter().flat_map(|r| r.try_iter()).collect();
        for a in acts {
            self.act(a);
            changed = true;
        }
        let evs = self.worker.poll();
        if !evs.is_empty() {
            changed = true;
        }
        for ev in evs {
            self.event(ev);
        }
        if changed {
            self.upkeep();
        }
        changed
    }

    /// Дождаться события (тесты).
    pub fn wait(&mut self, t: std::time::Duration) -> bool {
        match self.worker.wait(t) {
            Some(ev) => {
                self.event(ev);
                self.poll();
                self.upkeep();
                true
            }
            None => false,
        }
    }

    /// Нет запросов в полёте.
    pub fn idle(&self) -> bool {
        !self.pending_replace
            && !self.pending_before
            && !self.pending_after
            && !self.filter_pending
            && !self.searching
            && !self.scan_pending
            && self.pending.is_none()
            && self.commit.as_ref().is_none_or(|c| c.running.is_none())
    }

    fn event(&mut self, ev: Ev) {
        match ev {
            Ev::Window {
                gen,
                place,
                rows,
                first_row,
                at_start,
                at_end,
                mutated,
                transform_error: _,
            } => {
                if gen != self.gen {
                    return;
                }
                match place {
                    Place::Replace => self.mutations = mutated,
                    _ => self.mutations.extend(mutated),
                }
                self.window(place, rows, first_row, at_start, at_end);
            }
            Ev::TransformError(e) => self.status = format!("transform: {e}"),
            Ev::IndexProgress(p) => self.index_progress = p,
            Ev::Index(i) => {
                self.index = Some(i);
                self.index_progress = 1.0;
                // Номера строк станут известны: перечитать окно с того же места.
                if self.matches.is_none() {
                    if self.at_end {
                        // Окно у нижнего края файла (`End`, в т.ч. сразу после
                        // перечитывания хвоста): перечитка от `rows[top]` увела
                        // бы его в середину — держимся настоящего конца.
                        self.jump(At::End, Some(i64::MAX));
                    } else if let Some(r) = self.rows.get(self.top) {
                        let land = self.rows.get(self.sel).map(|r| r.seq);
                        self.jump(At::Seq(r.seq), land);
                    }
                }
            }
            Ev::Filter { id, matches } if id == self.filter_id => {
                self.filter_pending = false;
                self.matches = Some(matches);
                let land = self.rows.get(self.sel).map(|r| r.seq);
                self.jump(
                    At::Seq(self.rows.get(self.top).map(|r| r.seq).unwrap_or(i64::MIN)),
                    land,
                );
            }
            Ev::Filter { .. } => {}
            Ev::FilterTick { id } if id == self.filter_id => {
                if let Some(m) = self.matches.clone() {
                    if let Some(e) = m.error.lock().unwrap().as_ref() {
                        self.status = format!("фильтр: {e}");
                    }
                    if let Some(a) = self.filter_anchor {
                        let passed = m.seqs.read().unwrap().last().is_some_and(|&l| l >= a);
                        if passed || m.is_done() {
                            self.filter_anchor = None;
                            let row = m.row_of(a).min(m.len().saturating_sub(1));
                            let land = m.slice(row, 1).first().copied();
                            self.jump(At::Row(row.saturating_sub(self.height / 3)), land);
                            return;
                        }
                    }
                }
                if self.tail_wait || (self.rows.is_empty() && !self.pending_replace) {
                    self.tail_wait = false;
                    if self.rows.is_empty() {
                        self.jump(At::Row(0), None);
                    }
                }
            }
            Ev::FilterTick { .. } => {}
            Ev::Found { id, seq, error } if id == self.search_id => {
                self.searching = false;
                match (seq, error) {
                    (_, Some(e)) => self.status = format!("поиск: {e}"),
                    (None, None) => self.status = "не найдено".into(),
                    (Some(s), None) => {
                        self.status.clear();
                        self.show_seq(s);
                    }
                }
            }
            Ev::Found { .. } => {}
            Ev::Checked { id, result } => {
                if self.pending.as_ref().is_none_or(|p| p.id != id) {
                    return;
                }
                let p = self.pending.take().unwrap();
                match result {
                    Ok(_) => {
                        self.status = format!(
                            "правок: {} в {} сообщ. — w зафиксировать",
                            p.edits.fields(),
                            p.edits.messages()
                        );
                        self.apply(p.edits, Some(p.patch));
                    }
                    Err(e) => {
                        self.status = format!("правка не принята: {}", one_line(&e));
                        self.prompt = Some(p.prompt);
                    }
                }
            }
            Ev::CommitProgress(f) => {
                if let Some(c) = &mut self.commit {
                    if c.running.is_some() {
                        c.running = Some(f);
                    }
                }
            }
            Ev::Committed(r) => {
                let Some(c) = &mut self.commit else { return };
                c.running = None;
                match r {
                    Ok(p) => {
                        self.status = match c.mode {
                            CommitMode::NewFile => format!("записан {}", p.display()),
                            CommitMode::Overwrite => {
                                format!(
                                    "записан {}, исходник — {}",
                                    p.display(),
                                    c.backup_name.trim()
                                )
                            }
                        };
                        self.commit = None;
                        self.edits = Edits::default();
                        self.reopen = Some(p);
                    }
                    Err(e) => c.error = Some(e),
                }
            }
            Ev::MassScan { id, pairs } => {
                if id != self.scan_id {
                    return;
                }
                if let Some(me) = &mut self.mass_edit {
                    me.pairs = pairs.clone();
                    me.count = pairs.len();
                    self.status = format!(
                        "массовая правка {}: {} → {}  ({} сообщ.) — Enter/Esc",
                        me.path, me.old_value, me.new_value, me.count
                    );
                }
                self.mass_pairs = Some(pairs);
                self.scan_pending = false;
            }
            Ev::Error(e) => {
                self.status = e;
                self.pending_replace = false;
                self.pending_before = false;
                self.pending_after = false;
            }
        }
    }

    fn row_view(&self, m: Msg) -> RowView {
        let st = self.file.scheme.message(m.msgid);
        let name = st
            .map(|s| s.name.clone())
            .unwrap_or_else(|| format!("#{}", m.msgid));
        let cells = self
            .columns
            .iter()
            .map(|c| st.and_then(|s| decode::field(&self.file.scheme, s, &m.data, c)))
            .collect();
        RowView {
            seq: m.seq,
            msgid: m.msgid,
            name,
            data: m.data,
            cells,
        }
    }

    fn window(
        &mut self,
        place: Place,
        rows: Vec<Msg>,
        first_row: Option<usize>,
        at_start: bool,
        at_end: bool,
    ) {
        let views: Vec<RowView> = rows.into_iter().map(|m| self.row_view(m)).collect();
        let n = views.len();
        match place {
            Place::Replace => {
                self.pending_replace = false;
                self.rows = views;
                self.first_row = first_row;
                self.at_start = at_start;
                self.at_end = at_end;
                self.top = 0;
                self.sel = 0;
                match self.land.take() {
                    Some(i64::MAX) => {
                        self.sel = self.rows.len().saturating_sub(1);
                        self.top = self.rows.len().saturating_sub(self.height);
                    }
                    Some(s) => {
                        // За концом окна такого seq нет — выбираем последнюю строку.
                        let i = self
                            .rows
                            .iter()
                            .position(|r| r.seq >= s)
                            .unwrap_or(self.rows.len().saturating_sub(1));
                        self.sel = i;
                        self.top = i.saturating_sub(self.height / 3);
                    }
                    None => {}
                }
                if let (Some(r), Some(f)) = (self.land_row.take(), self.first_row) {
                    if r >= f && r - f < self.rows.len() {
                        self.sel = r - f;
                        self.top = self.sel.saturating_sub(self.height / 3);
                    }
                }
                self.fill_bottom();
                if self.matches.is_some() && n < self.height && !at_end {
                    self.tail_wait = true;
                }
            }
            Place::After => {
                self.pending_after = false;
                self.rows.extend(views);
                self.at_end = at_end;
                // Конец файла дочитан после PageDown: пустоту под последней убираем.
                self.fill_bottom();
                if self.first_row.is_none() {
                    self.first_row = first_row.and_then(|f| f.checked_sub(self.rows.len() - n));
                }
                if n == 0 && !at_end {
                    self.tail_wait = true;
                }
            }
            Place::Before => {
                self.pending_before = false;
                self.rows.splice(0..0, views);
                self.top += n;
                self.sel += n;
                self.at_start = at_start || n == 0;
                self.first_row =
                    first_row.or_else(|| self.first_row.and_then(|f| f.checked_sub(n)));
                self.fill_bottom();
            }
        }
        self.trim();
        self.refresh_detail();
    }

    /// Не держать в окне больше восьми экранов: дальний край отрезается.
    fn trim(&mut self) {
        let keep = (self.height * 8).max(MIN_FETCH * 4);
        if self.rows.len() <= keep {
            return;
        }
        let excess = self.rows.len() - keep;
        let before = self.top.saturating_sub(self.height * 2);
        let after = self.rows.len().saturating_sub(self.top + self.height * 3);
        if before >= after {
            let cut = excess.min(before);
            self.rows.drain(..cut);
            self.top -= cut;
            // Выбор мог остаться выше экрана (колесо прокрутки) — в отрезаемом.
            self.sel = self.sel.saturating_sub(cut);
            self.first_row = self.first_row.map(|f| f + cut);
            if cut > 0 {
                self.at_start = false;
            }
        } else {
            let cut = excess.min(after);
            self.rows.truncate(self.rows.len() - cut);
            self.sel = self.sel.min(self.rows.len() - 1);
            if cut > 0 {
                self.at_end = false;
            }
        }
        // Метки мутатора — только для живого окна.
        if let (Some(f), Some(l)) = (self.rows.first(), self.rows.last()) {
            let (lo, hi) = (f.seq, l.seq);
            self.mutations.retain(|s, _| *s >= lo && *s <= hi);
        }
    }

    /// Дочитать окно, если видимая часть близко к краю.
    fn upkeep(&mut self) {
        if self.pending_replace {
            return;
        }
        let h = self.height.max(1);
        let n = self.fetch_size();
        if !self.at_end
            && !self.pending_after
            && !self.tail_wait
            && self.rows.len() < self.top + h * 2
        {
            if let Some(last) = self.rows.last() {
                let edge = Edge {
                    seq: last.seq,
                    row: self.first_row.map(|f| f + self.rows.len() - 1),
                };
                self.pending_after = true;
                self.worker.send(Cmd::Extend {
                    gen: self.gen,
                    view: self.view(),
                    after: true,
                    edge,
                    n,
                });
            }
        }
        if !self.at_start && !self.pending_before && self.top < h {
            if let Some(first) = self.rows.first() {
                let edge = Edge {
                    seq: first.seq,
                    row: self.first_row,
                };
                self.pending_before = true;
                self.worker.send(Cmd::Extend {
                    gen: self.gen,
                    view: self.view(),
                    after: false,
                    edge,
                    n,
                });
            }
        }
    }

    /// Разобрать выбранное сообщение, если деталь открыта и выбор сменился.
    fn refresh_detail(&mut self) {
        if !self.detail_open {
            return;
        }
        let Some(r) = self.rows.get(self.sel) else {
            return;
        };
        // Тело могло смениться без смены seq: правки, переключение мутатора.
        if self
            .detail
            .as_ref()
            .is_some_and(|d| d.seq == r.seq && d.data == r.data)
        {
            return;
        }
        let root = match self.file.scheme.message(r.msgid) {
            Some(st) => decode::decode(&self.file.scheme, st, &r.data),
            None => Node {
                name: r.name.clone(),
                path: String::new(),
                ty: "?".into(),
                offset: 0,
                size: r.data.len(),
                value: None,
                children: Vec::new(),
                error: Some(format!("msgid {} нет в схеме", r.msgid)),
            },
        };
        // Тот же путь поля в новом сообщении — курсор остаётся на нём.
        let path = self
            .detail
            .as_ref()
            .and_then(|d| d.current().map(|n| n.path.clone()))
            .or_else(|| self.detail_hint.take());
        let cursor = path
            .and_then(|p| root.walk().iter().position(|(_, n)| n.path == p))
            .unwrap_or(0);
        self.detail = Some(Detail {
            seq: r.seq,
            data: r.data.clone(),
            root,
            cursor,
            top: cursor.saturating_sub(self.detail_height / 2),
        });
    }

    /// Снимок для сессии (`agr:session-by-name`): фильтр, поиск, истории,
    /// колонки, выбор, деталь. Окно дописывает своё.
    pub fn session(&self) -> Session {
        let (search, search_back) = match &self.last_search {
            Some((f, back)) => (Some(f.expr.clone()), *back),
            None => (None, false),
        };
        Session {
            file: self.file.path.display().to_string(),
            messages: self.filter.messages.clone(),
            expr: self.filter.expr.clone(),
            search,
            search_back,
            history_filter: self.history_filter.clone(),
            history_search: self.history_search.clone(),
            columns: Some(self.columns.clone()),
            preset: self.preset,
            hscroll: self.hscroll,
            seq: self
                .selected()
                .map(|r| r.seq)
                .or(self.land.filter(|&l| l != i64::MAX)),
            detail_open: self.detail_open,
            detail_path: self
                .detail
                .as_ref()
                .and_then(|d| d.current().map(|n| n.path.clone()))
                .or_else(|| self.detail_hint.clone()),
            focus_detail: self.focus == Focus::Detail,
            show_hex: self.show_hex,
            show_types: self.show_types,
            edits: self.edits.saved(),
            detail_ratio: Some(self.detail_ratio),
            lua_transform: if self.lua_code.is_empty() {
                None
            } else {
                Some(self.lua_code.clone())
            },
            lua_transform_enabled: if self.lua_code.is_empty() {
                None
            } else {
                Some(self.lua_enabled)
            },
            ..Default::default()
        }
    }

    /// Вернуть сессию: колонки, истории, деталь, затем фильтр и выбор — фильтр
    /// вернёт выбор, когда скан дойдёт до сохранённого seq.
    pub fn restore(&mut self, s: &Session) {
        if s.preset < self.presets.len() {
            self.preset = s.preset;
        }
        if let Some(c) = &s.columns {
            self.columns = c.clone();
            self.recompute_cells();
        }
        self.hscroll = s.hscroll.min(self.columns.len().saturating_sub(1));
        self.history_filter = s.history_filter.clone();
        self.history_search = s.history_search.clone();
        self.last_search = s
            .search
            .as_ref()
            .map(|e| (Filter::expr(e.clone()), s.search_back));
        self.detail_open = s.detail_open;
        self.show_hex = s.show_hex;
        self.show_types = s.show_types;
        self.detail_ratio = s.detail_ratio.unwrap_or(0.3);
        self.detail_hint = s.detail_path.clone();
        if s.focus_detail && s.detail_open {
            self.focus = Focus::Detail;
        }
        if !s.edits.is_empty() {
            // Первое окно уже запрошено без правок — перечитать с ними.
            let e = Edits::from_saved(&s.edits);
            match self.store.write(&e) {
                Ok(p) => {
                    self.status = format!(
                        "правки из сессии: {} в {} сообщ. — w зафиксировать, u снять",
                        e.fields(),
                        e.messages()
                    );
                    self.edits = e;
                    self.file.patch = p.clone();
                    self.worker.send(Cmd::Patch(p));
                    self.jump(At::Seq(i64::MIN), None);
                }
                Err(x) => self.status = format!("правки из сессии: {x}"),
            }
        }
        // Transform-код из сессии: применить до фильтра — воркер снимает копию
        // файла при `Cmd::Filter`, и первый скан обязан видеть прогоные тела
        // (`agr:filter-post-eval`). Если код был выключен тумблером — вернуть,
        // но без применения.
        self.lua_enabled = s.lua_transform_enabled.unwrap_or(true);
        if let Some(code) = &s.lua_transform {
            if !code.is_empty() {
                self.lua_code = code.clone();
                if self.lua_enabled {
                    self.worker.send(Cmd::Lua {
                        lua: Some(code.clone()),
                        lua_mode: Some(LuaMode::Transform),
                    });
                    self.lua_mode = LuaMode::Transform;
                    self.transform_pending = true;
                    self.status = format!(
                        "Lua transform из сессии: {} строк кода",
                        code.lines().count()
                    );
                } else {
                    self.status = format!(
                        "Lua transform из сессии выключен: {} строк кода",
                        code.lines().count()
                    );
                }
            }
        }
        let filter = Filter {
            messages: s.messages.clone(),
            expr: s.expr.clone(),
        };
        // Возврат к строке без фильтра — прыжок `show_seq`: воркер уже получил
        // transform-код, и окно по этому прыжку читается прогоным.
        let jumped = filter.is_empty() && s.seq.is_some();
        if !filter.is_empty() {
            self.set_filter(filter);
            self.filter_anchor = s.seq.filter(|&q| Some(q) != self.file.info.seq_begin);
        } else if let Some(q) = s.seq {
            self.show_seq(q);
        }
        // Иначе первое окно ушло воркеру до transform и осталось сырым —
        // перечитываем. Если прыжок уже был, перезапускать окно нельзя: выбор
        // ещё не пришёл (`selected()` пуст) и перечтение с начала файла стёрло
        // бы возврат к строке.
        if self.transform_pending && !jumped {
            // Перечитать окно уже прогоным: тела едут через transform.
            let land = self.selected().map(|r| r.seq);
            let top = self.rows.get(self.top).map(|r| r.seq).unwrap_or(i64::MIN);
            self.jump(At::Seq(top), land);
        }
    }

    /// Окно дочитано до конца файла: под последней строкой пусто не оставляем,
    /// пока выше есть строки — она стремится к низу экрана, а не к верху
    /// (`agr:anchor`). Колесо мыши (`scroll`) это не трогает: пустоту там
    /// пользователь делает сам.
    fn fill_bottom(&mut self) {
        if self.at_end {
            self.top = self.top.min(self.rows.len().saturating_sub(self.height));
        }
    }

    /// Видимых строк столько: вид узнаёт высоту при отрисовке, и выбор,
    /// поставленный по прежней высоте, возвращается на экран.
    pub fn set_height(&mut self, h: usize) {
        let h = h.max(1);
        if h != self.height {
            self.height = h;
            if self.sel >= self.top + h {
                self.top = self.sel + 1 - h;
            }
            self.fill_bottom();
            self.upkeep();
        }
    }

    /// Выбранная строка.
    pub fn selected(&self) -> Option<&RowView> {
        self.rows.get(self.sel)
    }

    /// Выбрать строку окна (щелчок мышью).
    pub fn select(&mut self, i: usize) {
        if i < self.rows.len() {
            self.sel = i;
            self.follow();
        }
    }

    /// Прокрутить на `d` строк без смены выбора (колесо мыши).
    pub fn scroll(&mut self, d: isize) {
        let max = self.rows.len().saturating_sub(1);
        self.top = (self.top as isize + d).clamp(0, max as isize) as usize;
        self.upkeep();
    }

    /// Сдвинуть выбор на `d` строк.
    fn step(&mut self, d: isize) {
        if self.rows.is_empty() {
            return;
        }
        let max = self.rows.len() as isize - 1;
        self.sel = (self.sel as isize + d).clamp(0, max) as usize;
        self.follow();
    }

    /// Держать выбранную строку на экране и дочитывать.
    fn follow(&mut self) {
        let h = self.height.max(1);
        if self.sel < self.top {
            self.top = self.sel;
        } else if self.sel >= self.top + h {
            self.top = self.sel + 1 - h;
        }
        self.upkeep();
        self.refresh_detail();
    }

    /// Применить transform-код Lua (режим Normal tll-lua): перечитать файл
    /// через `patch+[lua+]file://` без preset=filter, зовущий `tll_on_data` —
    /// тела в виде прогоные, ручная правка поверх мутации скрипта.
    pub fn set_transform(&mut self, code: String) {
        self.lua_code = code.clone();
        self.lua_mode = LuaMode::Transform;
        self.lua_enabled = true;
        // Фильтр (типы и выражение) не трогаем: он работает по сырому потоку,
        // transform — теневой прогон тел и им не мешает.
        self.transform_pending = true;
        self.mutations.clear();
        // Сообщить воркеру новый Lua-код.
        self.worker.send(Cmd::Lua {
            lua: Some(code),
            lua_mode: Some(LuaMode::Transform),
        });
        // Перечитать с нового места.
        let land = self.selected().map(|r| r.seq);
        let top = self.rows.get(self.top).map(|r| r.seq).unwrap_or(i64::MIN);
        self.jump(At::Seq(top), land);
        self.status = format!(
            "Lua transform: {} строк кода",
            self.lua_code.lines().count()
        );
    }

    /// Включить/выключить transform-код одной кнопкой, не трогая написанный
    /// код: выключенный код отменяется у воркера, но остаётся в [`Desk::lua_code`].
    pub fn toggle_transform(&mut self) {
        self.lua_enabled = !self.lua_enabled;
        if self.lua_enabled && !self.lua_code.is_empty() {
            self.set_transform(self.lua_code.clone());
        } else if !self.lua_enabled {
            self.lua_mode = LuaMode::Filter;
            self.transform_pending = false;
            self.mutations.clear();
            self.worker.send(Cmd::Lua {
                lua: None,
                lua_mode: None,
            });
            let land = self.selected().map(|r| r.seq);
            let top = self.rows.get(self.top).map(|r| r.seq).unwrap_or(i64::MIN);
            self.jump(At::Seq(top), land);
            self.status = "transform выключен".into();
        }
    }

    /// Сбросить код редактора на шаблон по умолчанию (не применяя).
    pub fn reset_lua_to_default(&mut self) {
        self.lua_code = crate::lua::transform_template_for(self.file.info.backend);
        self.lua_cursor = 0;
        self.lua_scroll = 0;
        self.lua_reset_confirm = false;
        self.status = "Lua: код сброшен в дефолт".into();
    }

    /// Снять transform — вернуться к обычному чтению файла.
    pub fn clear_transform(&mut self) {
        self.lua_code.clear();
        self.lua_mode = LuaMode::Filter;
        self.lua_enabled = true;
        self.transform_pending = false;
        self.mutations.clear();
        self.worker.send(Cmd::Lua {
            lua: None,
            lua_mode: None,
        });
        let land = self.selected().map(|r| r.seq);
        let top = self.rows.get(self.top).map(|r| r.seq).unwrap_or(i64::MIN);
        self.jump(At::Seq(top), land);
        self.status = "transform снят".into();
    }

    /// Показать сообщение seq: в окне — выбрать, иначе новое окно вокруг.
    pub fn show_seq(&mut self, s: i64) {
        if let Some(i) = self.rows.iter().position(|r| r.seq == s) {
            self.sel = i;
            self.follow();
            return;
        }
        let from = match &self.matches {
            Some(m) => At::Row(m.row_of(s).saturating_sub(self.height / 3)),
            // Строки выше нужны сразу: без них выбранная встала бы на верхний край
            // экрана, а предыдущие лежали бы только за прокруткой (`agr:anchor`).
            None => At::Around(s, self.fetch_size()),
        };
        self.jump(from, Some(s));
    }

    /// Применить фильтр (пустой — снять).
    pub fn set_filter(&mut self, f: Filter) {
        self.filter = f;
        self.filter_id += 1;
        self.matches = None;
        self.status.clear();
        // В начале файла возвращаться некуда: туда и так встанет первое окно.
        self.filter_anchor = self
            .selected()
            .map(|r| r.seq)
            .filter(|&s| Some(s) != self.file.info.seq_begin);
        if self.filter.is_empty() {
            self.filter_pending = false;
            self.filter_anchor = None;
            self.worker.send(Cmd::Filter {
                id: self.filter_id,
                filter: Filter::default(),
            });
            let land = self.selected().map(|r| r.seq);
            let top = self.rows.get(self.top).map(|r| r.seq).unwrap_or(i64::MIN);
            self.jump(At::Seq(top), land);
        } else {
            self.filter_pending = true;
            self.worker.send(Cmd::Filter {
                id: self.filter_id,
                filter: self.filter.clone(),
            });
        }
    }

    /// Искать (внутри фильтра) от выбранной строки.
    pub fn search(&mut self, f: Filter, back: bool) {
        let from = self
            .selected()
            .map(|r| r.seq)
            .or(self.file.info.seq_begin)
            .unwrap_or(0);
        self.search_id += 1;
        self.searching = true;
        self.status = "ищу…".into();
        self.last_search = Some((f.clone(), back));
        let f = self.filter.and(&f);
        self.worker.send(Cmd::Search {
            id: self.search_id,
            filter: f,
            from,
            back,
        });
    }

    /// Повторить поиск: `same` — в ту же сторону.
    fn search_again(&mut self, same: bool) {
        match self.last_search.clone() {
            Some((f, back)) => {
                let back = if same { back } else { !back };
                let keep = self.last_search.clone();
                self.search(f, back);
                self.last_search = keep;
            }
            None => self.status = "поиска ещё не было: /".into(),
        }
    }

    /// Применить пресет колонок.
    pub fn use_preset(&mut self, i: usize) {
        if let Some(p) = self.presets.get(i) {
            self.preset = i;
            self.columns = p.columns.clone();
            self.recompute_cells();
        }
    }

    /// Включить/выключить колонку поля.
    pub fn toggle_column(&mut self, path: &str) {
        match self.columns.iter().position(|c| c == path) {
            Some(i) => {
                self.columns.remove(i);
            }
            None => self.columns.push(path.to_string()),
        }
        self.recompute_cells();
    }

    fn recompute_cells(&mut self) {
        self.hscroll = self.hscroll.min(self.columns.len().saturating_sub(1));
        let scheme = self.file.scheme.clone();
        for r in &mut self.rows {
            let st = scheme.message(r.msgid);
            r.cells = self
                .columns
                .iter()
                .map(|c| st.and_then(|s| decode::field(&scheme, s, &r.data, c)))
                .collect();
        }
    }

    /// Перейти по тексту `g`: seq, `+N` — строка, `-N` — от конца.
    pub fn goto(&mut self, text: &str) {
        match Border::parse(text) {
            Ok(Border::Seq(s)) => self.show_seq(s),
            Ok(Border::Count(n)) => {
                if self.total().is_none() {
                    self.status = "номер строки — после индекса".into();
                    return;
                }
                let n = n as usize;
                self.jump(At::Row(n.saturating_sub(self.height / 3)), None);
                self.land_row = Some(n);
            }
            Ok(Border::FromLast(n)) => match self.file.info.seq_last {
                Some(l) => self.show_seq(l - n as i64),
                None => self.status = "файл пуст".into(),
            },
            Err(e) => self.status = e,
        }
    }

    /// Перечитать файл заново на том же месте: растущий `*.current.dat` при
    /// переоткрытии отдаёт хвост, дописанный после открытия (`agr:reload`).
    /// Курсор остаётся на прежнем seq (или ближайшем после него, если его уже
    /// нет), окно держится там, где было. Файл не вырос — пересборка вида ни к
    /// чему, и её не делаем: так `↓`/`PgDn` на последней строке неподвижного
    /// файла остаются дешёвым «ничего не происходит», а не заново пересоздают
    /// воркер и его потоки. Фильтр, поиск, колонки, правки и transform
    /// возвращаются из сессии — как при [`Desk::reopen`], только файл тот же и
    /// правки не сбрасываются.
    pub fn reload(&mut self) {
        self.reload_to(None);
    }

    /// Перечитать растущий файл; `land` — куда встать выбором после
    /// перечитывания: `Some(i64::MAX)` — на новую последнюю строку, `None` —
    /// остаться на прежнем seq (`agr:reload`).
    fn reload_to(&mut self, land: Option<i64>) {
        let saved = self.file.path.clone();
        let old_last = self.file.last();
        let Ok(file) = File::open(&saved) else {
            self.status = "обновить не удалось".into();
            return;
        };
        if file.last() <= old_last {
            // Хвост с прошлого открытия не дописывался: перечитывать нечего.
            self.status = "файл не изменился".into();
            return;
        }
        // Сколько дописалось: считаем сообщения хвоста за прежним последним seq.
        let extra = count_tail(&saved, old_last, file.last());
        let mut keep = self.session();
        if land == Some(i64::MAX) {
            // Для «встать в конец» не цепляемся за прежний seq: `restore` иначе
            // прыгнет к нему (`show_seq` → `At::Around`), и это окно посреди
            // файла перебьёт финальный `jump(At::End)`.
            keep.seq = None;
        }
        // `open_with` пересоздаёт Desk и сбрасывает высоту на 30: вернём её,
        // иначе `jump(At::End)` рассчитает `top`/`sel` не по той высоте.
        let height = self.height;
        let detail_height = self.detail_height;
        self.file_gen
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *self = Desk::open_with(
            file,
            self.notify.clone(),
            self.act_tx.clone(),
            self.acts.take(),
            self.file_gen.clone(),
        );
        self.height = height;
        self.detail_height = detail_height;
        self.restore(&keep);
        if let Some(l) = land {
            // Встать на новую последнюю строку: то же, что обычный `End`, но
            // после перечитывания хвоста.
            self.jump(At::End, Some(l));
        }
        // Курсор — на прежнем месте: `restore` вернул выбор по сохранённому seq
        // (или ближайшему после него), окно — с контекстом выше (`At::Around`).
        self.status = match extra {
            0 => "файл перечитан".into(),
            1 => "файл перечитан: +1 строка".into(),
            n => format!("файл перечитан: +{n} строк"),
        };
    }

    /// Выбор на последней строке и окно у конца файла: `↓`/`PgDn` отсюда —
    /// перечитать хвост растущего файла (`agr:reload`), а не шаг в никуда.
    /// В середине файла последняя строка окна — ещё не конец: там шаг просто
    /// дочитывает дальше.
    fn at_last_row(&self) -> bool {
        self.at_end && !self.rows.is_empty() && self.sel + 1 == self.rows.len()
    }

    /// Обработать клавишу.
    pub fn key(&mut self, k: Key) {
        if self.help.open || k == Key::Char('\u{F1}') {
            self.help_key(k);
            return;
        }
        if self.commit.is_some() {
            self.commit_key(k);
            return;
        }
        if self.mass_edit.is_some() {
            self.mass_key(k);
            return;
        }
        if self.prompt.is_some() {
            self.prompt_key(k);
            return;
        }
        if self.focus == Focus::Detail && self.detail_open && self.detail_key(k) {
            return;
        }
        let page = self.height.max(1) as isize;
        match k {
            Key::Up | Key::Char('k') => self.step(-1),
            Key::Down | Key::Char('j') => {
                if self.at_last_row() {
                    // На последней строке: прежде чем шагнуть в никуда — дочитать
                    // хвост растущего файла (`agr:reload`).
                    self.reload();
                } else {
                    self.step(1);
                }
            }
            Key::PageUp => {
                self.top = self.top.saturating_sub(page as usize);
                self.step(-page)
            }
            Key::PageDown => {
                if self.at_last_row() {
                    self.reload();
                } else {
                    self.top = (self.top + page as usize).min(self.rows.len().saturating_sub(1));
                    self.step(page);
                    // У конца файла страница не уводит последнюю строку на верх экрана.
                    self.fill_bottom();
                }
            }
            Key::Left | Key::Char('h') => self.hscroll = self.hscroll.saturating_sub(1),
            Key::Right | Key::Char('l') => {
                self.hscroll = (self.hscroll + 1).min(self.columns.len().saturating_sub(1))
            }
            Key::Home => self.jump(
                if self.matches.is_some() {
                    At::Row(0)
                } else {
                    At::Seq(i64::MIN)
                },
                None,
            ),
            Key::End => {
                if self.at_last_row() {
                    // Уже на последней строке: прежде чем прыгнуть «в конец» —
                    // перечитать хвост и встать на новую последнюю строку
                    // (`agr:reload`).
                    self.reload_to(Some(i64::MAX));
                } else {
                    // Обновление файла — только с последней строки: из середины
                    // `End` — навигация внутри открытого снимка, без диска.
                    // Держит тест `end_from_middle_stays_within_the_open_snapshot`.
                    self.jump(At::End, Some(i64::MAX));
                }
            }
            Key::Enter => {
                self.detail_open = !self.detail_open;
                if !self.detail_open && self.focus == Focus::Detail {
                    self.focus = Focus::Table;
                }
                self.detail = None;
                self.refresh_detail();
            }
            Key::Tab | Key::BackTab if self.detail_open => {
                self.focus = if self.focus == Focus::Table {
                    Focus::Detail
                } else {
                    Focus::Table
                };
            }
            Key::Esc => {
                if self.searching {
                    self.searching = false;
                    self.worker.send(Cmd::StopSearch);
                    self.status = "поиск остановлен".into();
                } else if self.detail_open {
                    self.detail_open = false;
                    self.focus = Focus::Table;
                }
            }
            Key::Char('f') => {
                self.prompt = Some(Prompt::line(PromptKind::Filter, self.filter.expr.clone()))
            }
            Key::Char('F') => self.set_filter(Filter {
                messages: self.filter.messages.clone(),
                expr: String::new(),
            }),
            Key::Char('/') => {
                self.prompt = Some(Prompt::line(
                    PromptKind::Search { back: false },
                    String::new(),
                ))
            }
            Key::Char('?') => {
                self.prompt = Some(Prompt::line(
                    PromptKind::Search { back: true },
                    String::new(),
                ))
            }
            Key::Char('n') => self.search_again(true),
            Key::Char('N') => self.search_again(false),
            Key::Char('g') => self.prompt = Some(Prompt::line(PromptKind::Goto, String::new())),
            Key::Char('t') => self.open_types(),
            Key::Char('c') => self.open_columns(),
            Key::Char('r') => self.reload(),
            Key::Char('x') => self.show_hex = !self.show_hex,
            Key::Char('i') => self.show_types = !self.show_types,
            // E — включить/выключить transform-код, не стирая его (правка — F4).
            Key::Char('E') if !self.lua_code.is_empty() => self.toggle_transform(),
            Key::Char('p') => {
                let n = self.presets.len().max(1);
                self.use_preset((self.preset + 1) % n);
                self.status = format!(
                    "колонки: {}",
                    self.presets
                        .get(self.preset)
                        .map(|p| p.name.as_str())
                        .unwrap_or("")
                );
            }
            Key::Char('w') => self.open_commit(),
            Key::Char('q') => self.quit = true,
            Key::Char('{') => {
                self.detail_ratio = (self.detail_ratio - 0.05).max(0.05);
                self.status = format!("высота детали: {:.0}%", self.detail_ratio * 100.0);
                if let Some(d) = &mut self.detail {
                    d.top = d.cursor.saturating_sub(self.detail_height / 2);
                }
            }
            Key::Char('}') => {
                self.detail_ratio = (self.detail_ratio + 0.05).min(0.8);
                self.status = format!("высота детали: {:.0}%", self.detail_ratio * 100.0);
                if let Some(d) = &mut self.detail {
                    d.top = d.cursor.saturating_sub(self.detail_height / 2);
                }
            }
            _ => {}
        }
    }

    /// Клавиши в детали; `false` — пусть обработает таблица.
    fn detail_key(&mut self, k: Key) -> bool {
        let h = self.detail_height.max(1);
        let Some(d) = &mut self.detail else {
            return false;
        };
        let n = d.lines().len();
        match k {
            Key::Up | Key::Char('k') => d.cursor = d.cursor.saturating_sub(1),
            Key::Down | Key::Char('j') => d.cursor = (d.cursor + 1).min(n.saturating_sub(1)),
            Key::PageUp => d.cursor = d.cursor.saturating_sub(h),
            Key::PageDown => d.cursor = (d.cursor + h).min(n.saturating_sub(1)),
            Key::Home => d.cursor = 0,
            Key::End => d.cursor = n.saturating_sub(1),
            Key::Enter | Key::Char('e') => {
                self.edit_field();
                return true;
            }
            Key::Char('M') => {
                self.edit_field_mass();
                return true;
            }
            Key::Char('u') => {
                self.unset_field();
                return true;
            }
            Key::Char('a') => {
                if let Some(path) = d
                    .current()
                    .filter(|c| c.value.is_some() && !c.path.contains('['))
                    .map(|c| c.path.clone())
                {
                    self.toggle_column(&path);
                    self.status = format!("колонка {path}");
                }
                return true;
            }
            _ => return false,
        }
        if d.cursor < d.top {
            d.top = d.cursor;
        } else if d.cursor >= d.top + h {
            d.top = d.cursor + 1 - h;
        }
        self.detail_follow = true;
        true
    }

    fn open_types(&mut self) {
        let hist = self.index.as_ref().map(|i| &i.hist);
        let items = self
            .file
            .scheme
            .messages()
            .filter(|m| hist.is_none_or(|h| h.contains_key(&m.msgid)))
            .map(|m| Pick {
                label: m.name.clone(),
                hint: hist
                    .and_then(|h| h.get(&m.msgid))
                    .map(|c| c.to_string())
                    .unwrap_or_default(),
                on: self.filter.messages.contains(&m.name),
                value: None,
            })
            .collect();
        self.prompt = Some(Prompt {
            items,
            ..Prompt::line(PromptKind::Types, String::new())
        });
    }

    fn open_columns(&mut self) {
        let mut items: Vec<Pick> = self
            .columns
            .iter()
            .map(|c| Pick {
                label: c.clone(),
                hint: String::new(),
                on: true,
                value: None,
            })
            .collect();
        for (p, n) in columns::all_paths(&self.file.scheme) {
            if !self.columns.contains(&p) {
                items.push(Pick {
                    label: p,
                    hint: format!("{n} типов"),
                    on: false,
                    value: None,
                });
            }
        }
        self.prompt = Some(Prompt {
            items,
            ..Prompt::line(PromptKind::Columns, String::new())
        });
    }

    fn prompt_key(&mut self, k: Key) {
        let Some(p) = &mut self.prompt else { return };
        let edit = matches!(
            p.kind,
            PromptKind::Edit {
                kind: EditKind::Enum | EditKind::Bits,
                ..
            } | PromptKind::MassEdit {
                kind: EditKind::Enum | EditKind::Bits,
                ..
            }
        );
        let list = matches!(p.kind, PromptKind::Types | PromptKind::Columns) || edit;
        match k {
            Key::Esc => {
                // Выбор типов применяется и по Esc: галочки уже стоят.
                let p = self.prompt.take().unwrap();
                if p.kind == PromptKind::Types {
                    self.apply_types(&p);
                }
            }
            Key::Enter if !list => {
                let p = self.prompt.take().unwrap();
                self.submit(p);
            }
            Key::Enter if list => match p.kind {
                PromptKind::Edit { .. } | PromptKind::MassEdit { .. } => {
                    let p = self.prompt.take().unwrap();
                    self.submit(p);
                }
                _ => {
                    let vis = p.visible();
                    if let Some(&(i, _)) = vis.get(p.sel) {
                        match p.kind {
                            PromptKind::Columns => {
                                p.items[i].on = !p.items[i].on;
                                let path = p.items[i].label.clone();
                                self.toggle_column(&path);
                            }
                            PromptKind::Types => p.items[i].on = !p.items[i].on,
                            _ => {}
                        }
                    }
                }
            },
            Key::Char(' ') if list => {
                let vis = p.visible();
                let kinds = p.kind;
                if let Some(&(i, _)) = vis.get(p.sel) {
                    match kinds {
                        PromptKind::Columns => {
                            p.items[i].on = !p.items[i].on;
                            let path = p.items[i].label.clone();
                            self.toggle_column(&path);
                        }
                        PromptKind::Types => p.items[i].on = !p.items[i].on,
                        PromptKind::Edit {
                            kind: EditKind::Enum,
                            ..
                        }
                        | PromptKind::MassEdit {
                            kind: EditKind::Enum,
                            ..
                        } => {
                            // Один выбор: снять все, поставить этот.
                            for it in &mut p.items {
                                it.on = false;
                            }
                            p.items[i].on = true;
                            if let Some(v) = p.items[i].value {
                                // Enum: всегда десятичное число
                                p.text = v.to_string();
                                p.cursor = p.text.chars().count();
                            }
                        }
                        PromptKind::Edit {
                            kind: EditKind::Bits,
                            ..
                        }
                        | PromptKind::MassEdit {
                            kind: EditKind::Bits,
                            ..
                        } => {
                            p.items[i].on = !p.items[i].on;
                            // Bits: собираем по маскам (каждая item.value — это маска одного флага)
                            let raw: u64 = p
                                .items
                                .iter()
                                .filter(|it| it.on)
                                .filter_map(|it| it.value)
                                .fold(0u64, |acc, m| acc | (m as u64));
                            p.text = format!("{raw:#x}");
                            p.cursor = p.text.chars().count();
                        }
                        _ => {}
                    }
                }
            }
            Key::Up if list => p.sel = p.sel.saturating_sub(1),
            Key::Down if list => p.sel = (p.sel + 1).min(p.visible().len().saturating_sub(1)),
            Key::PageUp if list => p.sel = p.sel.saturating_sub(10),
            Key::PageDown if list => p.sel = (p.sel + 10).min(p.visible().len().saturating_sub(1)),
            Key::Up | Key::Down => {
                let hist = match p.kind {
                    PromptKind::Filter => &self.history_filter,
                    PromptKind::Search { .. } => &self.history_search,
                    _ => return,
                };
                let pos = hist.iter().position(|h| *h == p.text);
                let next = match (k, pos) {
                    (Key::Up, None) => hist.len().checked_sub(1),
                    (Key::Up, Some(i)) => i.checked_sub(1).or(Some(i)),
                    (Key::Down, Some(i)) if i + 1 < hist.len() => Some(i + 1),
                    _ => None,
                };
                if let Some(i) = next {
                    p.text = hist[i].clone();
                    p.cursor = p.text.chars().count();
                }
            }
            _ => {
                p.edit(k);
                if list {
                    p.sel = 0;
                    p.top = 0;
                }
            }
        }
    }

    fn apply_types(&mut self, p: &Prompt) {
        let messages: Vec<String> = p
            .items
            .iter()
            .filter(|i| i.on)
            .map(|i| i.label.clone())
            .collect();
        if messages != self.filter.messages {
            self.set_filter(Filter {
                messages,
                expr: self.filter.expr.clone(),
            });
        }
    }

    fn submit(&mut self, p: Prompt) {
        let text = p.text.trim().to_string();
        let remember = |h: &mut Vec<String>| {
            if !text.is_empty() {
                h.retain(|x| *x != text);
                h.push(text.clone());
            }
        };
        match p.kind {
            PromptKind::Filter => {
                remember(&mut self.history_filter);
                self.set_filter(Filter {
                    messages: self.filter.messages.clone(),
                    expr: text,
                });
            }
            PromptKind::Search { back } => {
                if text.is_empty() {
                    return;
                }
                remember(&mut self.history_search);
                self.search(Filter::expr(text), back);
            }
            PromptKind::Goto => self.goto(&text),
            PromptKind::Edit { seq, .. } => self.propose(seq, &p.path, &text, p.clone()),
            PromptKind::MassEdit { kind } => {
                // Сохранить состояние массовой правки — дальше диалог подтверждения.
                let Some(d) = &self.detail else { return };
                let Some(n) = d.current() else { return };
                let old = match kind {
                    EditKind::Bits => {
                        format!("{:#x}", self.field_raw_u64(&p.path).unwrap_or(0))
                    }
                    EditKind::Enum => self.field_raw_u64(&p.path).unwrap_or(0).to_string(),
                    _ => n.text(),
                };
                self.mass_edit = Some(MassEdit {
                    path: p.path.clone(),
                    new_value: text.clone(),
                    old_value: old,
                    name: d.root.name.clone(),
                    kind,
                    count: 0,
                    pairs: Vec::new(),
                });
                // Запустить сканирование всех сообщений с нужным полем.
                self.scan_id += 1;
                self.scan_pending = true;
                let seeds = self
                    .matches
                    .as_ref()
                    .map(|m| m.seqs.read().unwrap().clone());
                let field_path = p.path.clone();
                let scheme = self.file.scheme.clone();
                self.worker.send(Cmd::MassScan {
                    id: self.scan_id,
                    seeds,
                    begin: self.file.begin(),
                    last: self.file.last(),
                    field_path,
                    scheme,
                });
                self.status = "массовая правка: подсчитываю…".into();
            }
            PromptKind::Types | PromptKind::Columns => {}
        }
    }
}

/// Первая строка ошибки libtll — для строки состояния.
fn one_line(e: &str) -> String {
    e.lines().next().unwrap_or("").to_string()
}

/// Сколько сообщений дописалось в файл после последнего открытого seq:
/// читаем строго хвост за `old_last` и считаем, не держа тела в памяти.
fn count_tail(path: &std::path::Path, old_last: i64, new_last: i64) -> usize {
    let ctx = Context::new();
    let Ok(r) = Reader::open(
        &ctx,
        &Open::file(path).at(old_last.saturating_add(1)).meta(),
    ) else {
        return new_last.saturating_sub(old_last).max(0) as usize;
    };
    r.filter(|m| m.seq > old_last).count()
}

/// Править можно скаляр вне списков и без ошибки разбора.
pub fn editable(n: &Node) -> bool {
    n.value.is_some() && n.error.is_none() && !n.path.contains('[')
}

/// Правка полей и фиксация.
impl Desk {
    /// Seq сообщения в файле, если мутатор его поменял: `seq` — тот, что в
    /// виде, ответ — исходный (`agr:transform-shadow`).
    pub fn seq_in_file(&self, seq: i64) -> Option<i64> {
        self.mutated(seq, crate::data::SEQ_PATH)?.parse().ok()
    }

    /// Исходный seq строки: правки (`patch`) адресуются им, а не новым.
    pub fn orig_seq(&self, seq: i64) -> i64 {
        self.seq_in_file(seq).unwrap_or(seq)
    }

    /// Поле сообщения `seq` правлено.
    pub fn edited(&self, seq: i64, path: &str) -> bool {
        self.edits.get(self.orig_seq(seq), path).is_some()
    }

    /// В сообщении `seq` есть правки.
    pub fn edited_row(&self, seq: i64) -> bool {
        self.edits.message(self.orig_seq(seq)).is_some()
    }

    /// Тело сообщения `seq` менял мутатор (или выбросило его) —
    /// `agr:transform-shadow`. Само тело в виде уже прогоное.
    pub fn mutated_row(&self, seq: i64) -> bool {
        self.mutations.contains_key(&seq)
    }

    /// Значение поля в файле, до мутатора, если поле менялось.
    pub fn mutated(&self, seq: i64, path: &str) -> Option<&str> {
        self.mutations.get(&seq)?.get(path).map(String::as_str)
    }

    /// Начать правку поля под курсором детали.
    pub fn edit_field(&mut self) {
        if self.commit.is_some() || self.pending.is_some() {
            return;
        }
        let Some(d) = &self.detail else { return };
        let Some(n) = d.current() else { return };
        if !editable(n) {
            self.status = if n.path.contains('[') {
                "элементы списков пока не правятся".into()
            } else {
                format!("{} — не значение, править нечего", n.path)
            };
            return;
        }
        let (edit_kind, items, text) = self.edit_state(n);
        let mut p = Prompt::line(
            PromptKind::Edit {
                seq: self.orig_seq(d.seq),
                kind: edit_kind,
            },
            text.clone(),
        );
        p.path = n.path.clone();
        p.items = items;

        // Выбрать позицию в пикере так, чтобы известное текущее значение было видно как "on".
        // Для Enum — поставить курсор на единственный включённый пункт (если есть).
        // Для Bits — поставить на первый включённый (или 0).
        let vis = p.visible();
        if !vis.is_empty() {
            let target = if matches!(edit_kind, EditKind::Enum) {
                vis.iter().position(|(_, it)| it.on)
            } else {
                vis.iter().position(|(_, it)| it.on).or(Some(0))
            };
            if let Some(pos) = target {
                p.sel = pos;
            }
        }

        self.status = format!("{}: Enter — принять, Esc — отмена", n.path);
        self.prompt = Some(p);
    }

    /// Начать массовую правку поля под курсором детали — тот же prompt, что и
    /// для одиночной, но с пометкой `MassEdit`.
    pub fn edit_field_mass(&mut self) {
        if self.commit.is_some() || self.pending.is_some() {
            return;
        }
        let Some(d) = &self.detail else { return };
        let Some(n) = d.current() else { return };
        if !editable(n) {
            self.status = if n.path.contains('[') {
                "элементы списков пока не правятся".into()
            } else {
                format!("{} — не значение, править нечего", n.path)
            };
            return;
        }
        let (edit_kind, items, text) = self.edit_state(n);
        let mut p = Prompt::line(PromptKind::MassEdit { kind: edit_kind }, text.clone());
        p.path = n.path.clone();
        p.items = items;
        let vis = p.visible();
        if !vis.is_empty() {
            let target = if matches!(edit_kind, EditKind::Enum) {
                vis.iter().position(|(_, it)| it.on)
            } else {
                vis.iter().position(|(_, it)| it.on).or(Some(0))
            };
            if let Some(pos) = target {
                p.sel = pos;
            }
        }
        self.status = format!("массовая правка {}: Enter — принять, Esc — отмена", n.path);
        self.prompt = Some(p);
    }

    /// Допустимые значения и начальный текст для правки поля.
    fn edit_state(&self, n: &Node) -> (EditKind, Vec<Pick>, String) {
        let Some(kind) = self.field_kind(&n.path) else {
            return (EditKind::Text, vec![], n.text());
        };
        match kind {
            Kind::Int(_, Sub::Enum(ref e)) => {
                let current: i64 = match &n.value {
                    Some(crate::decode::Value::Enum { raw, .. }) => *raw,
                    _ => n.text().parse().unwrap_or(0),
                };
                let items: Vec<Pick> = e
                    .values
                    .iter()
                    .map(|(name, val)| Pick {
                        label: name.clone(),
                        hint: String::new(),
                        on: *val == current,
                        value: Some(*val),
                    })
                    .collect();
                // Always numeric (decimal) in the text field for Enum
                let text = current.to_string();
                (EditKind::Enum, items, text)
            }
            Kind::Int(_, Sub::Bits(ref b)) => {
                let full = self.field_raw_u64(&n.path).unwrap_or(0);
                let items: Vec<Pick> = b
                    .fields
                    .iter()
                    .map(|(name, off, w)| {
                        let mask = if *w >= 64 { u64::MAX } else { (1u64 << w) - 1 };
                        let extracted = (full >> off) & mask;
                        Pick {
                            label: name.clone(),
                            hint: format!("{}+{}", off, w),
                            on: extracted != 0,
                            value: Some((mask << off) as i64),
                        }
                    })
                    .collect();
                // Always hex in the text field for Bits
                let text = format!("{full:#x}");
                (EditKind::Bits, items, text)
            }
            _ => (EditKind::Text, vec![], n.text()),
        }
    }

    /// Прочитать исходное целое (как битовый шаблон) для скалярного Int-поля по пути.
    /// Возвращает u64 представление байт поля (little-endian).
    /// Учитывает текущую деталь (если открыта) — берём данные из строки, соответствующей seq детали.
    fn field_raw_u64(&self, path: &str) -> Option<u64> {
        let seq = self.detail.as_ref().map(|d| d.seq);
        let row = seq
            .and_then(|s| self.rows.iter().find(|r| r.seq == s))
            .or_else(|| self.selected())?;
        let data = &row.data;
        let msg = self.file.scheme.message(row.msgid)?;
        let (off, f) = self.file.scheme.field_path(msg, path)?;
        if let Kind::Int(_it, _) = &f.kind {
            let sz = f.size.min(8);
            if off + sz > data.len() {
                return None;
            }
            let mut buf = [0u8; 8];
            buf[..sz].copy_from_slice(&data[off..off + sz]);
            Some(u64::from_le_bytes(buf))
        } else {
            None
        }
    }

    /// Тип [`Kind`] поля по пути `header.status` в сообщении детали (или выделенной строке).
    fn field_kind(&self, path: &str) -> Option<Kind> {
        let seq = self.detail.as_ref().map(|d| d.seq);
        let r = seq
            .and_then(|s| self.rows.iter().find(|r| r.seq == s))
            .or_else(|| self.selected())?;
        let msg = self.file.scheme.message(r.msgid)?;
        let (_, field) = self.file.scheme.field_path(msg, path)?;
        Some(field.kind.clone())
    }

    /// Новое значение поля: проверить прогоном tll-patch, потом наложить.
    fn propose(&mut self, seq: i64, path: &str, text: &str, prompt: Prompt) {
        let Some(d) = self.detail.as_ref().filter(|d| d.seq == seq) else {
            self.status = "сообщение ушло с экрана — правка не принята".into();
            return;
        };
        let cur = match &prompt.kind {
            PromptKind::Edit {
                kind: EditKind::Bits,
                ..
            } => {
                format!("{:#x}", self.field_raw_u64(path).unwrap_or(0))
            }
            PromptKind::Edit {
                kind: EditKind::Enum,
                ..
            } => self.field_raw_u64(path).unwrap_or(0).to_string(),
            _ => {
                let Some(v) = d
                    .root
                    .walk()
                    .into_iter()
                    .find(|(_, n)| n.path == path)
                    .map(|(_, n)| n.text())
                else {
                    return;
                };
                v
            }
        };
        let name = d.root.name.clone();
        let mut e = self.edits.clone();
        e.set(seq, &name, path, &cur, text);
        if e == self.edits {
            self.status.clear();
            return;
        }
        let patch = match self.store.write(&e) {
            Ok(p) => p,
            Err(x) => {
                self.status = x;
                return;
            }
        };
        let Some(patch) = patch else {
            // Правок не осталось — проверять нечего.
            self.status = "правок нет".into();
            self.apply(e, None);
            return;
        };
        self.check_id += 1;
        self.pending = Some(Pending {
            id: self.check_id,
            edits: e,
            patch: patch.clone(),
            prompt,
        });
        self.status = "проверяю правку…".into();
        self.worker.send(Cmd::Check {
            id: self.check_id,
            patch,
            seq,
        });
    }

    /// Снять правку поля под курсором детали.
    fn unset_field(&mut self) {
        if self.commit.is_some() || self.pending.is_some() {
            return;
        }
        let Some((seq, path)) = self
            .detail
            .as_ref()
            .and_then(|d| d.current().map(|n| (d.seq, n.path.clone())))
        else {
            return;
        };
        let mut e = self.edits.clone();
        if !e.unset(seq, &path) {
            self.status = format!("{path} не правлено");
            return;
        }
        match self.store.write(&e) {
            Ok(p) => {
                self.status = format!("правка {path} снята");
                self.apply(e, p);
            }
            Err(x) => self.status = x,
        }
    }

    /// Наложить правки: вид, фильтр и деталь перечитываются через новый yaml.
    fn apply(&mut self, e: Edits, patch: Option<PathBuf>) {
        self.edits = e;
        self.file.patch = patch.clone();
        self.worker.send(Cmd::Patch(patch));
        // Деталь разберётся заново из перечитанного окна — на том же поле.
        if let Some(path) = self
            .detail
            .take()
            .and_then(|d| d.current().map(|n| n.path.clone()))
        {
            self.detail_hint = Some(path);
        }
        if self.filter.is_empty() {
            let land = self.selected().map(|r| r.seq);
            let top = self.rows.get(self.top).map(|r| r.seq).unwrap_or(i64::MIN);
            self.jump(At::Seq(top), land);
        } else {
            // Правка могла ввести сообщение в фильтр или вывести из него.
            let status = std::mem::take(&mut self.status);
            self.set_filter(self.filter.clone());
            self.status = status;
        }
    }

    /// Открыть диалог фиксации.
    pub fn open_commit(&mut self) {
        let transform =
            self.lua_enabled && !self.lua_code.is_empty() && self.lua_mode == LuaMode::Transform;
        if self.edits.is_empty() && !transform {
            self.status = "правок нет — фиксировать нечего".into();
            return;
        }
        if self.pending.is_some() {
            return;
        }
        let path = &self.file.path;
        self.commit = Some(Commit {
            mode: CommitMode::NewFile,
            new_name: commit::free_name(path, "patched").display().to_string(),
            backup_name: commit::free_name(path, "orig").display().to_string(),
            summary: self.edits.summary(),
            yaml: self.edits.yaml(),
            running: None,
            error: None,
            scroll: 0,
        });
    }

    /// Начать запись по диалогу.
    pub fn start_commit(&mut self) {
        let Some(c) = &mut self.commit else { return };
        if c.running.is_some() {
            return;
        }
        let target = c.target();
        if let Err(e) = commit::check(&self.file.path, &target) {
            c.error = Some(e);
            return;
        }
        let send_code =
            self.lua_enabled && !self.lua_code.is_empty() && self.lua_mode == LuaMode::Transform;
        let patch = match self.store.write(&self.edits) {
            Ok(Some(p)) => Some(p),
            Ok(None) => {
                if !send_code {
                    // Нет ни правок полей, ни transform-кода — писать нечего.
                    c.error = Some("нечего фиксировать".into());
                    return;
                }
                None
            }
            Err(e) => {
                c.error = Some(e);
                return;
            }
        };
        c.error = None;
        c.running = Some(0.0);
        self.worker.send(Cmd::Commit {
            patch,
            target,
            lua_code: if send_code {
                Some(self.lua_code.clone())
            } else {
                None
            },
            lua_mode: if send_code { Some(self.lua_mode) } else { None },
        });
    }

    /// Клавиши диалога фиксации.
    /// Клавиши справки: глотает всё, листается, закрывается `F1`/`Esc`.
    /// Позиция листания переживает закрытие.
    fn help_key(&mut self, k: Key) {
        if !self.help.open {
            if k == Key::Char('\u{F1}') {
                self.help.open = true;
            }
            return;
        }
        let h = &mut self.help;
        match k {
            Key::Char('\u{F1}') | Key::Esc => h.open = false,
            Key::Up => {
                h.scroll = h.scroll.saturating_sub(1);
                h.keyboard = true;
            }
            Key::Down => {
                h.scroll += 1;
                h.keyboard = true;
            }
            Key::PageUp => {
                h.scroll = h.scroll.saturating_sub(10);
                h.keyboard = true;
            }
            Key::PageDown => {
                h.scroll += 10;
                h.keyboard = true;
            }
            Key::Home => {
                h.scroll = 0;
                h.keyboard = true;
            }
            // Рендер ограничит снизу по числу строк.
            Key::End => {
                h.scroll = usize::MAX;
                h.keyboard = true;
            }
            _ => {}
        }
    }

    fn commit_key(&mut self, k: Key) {
        let Some(c) = &mut self.commit else { return };
        if c.running.is_some() {
            return;
        }
        match k {
            Key::Esc => self.commit = None,
            Key::Enter => self.start_commit(),
            Key::Tab | Key::BackTab => {
                c.mode = match c.mode {
                    CommitMode::NewFile => CommitMode::Overwrite,
                    CommitMode::Overwrite => CommitMode::NewFile,
                };
                c.error = None;
            }
            Key::Up => c.scroll = c.scroll.saturating_sub(1),
            Key::Down => c.scroll += 1,
            Key::PageUp => c.scroll = c.scroll.saturating_sub(10),
            Key::PageDown => c.scroll += 10,
            Key::Backspace => {
                c.name_mut().pop();
                c.error = None;
            }
            Key::Char(ch) => {
                c.name_mut().push(ch);
                c.error = None;
            }
            _ => {}
        }
    }

    /// Клавиши диалога массовой правки.
    fn mass_key(&mut self, k: Key) {
        match k {
            Key::Esc => {
                self.mass_edit = None;
                self.mass_pairs = None;
                self.status = "массовая правка отменена".into();
            }
            Key::Enter => {
                let Some(me) = &self.mass_edit else { return };
                if me.pairs.is_empty() {
                    self.status = "нет сообщений для массовой правки".into();
                    return;
                }
                self.apply_mass_edit();
            }
            _ => {}
        }
    }

    /// Применить правку ко всем seq из массового сканирования.
    pub fn apply_mass_edit(&mut self) {
        let Some(me) = self.mass_edit.take() else {
            return;
        };
        let mut e = self.edits.clone();
        for (seq, name) in &me.pairs {
            e.set(*seq, name, &me.path, &me.old_value, &me.new_value);
        }
        if e == self.edits {
            self.status = "правок нет".into();
            return;
        }
        let patch = match self.store.write(&e) {
            Ok(p) => p,
            Err(x) => {
                self.status = x;
                return;
            }
        };
        let Some(patch) = patch else {
            self.status = "правок нет".into();
            self.apply(e, None);
            return;
        };
        // Проверяем выбранное сообщение через tll-patch.
        let seq = self
            .detail
            .as_ref()
            .map(|d| d.seq)
            .or_else(|| self.selected().map(|r| r.seq))
            .unwrap_or(0);
        self.check_id += 1;
        self.pending = Some(Pending {
            id: self.check_id,
            edits: e,
            patch: patch.clone(),
            prompt: Prompt::line(PromptKind::Columns, String::new()),
        });
        self.status = "проверяю правку…".into();
        self.worker.send(Cmd::Check {
            id: self.check_id,
            patch,
            seq,
        });
        self.mass_pairs = None;
    }

    /// Открыть записанный файл вместо этого: вид (колонки, фильтр, место,
    /// деталь) переезжает, правок нет.
    pub fn reopen(&mut self, path: &std::path::Path) -> Result<(), String> {
        let file = File::open(path)?;
        let mut s = self.session();
        s.edits.clear();
        // Transform уже применился при записи — в открытом файле включать его
        // снова нельзя (обработает сообщения дважды). Код в F4 остаётся, но
        // выключается как по нажатию `E`.
        if s.lua_transform.is_some() {
            s.lua_transform_enabled = Some(false);
        }
        let status = std::mem::take(&mut self.status);
        let height = self.height;
        let detail_height = self.detail_height;
        // Приёмник действий агента переезжает в новый `Desk`: агент остаётся
        // привязан к тому же виду (`agr:agent-same-desk`). Поколение файла
        // растёт: правки агента по прежнему файлу вид отбросит (`agr:agent-gen`).
        self.file_gen
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *self = Desk::open_with(
            file,
            self.notify.clone(),
            self.act_tx.clone(),
            self.acts.take(),
            self.file_gen.clone(),
        );
        self.height = height;
        self.detail_height = detail_height;
        self.restore(&s);
        self.status = status;
        Ok(())
    }
}
