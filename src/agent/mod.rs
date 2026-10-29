//!
//! Режим агента: диалог с моделью, которая читает датник через инструменты и
//! собирает Lua-фильтр ([`crate::lua`]). Модель никогда не касается файла сама —
//! она только просит инструменты, а тяжёлое отдаётся сводкой плюс выборкой
//! окнами. Сеть к LLM — новая возможность: без ключа и адреса агент просто не
//! отвечает.
//!
//! Устройство как в black-d: `start` поднимает поток с воркером и отдаёт канал
//! команд ([`Cmd`]) и канал событий ([`Ev`]); воркер один, диалоги и настройки
//! переживают перезапуск (`store`). Диалоги привязаны к имени файла датника
//! (`store::owner_of`) — как сессия просмотрщика (`agr:session-by-name`).
//!
//! Правки агент готовит пачкой: один вызов `edit_messages` собирает всю пачку
//! (`agr:agent-batch`), человек видит одну сводку и подтверждает её один раз;
//! файл агент не фиксирует сам, окно фиксации открывает человек.
//!
//! Ход — машина состояний (`Phase`): стрим модели идёт в отдельной задаче,
//! воркер через `select!` читает команды и куски ответа одновременно
//! (`agr:agent-loop`); итог стрима (`resume`) кладёт вызовы в очередь,
//! `run_queue` исполняет их, пачковый встаёт на подтверждение. Уточнения
//! за ход копятся в очереди `State` и уходят новым ходом (`end_of_turn`,
//! `agr:agent-queue`).
//!
//! Файлы: `store.rs` — настройки и диалоги на диске, `llm.rs` — клиент к модели
//! (поток SSE и разовые запросы), `tools.rs` — схемы и исполнение инструментов,
//! `prompt.rs` — системный промпт, `trace.rs` — трассировка хода агента.
//!
//! Наружу: [`Cmd`], [`CmdTx`], [`Ev`], [`DialogView`], [`MsgView`], [`Act`],
//! [`ActTx`], [`ToolCtx`], [`LuaView`], [`start`], [`Settings`], [`Dialog`],
//! [`Msg`], [`CallReq`], [`Store`], [`UNTITLED`], [`build_tools`],
//! [`needs_confirm`], [`build_system`], [`Snap`].

mod llm;
mod prompt;
mod store;
mod tools;
mod trace;

pub use prompt::build_system;
pub use store::{CallReq, Dialog, Msg, Settings, Store, UNTITLED};
pub use tools::{build_tools, needs_confirm};

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::SyncSender as Sender;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::lua::Filter;
use llm::{chat_stream, estimate_tokens, Reply, ToolCall};

/// Канал команд воркеру.
pub type CmdTx = UnboundedSender<Cmd>;

/// Канал действий агента: их применяет вид к тому же [`crate::desk::Desk`],
/// что и человек, — агент не трогает состояние просмотрщика сам
/// (`agr:agent-same-desk`).
pub type ActTx = Sender<Act>;

/// Что агент просит сделать в просмотрщике. Читающие инструменты работают
/// сами, а всё, что меняет вид или правки, идёт сюда.
#[derive(Debug, Clone)]
pub enum Act {
    /// Поставить фильтр (пустой — снять).
    Filter(Filter),
    /// Искать внутри фильтра от текущего места.
    Search { filter: Filter, back: bool },
    /// Перейти по тексту как `g`: seq, `+N`, `-N`.
    Goto(String),
    /// Выставить список колонок таблицы (заменяет текущий).
    SetColumns(Vec<String>),
    /// Установить Lua-transform-код и его состояние (`enabled=false` — код в
    /// просмотрщике не применяется). Изменяет файл так же, как человек через
    /// F4: без записи на диск, только просмотр.
    LuaTransform { code: String, enabled: bool },
    /// Отдать виду незафиксированные правки: он перечитает файл через
    /// `patch+` (`agr:patch-overlay`). Фиксацию агент не делает
    /// (`agr:agent-no-write`). `gen` — поколение файла на момент правки:
    /// если человек успел переоткрыть файл, вид правку отбросит
    /// (`agr:agent-gen`).
    Sync { edits: crate::edit::Edits, gen: u64 },
    /// Агент посмотрел на незафиксированные правки: вид больше их не шлёт
    /// (`agr:agent-sync`).
    MarkSynced,
}

/// Снимок состояния вида: что сейчас открыто, какой фильтр, колонки,
/// позиция. Вид отдаёт при каждом `poll()` через [`Cmd::UpdateSnap`]
/// (`agr:agent-snap`).
#[derive(Debug)]
pub struct Snap {
    pub total: Option<usize>,
    pub index: Option<std::sync::Arc<crate::data::Index>>,
    pub filter: Filter,
    pub columns: Vec<String>,
    pub sel_seq: Option<i64>,
    pub sel_row: Option<usize>,
}

/// Команды от интерфейса.
#[derive(Debug)]
pub enum Cmd {
    /// Завести новый диалог и сделать его активным.
    NewDialog,
    /// Открыть диалог по id.
    PickDialog { id: String },
    /// Закрыть диалог: скрыть из списка, не стирая.
    CloseDialog { id: String },
    /// Отправить вопрос активному диалогу (нет диалога — вопрос его создаёт).
    Send { text: String },
    /// Сохранить настройки подключения.
    SaveSettings { settings: Settings },
    /// Запросить список моделей с адреса API.
    ListModels { settings: Settings },
    /// Проверить подключение: короткий запрос к модели (`chat_once`).
    CheckSettings { settings: Settings },
    /// Подтвердить мутирующий вызов инструмента.
    Confirm,
    /// Отклонить мутирующий вызов инструмента.
    Deny,
    /// Прервать ход: на подтверждении — отказ с причиной, иначе ничего.
    Abort,
    /// Завершить воркер.
    Shutdown,
    /// Передать воркеру свежий снимок состояния вида (`agr:agent-snap`).
    UpdateSnap { snap: Snap },
}

/// Короткая строка диалога для списка.
#[derive(Clone, Debug)]
pub struct DialogView {
    pub id: String,
    pub title: String,
    pub closed: bool,
}

/// Сообщение для ленты: роль, текст, имя инструмента и признак ошибки.
#[derive(Clone, Debug)]
pub struct MsgView {
    pub role: String,
    pub text: String,
    pub index: usize,
    /// Имя инструмента — только у `tool`; пусто у человека и модели.
    pub name: String,
    /// Результат инструмента — ошибка (`agr:agent-loop`).
    pub is_error: bool,
}

/// События для интерфейса.
#[derive(Clone, Debug)]
pub enum Ev {
    /// Список диалогов и активный из них.
    Dialogs {
        active: Option<String>,
        dialogs: Vec<DialogView>,
    },
    /// Полная лента диалога (после открытия или переключения).
    Messages {
        dialog: String,
        messages: Vec<MsgView>,
    },
    /// Настройки подключения.
    Settings(Settings),
    /// Ход начался: модель вызвана, кусков ещё не было.
    Started {
        dialog: String,
    },
    /// Кусок стриминга ответа.
    Delta {
        dialog: String,
        text: String,
    },
    /// Диалог получил имя по первому вопросу.
    Title {
        dialog: String,
        title: String,
    },
    /// Ответ завершён.
    Done {
        dialog: String,
    },
    Error {
        dialog: String,
        text: String,
    },
    /// Список моделей с адреса API (или ошибка запроса).
    Models(Result<Vec<String>, String>),
    /// Проверка подключения: ответ модели или ошибка.
    Checked(Result<String, String>),
    /// Размер контекста диалога: `exact` — по счёту API, иначе оценка.
    Context {
        dialog: String,
        tokens: u64,
        exact: bool,
    },
    /// Журнал хода агента: вызовы, результаты, подтверждения.
    Trace {
        text: String,
    },
    /// Пачка правок ждёт человека: одна сводка, одно подтверждение.
    ConfirmRequest {
        tool: String,
        summary: crate::edit::Summary,
    },
    /// Вопрос принят в очередь: модель ответит, когда закончит ход.
    Notice {
        dialog: String,
        text: String,
    },
    /// Перед исполнением инструмента: вид показывает, что работает он, а не
    /// модель (`agr:agent-progress`).
    Tool {
        dialog: String,
        name: String,
    },
    /// Результат инструмента ушёл модели: имя, строк в нём, ошибка ли
    /// (`agr:agent-progress`).
    ToolDone {
        dialog: String,
        name: String,
        lines: usize,
        is_error: bool,
    },
    /// Запрос повторяется после ошибки соединения или 429 (`agr:agent-retry`).
    Retry {
        dialog: String,
        attempt: u32,
    },
    /// Результат проверки поддержки `tools` сервером (`Cmd::CheckSettings`).
    ToolsProbed(bool),
}

/// Контекст воркера: где файл, где читать, куда слать действия.
pub struct ToolCtx {
    /// Файл, с которым работает просмотрщик, — тот же, что у `Desk`.
    pub file: crate::data::File,
    /// Поколение файла (`crate::desk::Desk::file_gen`): правка несёт его в
    /// [`Act::Sync`], и вид отбрасывает правку устаревшего поколения
    /// (`agr:agent-gen`).
    pub gen: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// yaml незафиксированных правок, через который идёт чтение (его заводит
    /// `ToolCtx::write_patch`]).
    pub patch: Option<PathBuf>,
    /// Свободный контекст libtll для чтения в этом потоке.
    pub ctx: crate::io::Context,
    /// Число сообщений в файле: `None` — индекс ещё не готов.
    pub total: Option<usize>,
    /// Индекс, если готов, — для чтения по номеру строки.
    pub index: Option<std::sync::Arc<crate::data::Index>>,
    /// Канал действий для просмотрщика.
    pub acts: Option<ActTx>,
    /// Куда писать yaml правок.
    pub store: crate::edit::Store,
    /// Незафиксированные правки — вид их отдаёт воркеру.
    pub edits: crate::edit::Edits,
    /// Текущий Lua-transform-код (пусто — transform не задан).
    pub lua_code: String,
    /// Включён ли Lua-transform-код.
    pub lua_enabled: bool,
    /// Ход прерван: долгие сканы и остаток очереди вызовов останавливаются
    /// (`agr:agent-loop`).
    pub stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Показывать трассировку хода агента в ленте.
    pub debug: bool,
    /// Активный фильтр вида — обновляется через [`Cmd::UpdateSnap`].
    pub filter: Filter,
    /// Видимые колонки таблицы — обновляются через [`Cmd::UpdateSnap`].
    pub columns: Vec<String>,
    /// Seq выбранного сообщения — обновляется через [`Cmd::UpdateSnap`].
    pub sel_seq: Option<i64>,
    /// Номер строки выбранного сообщения — обновляется через [`Cmd::UpdateSnap`].
    pub sel_row: Option<usize>,
    /// Поколение файла на момент последнего начала хода: при расхождении с
    /// [`ToolCtx::gen`] — файл переоткрылся (`agr:agent-gen`).
    pub last_gen: u64,
}

impl ToolCtx {
    /// Что открывать для чтения: части с наложенными правками, если они есть.
    fn src(&self) -> Vec<crate::data::fetch::Src> {
        self.file.sources()
    }

    /// Записать правки в yaml и перечитать через него: и вид, и агент видят
    /// одну и ту же картину.
    fn write_patch(&mut self, edits: &crate::edit::Edits) -> Result<(), String> {
        // Читать через `patch+` умеет tll-patch; без модуля правку не проверить.
        if !self.ctx.ensure_patch(self.file.info.backend) {
            return Err("нет tll-patch: правки недоступны".into());
        }
        self.patch = self.store.write(edits)?;
        self.file.patch = self.patch.clone();
        Ok(())
    }

    /// Послать действие просмотрщику.
    fn act(&self, act: Act) {
        if let Some(tx) = &self.acts {
            let _ = tx.send(act);
        }
    }

    /// Отдать виду незафиксированные правки: он перечитает файл через
    /// `patch+`, и человек увидит то же, что читает агент (`agr:agent-same-desk`).
    /// Правка несёт поколение файла: переоткрыл человек — вид её отбросит
    /// (`agr:agent-gen`).
    fn sync(&self) {
        if !self.edits.is_empty() {
            self.act(Act::Sync {
                edits: self.edits.clone(),
                gen: self.gen.load(std::sync::atomic::Ordering::SeqCst),
            });
        }
    }
}

/// Состояние воркера.
struct State {
    /// Владелец диалогов — имя файла датника.
    owner: String,
    store: Store,
    settings: Settings,
    /// Счёт контекста от API: сколько токенов и на каком числе сообщений.
    usage: HashMap<String, (u64, usize)>,
    /// Вопросы, пришедшие, пока модель отвечает: уйдут одним сообщением.
    queued: VecDeque<String>,
}

/// Сколько уточнений копить, пока модель отвечает.
const QUEUE_LIMIT: usize = 8;

/// Текущий ход: один на воркер — модель и инструменты идут по очереди
/// (`agr:agent-one-turn`). На подтверждении ход встаёт на паузу и переживает
/// команды интерфейса, пока человек не ответит.
struct Turn {
    dialog: String,
    partial: String,
    /// Вызовы текущего витка, ещё не исполненные.
    queue: VecDeque<ToolCall>,
}

/// Чем занят воркер: один ход за раз (`agr:agent-one-turn`).
enum Phase {
    /// Ничего не идёт: принимаются любые команды.
    Idle,
    /// Модель отвечает: стрим — в отдельной задаче, воркер читает команды
    /// (`agr:agent-loop`).
    Stream { turn: Turn, job: Job },
    /// Пачковый вызов собран и ждёт человека, ход встал. Пачка уже проверена
    /// ([`tools::prepare`]) — на подтверждении её применяет [`tools::apply_batch`].
    Confirm { turn: Turn, batch: tools::Batch },
}

/// Задача стрима: куски и итог — одним каналом, по [`Cmd::Abort`] задача
/// снимается.
struct Job {
    frames: UnboundedReceiver<Frame>,
    handle: tokio::task::JoinHandle<()>,
}

/// Что прислала задача стрима, пока идёт ответ.
enum Frame {
    /// Кусок текста ответа.
    Delta(String),
    /// Запрос повторяется — номер попытки (`agr:agent-retry`).
    Retry(u32),
    /// Итог стрима: тем же каналом и строго после кусков — отдельный канал
    /// позволил бы `select!` взять итог раньше кусков, оставшихся в очереди,
    /// и хвост ответа пропал бы.
    Done(Result<Reply, String>),
}

/// Запустить воркер агента для файла датника; владелец диалогов — имя файла
/// Состояние transform-кода файла, которое вид передаёт агенту при старте.
#[derive(Debug, Clone)]
pub struct LuaView {
    pub code: String,
    pub enabled: bool,
}

/// (`store::owner_of`), `acts` — канал действий к тому же
/// [`crate::desk::Desk`], что у человека (`agr:agent-same-desk`), `gen` —
/// счётчик поколений файла (`crate::desk::Desk::file_gen`, `agr:agent-gen`).
pub fn start(
    file: crate::data::File,
    acts: ActTx,
    gen: std::sync::Arc<std::sync::atomic::AtomicU64>,
    lua: LuaView,
) -> (CmdTx, std::sync::mpsc::Receiver<Ev>) {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = std::sync::mpsc::sync_channel(512);
    let owner = store::owner_of(std::path::Path::new(&file.name));
    std::thread::Builder::new()
        .name("agent".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ev_tx.send(Ev::Error {
                        dialog: String::new(),
                        text: format!("поток агента: {e}"),
                    });
                    return;
                }
            };
            rt.block_on(worker(owner, file, acts, gen, cmd_rx, ev_tx, lua));
        })
        .ok();
    (cmd_tx, ev_rx)
}

/// Тело воркера: начальное состояние и цикл команд.
async fn worker(
    owner: String,
    file: crate::data::File,
    acts: ActTx,
    gen: std::sync::Arc<std::sync::atomic::AtomicU64>,
    mut cmd_rx: UnboundedReceiver<Cmd>,
    ev_tx: Sender<Ev>,
    lua: LuaView,
) {
    let settings = store::load_settings();
    let mut ctx = ToolCtx {
        file,
        gen,
        stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        patch: None,
        ctx: crate::io::Context::new(),
        total: None,
        index: None,
        acts: Some(acts),
        store: crate::edit::Store::new(),
        edits: crate::edit::Edits::default(),
        lua_code: lua.code,
        lua_enabled: lua.enabled,
        debug: settings.debug,
        filter: Filter::default(),
        columns: Vec::new(),
        sel_seq: None,
        sel_row: None,
        last_gen: 0,
    };
    let mut state = State {
        store: store::load_store(&owner),
        settings,
        usage: HashMap::new(),
        owner,
        queued: VecDeque::new(),
    };
    let mut phase = Phase::Idle;
    send_dialogs(&ev_tx, &state.store);
    send(&ev_tx, Ev::Settings(state.settings.clone()));
    if let Some(id) = state.store.active.clone() {
        send_messages(&ev_tx, &state.store, &id);
        send_context(&ev_tx, &state, &id);
    }

    loop {
        let cmd = match &mut phase {
            // Ход стоит (или его нет): воркер ждёт только команды.
            Phase::Idle | Phase::Confirm { .. } => cmd_rx.recv().await,
            // Модель отвечает: команды и события стрима наперегонки
            // (`agr:agent-loop`).
            Phase::Stream { .. } => {
                enum Step {
                    Cmd(Option<Cmd>),
                    Frame(Option<Frame>),
                }
                let step = {
                    let Phase::Stream { job, .. } = &mut phase else {
                        unreachable!("фаза была Stream");
                    };
                    tokio::select! {
                        cmd = cmd_rx.recv() => Step::Cmd(cmd),
                        frame = job.frames.recv() => Step::Frame(frame),
                    }
                };
                match step {
                    Step::Frame(frame) => {
                        // Канал кусков закрылся без итога — задача умерла:
                        // ход не должен висеть в Stream без кадров.
                        let frame =
                            frame.unwrap_or(Frame::Done(Err("задача стрима умерла".into())));
                        phase = apply_frame(&mut state, &mut ctx, &ev_tx, phase, frame);
                        continue;
                    }
                    Step::Cmd(cmd) => cmd,
                }
            }
        };
        let Some(cmd) = cmd else { break };
        match cmd {
            Cmd::Shutdown => break,
            Cmd::Confirm | Cmd::Deny | Cmd::Abort => match &mut phase {
                Phase::Confirm { turn, batch } => {
                    let turn = std::mem::replace(
                        turn,
                        Turn {
                            dialog: String::new(),
                            partial: String::new(),
                            queue: VecDeque::new(),
                        },
                    );
                    let batch = std::mem::take(batch);
                    phase = confirm(&mut state, &mut ctx, &ev_tx, turn, batch, cmd);
                }
                Phase::Stream { job, turn } => {
                    // Прервать ответ можно в любой момент: задача снимается,
                    // то, что успело прийти, остаётся в ленте (`agr:agent-loop`).
                    job.handle.abort();
                    if let Cmd::Abort = cmd {
                        ctx.stop.store(true, std::sync::atomic::Ordering::SeqCst);
                        let turn = std::mem::replace(
                            turn,
                            Turn {
                                dialog: String::new(),
                                partial: String::new(),
                                queue: VecDeque::new(),
                            },
                        );
                        phase = abort_stream(&mut state, &ev_tx, turn);
                    }
                }
                Phase::Idle => {}
            },
            Cmd::NewDialog => {
                if busy(&phase) {
                    continue;
                }
                store::new_dialog(&mut state.store);
                state.save(&ev_tx);
                send_dialogs(&ev_tx, &state.store);
                if let Some(id) = state.store.active.clone() {
                    send_messages(&ev_tx, &state.store, &id);
                }
            }
            Cmd::PickDialog { id } => {
                if !matches!(phase, Phase::Idle) || state.store.by_id(&id).is_none() {
                    continue;
                }
                state.store.active = Some(id.clone());
                state.save(&ev_tx);
                send_dialogs(&ev_tx, &state.store);
                send_messages(&ev_tx, &state.store, &id);
                send_context(&ev_tx, &state, &id);
            }
            Cmd::CloseDialog { id } => {
                if let Some(d) = state.store.by_id_mut(&id) {
                    d.closed = true;
                }
                if state.store.active.as_deref() == Some(id.as_str()) {
                    state.store.active = state
                        .store
                        .dialogs
                        .iter()
                        .rev()
                        .find(|d| !d.closed)
                        .map(|d| d.id.clone());
                }
                state.save(&ev_tx);
                send_dialogs(&ev_tx, &state.store);
                if let Some(active) = state.store.active.clone() {
                    send_messages(&ev_tx, &state.store, &active);
                }
            }
            Cmd::SaveSettings { settings } => {
                state.settings = settings;
                let _ = store::save_settings(&state.settings);
                send(&ev_tx, Ev::Settings(state.settings.clone()));
            }
            Cmd::ListModels { settings } => {
                let result = llm::list_models(&settings).await;
                send(&ev_tx, Ev::Models(result));
            }
            Cmd::CheckSettings { settings } => {
                let result = llm::chat_once(
                    &settings,
                    "You check that the connection works. Answer with one word: ok.",
                    "ping",
                )
                .await;
                if result.is_ok() {
                    let ok = llm::probe_tools(&settings).await;
                    send(&ev_tx, Ev::ToolsProbed(ok));
                }
                send(&ev_tx, Ev::Checked(result));
            }
            Cmd::Send { text } => {
                let text = text.trim().to_string();
                if text.is_empty() {
                    continue;
                }
                if matches!(phase, Phase::Idle) {
                    ctx.stop.store(false, std::sync::atomic::Ordering::SeqCst);
                    // `agr:agent-snap` — проверить, не переоткрыт ли файл.
                    let current_gen = ctx.gen.load(std::sync::atomic::Ordering::SeqCst);
                    if current_gen != ctx.last_gen {
                        ctx.last_gen = current_gen;
                        if state.store.active.is_none() {
                            store::new_dialog(&mut state.store);
                        }
                        if let Some(id) = state.store.active.clone() {
                            store_message(
                                &mut state,
                                &ev_tx,
                                &id,
                                Msg::System {
                                    content: "файл переоткрыт — обзор устарел, вызови view_state"
                                        .into(),
                                },
                            );
                        }
                    }
                    phase = begin_question(&mut state, &ev_tx, text);
                } else {
                    // Не теряем: уточнение уйдёт, когда модель закончит
                    // (`agr:agent-queue`).
                    if state.queued.len() >= QUEUE_LIMIT {
                        send(
                            &ev_tx,
                            Ev::Error {
                                dialog: state.store.active.clone().unwrap_or_default(),
                                text: "очередь полна — дождись ответа".into(),
                            },
                        );
                    } else {
                        let id = state.store.active.clone().unwrap_or_default();
                        state.queued.push_back(text);
                        send(
                            &ev_tx,
                            Ev::Notice {
                                dialog: id,
                                text: "вопрос в очереди — уйдёт после ответа".into(),
                            },
                        );
                    }
                }
            }
            Cmd::UpdateSnap { snap } => {
                ctx.total = snap.total;
                ctx.index = snap.index;
                ctx.filter = snap.filter;
                ctx.columns = snap.columns;
                ctx.sel_seq = snap.sel_seq;
                ctx.sel_row = snap.sel_row;
            }
        }
    }
}

/// Ход стоит, если воркер не свободен: команды, меняющие активный диалог,
/// в это время пропускаются (`agr:agent-one-turn`).
fn busy(phase: &Phase) -> bool {
    !matches!(phase, Phase::Idle)
}

impl State {
    /// Записать диалоги владельца; ошибку показываем в ленте, не в stderr
    /// (в терминале он лёг бы поверх экрана, `agr:term-quiet`).
    fn save(&self, ev_tx: &Sender<Ev>) {
        if let Err(e) = store::save_store(&self.owner, &self.store) {
            send(
                ev_tx,
                Ev::Error {
                    dialog: self.store.active.clone().unwrap_or_default(),
                    text: format!("диалоги: {e}"),
                },
            );
        }
    }
}

/// Имя диалога по первому вопросу: первая строка, обрезанная до 40 знаков.
fn title_of(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() <= 40 {
        return line.to_string();
    }
    let head: String = line.chars().take(40).collect();
    format!("{head}…")
}

/// Новый вопрос: в ленту, диалогу — имя, воркеру — задача стрима.
fn begin_question(state: &mut State, ev_tx: &Sender<Ev>, text: String) -> Phase {
    if state.store.active.is_none() {
        store::new_dialog(&mut state.store);
    }
    let Some(id) = state.store.active.clone() else {
        return Phase::Idle;
    };
    let first = state
        .store
        .by_id(&id)
        .is_some_and(|d| d.messages.is_empty());
    store_message(
        state,
        ev_tx,
        &id,
        Msg::User {
            content: text.clone(),
        },
    );
    if first {
        // Имя диалога — по первому вопросу: без запроса к модели.
        let title = title_of(&text);
        if let Some(d) = state.store.by_id_mut(&id) {
            d.title = title.clone();
        }
        send(
            ev_tx,
            Ev::Title {
                dialog: id.clone(),
                title,
            },
        );
        // Имя — метаданные: дневник перезаписывается целиком.
        state.save(ev_tx);
        send_dialogs(ev_tx, &state.store);
    }
    send_messages(ev_tx, &state.store, &id);
    spawn_stream(
        state,
        ev_tx,
        Turn {
            dialog: id,
            partial: String::new(),
            queue: VecDeque::new(),
        },
    )
}

/// Стрим в отдельной задаче: куски и итог — одним каналом, воркер свободен
/// (`agr:agent-loop`). Итог идёт следом за последним куском: отдельный канал
/// дал бы `select!` взять его раньше очереди кусков.
fn spawn_stream(state: &State, ev_tx: &Sender<Ev>, turn: Turn) -> Phase {
    let Some(dialog) = state.store.by_id(&turn.dialog) else {
        return Phase::Idle;
    };
    // Индикатор занятости — с момента вызова модели, не с первого куска.
    send(
        ev_tx,
        Ev::Started {
            dialog: turn.dialog.clone(),
        },
    );
    let system = build_system();
    let tools = build_tools();
    let settings = state.settings.clone();
    let messages = dialog.messages.clone();
    let (ftx, frx) = tokio::sync::mpsc::unbounded_channel();
    let handle = tokio::spawn(async move {
        let reply = chat_stream(
            &settings,
            &system,
            &messages,
            &tools,
            |text| {
                let _ = ftx.send(Frame::Delta(text.to_string()));
            },
            |attempt| {
                let _ = ftx.send(Frame::Retry(attempt));
            },
        )
        .await;
        let _ = ftx.send(Frame::Done(reply));
    });
    Phase::Stream {
        turn,
        job: Job {
            frames: frx,
            handle,
        },
    }
}

/// Кадр стрима в ход: кусок — в накопленный текст и виду, итог — в [`resume`].
/// Ход берётся и возвращается целиком, чтобы кадр не мог применить себя к
/// фазе, которая уже не Stream. Куски до итога не теряются: итог приходит тем
/// же каналом и обрабатывается тем же путём (`agr:agent-loop`).
fn apply_frame(
    state: &mut State,
    ctx: &mut ToolCtx,
    ev_tx: &Sender<Ev>,
    phase: Phase,
    frame: Frame,
) -> Phase {
    match frame {
        Frame::Delta(text) => {
            let Phase::Stream { turn, job } = phase else {
                return phase;
            };
            let mut turn = turn;
            turn.partial.push_str(&text);
            send_lossy(
                ev_tx,
                Ev::Delta {
                    dialog: turn.dialog.clone(),
                    text,
                },
            );
            Phase::Stream { turn, job }
        }
        Frame::Retry(attempt) => {
            let Phase::Stream { turn, job } = phase else {
                return phase;
            };
            send_lossy(
                ev_tx,
                Ev::Retry {
                    dialog: turn.dialog.clone(),
                    attempt,
                },
            );
            Phase::Stream { turn, job }
        }
        Frame::Done(reply) => {
            let Phase::Stream { turn, .. } = phase else {
                return phase;
            };
            resume(state, ctx, ev_tx, turn, reply)
        }
    }
}

/// Стрим кончился: итог — в лог, вызовы — в очередь, дальше инструменты или
/// конец хода.
fn resume(
    state: &mut State,
    ctx: &mut ToolCtx,
    ev_tx: &Sender<Ev>,
    mut turn: Turn,
    reply: Result<Reply, String>,
) -> Phase {
    let reply = match reply {
        Ok(r) => r,
        Err(e) => {
            // Обрыв посреди ответа: то, что успело прийти, не теряем
            // (`agr:agent-jsonl`), незакрытые вызовы закрываются заглушками
            // (`agr:agent-tool-pairing`).
            let partial = std::mem::take(&mut turn.partial);
            if !partial.is_empty() {
                append_assistant(state, ev_tx, &turn.dialog, partial, Vec::new());
            }
            close_unpaired(state, ev_tx, &turn.dialog);
            send(
                ev_tx,
                Ev::Error {
                    dialog: turn.dialog.clone(),
                    text: e,
                },
            );
            return end_of_turn(state, ev_tx, &turn.dialog);
        }
    };
    if reply.finish.as_deref() == Some("tool_calls")
        && (reply.tool_calls.is_empty() || reply.calls_dropped > 0)
    {
        // Модель просила вызовы, но они не собрались: молчаливый конец хода
        // потерял бы их.
        let partial = std::mem::take(&mut turn.partial);
        if !partial.is_empty() {
            append_assistant(state, ev_tx, &turn.dialog, partial, Vec::new());
        }
        send(
            ev_tx,
            Ev::Error {
                dialog: turn.dialog.clone(),
                text: "модель запросила вызовы, но они не собрались".into(),
            },
        );
        return end_of_turn(state, ev_tx, &turn.dialog);
    }
    if reply.tool_calls.is_empty() && turn.partial.is_empty() {
        // Ни текста, ни вызовов: молчаливый конец хода не отличить от
        // зависания (`agr:agent-empty`).
        send(
            ev_tx,
            Ev::Error {
                dialog: turn.dialog.clone(),
                text: format!(
                    "модель не ответила (finish: {}) — попробуйте другую модель",
                    reply.finish.as_deref().unwrap_or("—")
                ),
            },
        );
        return end_of_turn(state, ev_tx, &turn.dialog);
    }
    if reply.finish.as_deref() == Some("content_filter") {
        send(
            ev_tx,
            Ev::Notice {
                dialog: turn.dialog.clone(),
                text: "ответ оборван фильтром модели".into(),
            },
        );
    }
    if reply.finish.as_deref() == Some("length") {
        send(
            ev_tx,
            Ev::Notice {
                dialog: turn.dialog.clone(),
                text: "ответ обрезан: не хватило лимита длины".into(),
            },
        );
    }

    // Накопленный текст и вызовы — сообщением ассистента в лог и на провод.
    let calls: Vec<CallReq> = reply
        .tool_calls
        .iter()
        .map(|c| CallReq {
            id: c.id.clone(),
            name: c.name.clone(),
            arguments: c.arguments.clone(),
        })
        .collect();
    let partial = std::mem::take(&mut turn.partial);
    if !calls.is_empty() || !partial.is_empty() {
        append_assistant(state, ev_tx, &turn.dialog, partial, calls.clone());
    }
    if calls.is_empty() {
        if let Some(tokens) = reply.tokens {
            let len = state
                .store
                .by_id(&turn.dialog)
                .map_or(0, |d| d.messages.len());
            state.usage.insert(turn.dialog.clone(), (tokens, len));
        }
        return end_of_turn(state, ev_tx, &turn.dialog);
    }
    turn.queue = reply.tool_calls.into();
    run_queue(state, ctx, ev_tx, turn)
}

/// Исполнить вызовы витка по очереди; на мутирующем — пауза и подтверждение.
/// Прерванный ход (`ctx.stop`) вызовы не исполняет — модель видит заглушки.
fn run_queue(state: &mut State, ctx: &mut ToolCtx, ev_tx: &Sender<Ev>, mut turn: Turn) -> Phase {
    let stopped = ctx.stop.load(std::sync::atomic::Ordering::SeqCst);
    while let Some(call) = turn.queue.pop_front() {
        if let Some(ev) = trace::trace(ctx, format!("→ {} {}", call.name, call.arguments)) {
            send_lossy(ev_tx, ev);
        }
        // Фаза хода для вида: работает инструмент (`agr:agent-progress`).
        // Пачка ждёт человека — о ней скажет запрос подтверждения.
        if !needs_confirm(&call.name) {
            send_lossy(
                ev_tx,
                Ev::Tool {
                    dialog: turn.dialog.clone(),
                    name: call.name.clone(),
                },
            );
        }
        if stopped {
            store_tool_stub(
                state,
                ev_tx,
                &turn.dialog,
                &call,
                "ERROR: the turn was aborted; stop and wait for new instructions",
            );
            continue;
        }
        if needs_confirm(&call.name) {
            match tools::prepare(ctx, &call.arguments) {
                Ok(batch) => {
                    if let Some(ev) =
                        trace::trace(ctx, format!("? {} ждёт подтверждения", call.name))
                    {
                        send_lossy(ev_tx, ev);
                    }
                    send(
                        ev_tx,
                        Ev::ConfirmRequest {
                            tool: call.name.clone(),
                            summary: batch.summary(),
                        },
                    );
                    turn.queue.push_front(call);
                    return Phase::Confirm { turn, batch };
                }
                // Пачка не собралась: отказ виден модели, человека не зовём.
                Err(e) => {
                    if let Some(ev) = trace::trace(ctx, format!("✗ {} отклонён: {e}", call.name))
                    {
                        send_lossy(ev_tx, ev);
                    }
                    store_tool_stub(state, ev_tx, &turn.dialog, &call, &format!("ERROR: {e}"));
                }
            }
            continue;
        }
        run_call(state, ctx, ev_tx, &mut turn, &call);
    }
    if stopped {
        // Вызовы прерванного хода не исполнялись — ход закончен.
        return end_of_turn(state, ev_tx, &turn.dialog);
    }
    spawn_stream(state, ev_tx, turn)
}

/// Ответ человека на подтверждение: применить пачку (или заглушка) и дальше по
/// очереди. Прерывание закрывает заглушками и остаток очереди.
fn confirm(
    state: &mut State,
    ctx: &mut ToolCtx,
    ev_tx: &Sender<Ev>,
    mut turn: Turn,
    batch: tools::Batch,
    cmd: Cmd,
) -> Phase {
    if matches!(cmd, Cmd::Abort) {
        // Модель прекращает начатое: и ждущий вызов, и остаток очереди.
        tools::restore(ctx);
        if let Some(call) = turn.queue.pop_front() {
            if let Some(ev) = trace::trace(ctx, format!("✗ {} прерван", call.name)) {
                send_lossy(ev_tx, ev);
            }
            store_tool_stub(
                state,
                ev_tx,
                &turn.dialog,
                &call,
                "ERROR: the human aborted the turn; stop and wait for new instructions",
            );
        }
        ctx.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        return run_queue(state, ctx, ev_tx, turn);
    }
    let Some(call) = turn.queue.pop_front() else {
        return Phase::Idle;
    };
    match cmd {
        Cmd::Confirm => {
            if let Some(ev) = trace::trace(ctx, format!("✓ {} подтверждён", call.name))
            {
                send_lossy(ev_tx, ev);
            }
            // Пачка применялась целиком и одним действием: один `Act::Sync`
            // (`agr:agent-batch`).
            let text = tools::apply_batch(ctx, batch);
            if let Some(ev) = trace::trace(ctx, format!("← {}: {text}", call.name)) {
                send_lossy(ev_tx, ev);
            }
            store_message(
                state,
                ev_tx,
                &turn.dialog,
                Msg::Tool {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    content: text,
                    is_error: false,
                },
            );
        }
        _ => {
            tools::restore(ctx);
            if let Some(ev) = trace::trace(ctx, format!("✗ {} отклонён", call.name)) {
                send_lossy(ev_tx, ev);
            }
            // Отказ виден модели: она не будет повторять вызов вслепую.
            store_tool_stub(
                state,
                ev_tx,
                &turn.dialog,
                &call,
                "ERROR: the human declined this call; do not repeat it",
            );
        }
    }
    run_queue(state, ctx, ev_tx, turn)
}

/// [`Cmd::Abort`] во время стрима: ответ сохранён как был, ход закончен.
fn abort_stream(state: &mut State, ev_tx: &Sender<Ev>, turn: Turn) -> Phase {
    if !turn.partial.is_empty() {
        let partial = turn.partial.clone();
        append_assistant(state, ev_tx, &turn.dialog, partial, Vec::new());
    }
    send(
        ev_tx,
        Ev::Notice {
            dialog: turn.dialog.clone(),
            text: "ход прерван".into(),
        },
    );
    end_of_turn(state, ev_tx, &turn.dialog)
}

/// Ход кончился: подхватить очередь уточнений или отпустить воркер
/// (`agr:agent-queue`).
fn end_of_turn(state: &mut State, ev_tx: &Sender<Ev>, dialog_id: &str) -> Phase {
    if state.queued.is_empty() {
        send_context(ev_tx, state, dialog_id);
        send(
            ev_tx,
            Ev::Done {
                dialog: dialog_id.to_string(),
            },
        );
        return Phase::Idle;
    }
    let text = drain_queue(state);
    begin_question(state, ev_tx, text)
}

/// Исполнить один вызов: результат сразу в лог как [`Msg::Tool`] — пары
/// «вызов — результат» живут в диалоге, а не в ходе (`agr:agent-tool-pairing`).
fn run_call(
    state: &mut State,
    ctx: &mut ToolCtx,
    ev_tx: &Sender<Ev>,
    turn: &mut Turn,
    call: &ToolCall,
) {
    let result = tools::execute(ctx, &call.name, &call.arguments);
    let is_error = result.is_err();
    if let Some(ev) = trace::trace(ctx, format!("← {}: {result:?}", call.name)) {
        send_lossy(ev_tx, ev);
    }
    let text = match result {
        Ok(s) => s,
        Err(e) => format!("ERROR: {e}"),
    };
    store_message(
        state,
        ev_tx,
        &turn.dialog,
        Msg::Tool {
            call_id: call.id.clone(),
            name: call.name.clone(),
            content: text,
            is_error,
        },
    );
}

/// Закрыть заглушками вызовы последнего ассистента без пары: лог остаётся
/// самодостаточным после обрыва (`agr:agent-tool-pairing`).
fn close_unpaired(state: &mut State, ev_tx: &Sender<Ev>, dialog_id: &str) {
    let Some(d) = state.store.by_id(dialog_id) else {
        return;
    };
    let Some(last) = d.messages.iter().rfind(|m| m.role() == "assistant") else {
        return;
    };
    let Msg::Assistant { tool_calls, .. } = last else {
        return;
    };
    let mut answered = std::collections::HashSet::new();
    let mut after = false;
    for m in &d.messages {
        if after {
            if let Msg::Tool { call_id, .. } = m {
                answered.insert(call_id.clone());
            }
        }
        if std::ptr::eq(m, last) {
            after = true;
        }
    }
    let stubs: Vec<Msg> = tool_calls
        .iter()
        .filter(|c| !answered.contains(&c.id))
        .map(|c| Msg::Tool {
            call_id: c.id.clone(),
            name: c.name.clone(),
            content: "ERROR: turn aborted".into(),
            is_error: true,
        })
        .collect();
    for m in stubs {
        store_message(state, ev_tx, dialog_id, m);
    }
}

/// Склеить накопленные за ход уточнения в одно сообщение: человек писал их
/// как продолжение одной мысли (`agr:agent-queue`).
fn drain_queue(state: &mut State) -> String {
    state.queued.drain(..).collect::<Vec<_>>().join("\n\n")
}

/// Результат без исполнения — для отказа и прерывания: пара в логе остаётся
/// парой (`agr:agent-tool-pairing`).
fn store_tool_stub(
    state: &mut State,
    ev_tx: &Sender<Ev>,
    dialog_id: &str,
    call: &ToolCall,
    text: &str,
) {
    store_message(
        state,
        ev_tx,
        dialog_id,
        Msg::Tool {
            call_id: call.id.clone(),
            name: call.name.clone(),
            content: text.into(),
            is_error: true,
        },
    );
}

/// Сообщение в ленту диалога и в дневник: строка дописывается в конец файла,
/// метаданные не трогаются (`agr:agent-jsonl`).
fn store_message(state: &mut State, ev_tx: &Sender<Ev>, dialog_id: &str, msg: Msg) {
    if let Some(d) = state.store.by_id_mut(dialog_id) {
        d.messages.push(msg.clone());
    }
    // Ответ модели инструментом — итогом в строку состояния
    // (`agr:agent-progress`); в ленту он не идёт.
    if let Msg::Tool {
        name,
        content,
        is_error,
        ..
    } = &msg
    {
        send_lossy(
            ev_tx,
            Ev::ToolDone {
                dialog: dialog_id.to_string(),
                name: name.clone(),
                lines: content.lines().count().max(1),
                is_error: *is_error,
            },
        );
    }
    if let Err(e) = store::append_message(&state.owner, &state.store, dialog_id, &msg) {
        send(
            ev_tx,
            Ev::Error {
                dialog: dialog_id.to_string(),
                text: format!("дневник: {e}"),
            },
        );
    }
}

/// Ассистент: текст и вызовы инструментов — форма провода, в логе тоже
/// (`agr:agent-tool-pairing`).
fn append_assistant(
    state: &mut State,
    ev_tx: &Sender<Ev>,
    dialog_id: &str,
    text: String,
    tool_calls: Vec<CallReq>,
) {
    store_message(
        state,
        ev_tx,
        dialog_id,
        Msg::Assistant {
            content: text,
            tool_calls,
        },
    );
}

/// Размер контекста диалога: счёт API, пока он покрывает ленту, иначе оценка.
fn context_of(state: &State, dialog: &Dialog) -> (u64, bool) {
    if let Some((tokens, messages)) = state.usage.get(&dialog.id) {
        if *messages >= dialog.messages.len() {
            return (*tokens, true);
        }
    }
    let system = build_system();
    let mut sum = estimate_tokens(&system);
    for m in &dialog.messages {
        sum += llm::msg_estimate(m);
    }
    (sum, false)
}

fn send(ev_tx: &Sender<Ev>, ev: Ev) {
    let _ = ev_tx.send(ev);
}

/// Отправить высокочастотное событие без блокировки: молча дропаем если UI не успевает.
fn send_lossy(ev_tx: &Sender<Ev>, ev: Ev) {
    let _ = ev_tx.try_send(ev);
}

fn send_dialogs(ev_tx: &Sender<Ev>, store: &Store) {
    send(
        ev_tx,
        Ev::Dialogs {
            active: store.active.clone(),
            dialogs: store
                .dialogs
                .iter()
                .map(|d| DialogView {
                    id: d.id.clone(),
                    title: d.title.clone(),
                    closed: d.closed,
                })
                .collect(),
        },
    );
}

fn send_messages(ev_tx: &Sender<Ev>, store: &Store, dialog_id: &str) {
    let Some(d) = store.by_id(dialog_id) else {
        return;
    };
    send(
        ev_tx,
        Ev::Messages {
            dialog: dialog_id.to_string(),
            messages: d
                .messages
                .iter()
                .enumerate()
                .map(|(index, m)| MsgView {
                    role: m.role().to_string(),
                    text: m.content().to_string(),
                    index,
                    name: match m {
                        Msg::Tool { name, .. } => name.clone(),
                        _ => String::new(),
                    },
                    is_error: matches!(m, Msg::Tool { is_error: true, .. }),
                })
                .collect(),
        },
    );
}

fn send_context(ev_tx: &Sender<Ev>, state: &State, dialog_id: &str) {
    let Some(d) = state.store.by_id(dialog_id) else {
        return;
    };
    let (tokens, exact) = context_of(state, d);
    send(
        ev_tx,
        Ev::Context {
            dialog: dialog_id.to_string(),
            tokens,
            exact,
        },
    );
}

#[cfg(all(test, feature = "tll"))]
mod tests {
    use super::*;

    fn state() -> State {
        // Ход пишет дневник: только в песочницу, не в настоящий конфиг.
        store::sandbox();
        State {
            owner: "f.dat".into(),
            store: Store::default(),
            settings: Settings::default(),
            usage: HashMap::new(),
            queued: VecDeque::new(),
        }
    }

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: format!("c-{name}"),
            name: name.into(),
            arguments: "{}".into(),
        }
    }

    /// Пачковый вызов останавливает ход и ждёт человека, а не исполняется:
    /// одна сводка на всю пачку, одна команда подтверждения (`agr:agent-batch`).
    #[test]
    fn batch_call_pauses_the_turn() {
        let mut s = state();
        let id = store::new_dialog(&mut s.store);
        let mut c = tools::tests_ctx();
        let seq = c.file.info.seq_begin.unwrap();
        let mut batch = call("edit_messages");
        batch.arguments =
            format!(r#"{{"edits":[{{"seq":{seq},"path":"header.order_id","value":"777"}}]}}"#);
        let turn = Turn {
            dialog: id.clone(),
            partial: String::new(),
            queue: vec![batch, call("goto")].into(),
        };
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        let phase = run_queue(&mut s, &mut c, &tx, turn);
        let Phase::Confirm { turn, batch } = phase else {
            panic!("ход должен встать на подтверждение");
        };
        // Ждущий вызов вернулся в голову очереди, второй за ним.
        assert_eq!(turn.queue.len(), 2);
        assert_eq!(turn.queue[0].name, "edit_messages");
        assert_eq!(turn.queue[1].name, "goto");
        assert_eq!(batch.edits.messages(), 1, "пачка собрана");
        assert!(c.edits.is_empty(), "пачка ждёт человека, не применена");
        // Человеку ушла одна сводка с количеством.
        let Ev::ConfirmRequest { tool, summary } = rx.try_recv().unwrap() else {
            panic!("должен прийти запрос подтверждения");
        };
        assert_eq!(tool, "edit_messages");
        assert!(
            summary
                .totals
                .iter()
                .any(|(k, v)| k == "сообщений" && v == "1"),
            "{summary:?}"
        );
        // Первой строкой — прогноз последнего seq (`agr:seq-forecast`).
        assert!(summary.lines[0].starts_with("последний seq"), "{summary:?}");
        assert!(summary.lines[1].contains("1000 → 777"), "{summary:?}");
    }

    /// Собрать пачку не удалось: модель получает отказ сама, человека не зовут.
    #[tokio::test]
    async fn broken_batch_never_asks_the_human() {
        let mut s = state();
        let id = store::new_dialog(&mut s.store);
        let mut batch = call("edit_messages");
        batch.arguments = r#"{"edits":[{"seq":0,"path":"нет.такого","value":"1"}]}"#.into();
        let turn = Turn {
            dialog: id.clone(),
            partial: String::new(),
            queue: vec![batch].into(),
        };
        let mut c = tools::tests_ctx();
        let (tx, _rx) = std::sync::mpsc::sync_channel(64);
        let _phase = run_queue(&mut s, &mut c, &tx, turn);
        assert!(c.edits.is_empty());
        let last = s.store.by_id(&id).unwrap().messages.last().unwrap().clone();
        assert_eq!(last.role(), "tool");
        assert!(last.content().contains("нет поля"), "{}", last.content());
    }

    /// Отказ виден модели: пачка не применена, кандидат откачен.
    #[tokio::test]
    async fn denial_reports_to_the_model() {
        let mut s = state();
        let id = store::new_dialog(&mut s.store);
        let mut c = tools::tests_ctx();
        let seq = c.file.info.seq_begin.unwrap();
        let mut batch = call("edit_messages");
        batch.arguments =
            format!(r#"{{"edits":[{{"seq":{seq},"path":"header.order_id","value":"777"}}]}}"#);
        let turn = Turn {
            dialog: id.clone(),
            partial: String::new(),
            queue: vec![batch].into(),
        };
        let (tx, _rx) = std::sync::mpsc::sync_channel(64);
        let phase = run_queue(&mut s, &mut c, &tx, turn);
        let (turn, batch) = match phase {
            Phase::Confirm { turn, batch } => (turn, batch),
            _ => panic!("ход должен встать на подтверждение"),
        };
        let phase = confirm(&mut s, &mut c, &tx, turn, batch, Cmd::Deny);
        assert!(
            matches!(phase, Phase::Idle) || matches!(phase, Phase::Stream { .. }),
            "после отказа ход продолжается"
        );
        assert!(c.edits.is_empty(), "отказ не создаёт правку");
        assert!(c.patch.is_none(), "кандидат откачен");
        // Заглушка — в лог: пара «вызов — результат» остаётся парой.
        let d = s.store.by_id(&id).unwrap();
        let last = d.messages.last().unwrap();
        assert_eq!(last.role(), "tool");
        assert!(last.content().contains("declined"), "{}", last.content());
    }

    /// Прерывание — отказ с другой причиной: модель прекращает начатое.
    #[tokio::test]
    async fn abort_denies_with_its_own_reason() {
        let mut s = state();
        let id = store::new_dialog(&mut s.store);
        let mut c = tools::tests_ctx();
        let seq = c.file.info.seq_begin.unwrap();
        let mut batch = call("edit_messages");
        batch.arguments =
            format!(r#"{{"edits":[{{"seq":{seq},"path":"header.order_id","value":"777"}}]}}"#);
        let turn = Turn {
            dialog: id.clone(),
            partial: String::new(),
            queue: vec![batch].into(),
        };
        let (tx, _rx) = std::sync::mpsc::sync_channel(64);
        let phase = run_queue(&mut s, &mut c, &tx, turn);
        let (turn, batch) = match phase {
            Phase::Confirm { turn, batch } => (turn, batch),
            _ => panic!("ход должен встать на подтверждение"),
        };
        let _ = confirm(&mut s, &mut c, &tx, turn, batch, Cmd::Abort);
        assert!(c.edits.is_empty(), "прерывание не создаёт правку");
        let d = s.store.by_id(&id).unwrap();
        let last = d.messages.last().unwrap();
        assert_eq!(last.role(), "tool");
        assert!(last.content().contains("aborted"), "{}", last.content());
    }

    /// Подтверждённая пачка применяется целиком одним действием и модель видит
    /// результат (`agr:agent-batch`).
    #[tokio::test]
    async fn confirmed_batch_applies_whole() {
        let mut s = state();
        let id = store::new_dialog(&mut s.store);
        let mut c = tools::tests_ctx();
        let seq = c.file.info.seq_begin.unwrap();
        let mut batch_call = call("edit_messages");
        batch_call.arguments = format!(
            r#"{{"edits":[{{"seq":{seq},"path":"header.order_id","value":"777"}},{{"seq":{},"path":"symbol","value":"VTBR"}}]}}"#,
            seq + 1
        );
        let turn = Turn {
            dialog: id.clone(),
            partial: String::new(),
            queue: vec![batch_call].into(),
        };
        let (tx, _rx) = std::sync::mpsc::sync_channel(64);
        let phase = run_queue(&mut s, &mut c, &tx, turn);
        let (turn, batch) = match phase {
            Phase::Confirm { turn, batch } => (turn, batch),
            _ => panic!("ход должен встать на подтверждение"),
        };
        let _ = confirm(&mut s, &mut c, &tx, turn, batch, Cmd::Confirm);
        assert_eq!(c.edits.messages(), 2, "пачка применена целиком");
        let last = s.store.by_id(&id).unwrap().messages.last().unwrap().clone();
        assert_eq!(last.role(), "tool");
        assert!(last.content().contains("2 сообщений"), "{}", last.content());
    }

    /// Уточнения за ход склеиваются в одно сообщение через пустую строку
    /// (`agr:agent-queue`).
    #[test]
    fn queued_questions_join_into_one_message() {
        let mut s = state();
        s.queued.push_back("а сколько их?".into());
        s.queued.push_back("покажи первый".into());
        assert_eq!(drain_queue(&mut s), "а сколько их?\n\nпокажи первый");
        assert!(s.queued.is_empty(), "очередь опустела");
    }

    /// Обрыв после ассистента с вызовами: непарные закрываются заглушками в
    /// логе, пары не тронуты (`agr:agent-tool-pairing`).
    #[test]
    fn close_unpaired_stubs_only_missing_calls() {
        let mut s = state();
        let id = store::new_dialog(&mut s.store);
        append_assistant(
            &mut s,
            &(std::sync::mpsc::sync_channel(64).0),
            &id,
            "смотрю".into(),
            vec![
                CallReq {
                    id: "a".into(),
                    name: "read_messages".into(),
                    arguments: "{}".into(),
                },
                CallReq {
                    id: "b".into(),
                    name: "search".into(),
                    arguments: "{}".into(),
                },
            ],
        );
        let (tx, _rx) = std::sync::mpsc::sync_channel(64);
        store_tool_stub(
            &mut s,
            &tx,
            &id,
            &ToolCall {
                id: "b".into(),
                name: "search".into(),
                arguments: "{}".into(),
            },
            "нашлось",
        );
        close_unpaired(&mut s, &tx, &id);
        let d = s.store.by_id(&id).unwrap();
        let tools: Vec<_> = d
            .messages
            .iter()
            .filter(|m| m.role() == "tool")
            .map(|m| match m {
                Msg::Tool {
                    call_id,
                    content,
                    is_error,
                    ..
                } => (call_id.as_str(), content.clone(), *is_error),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(tools.len(), 2, "пара «search» не задублирована");
        assert_eq!(tools[0].0, "b");
        assert_eq!(tools[0].1, "нашлось", "настоящий результат на месте");
        assert_eq!(tools[1].0, "a");
        assert!(tools[1].2, "непарный закрыт заглушкой");
        assert!(tools[1].1.contains("aborted"));
    }

    /// Пока ход ждёт подтверждения или отвечает модель, команды, меняющие
    /// активный диалог, не принимаются (`agr:agent-one-turn`).
    #[tokio::test]
    async fn busy_phases_block_dialog_commands() {
        let idle = Phase::Idle;
        assert!(!busy(&idle));
        let confirm = Phase::Confirm {
            turn: Turn {
                dialog: "x".into(),
                partial: String::new(),
                queue: VecDeque::new(),
            },
            batch: tools::Batch::default(),
        };
        assert!(busy(&confirm));
        let stream = Phase::Stream {
            turn: Turn {
                dialog: "d1".into(),
                partial: String::new(),
                queue: VecDeque::new(),
            },
            job: Job {
                frames: tokio::sync::mpsc::unbounded_channel().1,
                handle: tokio::spawn(async {}),
            },
        };
        assert!(busy(&stream), "активный ход должен блокировать команды");
    }

    /// Куски стрима не теряются, когда итог уже лежит в очереди: итог идёт тем
    /// же каналом и применяется после них, а не вместо них (`agr:agent-loop`).
    #[tokio::test]
    async fn deltas_survive_a_ready_result() {
        let mut s = state();
        let id = store::new_dialog(&mut s.store);
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        let (ftx, frx) = tokio::sync::mpsc::unbounded_channel();
        let mut phase = Phase::Stream {
            turn: Turn {
                dialog: id.clone(),
                partial: String::new(),
                queue: VecDeque::new(),
            },
            job: Job {
                frames: frx,
                handle: tokio::spawn(async {}),
            },
        };
        // Задача стрима успела прислать всё: куски и итог уже в канале.
        ftx.send(Frame::Delta("начало ".into())).unwrap();
        ftx.send(Frame::Delta("конец".into())).unwrap();
        ftx.send(Frame::Done(Ok(Reply {
            content: "начало конец".into(),
            ..Default::default()
        })))
        .unwrap();
        drop(ftx);
        let mut c = tools::tests_ctx();
        // Воркер читает кадры по одному: оба куска применяются до итога.
        while let Phase::Stream { .. } = phase {
            let frame = {
                let Phase::Stream { job, .. } = &mut phase else {
                    unreachable!("фаза была Stream");
                };
                match job.frames.try_recv() {
                    Ok(f) => f,
                    Err(..) => break,
                }
            };
            phase = apply_frame(&mut s, &mut c, &tx, phase, frame);
        }
        let d = s.store.by_id(&id).unwrap();
        let last = d.messages.last().expect("ответ сохранён");
        assert_eq!(last.role(), "assistant");
        assert_eq!(last.content(), "начало конец", "хвост не потерян");
        let deltas: Vec<_> = rx.try_iter().collect();
        assert!(
            deltas
                .iter()
                .any(|e| matches!(e, Ev::Delta { text, .. } if text == "начало ")),
            "куски дошли до вида: {deltas:?}"
        );
    }

    /// `agr:agent-empty`
    /// Пустой ответ модели — ошибка с причиной, а не молчаливый конец хода.
    #[test]
    fn empty_reply_is_an_error_with_finish_reason() {
        let mut s = state();
        let id = store::new_dialog(&mut s.store);
        let mut c = tools::tests_ctx();
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        let turn = Turn {
            dialog: id.clone(),
            partial: String::new(),
            queue: VecDeque::new(),
        };
        let reply = Reply {
            finish: Some("content_filter".into()),
            ..Default::default()
        };
        let phase = resume(&mut s, &mut c, &tx, turn, Ok(reply));
        assert!(matches!(phase, Phase::Idle));
        let evs: Vec<_> = rx.try_iter().collect();
        assert!(
            evs.iter().any(|e| matches!(e, Ev::Error { text, .. }
                if text.contains("не ответила") && text.contains("content_filter"))),
            "{evs:?}"
        );
    }

    /// `agr:agent-progress`
    /// Перед инструментом вид узнаёт его имя.
    #[tokio::test]
    async fn tool_phase_is_announced_before_execution() {
        let mut s = state();
        let id = store::new_dialog(&mut s.store);
        let mut c = tools::tests_ctx();
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        let turn = Turn {
            dialog: id.clone(),
            partial: String::new(),
            queue: vec![call("file_overview")].into(),
        };
        let _ = run_queue(&mut s, &mut c, &tx, turn);
        let evs: Vec<_> = rx.try_iter().collect();
        assert!(
            evs.iter()
                .any(|e| matches!(e, Ev::Tool { name, .. } if name == "file_overview")),
            "{evs:?}"
        );
    }

    /// `agr:agent-progress`
    /// Результат инструмента виден виду итогом: имя и число строк.
    #[tokio::test]
    async fn tool_result_is_announced_with_line_count() {
        let mut s = state();
        let id = store::new_dialog(&mut s.store);
        let mut c = tools::tests_ctx();
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        let turn = Turn {
            dialog: id.clone(),
            partial: String::new(),
            queue: vec![call("file_overview")].into(),
        };
        let _ = run_queue(&mut s, &mut c, &tx, turn);
        let evs: Vec<_> = rx.try_iter().collect();
        assert!(
            evs.iter().any(|e| matches!(e, Ev::ToolDone { name, lines, is_error: false, .. }
                if name == "file_overview" && *lines > 1)),
            "{evs:?}"
        );
    }

    /// Прерванный ход не исполняет остаток вызовов — модель видит заглушки
    /// (`agr:agent-loop`).
    #[test]
    fn aborted_turn_stubs_the_queue() {
        let mut s = state();
        let id = store::new_dialog(&mut s.store);
        let mut c = tools::tests_ctx();
        c.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let (tx, _rx) = std::sync::mpsc::sync_channel(64);
        let turn = Turn {
            dialog: id.clone(),
            partial: String::new(),
            queue: vec![call("goto"), call("read_messages")].into(),
        };
        let phase = run_queue(&mut s, &mut c, &tx, turn);
        assert!(matches!(phase, Phase::Idle), "прерванный ход кончился");
        let tools: Vec<_> = s
            .store
            .by_id(&id)
            .unwrap()
            .messages
            .iter()
            .filter(|m| m.role() == "tool")
            .collect();
        assert_eq!(tools.len(), 2, "оба вызова закрыты заглушками");
        assert!(tools.iter().all(|m| m.content().contains("aborted")));
    }

    /// Имя диалога — первая строка вопроса, длинная обрезается.
    #[test]
    fn title_from_first_question() {
        assert_eq!(title_of("  почему пусто?\nещё строка"), "почему пусто?");
        let long = "я".repeat(60);
        let title = title_of(&long);
        assert_eq!(title.chars().count(), 41);
        assert!(title.ends_with('…'));
    }

    /// Оценка контекста растёт с лентой и помечена как неточная.
    #[test]
    fn context_estimate_grows_with_messages() {
        let mut state = state();
        let id = store::new_dialog(&mut state.store);
        let (empty, exact) = context_of(&state, state.store.by_id(&id).unwrap());
        assert!(!exact);
        state
            .store
            .by_id_mut(&id)
            .unwrap()
            .messages
            .push(Msg::User {
                content: "п".repeat(160),
            });
        let (full, _) = context_of(&state, state.store.by_id(&id).unwrap());
        assert!(full > empty);
    }

    /// Счёт API считается точным, пока покрывает ленту, и протухает с новым ходом.
    #[test]
    fn api_count_is_exact_until_messages_grow() {
        let mut state = state();
        let id = store::new_dialog(&mut state.store);
        state.usage.insert(id.clone(), (500, 0));
        assert_eq!(
            context_of(&state, state.store.by_id(&id).unwrap()),
            (500, true)
        );
        state
            .store
            .by_id_mut(&id)
            .unwrap()
            .messages
            .push(Msg::User {
                content: "вопрос".into(),
            });
        let (tokens, exact) = context_of(&state, state.store.by_id(&id).unwrap());
        assert!(!exact);
        assert!(tokens > 0);
    }
}
