//! Сессия на диске: запись и чтение по имени файла в `$XDG_CONFIG_HOME`.
//! Отдельный бинарник — переменная окружения не задевает другие тесты.

use std::path::Path;

use tui_patch::session::{self, Session};

/// `agr:session-by-name` — записанная сессия читается для того же имени файла
/// из любого каталога; чужое имя — своей сессии нет.
#[test]
fn session_saves_and_loads_by_name() {
    let dir = std::env::temp_dir().join(format!("mutator-session-{}", std::process::id()));
    std::env::set_var("XDG_CONFIG_HOME", &dir);
    let s = Session {
        file: "/stand/out-mu0.0.dat".into(),
        expr: "data.price > 1".into(),
        messages: vec!["NewOrder".into()],
        seq: Some(42),
        window: Some([10.0, 20.0, 800.0, 600.0]),
        look: Some("tui".into()),
        lua_transform: Some(
            "function tll_on_data(seq, name, data)\n  tll_callback(seq, name, data)\nend".into(),
        ),
        ..Default::default()
    };
    session::save(Path::new("/stand/out-mu0.0.dat"), &s).unwrap();
    assert_eq!(
        session::load(Path::new("/local/copy/out-mu0.0.dat")),
        Some(s),
        "transform-код, как и прочее, переживает перезапуск"
    );
    assert_eq!(session::load(Path::new("/local/other.dat")), None);
    assert!(dir.join("mutator/sessions/out-mu0.0.dat.toml").exists());
    let _ = std::fs::remove_dir_all(dir);
}
