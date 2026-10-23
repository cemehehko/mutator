//! Фиксация правок: весь исходник проходит через transform (если задан) и
//! tll-patch — `patch+[lua+]file://`, ручная правка поверх мутации скрипта — и
//! пишется в новый файл. Исходник не теряется никогда
//! (`agr:commit-keeps-original`): либо результат ложится под новым именем, либо
//! исходник переименовывается в бэкап, а результат занимает его место. Имена —
//! только свободные. Пишется во временный файл в том же каталоге; до
//! переименований ничего не тронуто, ошибка на любом шаге убирает только
//! временный файл.
//!
//! Наружу: [`Target`], [`free_name`], [`growing`], [`check`], [`run`].

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::tll::{Context, LuaMode, Open, Reader, Writer};

/// Куда ляжет результат.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Новый файл рядом; исходник остаётся как был.
    NewFile(PathBuf),
    /// Результат под именем исходника, исходник — в бэкап.
    Overwrite { backup: PathBuf },
}

/// Свободное имя рядом с `path`: `x.dat` → `x.<tag>.dat`, `x.<tag>-2.dat`, …
pub fn free_name(path: &Path, tag: &str) -> PathBuf {
    let dir = path.parent().unwrap_or(Path::new(""));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (stem, ext) = match name.strip_suffix(".dat") {
        Some(s) => (s.to_string(), ".dat"),
        None => (name.clone(), ""),
    };
    (1..)
        .map(|i| {
            let n = if i == 1 {
                format!("{stem}.{tag}{ext}")
            } else {
                format!("{stem}.{tag}-{i}{ext}")
            };
            dir.join(n)
        })
        .find(|p| !p.exists())
        .unwrap()
}

/// Растущий файл писателя: перезаписывать его нельзя — писатель потеряет
/// хвост.
pub fn growing(path: &Path) -> bool {
    path.to_string_lossy().ends_with(".current.dat")
}

/// Проверить цель до записи: имя свободно, бэкап не для растущего файла.
pub fn check(src: &Path, target: &Target) -> Result<(), String> {
    let taken = |p: &Path| {
        if p.as_os_str().is_empty() {
            Err("имя не задано".to_string())
        } else if p.exists() {
            Err(format!("{} уже есть", p.display()))
        } else {
            Ok(())
        }
    };
    match target {
        Target::NewFile(p) => taken(p),
        Target::Overwrite { backup } => {
            if growing(src) {
                return Err("растущий файл перезаписывать нельзя — только новым файлом".into());
            }
            taken(backup)
        }
    }
}

/// Записать исходник `src` через правки `yaml` в цель. `progress` — доля
/// пройденного по seq. Ответ — путь результата.
pub fn run(
    src: &Path,
    yaml: Option<&Path>,
    target: &Target,
    stop: &AtomicBool,
    lua_code: Option<&str>,
    lua_mode: Option<LuaMode>,
    mut progress: impl FnMut(f32),
) -> Result<PathBuf, String> {
    check(src, target)?;
    let out = match target {
        Target::NewFile(p) => p.clone(),
        Target::Overwrite { .. } => src.to_path_buf(),
    };
    let dir = out.parent().unwrap_or(Path::new(""));
    let tmp = dir.join(format!(
        ".{}.tui-patch.tmp",
        out.file_name().unwrap_or_default().to_string_lossy()
    ));
    let r =
        write(src, yaml, &tmp, stop, lua_code, lua_mode, &mut progress).and_then(
            |()| match target {
                Target::NewFile(p) => {
                    check(src, target)?;
                    std::fs::rename(&tmp, p).map_err(|e| format!("{}: {e}", p.display()))
                }
                Target::Overwrite { backup } => {
                    check(src, target)?;
                    std::fs::rename(src, backup)
                        .map_err(|e| format!("бэкап {}: {e}", backup.display()))?;
                    std::fs::rename(&tmp, src).map_err(|e| {
                        format!("{}: {e}; исходник — в {}", src.display(), backup.display())
                    })
                }
            },
        );
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r.map(|()| out)
}

fn write(
    src: &Path,
    yaml: Option<&Path>,
    tmp: &Path,
    stop: &AtomicBool,
    lua_code: Option<&str>,
    lua_mode: Option<LuaMode>,
    progress: &mut impl FnMut(f32),
) -> Result<(), String> {
    let ctx = Context::new();
    // Без правок — сколько сообщений в исходнике: правка поля их число не меняет.
    let plain = Reader::open(&ctx, &Open::file(src))?;
    let info = plain.info();
    let scheme = plain.scheme_url().ok_or("у файла нет схемы")?;
    drop(plain);
    let mut open = Open::file(src);
    if let Some(p) = yaml {
        open = open.patch(Some(p.to_path_buf()));
    }
    open.lua = lua_code.map(|s| s.to_string());
    open.lua_mode = lua_mode;
    let mut r = Reader::open(&ctx, &open)?;
    let mut w = Writer::create(&ctx, tmp, &scheme, &info)?;
    let (b, l) = (info.seq_begin.unwrap_or(0), info.seq_last.unwrap_or(0));
    let span = (l - b).max(1) as f32;
    let mut n = 0usize;
    let mut last = -1.0;
    for m in r.by_ref() {
        if stop.load(Ordering::Relaxed) {
            return Err("отменено".into());
        }
        w.post(&m)?;
        n += 1;
        let p = (m.seq - b) as f32 / span;
        if p - last >= 0.01 {
            last = p;
            progress(p);
        }
    }
    if let Some(e) = r.error() {
        return Err(e);
    }
    drop(w);
    let mut check = Reader::open(&ctx, &Open::file(src).meta())?;
    let want = check.by_ref().count();
    if n != want {
        return Err(format!("записано {n} сообщений из {want}"));
    }
    progress(1.0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "tui-patch-names-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn free_name_skips_taken_names() {
        let d = dir();
        let src = d.join("out-mu0.0.dat");
        assert_eq!(free_name(&src, "orig"), d.join("out-mu0.0.orig.dat"));
        std::fs::write(d.join("out-mu0.0.orig.dat"), "").unwrap();
        assert_eq!(free_name(&src, "orig"), d.join("out-mu0.0.orig-2.dat"));
        assert_eq!(free_name(&d.join("x"), "patched"), d.join("x.patched"));
    }

    /// `agr:commit-keeps-original`
    #[test]
    fn taken_target_and_growing_overwrite_are_refused() {
        let d = dir();
        let src = d.join("a.dat");
        let busy = d.join("b.dat");
        std::fs::write(&busy, "").unwrap();
        assert!(check(&src, &Target::NewFile(busy.clone())).is_err());
        assert!(check(&src, &Target::Overwrite { backup: busy }).is_err());
        assert!(check(&src, &Target::NewFile(PathBuf::new())).is_err());
        let cur = d.join("x.current.dat");
        assert!(check(
            &cur,
            &Target::Overwrite {
                backup: d.join("y.dat")
            }
        )
        .is_err());
        assert!(check(&cur, &Target::NewFile(d.join("y.dat"))).is_ok());
    }
}
