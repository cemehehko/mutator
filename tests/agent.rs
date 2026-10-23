//! Действия агента на живом [`Desk`]: агент не трогает состояние просмотрщика
//! сам, а шлёт просьбу по каналу, и её применяет тот же `Desk`, что у человека
//! (`agr:agent-same-desk`). Сети здесь нет: проверяется связка «канал действий →
//! вид», а не разговор с моделью.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tui_patch::agent::Act;
use tui_patch::data::File;
use tui_patch::desk::Desk;
use tui_patch::edit::Edits;
use tui_patch::io;
use tui_patch::lua::Filter;

fn root() -> PathBuf {
    std::env::var_os("CARGO_MANIFEST_DIR")
        .map(Into::into)
        .unwrap_or_else(|| env!("CARGO_MANIFEST_DIR").into())
}

fn fixture(name: &str) -> PathBuf {
    root().join("tests/data").join(format!("{name}.dat"))
}

fn open(name: &str, height: usize) -> Desk {
    io::quiet();
    let mut d = Desk::open(File::open(&fixture(name)).unwrap(), Arc::new(|| {}));
    d.height = height;
    let t = Instant::now();
    while d.index.is_none() {
        d.wait(Duration::from_millis(100));
        assert!(t.elapsed() < Duration::from_secs(30), "индекс не пришёл");
    }
    settle(&mut d);
    d
}

/// Дождаться, пока не останется запросов в полёте.
fn settle(d: &mut Desk) {
    let t = Instant::now();
    loop {
        d.poll();
        if d.idle() {
            return;
        }
        let got = d.wait(Duration::from_millis(100));
        if !got && d.idle() {
            return;
        }
        assert!(
            t.elapsed() < Duration::from_secs(30),
            "не успокоилось: {}",
            d.status
        );
    }
}

fn lua() -> bool {
    io::quiet();
    let ok = io::Context::new().ensure_lua(tui_patch::io::Backend::TLL);
    if !ok {
        eprintln!("нет tll-lua (TUI_PATCH_LUA) — проверки фильтра пропущены");
    }
    ok
}

fn patch() -> bool {
    io::quiet();
    let ok = io::Context::new().ensure_patch(tui_patch::io::Backend::TLL);
    if !ok {
        eprintln!("нет tll-patch (TUI_PATCH_PATCH) — проверки правок пропущены");
    }
    ok
}

/// `agr:agent-same-desk` — переход и колонка, присланные агентом, меняют тот же
/// `Desk`: просьба идёт по каналу к состоянию человека, а не к его копии.
#[test]
fn agent_actions_change_the_same_desk() {
    let mut d = open("lz4", 10);
    assert!(!d.columns.contains(&"u64".to_string()));
    let tx = d.act_tx();
    tx.send(Act::Goto("+123".into())).unwrap();
    tx.send(Act::SetColumns(vec!["u64".into()])).unwrap();
    assert!(d.poll(), "просьбы агента замечены");
    settle(&mut d);
    assert_eq!(d.sel_row(), Some(123), "{}", d.status);
    assert!(d.columns.contains(&"u64".to_string()));
}

/// `agr:agent-same-desk` — фильтр агента исполняет тот же `Desk`: человек
/// видит ровно те строки, что просил агент.
#[test]
fn agent_filter_applies_to_the_same_desk() {
    if !lua() {
        return;
    }
    let mut d = open("lz4", 8);
    let tx = d.act_tx();
    tx.send(Act::Filter(Filter::expr("name == 'Cancel'")))
        .unwrap();
    d.poll();
    let t = Instant::now();
    while !d.matches.as_ref().is_some_and(|m| m.is_done()) {
        d.wait(Duration::from_millis(100));
        assert!(t.elapsed() < Duration::from_secs(30), "{}", d.status);
    }
    settle(&mut d);
    let m = d.matches.clone().expect("фильтр агента поставлен");
    assert!(!m.is_empty(), "{}", d.status);
    assert!(
        d.rows.iter().all(|r| r.name == "Cancel"),
        "в окне чужие строки"
    );
}

/// `agr:agent-gen` — правка устаревшего поколения отбрасывается: пока агент
/// думал, человек переоткрыл файл, и правка по старой картине не применяется.
#[test]
fn stale_edit_is_refused_after_reopen() {
    if !patch() {
        return;
    }
    let mut d = open("lz4", 10);
    let seq = d.rows.first().map(|r| r.seq).expect("строка");
    let mut edits = Edits::default();
    edits.set(seq, "Order", "header.order_id", "1000", "777");
    // Файл «переоткрыли»: поколение выросло, правка несёт прошлое.
    let gen = d.file_gen().load(std::sync::atomic::Ordering::SeqCst);
    d.file_gen()
        .store(gen + 1, std::sync::atomic::Ordering::SeqCst);
    d.act_tx().send(Act::Sync { edits, gen }).unwrap();
    settle(&mut d);
    assert!(
        !d.edited(seq, "header.order_id"),
        "устаревшая правка не легла: {}",
        d.status
    );
    assert!(d.status.contains("переоткрыт"), "{}", d.status);
}

/// `agr:agent-same-desk` `agr:agent-no-write` — правки агента ложатся в тот же
/// `Desk` незафиксированными: вид показывает их, а файл на диске не меняется.
#[test]
fn agent_edits_show_on_the_same_desk() {
    if !patch() {
        return;
    }
    let mut d = open("lz4", 10);
    let seq = d.rows.first().map(|r| r.seq).expect("строка");
    let before = std::fs::read(fixture("lz4")).unwrap();
    let mut edits = Edits::default();
    edits.set(seq, "Order", "header.order_id", "1000", "777");
    let gen = d.file_gen().load(std::sync::atomic::Ordering::SeqCst);
    d.act_tx().send(Act::Sync { edits, gen }).unwrap();
    assert!(d.poll(), "правки агента замечены");
    settle(&mut d);
    assert!(d.edited(seq, "header.order_id"), "{}", d.status);
    assert_eq!(
        std::fs::read(fixture("lz4")).unwrap(),
        before,
        "файл не тронут"
    );
}

/// `agr:agent-snap` — снимок вида говорит агенту то же, что видит человек:
/// выбранную строку, колонки и размер файла.
#[test]
fn snapshot_reflects_what_the_desk_shows() {
    let mut d = open("lz4", 10);
    let tx = d.act_tx();
    tx.send(Act::Goto("+123".into())).unwrap();
    tx.send(Act::SetColumns(vec!["u64".into()])).unwrap();
    d.poll();
    settle(&mut d);
    let snap = d.make_snap();
    assert_eq!(snap.sel_row, Some(123), "{}", d.status);
    assert_eq!(snap.sel_seq, d.rows.get(d.sel).map(|r| r.seq));
    assert_eq!(snap.columns, vec!["u64".to_string()]);
    assert_eq!(snap.total, d.index.as_ref().map(|i| i.total));
}

/// `agr:agent-sync` — просмотренные агентом правки второй раз не уходят, а
/// новая правка снова видна как непоказанная.
#[test]
fn synced_edits_are_not_sent_again() {
    let mut d = open("lz4", 10);
    let seq = d.rows.first().map(|r| r.seq).expect("строка");
    assert!(d.unsynced_edits().is_none(), "правок ещё нет");
    d.edits.set(seq, "Order", "header.order_id", "1000", "777");
    assert!(d.unsynced_edits().is_some(), "новая правка не показана");
    d.act_tx().send(Act::MarkSynced).unwrap();
    d.poll();
    assert!(d.unsynced_edits().is_none(), "показанная правка ушла снова");
    d.edits.set(seq, "Order", "header.order_id", "1000", "778");
    assert!(
        d.unsynced_edits().is_some(),
        "изменённая правка не показана"
    );
}
