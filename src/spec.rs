//! Спека открытия: несколько файлов одним аргументом (`agr:glue-spec`).
//!
//! Две записи. Склейка: `a.dat+b.dat+c.dat` — файлы открываются одним видом в
//! указанном порядке. Ротация: `prefix.current.dat-3` или `prefix.100.dat-3` —
//! указанный файл и ещё столько же более старых файлов той же ротации рядом;
//! порядок — по возрастанию, как читает их rotate+. `+` — только конкатенация
//! имён: `a.dat+1` — буквальный файл «a.dat+1», семантики «следующий файл» нет.
//!
//! Старшие ищем в каталоге по именам `prefix.{seq}.dat` и `prefix.current.dat`
//! (как `_build_map` в rotate+ — двойной stem равен префиксу), сортируем по
//! номеру, `current` — последний. Номер в имени монотонен диапазонам файла
//! (ключ имени — первый или последний seq), поэтому номеру можно верить, не
//! открывая файлы. Не хватает старых — берём сколько есть.
//!
//! Наружу: [`expand`].

use std::path::{Path, PathBuf};

/// Развернуть спеку в список файлов: по куску `+`, каждый — файл или якорь
/// ротации с «-N»; в порядке, как откроются видом.
pub fn expand(arg: &str) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for piece in arg.split('+') {
        if piece.is_empty() {
            return Err(format!("{arg}: пустое имя между +"));
        }
        out.extend(expand_piece(arg, piece)?);
    }
    Ok(out)
}

/// Один кусок спеки: буквальный файл, ротация «ствол-N» или ошибка.
fn expand_piece(arg: &str, piece: &str) -> Result<Vec<PathBuf>, String> {
    let path = Path::new(piece);
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    let not_found = || format!("{arg}: файл не найден: {piece}");
    let Some((stem, n)) = split_back(piece) else {
        return Err(not_found());
    };
    if !Path::new(stem).is_file() {
        return Err(not_found());
    }
    let anchor_name = Path::new(stem)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Some(prefix) = rotation_prefix(&anchor_name) else {
        return Err(format!(
            "{arg}: {piece} — не файл ротации (нужно prefix.current.dat-N или prefix.{{seq}}.dat-N)"
        ));
    };
    let dir = Path::new(stem).parent().unwrap_or(Path::new(""));
    let mut files = rotation_files(dir, prefix);
    let Some(pos) = files
        .iter()
        .position(|(_, p)| p.file_name().is_some_and(|n| n == anchor_name.as_str()))
    else {
        // Якорь в каталоге не нашёлся (переименован на наших глазах?) — он один.
        return Ok(vec![PathBuf::from(stem)]);
    };
    let from = pos.saturating_sub(n);
    Ok(files.drain(from..=pos).map(|(_, p)| p).collect())
}

/// Хвост «-N» с положительным числом: `(ствол, N)`.
fn split_back(piece: &str) -> Option<(&str, usize)> {
    let i = piece.rfind('-')?;
    let (stem, num) = piece.split_at(i);
    let n: usize = num[1..].parse().ok()?;
    (n >= 1 && !stem.is_empty()).then_some((stem, n))
}

/// Префикс ротации из имени `prefix.current.dat` или `prefix.{seq}.dat`.
fn rotation_prefix(name: &str) -> Option<&str> {
    if let Some(p) = name.strip_suffix(".current.dat") {
        return Some(p);
    }
    let stem = name.strip_suffix(".dat")?;
    let (prefix, num) = stem.rsplit_once('.')?;
    (!num.is_empty() && num.bytes().all(|b| b.is_ascii_digit())).then_some(prefix)
}

/// Файлы ротации `prefix` в каталоге по возрастанию номера; `current` —
/// последним. Середина имени — цифры или `current`: помеченные руками
/// (`prefix.100.orig.dat`) не считаются.
fn rotation_files(dir: &Path, prefix: &str) -> Vec<(Option<u64>, PathBuf)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    }) else {
        return out;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some((stem, mid)) = name.strip_suffix(".dat").and_then(|s| s.rsplit_once('.')) else {
            continue;
        };
        if stem != prefix {
            continue;
        }
        let key = if mid == "current" {
            None
        } else if !mid.is_empty() && mid.bytes().all(|b| b.is_ascii_digit()) {
            match mid.parse() {
                Ok(n) => Some(n),
                Err(_) => continue,
            }
        } else {
            continue;
        };
        if e.path().is_file() {
            out.push((key, e.path()));
        }
    }
    out.sort_by_key(|(k, _)| (k.is_none(), k.unwrap_or(0)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Каталог с файлами; имена — пустые файлы.
    fn dir(names: &[&str]) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "mutator-spec-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for n in names {
            std::fs::write(d.join(n), b"").unwrap();
        }
        d
    }

    fn names(files: &[PathBuf]) -> Vec<String> {
        files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    /// Спека с путём в каталоге теста: имена — уже абсолютные пути.
    fn in_dir(d: &Path, spec: &str) -> String {
        spec.split('+')
            .map(|p| {
                if p.is_empty() {
                    String::new()
                } else {
                    d.join(p).display().to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("+")
    }

    /// `agr:glue-spec` — буквальный файл бьёт толкование спеки: существует
    /// «x.dat-3» — открывается он, а не ротация.
    #[test]
    fn literal_file_wins_over_rotation_reading() {
        let d = dir(&["x.dat-3", "x.1.dat", "x.current.dat"]);
        assert_eq!(
            names(&expand(&in_dir(&d, "x.dat-3")).unwrap()),
            vec!["x.dat-3"]
        );
    }

    /// `agr:glue-spec` — `+` склеивает в указанном порядке.
    #[test]
    fn plus_concatenates_in_given_order() {
        let d = dir(&["a.dat", "b.dat", "c.dat"]);
        assert_eq!(
            names(&expand(&in_dir(&d, "a.dat+c.dat+b.dat")).unwrap()),
            vec!["a.dat", "c.dat", "b.dat"]
        );
    }

    /// `agr:glue-spec` — «-N» берёт якорь и N более старых файлов ротации,
    /// по возрастанию номера, `current` последним; меньше есть — сколько есть.
    #[test]
    fn minus_n_takes_anchor_and_older_rotation_files() {
        let d = dir(&[
            "out-md-trades.100.dat",
            "out-md-trades.200.dat",
            "out-md-trades.300.dat",
            "out-md-trades.current.dat",
            "other.5.dat",
            "out-md-trades2.7.dat",
        ]);
        let spec = |s: &str| {
            expand(&in_dir(&d, s))
                .unwrap()
                .iter()
                .map(|p| p.strip_prefix(&d).unwrap().display().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            spec("out-md-trades.current.dat-2"),
            vec![
                "out-md-trades.200.dat",
                "out-md-trades.300.dat",
                "out-md-trades.current.dat"
            ]
        );
        assert_eq!(
            spec("out-md-trades.300.dat-1"),
            vec!["out-md-trades.200.dat", "out-md-trades.300.dat"]
        );
        assert_eq!(
            spec("out-md-trades.100.dat-5"),
            vec!["out-md-trades.100.dat"],
            "старых меньше, чем N"
        );
        assert_eq!(
            spec("out-md-trades.current.dat-9"),
            vec![
                "out-md-trades.100.dat",
                "out-md-trades.200.dat",
                "out-md-trades.300.dat",
                "out-md-trades.current.dat"
            ]
        );
    }

    /// `agr:glue-spec` — не-ротация, `-0` и отсутствующий файл — ошибки с внятным
    /// текстом; `+N` семантики «следующий файл» нет.
    #[test]
    fn bad_specs_are_errors() {
        let d = dir(&["plain.dat", "r.current.dat", "a.dat"]);
        let err = |s: &str| expand(&in_dir(&d, s)).unwrap_err();
        assert!(err("plain.dat-3").contains("не файл ротации"));
        assert!(
            err("plain.dat-0").contains("не найден"),
            "-0 — не число назад"
        );
        assert!(err("нет.dat").contains("не найден"));
        assert!(err("нет.dat-2").contains("не найден"));
        assert!(err("a.dat+").contains("пустое имя"));
        assert!(
            err("a.dat+1").contains("не найден"),
            "плюс — не «следующий»"
        );
    }
}
