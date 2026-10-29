//! Список линков resolve — одно состояние на все виды, как [`crate::desk::Desk`]:
//! окно, TUI в окне, терминал и браузер читают и меняют один [`Browse`]
//! (`agr:resolve-browse`).
//!
//! Строки — снимок [`Browser`] на кадр ([`Browse::poll`]), отобранный фильтром
//! и отсортированный (`agr:resolve-filter`): записей сотни, пересчёт дешевле
//! любого кэша. Выбор держится за ключ (сервис, канал), а не за номер строки:
//! пришедший экспорт выбранное не сдвигает; ушёл выбранный — встаём на
//! соседа по прежнему месту.
//!
//! Ввод ([`Browse::key`]): печатное — в фильтр, стрелки и страницы — выбор,
//! Tab/Shift+Tab — колонка сортировки, Ctrl+R — обратный порядок, Enter —
//! [`Action::Open`] (`resolve://svc/ch`), Esc — очистить фильтр, а на пустом —
//! назад к виду ([`Action::Back`]) или выход. Ctrl+A — сменить сервер: строка
//! ввода `HOST:PORT`; она же открыта сразу, если сервера не знаем.
//!
//! Ctrl+O — форма «Открыть линк» ([`OpenForm`], `agr:link-open`): url
//! выбранной строки и параметры открытия, оба правятся; ↑↓ в параметрах —
//! пресеты и история. Та же форма открывается в виде линка клавишей `o`.
//!
//! Служебные дочерние экспорты (`svc/out/online`, `svc/out/request` у
//! stream-сервера) скрыты, пока не попросили Ctrl+S ([`is_child`]).
//!
//! Тот же отбор — для REST: [`listing`].
//!
//! Наружу: [`Browse`], [`Col`], [`Action`], [`OpenForm`], [`FormField`],
//! [`FormAction`], [`Line`], [`PRESETS`], [`listing`], [`is_child`], [`matches`](fn@matches).

use std::sync::Arc;

use crate::desk::Key;
use crate::io::link::LinkState;
use crate::io::resolve::{Browser, Entry, Snapshot};

/// Колонка списка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Col {
    Service,
    Channel,
    Proto,
    Addr,
    Host,
    Tags,
    Scheme,
    Params,
}

impl Col {
    pub const ALL: [Col; 8] = [
        Col::Service,
        Col::Channel,
        Col::Proto,
        Col::Addr,
        Col::Host,
        Col::Tags,
        Col::Scheme,
        Col::Params,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Col::Service => "сервис",
            Col::Channel => "канал",
            Col::Proto => "proto",
            Col::Addr => "адрес",
            Col::Host => "хост",
            Col::Tags => "теги",
            Col::Scheme => "схема",
            Col::Params => "параметры",
        }
    }

    /// Текст ячейки.
    pub fn cell(self, e: &Entry) -> String {
        match self {
            Col::Service => e.service.clone(),
            Col::Channel => e.channel.clone(),
            Col::Proto => e.proto.clone(),
            Col::Addr => e.addr.clone(),
            Col::Host => e.host.clone(),
            Col::Tags => e.tags.join(","),
            Col::Scheme => match e.messages {
                Some(n) if !e.scheme.is_empty() => format!("{} ·{n}", e.scheme),
                _ => e.scheme.clone(),
            },
            Col::Params => e.params.clone(),
        }
    }

    fn index(self) -> usize {
        Col::ALL.iter().position(|c| *c == self).unwrap_or(0)
    }

    /// Имя для REST: `service`, `channel`, `proto`…
    pub fn key(self) -> &'static str {
        match self {
            Col::Service => "service",
            Col::Channel => "channel",
            Col::Proto => "proto",
            Col::Addr => "addr",
            Col::Host => "host",
            Col::Tags => "tags",
            Col::Scheme => "scheme",
            Col::Params => "params",
        }
    }
}

/// Что виду сделать после клавиши.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Открыть линк `url` с параметрами `open`.
    Open(String, String),
    /// Вернуться к открытому виду.
    Back,
    /// Выйти (терминал); окну и браузеру — ничего.
    Quit,
    /// Подключились к новому серверу — запомнить адрес.
    Connect(String),
}

type Notify = Arc<dyn Fn() + Send + Sync>;

/// Состояние списка.
pub struct Browse {
    browser: Option<Arc<Browser>>,
    notify: Notify,
    /// Фильтр: подстрока; `svc/ch` — по сервису и каналу порознь.
    pub filter: String,
    pub sort: Col,
    pub asc: bool,
    /// Ключ выбранной строки.
    sel: Option<(String, String)>,
    /// Номер выбранной строки, пока она есть: куда встать, если её не станет.
    sel_at: usize,
    pub top: usize,
    /// Строк таблицы на экране — задаёт вид.
    pub height: usize,
    /// Строка ввода адреса сервера, если открыта.
    pub addr: Option<String>,
    /// Есть открытый вид, к которому Esc возвращает.
    pub back: bool,
    /// Почему не открылся выбранный линк — в строке состояния до следующей клавиши.
    pub note: Option<String>,
    /// Форма «Открыть линк» (Ctrl+O), если открыта.
    pub form: Option<OpenForm>,
    /// Показывать служебные дочерние экспорты ([`is_child`]); Ctrl+S.
    pub all: bool,
    rows: Vec<Entry>,
    snap: Snapshot,
}

impl Browse {
    /// Список сервера `browser`; `None` — сервер не знаем, спросить адрес.
    pub fn new(browser: Option<Arc<Browser>>, notify: Notify) -> Browse {
        let ask = browser.is_none();
        let mut b = Browse {
            browser,
            notify,
            filter: String::new(),
            sort: Col::Service,
            asc: true,
            sel: None,
            sel_at: 0,
            top: 0,
            height: 20,
            addr: ask.then(String::new),
            back: false,
            note: None,
            form: None,
            all: false,
            rows: Vec::new(),
            snap: Snapshot::default(),
        };
        b.poll();
        b
    }

    /// Подключиться к серверу `addr`. Уже есть список — адрес меняется у него
    /// же ([`Browser::retarget`]): вкладка и REST не остаются на старом.
    pub fn connect(&mut self, addr: &str) {
        let n = self.notify.clone();
        if let Some(b) = &self.browser {
            b.retarget(addr, n);
        } else {
            self.browser = Some(Arc::new(Browser::start(addr, n)));
        }
        self.addr = None;
        self.poll();
    }

    /// Сервер списка, если есть.
    pub fn browser(&self) -> Option<&Arc<Browser>> {
        self.browser.as_ref()
    }

    /// Взять свежий снимок, отобрать и отсортировать; выбор — на прежний ключ.
    pub fn poll(&mut self) {
        self.snap = self
            .browser
            .as_ref()
            .map(|b| b.snapshot())
            .unwrap_or_default();
        self.rows = rows(&self.snap, &self.filter, self.sort, self.asc, self.all);
        self.fix_sel();
    }

    /// Отобранные строки по порядку.
    pub fn rows(&self) -> &[Entry] {
        &self.rows
    }

    /// Всего записей у сервера.
    pub fn total(&self) -> usize {
        self.snap.entries.len()
    }

    /// Снимок сервера с прошлого [`Browse::poll`].
    pub fn snapshot(&self) -> &Snapshot {
        &self.snap
    }

    /// Номер выбранной строки.
    pub fn sel_row(&self) -> Option<usize> {
        let k = self.sel.as_ref()?;
        self.rows
            .iter()
            .position(|e| e.service == k.0 && e.channel == k.1)
    }

    /// Выбранная строка.
    pub fn selected(&self) -> Option<&Entry> {
        self.sel_row().map(|i| &self.rows[i])
    }

    /// Строка состояния: сервер, его состояние, сколько видно.
    pub fn status(&self) -> String {
        let s = &self.snap;
        if let Some(n) = &self.note {
            return n.lines().next().unwrap_or("").to_string();
        }
        if self.browser.is_none() {
            return "resolve-сервер не задан: HOST:PORT и Enter".into();
        }
        let mut line = format!(
            "resolve {} · {} · {} из {}",
            s.addr,
            s.state.name(),
            self.rows.len(),
            self.total()
        );
        let hidden = self.hidden();
        if hidden > 0 {
            line.push_str(&format!(" · скрыто служебных {hidden} (^S)"));
        }
        if let Some(e) = &s.error {
            line.push_str(" · ");
            line.push_str(e.lines().next().unwrap_or(""));
        }
        line
    }

    /// Сколько служебных строк скрыто: под фильтром, но не показаны.
    pub fn hidden(&self) -> usize {
        if self.all {
            return 0;
        }
        self.snap
            .entries
            .values()
            .filter(|e| matches(e, &self.filter) && is_child(&self.snap, e))
            .count()
    }

    /// Состояние сервера.
    pub fn state(&self) -> LinkState {
        self.snap.state
    }

    /// Выбрать строку `i`.
    pub fn select(&mut self, i: usize) {
        if let Some(e) = self.rows.get(i) {
            self.sel = Some(e.key());
            self.sel_at = i;
            self.follow();
        }
    }

    /// Сдвинуть выбор на `d` строк.
    pub fn move_by(&mut self, d: isize) {
        if self.rows.is_empty() {
            return;
        }
        let at = self.sel_row().unwrap_or(0) as isize + d;
        self.select(at.clamp(0, self.rows.len() as isize - 1) as usize);
    }

    /// Прокрутить окно на `d` строк, не трогая выбор.
    pub fn scroll(&mut self, d: isize) {
        let max = self.rows.len().saturating_sub(self.height.max(1));
        self.top = (self.top as isize + d).clamp(0, max as isize) as usize;
    }

    /// Сортировать по колонке `c`; та же — обратный порядок.
    pub fn sort_by(&mut self, c: Col) {
        if self.sort == c {
            self.asc = !self.asc;
        } else {
            self.sort = c;
            self.asc = true;
        }
        self.resort();
    }

    fn resort(&mut self) {
        self.rows = rows(&self.snap, &self.filter, self.sort, self.asc, self.all);
        self.fix_sel();
        self.follow();
    }

    /// Выбор на месте: ключа нет — на строку с прежним номером.
    fn fix_sel(&mut self) {
        match self.sel_row() {
            Some(i) => self.sel_at = i,
            None if self.rows.is_empty() => {}
            None => {
                let i = self.sel_at.min(self.rows.len() - 1);
                self.sel = Some(self.rows[i].key());
                self.sel_at = i;
            }
        }
        let max = self.rows.len().saturating_sub(self.height.max(1));
        self.top = self.top.min(max);
    }

    /// Окно — так, чтобы выбранная была видна.
    fn follow(&mut self) {
        let Some(i) = self.sel_row() else { return };
        let h = self.height.max(1);
        if i < self.top {
            self.top = i;
        } else if i >= self.top + h {
            self.top = i + 1 - h;
        }
    }

    /// Клавиша от вида.
    pub fn key(&mut self, k: Key) -> Option<Action> {
        self.note = None;
        if let Some(f) = self.form.as_mut() {
            return match f.key(k)? {
                FormAction::Open(url, open) => {
                    self.form = None;
                    Some(Action::Open(url, open))
                }
                FormAction::Cancel => {
                    self.form = None;
                    None
                }
            };
        }
        if let Some(a) = self.addr.as_mut() {
            match k {
                Key::Char(c) if !c.is_control() && !('\u{F1}'..='\u{F9}').contains(&c) => a.push(c),
                Key::Backspace => {
                    a.pop();
                }
                Key::Esc if self.browser.is_some() => self.addr = None,
                Key::Esc if self.back => return Some(Action::Back),
                Key::Esc | Key::Ctrl('c') => return Some(Action::Quit),
                Key::Enter => {
                    let a = a.trim().to_string();
                    if !a.is_empty() {
                        self.connect(&a);
                        return Some(Action::Connect(a));
                    }
                }
                _ => {}
            }
            return None;
        }
        let page = self.height.max(1) as isize;
        match k {
            Key::Char(c) if !c.is_control() && !('\u{F1}'..='\u{F9}').contains(&c) => {
                self.filter.push(c);
                self.resort();
            }
            Key::Backspace => {
                self.filter.pop();
                self.resort();
            }
            Key::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.resort();
            }
            Key::Esc if self.back => return Some(Action::Back),
            Key::Esc => return Some(Action::Quit),
            Key::Up => self.move_by(-1),
            Key::Down => self.move_by(1),
            Key::PageUp => self.move_by(-page),
            Key::PageDown => self.move_by(page),
            Key::Home => self.move_by(isize::MIN / 2),
            Key::End => self.move_by(isize::MAX / 2),
            Key::Tab => {
                let i = (self.sort.index() + 1) % Col::ALL.len();
                self.sort = Col::ALL[i];
                self.resort();
            }
            Key::BackTab => {
                let n = Col::ALL.len();
                self.sort = Col::ALL[(self.sort.index() + n - 1) % n];
                self.resort();
            }
            Key::Ctrl('r') => {
                self.asc = !self.asc;
                self.resort();
            }
            Key::Ctrl('s') => {
                self.all = !self.all;
                self.resort();
            }
            Key::Ctrl('a') => {
                self.addr = Some(self.browser.as_ref().map(|b| b.addr()).unwrap_or_default());
            }
            Key::Enter => {
                return self.selected().map(|e| {
                    let url = e.link();
                    let open = crate::io::link::default_open(&url);
                    Action::Open(url, open)
                })
            }
            Key::Ctrl('o') => {
                if let Some(e) = self.selected() {
                    let url = e.link();
                    let open = crate::io::link::default_open(&url);
                    self.form = Some(OpenForm::new(&url, &open));
                }
            }
            _ => {}
        }
        None
    }
}

/// Подходит ли строка под фильтр (`agr:resolve-filter`): без регистра; `a/b` —
/// `a` в сервисе или его тегах, `b` в канале; без `/` — в любом из них.
pub fn matches(e: &Entry, filter: &str) -> bool {
    let f = filter.trim().to_lowercase();
    if f.is_empty() {
        return true;
    }
    let has = |s: &str, p: &str| s.to_lowercase().contains(p);
    let svc = |p: &str| has(&e.service, p) || e.tags.iter().any(|t| has(t, p));
    match f.split_once('/') {
        Some((s, c)) => svc(s) && has(&e.channel, c),
        None => svc(&f) || has(&e.channel, &f),
    }
}

/// Поле формы «Открыть линк».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    Url,
    Open,
}

/// Чем кончилась форма.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormAction {
    /// Открыть `url` с параметрами `open`.
    Open(String, String),
    Cancel,
}

/// Пресеты параметров открытия — первыми в ↑↓; `N` дописывают.
pub const PRESETS: [&str; 4] = [
    "mode=online",
    "mode=initial",
    "mode=seq;seq=",
    "mode=block;block=",
];

/// Сколько параметров открытия помнит история.
const HISTORY_LIMIT: usize = 10;

/// Параметры, с которыми открывали линки в этом процессе, свежие первыми:
/// одна история у списка и у вида.
static HISTORY: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Строка ввода с курсором (в знаках).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Line {
    pub text: String,
    pub cursor: usize,
}

impl Line {
    fn new(text: &str) -> Line {
        Line {
            text: text.to_string(),
            cursor: text.chars().count(),
        }
    }

    fn byte(&self, at: usize) -> usize {
        self.text
            .char_indices()
            .nth(at)
            .map_or(self.text.len(), |(i, _)| i)
    }

    /// Правка клавишей; `false` — клавиша не строки.
    fn key(&mut self, k: Key) -> bool {
        let n = self.text.chars().count();
        match k {
            Key::Char(c) if !c.is_control() && !('\u{F1}'..='\u{F9}').contains(&c) => {
                let at = self.byte(self.cursor);
                self.text.insert(at, c);
                self.cursor += 1;
            }
            Key::Backspace if self.cursor > 0 => {
                let at = self.byte(self.cursor - 1);
                self.text.remove(at);
                self.cursor -= 1;
            }
            Key::Delete if self.cursor < n => {
                let at = self.byte(self.cursor);
                self.text.remove(at);
            }
            Key::Left => self.cursor = self.cursor.saturating_sub(1),
            Key::Right => self.cursor = (self.cursor + 1).min(n),
            Key::Home => self.cursor = 0,
            Key::End => self.cursor = n,
            Key::Ctrl('u') => *self = Line::default(),
            Key::Backspace | Key::Delete => {}
            _ => return false,
        }
        true
    }
}

/// Форма «Открыть линк»: url и параметры открытия (`agr:link-open`). Одна
/// для списка (Ctrl+O) и вида линка (`o`); рисуют её окно, TUI и браузер.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenForm {
    pub url: Line,
    pub open: Line,
    pub field: FormField,
    /// Номер в [`OpenForm::choices`], выбранный ↑↓.
    pick: Option<usize>,
}

impl OpenForm {
    /// Форма с url и параметрами; курсор — в параметрах: их меняют чаще.
    pub fn new(url: &str, open: &str) -> OpenForm {
        OpenForm {
            url: Line::new(url),
            open: Line::new(open),
            field: FormField::Open,
            pick: None,
        }
    }

    /// Что листают ↑↓ в параметрах: пресеты, потом история без повторов.
    pub fn choices() -> Vec<String> {
        let mut v: Vec<String> = PRESETS.iter().map(|s| s.to_string()).collect();
        for h in HISTORY.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            if !v.contains(h) {
                v.push(h.clone());
            }
        }
        v
    }

    /// Запомнить параметры открытия в истории.
    pub fn remember(open: &str) {
        let open = open.trim();
        if open.is_empty() || PRESETS.contains(&open) {
            return;
        }
        let mut h = HISTORY.lock().unwrap_or_else(|e| e.into_inner());
        h.retain(|x| x != open);
        h.insert(0, open.to_string());
        h.truncate(HISTORY_LIMIT);
    }

    /// Клавиша формы; `None` — форма остаётся.
    pub fn key(&mut self, k: Key) -> Option<FormAction> {
        match k {
            Key::Esc => return Some(FormAction::Cancel),
            Key::Enter => {
                let url = self.url.text.trim().to_string();
                if url.is_empty() {
                    self.field = FormField::Url;
                    return None;
                }
                let open = self.open.text.trim().to_string();
                OpenForm::remember(&open);
                return Some(FormAction::Open(url, open));
            }
            Key::Tab | Key::BackTab => {
                self.field = match self.field {
                    FormField::Url => FormField::Open,
                    FormField::Open => FormField::Url,
                };
            }
            Key::Up | Key::Down if self.field == FormField::Open => {
                let all = OpenForm::choices();
                let n = all.len();
                let i = match (self.pick, k) {
                    (None, Key::Down) => 0,
                    (None, _) => n - 1,
                    (Some(i), Key::Down) => (i + 1) % n,
                    (Some(i), _) => (i + n - 1) % n,
                };
                self.pick = Some(i);
                self.open = Line::new(&all[i]);
            }
            k => {
                let line = match self.field {
                    FormField::Url => &mut self.url,
                    FormField::Open => &mut self.open,
                };
                if line.key(k) && self.field == FormField::Open {
                    self.pick = None;
                }
            }
        }
        None
    }
}

/// Список для REST (`GET /api/v1/resolve`): тот же отбор и порядок, что у
/// вида; `sort` — имя колонки ([`Col::key`]), `-` впереди — обратный порядок.
pub fn listing(
    b: &Browser,
    filter: &str,
    sort: &str,
    all: bool,
) -> Result<serde_json::Value, String> {
    let (asc, name) = match sort.strip_prefix('-') {
        Some(n) => (false, n),
        None => (true, sort),
    };
    let col = match name {
        "" => Col::Service,
        n => Col::ALL.into_iter().find(|c| c.key() == n).ok_or_else(|| {
            let all: Vec<_> = Col::ALL.iter().map(|c| c.key()).collect();
            format!("sort: колонки {n} нет (есть: {})", all.join(", "))
        })?,
    };
    let s = b.snapshot();
    let links: Vec<_> = rows(&s, filter, col, asc, all)
        .iter()
        .map(|e| {
            serde_json::json!({
                "service": e.service,
                "channel": e.channel,
                "link": e.link(),
                "url": e.url,
                "proto": e.proto,
                "addr": e.addr,
                "host": e.host,
                "tags": e.tags,
                "scheme": e.scheme,
                "messages": e.messages,
                "params": e.params,
            })
        })
        .collect();
    Ok(serde_json::json!({
        "server": s.addr,
        "state": s.state.name(),
        "error": s.error,
        "total": s.entries.len(),
        "links": links,
    }))
}

/// Служебный дочерний экспорт: канал `P/C`, у которого родитель `P` (или
/// выше по `/`) того же сервиса тоже экспортирован. Так stream-сервер
/// выставляет свои части `online` и `request` — для сборки клиента по
/// частям, а не для просмотра (`agr:resolve-filter`).
pub fn is_child(s: &Snapshot, e: &Entry) -> bool {
    let mut ch = e.channel.as_str();
    while let Some((parent, _)) = ch.rsplit_once('/') {
        if s.entries
            .contains_key(&(e.service.clone(), parent.to_string()))
        {
            return true;
        }
        ch = parent;
    }
    false
}

/// Строки снимка под фильтром, по колонке `sort`; при равенстве — по ключу.
/// Служебные дочерние ([`is_child`]) — только с `all`.
fn rows(s: &Snapshot, filter: &str, sort: Col, asc: bool, all: bool) -> Vec<Entry> {
    let mut v: Vec<Entry> = s
        .entries
        .values()
        .filter(|e| matches(e, filter) && (all || !is_child(s, e)))
        .cloned()
        .collect();
    v.sort_by(|a, b| {
        let o = sort
            .cell(a)
            .to_lowercase()
            .cmp(&sort.cell(b).to_lowercase())
            .then_with(|| a.key().cmp(&b.key()));
        if asc {
            o
        } else {
            o.reverse()
        }
    });
    v
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::io::resolve::{BrowseEvent, Export};

    pub fn export(svc: &str, ch: &str, proto: &str) -> Export {
        Export {
            service: svc.into(),
            channel: ch.into(),
            tags: vec![format!("{svc}-tag")],
            config: vec![("init.tll.proto".into(), proto.into())],
            ..Default::default()
        }
    }

    /// Список из трёх сервисов без сети.
    pub fn sample() -> Browse {
        let mut s = Snapshot {
            addr: "h:1".into(),
            state: LinkState::Active,
            ..Default::default()
        };
        for (svc, ch, p) in [
            ("gateway", "orders", "stream+pub+tcp"),
            ("gateway", "quotes", "pub+tcp"),
            ("risk", "orders", "tcp"),
            ("md", "feed", "pub+udp"),
        ] {
            s.apply(BrowseEvent::Export(export(svc, ch, p)));
        }
        let b = Arc::new(Browser::fixed(s));
        Browse::new(Some(b), Arc::new(|| {}))
    }

    fn keys(b: &Browse) -> Vec<String> {
        b.rows()
            .iter()
            .map(|e| format!("{}/{}", e.service, e.channel))
            .collect()
    }

    fn typed(b: &mut Browse, s: &str) {
        for c in s.chars() {
            b.key(Key::Char(c));
        }
    }

    /// `agr:resolve-filter` — без `/` — сервис, тег или канал; `a/b` — сервис
    /// и канал порознь; регистр не важен.
    #[test]
    fn filter_matches_service_and_channel() {
        let mut b = sample();
        typed(&mut b, "ORD");
        assert_eq!(keys(&b), ["gateway/orders", "risk/orders"]);
        b.key(Key::Esc);
        typed(&mut b, "gate/");
        assert_eq!(keys(&b), ["gateway/orders", "gateway/quotes"]);
        typed(&mut b, "q");
        assert_eq!(keys(&b), ["gateway/quotes"]);
        b.key(Key::Esc);
        typed(&mut b, "/orders");
        assert_eq!(keys(&b), ["gateway/orders", "risk/orders"]);
        b.key(Key::Esc);
        typed(&mut b, "md-tag");
        assert_eq!(keys(&b), ["md/feed"], "тег — как имя сервиса");
        assert!(b.status().contains("1 из 4"), "{}", b.status());
    }

    /// `agr:resolve-filter` — сортировка по колонке, повтор — обратный порядок;
    /// Tab — следующая колонка, Ctrl+R — обратный порядок.
    #[test]
    fn sorting_by_column_and_back() {
        let mut b = sample();
        assert_eq!(
            keys(&b),
            ["gateway/orders", "gateway/quotes", "md/feed", "risk/orders"]
        );
        b.sort_by(Col::Proto);
        assert_eq!(
            keys(&b),
            ["gateway/quotes", "md/feed", "gateway/orders", "risk/orders"]
        );
        b.sort_by(Col::Proto);
        assert_eq!(
            keys(&b),
            ["risk/orders", "gateway/orders", "md/feed", "gateway/quotes"]
        );
        b.key(Key::BackTab);
        assert_eq!(b.sort, Col::Channel);
        b.key(Key::Tab);
        assert_eq!((b.sort, b.asc), (Col::Proto, false));
        b.key(Key::Ctrl('r'));
        assert!(b.asc);
    }

    /// `agr:resolve-browse` — выбор держится за ключ: новый экспорт выше его не
    /// сдвигает, ушедший выбранный — встаём на соседа; Enter — `resolve://`.
    #[test]
    fn selection_follows_its_key() {
        let mut b = sample();
        b.key(Key::Down);
        b.key(Key::Down);
        assert_eq!(b.selected().unwrap().link(), "resolve://md/feed");
        let browser = b.browser().unwrap().clone();
        browser.apply(BrowseEvent::Export(export("alpha", "x", "tcp")));
        b.poll();
        assert_eq!(b.sel_row(), Some(3), "строка сдвинулась, выбор — с ней");
        assert_eq!(
            b.key(Key::Enter),
            Some(Action::Open(
                "resolve://md/feed".into(),
                "mode=online".into()
            ))
        );
        browser.apply(BrowseEvent::DropService {
            service: "md".into(),
        });
        b.poll();
        assert_eq!(b.selected().unwrap().link(), "resolve://risk/orders");
    }

    /// `agr:resolve-filter` — список для REST: тот же отбор и порядок, `-` —
    /// обратный; чужая колонка — ошибка с перечнем.
    #[test]
    fn rest_listing_filters_and_sorts_like_the_view() {
        let b = sample();
        let br = b.browser().unwrap();
        let v = listing(br, "orders", "-service", false).unwrap();
        let links: Vec<_> = v["links"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["link"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(links, ["resolve://risk/orders", "resolve://gateway/orders"]);
        assert_eq!(
            (v["total"].clone(), v["state"].clone()),
            (4.into(), "Active".into())
        );
        assert!(listing(br, "", "nope", false)
            .unwrap_err()
            .contains("proto"));
    }

    /// `agr:link-open` — Ctrl+O: форма с url выбранной строки и параметрами;
    /// оба правятся, Tab — между ними, Esc — назад к списку.
    #[test]
    fn ctrl_o_opens_the_form_with_url_and_params() {
        let mut b = sample();
        assert_eq!(b.key(Key::Ctrl('o')), None);
        let f = b.form.as_ref().expect("форма");
        assert_eq!(f.url.text, "resolve://gateway/orders");
        assert_eq!(
            (f.open.text.as_str(), f.field),
            ("mode=online", FormField::Open)
        );
        b.key(Key::Ctrl('u'));
        typed(&mut b, "mode=seq;seq=7");
        b.key(Key::Tab);
        b.key(Key::End);
        typed(&mut b, "/online");
        assert!(b.filter.is_empty(), "печатное — в форму, не в фильтр");
        assert_eq!(
            b.key(Key::Enter),
            Some(Action::Open(
                "resolve://gateway/orders/online".into(),
                "mode=seq;seq=7".into()
            ))
        );
        assert!(b.form.is_none());
        b.key(Key::Ctrl('o'));
        assert_eq!(b.key(Key::Esc), None, "Esc — закрыть форму, не уйти");
        assert!(b.form.is_none());
    }

    /// `agr:link-open` — ↑↓ в параметрах листают пресеты и историю;
    /// курсор правит середину; пустой url не открывается.
    #[test]
    fn form_presets_history_and_cursor() {
        OpenForm::remember("mode=seq;seq=42");
        let mut f = OpenForm::new("resolve://a/b", "");
        f.key(Key::Down);
        assert_eq!(f.open.text, "mode=online");
        f.key(Key::Up);
        f.key(Key::Up);
        // Назад с первого пресета — конец списка: история (её делят тесты).
        assert!(!PRESETS.contains(&f.open.text.as_str()), "{}", f.open.text);
        assert!(OpenForm::choices().contains(&"mode=seq;seq=42".to_string()));
        f.key(Key::Tab);
        f.key(Key::Home);
        for _ in 0.."resolve://".len() {
            f.key(Key::Right);
        }
        f.key(Key::Char('x'));
        assert_eq!(f.url.text, "resolve://xa/b");
        f.key(Key::Ctrl('u'));
        assert_eq!(f.key(Key::Enter), None, "пустой url");
        assert_eq!(f.field, FormField::Url);
    }

    /// `agr:resolve-filter` — служебные дочерние (`out/online`, `out/request`
    /// при живом `out`) скрыты и посчитаны; Ctrl+S — показать; сирота без
    /// родителя и чужой сервис — не служебные; REST — `all`.
    #[test]
    fn child_exports_are_hidden_until_asked() {
        let mut s = Snapshot {
            addr: "h:1".into(),
            state: LinkState::Active,
            ..Default::default()
        };
        for (svc, ch) in [
            ("mu0", "out"),
            ("mu0", "out/online"),
            ("mu0", "out/request"),
            ("mu0", "lone/online"),
            ("other", "out/online"),
        ] {
            s.apply(BrowseEvent::Export(export(svc, ch, "tcp")));
        }
        let br = Arc::new(Browser::fixed(s));
        let mut b = Browse::new(Some(br.clone()), Arc::new(|| {}));
        assert_eq!(keys(&b), ["mu0/lone/online", "mu0/out", "other/out/online"]);
        assert_eq!(b.hidden(), 2);
        assert!(b.status().contains("скрыто служебных 2"), "{}", b.status());
        b.key(Key::Ctrl('s'));
        assert_eq!(b.rows().len(), 5);
        assert_eq!(b.hidden(), 0);
        let n = |all| {
            listing(&br, "", "", all).unwrap()["links"]
                .as_array()
                .unwrap()
                .len()
        };
        assert_eq!((n(false), n(true)), (3, 5));
    }

    /// `agr:resolve-browse` — Esc: сначала фильтр, потом назад к виду, а без
    /// вида — выход. Сервер не знаем — сначала адрес.
    #[test]
    fn esc_clears_filter_then_goes_back() {
        let mut b = sample();
        typed(&mut b, "x");
        assert_eq!(b.key(Key::Esc), None);
        assert!(b.filter.is_empty());
        assert_eq!(b.key(Key::Esc), Some(Action::Quit));
        b.back = true;
        assert_eq!(b.key(Key::Esc), Some(Action::Back));

        let mut none = Browse::new(None, Arc::new(|| {}));
        assert_eq!(none.addr.as_deref(), Some(""));
        assert!(none.status().contains("не задан"));
        typed(&mut none, "q");
        assert!(none.filter.is_empty(), "печатное — в адрес, не в фильтр");
        assert_eq!(none.addr.as_deref(), Some("q"));
        none.back = true;
        assert_eq!(
            none.key(Key::Esc),
            Some(Action::Back),
            "без сервера — назад к виду"
        );
    }

    /// `agr:resolve-browse` — смена сервера не подменяет список: тот же
    /// `Browser`, новый адрес. Его же читает REST.
    #[test]
    fn connect_retargets_the_same_browser() {
        let shared = Arc::new(Browser::start("127.0.0.1:9", Arc::new(|| {})));
        let mut b = Browse::new(Some(shared.clone()), Arc::new(|| {}));
        b.addr = Some("127.0.0.1:8".into());
        assert_eq!(
            b.key(Key::Enter),
            Some(Action::Connect("127.0.0.1:8".into()))
        );
        assert!(
            Arc::ptr_eq(b.browser().unwrap(), &shared),
            "смена адреса не подменяет список"
        );
        assert_eq!(shared.addr(), "127.0.0.1:8");
        assert!(b.addr.is_none());
    }
}
