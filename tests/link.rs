//! Линк вместо файла: канал пишется в спул, вид читает спул как растущий файл.
//!
//! Живой линк здесь — `lua+file://` поверх растущего датника без `autoclose`:
//! настоящий канал libtll с префиксом, который не закрывается на конце
//! данных, — как онлайн-хвост сетевого потока. Сеть и `resolve://` с живым
//! сервером — в `MANUAL.md`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tui_patch::data::File;
use tui_patch::desk::{Desk, Key};
use tui_patch::io::link::{Link, LinkState};
use tui_patch::io::{self, Context, Info, Msg, Writer};

fn root() -> PathBuf {
    std::env::var_os("CARGO_MANIFEST_DIR")
        .map(Into::into)
        .unwrap_or_else(|| env!("CARGO_MANIFEST_DIR").into())
}

/// Нет tll-lua — живого линка не собрать.
fn lua() -> bool {
    io::quiet();
    let ok = Context::new().ensure_lua(io::Backend::TLL);
    if !ok {
        eprintln!("нет tll-lua — проверки линка пропущены");
    }
    ok
}

/// Каталог теста с копией `plain.dat` (seq 0..=399) — источник линка.
fn source(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mutator-link-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("src.dat");
    std::fs::copy(root().join("tests/data/plain.dat"), &path).unwrap();
    path
}

/// Живой линк на растущий файл: `lua+` с проходным кодом, без `autoclose`.
fn url(path: &Path) -> String {
    format!(
        "lua+file://{};autoclose=no;lua.code=function tll_on_data(seq, name, data) \
         tll_callback(seq, name, data) end",
        path.display()
    )
}

/// Дописать в источник сообщения с этими seq.
fn grow(path: &Path, seqs: &[i64]) {
    let f = File::open(path).unwrap();
    let scheme = f.scheme_url.clone().expect("схема");
    let info: Info = f.info.clone();
    let ctx = Context::new();
    let mut w = Writer::open(&ctx, path, &scheme, &info, false).expect("дописать");
    for &seq in seqs {
        w.post(&Msg {
            seq,
            msgid: 30,
            data: vec![0; 4],
        })
        .expect("post");
    }
}

fn start(url: &str, open: &str, keep: Option<&Path>) -> Link {
    Link::start(url, open, None, keep, Arc::new(|| {})).expect("линк")
}

/// Вид на готовом спуле линка.
fn desk(link: Link) -> Desk {
    link.ready(|_| {}).expect("спул готов");
    let mut d = Desk::open(File::open_link(link).unwrap(), Arc::new(|| {}));
    d.height = 8;
    d
}

/// Крутить вид, пока `ok` не станет правдой.
fn until(d: &mut Desk, what: &str, mut ok: impl FnMut(&Desk) -> bool) {
    let t = Instant::now();
    loop {
        d.poll();
        if ok(d) && d.idle() {
            return;
        }
        d.wait(Duration::from_millis(20));
        assert!(
            t.elapsed() < Duration::from_secs(30),
            "{what}: не дождались; статус {:?}, линк {:?}",
            d.status,
            d.link_status().map(|s| s.line())
        );
    }
}

fn total(d: &Desk) -> usize {
    d.index.as_ref().map_or(0, |i| i.total)
}

fn sel(d: &Desk) -> i64 {
    d.selected().expect("выбрана строка").seq
}

/// `agr:link-tail` — новые сообщения линка дописываются в вид сами: индекс
/// растёт, хвост достижим шагом вниз, а курсор стоит на своём seq — и в
/// середине, и на последней строке.
#[test]
fn live_tail_grows_without_moving_the_cursor() {
    if !lua() {
        return;
    }
    let src = source("tail");
    let mut d = desk(start(&url(&src), "", None));
    until(&mut d, "весь источник", |d| total(d) == 400);

    d.key(Key::End);
    until(&mut d, "в конце", |d| {
        d.selected().is_some_and(|r| r.seq == 399)
    });
    grow(&src, &[400, 401, 402]);
    until(&mut d, "хвост пришёл", |d| total(d) == 403);
    assert_eq!(
        sel(&d),
        399,
        "на последней строке курсор не уехал за хвостом"
    );
    d.key(Key::Down);
    until(&mut d, "шаг", |d| d.idle());
    assert_eq!(sel(&d), 400, "дописанное достижимо шагом вниз");

    d.key(Key::Up);
    d.key(Key::Up);
    until(&mut d, "вверх", |d| d.idle());
    assert_eq!(sel(&d), 398);
    grow(&src, &[403]);
    until(&mut d, "ещё хвост", |d| total(d) == 404);
    assert_eq!(sel(&d), 398, "в середине курсор стоит");
    let _ = std::fs::remove_dir_all(src.parent().unwrap());
}

/// `agr:link-tail` — фильтр линка находит и то, что пришло после его скана.
#[test]
fn filter_follows_the_live_tail() {
    if !lua() {
        return;
    }
    let src = source("filter");
    let mut d = desk(start(&url(&src), "", None));
    until(&mut d, "весь источник", |d| total(d) == 400);
    d.set_filter(tui_patch::lua::Filter::expr("msgid == 30"));
    until(&mut d, "фильтр", |d| {
        d.matches.as_ref().is_some_and(|m| m.is_done())
    });
    let before = d.matches.as_ref().unwrap().len();
    grow(&src, &[400, 401]);
    until(&mut d, "хвост в фильтре", |d| {
        d.matches.as_ref().is_some_and(|m| m.len() == before + 2)
    });
    let _ = std::fs::remove_dir_all(src.parent().unwrap());
}

/// `agr:link-read-only` — линк только смотрят: правка поля и фиксация
/// отказывают с причиной.
#[test]
fn link_view_refuses_edits_and_commit() {
    if !lua() {
        return;
    }
    let src = source("ro");
    let mut d = desk(start(&url(&src), "", None));
    until(&mut d, "весь источник", |d| total(d) == 400);
    assert!(d.read_only());
    d.key(Key::Char('w'));
    assert!(d.commit.is_none(), "диалог фиксации не открылся");
    assert!(d.status.contains("только просмотр"), "{}", d.status);
    d.key(Key::Enter);
    d.focus = tui_patch::desk::Focus::Detail;
    until(&mut d, "деталь", |d| d.detail.is_some());
    d.key(Key::Char('e'));
    assert!(d.prompt.is_none(), "строки правки нет");
    assert!(d.status.contains("только просмотр"), "{}", d.status);
    let _ = std::fs::remove_dir_all(src.parent().unwrap());
}

/// `agr:link-state` — состояние линка видно всегда: Active у живого, Error с
/// текстом у того, что не открылся.
#[test]
fn link_state_and_error_are_visible() {
    if !lua() {
        return;
    }
    let src = source("state");
    let mut d = desk(start(&url(&src), "", None));
    until(&mut d, "весь источник", |d| total(d) == 400);
    let s = d.link_status().expect("состояние линка");
    assert_eq!(s.state, LinkState::Active);
    assert!(s.line().contains("Active"), "{}", s.line());
    assert_eq!(s.count, 400);

    let bad = start("resolve://nosuch/x", "", None);
    let e = bad.ready(|_| {}).unwrap_err();
    assert!(e.contains("Error"), "{e}");
    assert_eq!(bad.status().state, LinkState::Error);
    assert!(bad.status().error.is_some(), "текст ошибки есть");
    let _ = std::fs::remove_dir_all(src.parent().unwrap());
}

/// `agr:link-state` — в кадре TUI заголовок — url линка, справа — его
/// состояние, цветом Active.
#[test]
fn tui_title_shows_the_link_and_its_state() {
    if !lua() {
        return;
    }
    let src = source("tui");
    let mut d = desk(start(&url(&src), "", None));
    until(&mut d, "весь источник", |d| total(d) == 400);
    let g = tui_patch::ui::tui::frame(&mut d, 400, 20);
    let title = g.text(0);
    assert!(title.contains("lua+file://"), "{title}");
    assert!(
        title.contains("● Active") && title.contains("400"),
        "{title}"
    );
    let (_, fg) = tui_patch::ui::tui::link_line(&d).unwrap();
    assert_eq!(fg, tui_patch::ui::theme::OK);
    let _ = std::fs::remove_dir_all(src.parent().unwrap());
}

/// `agr:link-spool` — временный спул удаляется вместе с линком, с `--keep` —
/// остаётся датником, который открывается как обычный файл.
#[test]
fn spool_is_removed_unless_kept() {
    if !lua() {
        return;
    }
    let src = source("spool");
    let tmp = start(&url(&src), "", None);
    let path = tmp.ready(|_| {}).unwrap();
    assert!(path.exists());
    drop(tmp);
    assert!(!path.exists(), "временный спул удалён");

    let keep = src.parent().unwrap().join("keep");
    let kept = start(&url(&src), "", Some(&keep));
    let path = kept.ready(|_| {}).unwrap();
    let t = Instant::now();
    while kept.status().count < 400 {
        assert!(t.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(kept);
    assert!(
        path.starts_with(&keep) && path.exists(),
        "{}",
        path.display()
    );
    let f = File::open(&path).unwrap();
    assert_eq!((f.info.seq_begin, f.info.seq_last), (Some(0), Some(399)));
    let _ = std::fs::remove_dir_all(src.parent().unwrap());
}

/// `agr:link-open` — параметры открытия уходят каналу (`seq=` — с этого seq);
/// открыть заново с другими — новый спул, вид переезжает на него.
#[test]
fn open_params_reach_the_channel_and_reopen_switches_spool() {
    if !lua() {
        return;
    }
    let src = source("open");
    let mut d = desk(start(&url(&src), "seq=300", None));
    until(&mut d, "с seq 300", |d| total(d) == 100);
    assert_eq!(d.file.info.seq_begin, Some(300));
    let first = d.file.parts[0].path.clone();

    d.key(Key::Char('o'));
    let f = d.link_form.as_ref().expect("форма «Открыть линк»");
    assert_eq!(
        (f.url.text.as_str(), f.open.text.as_str()),
        (url(&src).as_str(), "seq=300")
    );
    for _ in 0..3 {
        d.key(Key::Backspace);
    }
    for c in "350".chars() {
        d.key(Key::Char(c));
    }
    d.key(Key::Enter);
    assert!(d.open_link.is_none(), "тот же url — переоткрывает сам вид");
    until(&mut d, "новый спул", |d| {
        d.file.parts[0].path != first && total(d) == 50
    });
    assert_eq!(d.file.info.seq_begin, Some(350));
    // Прежний спул держат потоки старого воркера — удаляется, когда они уйдут.
    let t = Instant::now();
    while first.exists() {
        assert!(t.elapsed() < Duration::from_secs(10), "прежний спул удалён");
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = std::fs::remove_dir_all(src.parent().unwrap());
}

/// `agr:link-open` — форма `o` с другим url: вид сам файл не меняет, а
/// отдаёт url и параметры хозяину (окну, терминалу, браузеру).
#[test]
fn form_with_other_url_goes_to_the_host() {
    if !lua() {
        return;
    }
    let src = source("other");
    let mut d = desk(start(&url(&src), "", None));
    until(&mut d, "весь файл", |d| total(d) == 400);
    d.key(Key::Char('o'));
    let g = tui_patch::ui::tui::frame(&mut d, 100, 30);
    let screen: String = (0..g.rows()).map(|r| g.text(r) + "\n").collect();
    assert!(
        screen.contains("Открыть линк"),
        "форма видна в сетке F2:\n{screen}"
    );
    d.key(Key::Tab);
    d.key(Key::Ctrl('u'));
    for c in "resolve://svc/ch".chars() {
        d.key(Key::Char(c));
    }
    d.key(Key::Tab);
    for c in "mode=initial".chars() {
        d.key(Key::Char(c));
    }
    d.key(Key::Enter);
    assert!(d.link_form.is_none());
    assert_eq!(
        d.open_link,
        Some(("resolve://svc/ch".into(), "mode=initial".into()))
    );
    let _ = std::fs::remove_dir_all(src.parent().unwrap());
}

/// Пара tcp-линков на одной схеме: сервер и клиент к нему — как канал стенда,
/// который принимает сообщения.
fn tcp_pair(name: &str) -> (Link, Link) {
    let scheme = File::open(&root().join("tests/data/plain.dat"))
        .unwrap()
        .scheme_url
        .expect("схема");
    let port = 30000 + (std::process::id() % 10000) as u16 + name.len() as u16;
    let server = start(
        &format!("tcp://127.0.0.1:{port};mode=server;scheme={scheme}"),
        "",
        None,
    );
    server.ready(|_| {}).expect("сервер");
    let client = start(
        &format!("tcp://127.0.0.1:{port};mode=client;scheme={scheme}"),
        "",
        None,
    );
    client.ready(|_| {}).expect("клиент");
    (server, client)
}

/// Дождаться, пока `ok` не скажет «да» о состоянии линка.
fn until_link(l: &Link, what: &str, ok: impl Fn(&tui_patch::io::link::Status) -> bool) {
    let t = Instant::now();
    while !ok(&l.status()) {
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "{what}: {}",
            l.status().line()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `agr:link-post` — канал, который принимает сообщения, их и получает:
/// клиент шлёт собранное по полям сообщение, сервер пишет его в спул тем же
/// телом; линк без `TLL_CAPS_OUTPUT` — отказ без отправки.
#[test]
fn link_post_reaches_the_channel() {
    io::quiet();
    let (server, client) = tcp_pair("post");
    until_link(&client, "клиент Active", |s| s.post.is_some());
    assert_eq!(client.status().post, Some(true));
    let f = File::open(&root().join("tests/data/plain.dat")).unwrap();
    let body = [("header.order_id".to_string(), "777".to_string())]
        .into_iter()
        .collect();
    let m = tui_patch::io::link::compose(&f.scheme, "Order", 42, &body).unwrap();
    client.post(m.clone()).expect("отправка");
    until_link(&server, "сервер получил", |s| s.count == 1);
    let got = File::open(server.spool()).unwrap();
    let mut r =
        tui_patch::io::Reader::open(&Context::new(), &tui_patch::io::Open::file(server.spool()))
            .unwrap();
    let msg = r.next().expect("сообщение в спуле");
    assert_eq!(
        (msg.seq, msg.msgid, msg.data.clone()),
        (42, m.msgid, m.data.clone())
    );
    let st = got.scheme.message(msg.msgid).unwrap();
    assert_eq!(
        tui_patch::decode::field(&got.scheme, st, &msg.data, "header.order_id").as_deref(),
        Some("777")
    );
}

/// `agr:link-post` — канал, который только читает (`file://` под `lua+`),
/// сообщений не принимает: отправка — отказ без вызова канала.
#[test]
fn read_only_link_refuses_post() {
    if !lua() {
        return;
    }
    let src = source("ro-post");
    let l = start(&url(&src), "", None);
    l.ready(|_| {}).expect("спул");
    until_link(&l, "Active", |s| s.post.is_some());
    assert_eq!(l.status().post, Some(false), "{}", l.status().line());
    let e = l
        .post(Msg {
            seq: 1,
            msgid: 30,
            data: vec![0; 4],
        })
        .unwrap_err();
    assert!(e.contains("не принимает"), "{e}");
    let _ = std::fs::remove_dir_all(src.parent().unwrap());
}

/// `agr:link-post` — `s` в виде линка, который принимает сообщения: форма со
/// всеми полями типа, правка поля, Enter — сообщение у канала; у линка,
/// который только читает, `s` — отказ в строке состояния.
#[test]
fn send_form_posts_from_the_view() {
    io::quiet();
    let (server, client) = tcp_pair("send-view");
    let mut d = desk(client);
    until(&mut d, "линк Active", |d| {
        d.link_status().is_some_and(|s| s.post.is_some())
    });
    d.key(Key::Char('s'));
    let f = d.send_form.as_mut().expect("форма открылась");
    while f.name() != "Order" {
        f.sel = 0;
        f.key(Key::Right, &d.file.scheme.clone());
    }
    let at = f
        .fields
        .iter()
        .position(|x| x.path == "header.order_id")
        .unwrap();
    d.send_form.as_mut().unwrap().sel = at + 2;
    d.key(Key::Ctrl('u'));
    for c in "4242".chars() {
        d.key(Key::Char(c));
    }
    let g = tui_patch::ui::tui::frame(&mut d, 120, 30);
    let screen: String = (0..30).map(|r| g.text(r)).collect::<Vec<_>>().join("\n");
    assert!(
        screen.contains("Отправить в линк") && screen.contains("4242"),
        "{screen}"
    );
    d.key(Key::Enter);
    assert!(
        d.send_form.is_none(),
        "форма закрылась: {:?}",
        d.send_form.as_ref().map(|f| &f.error)
    );
    assert!(d.status.contains("отправлено Order"), "{}", d.status);
    until_link(&server, "сервер получил", |s| s.count == 1);
    let f = File::open(server.spool()).unwrap();
    let mut r =
        tui_patch::io::Reader::open(&Context::new(), &tui_patch::io::Open::file(server.spool()))
            .unwrap();
    let m = r.next().unwrap();
    let st = f.scheme.message(m.msgid).unwrap();
    assert_eq!(
        tui_patch::decode::field(&f.scheme, st, &m.data, "header.order_id").as_deref(),
        Some("4242")
    );

    if lua() {
        let src = source("ro-send");
        let mut d = desk(start(&url(&src), "", None));
        until(&mut d, "весь источник", |d| total(d) == 400);
        d.key(Key::Char('s'));
        assert!(d.send_form.is_none());
        assert!(d.status.contains("не принимает"), "{}", d.status);
        let _ = std::fs::remove_dir_all(src.parent().unwrap());
    }
}
