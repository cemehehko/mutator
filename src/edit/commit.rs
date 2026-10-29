//! Фиксация правок: весь исходник проходит через transform (если задан) и
//! правки со составом потока ([`Patch`] с [`Span`], `agr:stream-ops`) — ручная
//! правка поверх мутации скрипта — и пишется в новый файл. Исходник не теряется никогда
//! (`agr:commit-keeps-original`): либо результат ложится под новым именем, либо
//! исходник переименовывается в бэкап, а результат занимает его место. Имена —
//! только свободные. Пишется во временный файл в том же каталоге; до
//! переименований ничего не тронуто, ошибка на любом шаге убирает только
//! временный файл.
//!
//! Число сообщений после правок не обязано совпадать с исходником: удаления,
//! вставки и transform его меняют. Запись следит за seq (`agr:commit-seq-check`):
//! seq не больше предыдущего libtll не пишет — запись встаёт с местом ошибки,
//! а итог ([`Committed`]) несёт настоящий последний seq.
//!
//! Склейка (`agr:glue-commit`): [`Plan::Part`] — каждая часть отдельно, одними
//! и теми же правками (правки с seq не этой части в её потоке не срабатывают,
//! вставки — только из отрезка части), со сдвигом seq от правок ранних частей; имена результатов — по факту записи
//! (`prefix.{M}.dat` пересчитанным ключом, тег рядом у новых файлов); опустевшая
//! часть файла-результата не оставляет — пустой нумерованный файл ломает rotate+.
//!
//! Наружу: [`Target`], [`Plan`], [`Committed`], [`free_name`], [`growing`],
//! [`check`], [`run_all`], [`run_plan`].

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::{Patch, Span};
use crate::io::{Context, Info, LuaMode, Open, Reader, Writer};

/// Куда ляжет результат.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Новый файл рядом; исходник остаётся как был.
    NewFile(PathBuf),
    /// Результат под именем исходника, исходник — в бэкап.
    Overwrite { backup: PathBuf },
}

/// Что записать: один файл или часть склейки (`agr:glue-commit`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Одиночный файл: цель задана именем — как сегодня.
    Single { src: PathBuf, target: Target },
    /// Часть склейки: имя результата — по факту записи. Ротационная часть
    /// (`prefix.{seq}.dat`) получает пересчитанное имя `prefix.{M}.dat` тем же
    /// ключом, что у исходника; новым файлом — тег рядом (`prefix.{M}.patched.dat`,
    /// невидимо для rotate+), перезаписью — под пересчитанным именем, исходник —
    /// в бэкап. `shift` — сдвиг seq от правок ранних частей: канал части о них
    /// не знает. Опустевшая часть — файла-результата нет (пустой нумерованный
    /// файл ломает rotate+), у перезаписи исходник — в бэкап.
    Part {
        src: PathBuf,
        /// Границы исходника — для ключа имени.
        info: Info,
        overwrite: bool,
        tag: String,
        shift: i64,
        /// Чьи вставки пишет часть: перед seq от её начала до начала
        /// следующей части (у первой и последней — без границы).
        span: Span,
    },
}

/// Итог записи.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed {
    pub path: PathBuf,
    /// Последний записанный seq; `None` — не записано ни одного сообщения.
    pub last: Option<i64>,
    pub written: usize,
    /// После правок сообщений не осталось: файла-результата нет; путь — где
    /// данные (бэкап перезаписи или нетронутый исходник).
    pub empty: bool,
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

/// Записать планы по очереди (`agr:glue-commit`): каждая часть отдельно, одни
/// и те же правки — правки с seq не этой части в её потоке не срабатывают. Прогресс — доля по всем частям. Ошибка останавливает: записанное
/// до неё — законченные файлы (или бэкапы с результатом), исходники целы.
pub fn run_all(
    plans: &[Plan],
    patch: Option<&Patch>,
    stop: &AtomicBool,
    lua: Option<(&str, Option<LuaMode>)>,
    mut progress: impl FnMut(f32),
) -> Result<Vec<Committed>, String> {
    let n = plans.len().max(1);
    let mut out = Vec::with_capacity(plans.len());
    for (i, p) in plans.iter().enumerate() {
        out.push(run_plan(p, patch, stop, lua, |f| {
            progress((i as f32 + f) / n as f32)
        })?);
    }
    Ok(out)
}

/// Записать один план: `progress` — доля этой части.
pub fn run_plan(
    plan: &Plan,
    patch: Option<&Patch>,
    stop: &AtomicBool,
    lua: Option<(&str, Option<LuaMode>)>,
    progress: impl FnMut(f32),
) -> Result<Committed, String> {
    match plan {
        Plan::Single { src, target } => run_single(src, patch, target, stop, lua, progress),
        Plan::Part { .. } => run_part(plan, patch, stop, lua, progress),
    }
}

/// Записать исходник `src` через правки `patch` в цель. `progress` — доля
/// пройденного по seq. Ответ — путь результата.
fn run_single(
    src: &Path,
    patch: Option<&Patch>,
    target: &Target,
    stop: &AtomicBool,
    lua: Option<(&str, Option<LuaMode>)>,
    mut progress: impl FnMut(f32),
) -> Result<Committed, String> {
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
        write(src, patch, &tmp, stop, lua, 0, &mut progress).and_then(|(first, last, written)| {
            check(src, target)?;
            match target {
                Target::NewFile(p) => finalize(None, &tmp, p).map(|()| (first, last, written)),
                Target::Overwrite { backup } => {
                    finalize(Some((src, backup)), &tmp, src).map(|()| (first, last, written))
                }
            }
        });
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r.map(|(_, last, written)| Committed {
        path: out,
        last,
        written,
        empty: false,
    })
}

/// Записать часть склейки: имя результата — по факту записи (`agr:glue-commit`).
fn run_part(
    plan: &Plan,
    patch: Option<&Patch>,
    stop: &AtomicBool,
    lua: Option<(&str, Option<LuaMode>)>,
    mut progress: impl FnMut(f32),
) -> Result<Committed, String> {
    let Plan::Part {
        src,
        info,
        overwrite,
        tag,
        shift,
        span,
    } = plan
    else {
        unreachable!("run_part — только Plan::Part")
    };
    if *overwrite && growing(src) {
        return Err(format!(
            "{}: растущий файл перезаписывать нельзя — только новым файлом",
            src.display()
        ));
    }
    let dir = src.parent().unwrap_or(Path::new(""));
    let tmp = dir.join(format!(
        ".{}.tui-patch.tmp",
        src.file_name().unwrap_or_default().to_string_lossy()
    ));
    let patch = patch.map(|p| Patch {
        edits: p.edits.clone(),
        stream: p.stream.map(|_| *span),
    });
    let r = write(src, patch.as_ref(), &tmp, stop, lua, *shift, &mut progress);
    let (first, last, written) = match r {
        Ok(x) => x,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    // Опустевшая часть: файла-результата нет — пустой нумерованный файл ломает
    // rotate+. У перезаписи исходник уходит в бэкап: данные не теряются.
    if written == 0 {
        let _ = std::fs::remove_file(&tmp);
        if !*overwrite {
            return Ok(Committed {
                path: src.clone(),
                last: None,
                written: 0,
                empty: true,
            });
        }
        let backup = free_name(src, "orig");
        return std::fs::rename(src, &backup)
            .map_err(|e| format!("бэкап {}: {e}", backup.display()))
            .map(|()| Committed {
                path: backup,
                last: None,
                written: 0,
                empty: true,
            });
    }
    let (first, last) = (first.unwrap(), last.unwrap());
    let renamed = rotation_renamed(src, info, first, last);
    let r = finalize_part(src, &tmp, *overwrite, tag, renamed);
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r.map(|out| Committed {
        path: out,
        last: Some(last),
        written,
        empty: false,
    })
}

/// Финальные переименования: с `backup` — исходник сначала уходит в него
/// (перезапись), без — временный файл просто становится результатом. Ошибка
/// называет пути — исходник не теряется.
fn finalize(backup: Option<(&Path, &Path)>, tmp: &Path, out: &Path) -> Result<(), String> {
    if let Some((src, b)) = backup {
        std::fs::rename(src, b).map_err(|e| format!("бэкап {}: {e}", b.display()))?;
    }
    let note = backup
        .map(|(_, b)| format!("; исходник — в {}", b.display()))
        .unwrap_or_default();
    std::fs::rename(tmp, out).map_err(|e| format!("{}: {e}{note}", out.display()))
}

/// Переименовать готовый `tmp` части в имя результата: перезапись — под
/// пересчитанным ротационным именем (или именем исходника) с бэкапом исходника,
/// новый файл — тегом рядом с пересчитанным именем.
fn finalize_part(
    src: &Path,
    tmp: &Path,
    overwrite: bool,
    tag: &str,
    renamed: Option<PathBuf>,
) -> Result<PathBuf, String> {
    if !overwrite {
        let base = renamed.unwrap_or_else(|| src.to_path_buf());
        let out = free_name(&base, tag);
        return finalize(None, tmp, &out).map(|()| out);
    }
    let out = renamed.unwrap_or_else(|| src.to_path_buf());
    let backup = free_name(src, "orig");
    if out != src && out.exists() {
        return Err(format!(
            "{} уже есть — имя результата занято",
            out.display()
        ));
    }
    finalize(Some((src, &backup)), tmp, &out).map(|()| out)
}

/// Пересчитанное имя ротационной части: `prefix.{M}.dat`, где M — первый или
/// последний seq результата; ключ — тот, каким назван исходник (номер имени
/// против его фактических границ; не совпал — first, умолчание rotate+).
/// Не ротационное имя (включая `current.dat`) — `None`.
fn rotation_renamed(src: &Path, info: &Info, first: i64, last: i64) -> Option<PathBuf> {
    let name = src.file_name()?.to_string_lossy();
    let stem = name.strip_suffix(".dat")?;
    let (prefix, num) = stem.rsplit_once('.')?;
    let n: i64 = num.parse().ok()?;
    let m = if info.seq_last == Some(n) && info.seq_begin != Some(n) {
        last
    } else {
        first
    };
    let dir = src.parent().unwrap_or(Path::new(""));
    Some(dir.join(format!("{prefix}.{m}.dat")))
}

fn write(
    src: &Path,
    patch: Option<&Patch>,
    tmp: &Path,
    stop: &AtomicBool,
    lua: Option<(&str, Option<LuaMode>)>,
    shift: i64,
    progress: &mut impl FnMut(f32),
) -> Result<(Option<i64>, Option<i64>, usize), String> {
    let ctx = Context::new();
    let plain = Reader::open(&ctx, &Open::file(src))?;
    let info = plain.info();
    let scheme = plain.scheme_url().ok_or("у файла нет схемы")?;
    drop(plain);
    let mut open = Open::file(src).backend(info.backend).patch(patch.cloned());
    if let Some((code, mode)) = lua {
        open.lua = Some(code.to_string());
        open.lua_mode = mode;
    }
    let mut r = Reader::open(&ctx, &open)?;
    let mut w = Writer::create(&ctx, tmp, &scheme, &info)?;
    let (b, l) = (info.seq_begin.unwrap_or(0), info.seq_last.unwrap_or(0));
    let span = (l - b).max(1) as f32;
    let mut n = 0usize;
    let mut last = -1.0;
    let mut first: Option<i64> = None;
    let mut prev: Option<i64> = None;
    for m in r.by_ref() {
        if stop.load(Ordering::Relaxed) {
            return Err("отменено".into());
        }
        // Сдвиг от правок ранних частей склейки: канал этой части о них не
        // знает (`agr:glue-commit`), seq окончательные — здесь.
        let mut m = m;
        m.seq += shift;
        // libtll пишет seq только по возрастанию: дубль или откат (чаще всего
        // от transform) — ошибка с местом, исходник цел (`agr:commit-seq-check`).
        if let Some(p) = prev.filter(|&p| m.seq <= p) {
            return Err(format!(
                "seq {} после {p}: дубль или откат seq — файл не записан (transform?)",
                m.seq
            ));
        }
        prev = Some(m.seq);
        first.get_or_insert(m.seq);
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
    progress(1.0);
    Ok((first, prev, n))
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
