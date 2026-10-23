//! `mutator --dump`: сообщения текстом YAML, как печатает `tll-read`
//! (`agr:same-as-tll-read`).
//!
//! Нужен для сверки с tll-read и для скриптов: тот же диапазон `-s`, тот же
//! фильтр `-f`/`--message`, тот же вид дерева. Кавычки — по правилам PyYAML для
//! строк, похожих на числа, время и пустых.
//!
//! Правки и transform сессии файла (`agr:dump-mutations`): дамп показывает то,
//! что видит пользователь, — тела читаются через `patch+lua+file`, изменённые
//! поля помечены комментарием `# в файле: <старое>`. Фильтр при transform едет
//! как в виде: типы отбираются в Rust по `msgid`, выражение — пост-эвалом
//! парой `tll_filter` (`agr:filter-post-eval`); без transform — кодом фильтра
//! в канале, как у tll-read. `--once` печатает первое сообщение выборки
//! (`agr:dump-first`).
//!
//! Цвет (`agr:dump-color`): [`Painter`] красит вывод ANSI-кодами, как красит
//! tll-read (Pygments TerminalFormatter), — ключи ярко-голубым, строки жёлтым,
//! теги цианом, base64 красным, имя сообщения жирным цианом.
//!
//! [`stat`] — сводка по файлу: границы seq, всего сообщений и счётчики типов
//! (`agr:dump-stat`).
//!
//! Наружу: [`Dump`], [`run`], [`stat`], [`Painter`], [`yaml`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::decode::{self, Node, Value};
use crate::lua::Filter;
use crate::range::{Border, Range};
use crate::scheme::Scheme;
use crate::tll::{Context, LuaMode, Msg, Open, Reader, Transformer};

/// Сколько сообщений дифференцировать мутатором за один прогон.
const BATCH: usize = 256;

/// Что печатать.
#[derive(Debug, Clone, Default)]
pub struct Dump {
    pub path: PathBuf,
    pub range: Range,
    pub filter: Filter,
    pub with_size: bool,
    /// Только первое сообщение выборки (`--once`).
    pub once: bool,
    /// Незафиксированные правки сессии — читать через `patch+`, поля помечать.
    pub edits: crate::edit::Edits,
    /// Код Lua-transform сессии — читать через `lua+`, изменённые поля помечать.
    pub transform: Option<String>,
}

/// Напечатать сообщения в `out`.
pub fn run(d: &Dump, out: &mut dyn Write) -> Result<(), String> {
    let ctx = Context::new();
    let info = Reader::open(&ctx, &Open::file(&d.path))?.info();
    let mut skip = 0u64;
    let mut count: Option<u64> = None;
    let mut last: Option<i64> = None;
    let mut open = Open::file(&d.path);
    match d.range.begin {
        Some(Border::Seq(s)) => open.seq = Some(s),
        Some(Border::Count(n)) => skip = n,
        Some(Border::FromLast(n)) => open.seq = info.seq_last.map(|l| l - n as i64),
        None => {}
    }
    if d.range.single {
        count = Some(1);
    }
    match d.range.end {
        Some(Border::Seq(s)) => last = Some(s),
        Some(Border::Count(n)) => count = Some(n),
        _ => {}
    }
    if d.once {
        count = Some(1);
    }
    let mut store = crate::edit::Store::new();
    let patch = store.write(&d.edits)?;
    let mut fallback = false;
    if let Some(code) = &d.transform {
        // Lua-слой канала — мутатор; фильтр едет пост-эвалом.
        open.lua = Some(code.clone());
        open.lua_mode = Some(LuaMode::Transform);
    } else {
        // Как у tll-read: код фильтра прямо в канале.
        open.lua = d.filter.code();
    }
    open = open.patch(patch);
    let mut r = match Reader::open(&ctx, &open) {
        Ok(r) => r,
        Err(e) if d.transform.is_some() => {
            // Мутатор не открылся (битый код, нет tll-lua) — читать файл с
            // правками, как есть, как делает вид при откате.
            eprintln!("mutator: transform: {e}");
            fallback = true;
            open.lua = None;
            open.lua_mode = None;
            Reader::open(&ctx, &open)?
        }
        Err(e) => return Err(e),
    };
    let scheme = r.scheme();

    // Диф мутатора: те же seq читаются сырыми и прогоняются второй читкой
    // (`agr:transform-shadow`).
    let mut sh: Option<(crate::data::File, Transformer)> = match d.transform.as_ref() {
        Some(code) if !fallback => match crate::data::File::open(&d.path) {
            Ok(f) => {
                let url = f.scheme_url.clone().unwrap_or_default();
                match Transformer::new(&ctx, code, &url) {
                    Ok(tx) => Some((f, tx)),
                    Err(e) => {
                        eprintln!("mutator: transform: {e}");
                        None
                    }
                }
            }
            Err(e) => {
                eprintln!("mutator: transform: {e}");
                None
            }
        },
        _ => None,
    };

    // Пост-эвал фильтра при transform (`agr:filter-post-eval`): типы — в Rust,
    // выражение — парой `tll_filter` над прогоным телом.
    let transforming = d.transform.is_some() && !fallback;
    let wanted = if transforming {
        wanted_msgids(scheme.as_ref(), &d.filter.messages)
    } else {
        None
    };
    let mut eval = if transforming && !d.filter.expr.trim().is_empty() {
        sh.as_ref()
            .and_then(|(f, _)| f.scheme_url.clone())
            .and_then(|url| d.filter.expr_code().map(|code| (code, url)))
            .and_then(|(code, url)| match Transformer::filter(&ctx, &code, &url) {
                Ok(tf) => Some(tf),
                Err(e) => {
                    eprintln!("mutator: filter: {e}");
                    None
                }
            })
    } else {
        None
    };

    let mut batch: Vec<Msg> = Vec::new();
    for m in r.by_ref() {
        if skip > 0 {
            skip -= 1;
            continue;
        }
        if let Some(w) = &wanted {
            if !w.contains(&m.msgid) {
                continue;
            }
        }
        if let Some(tf) = &mut eval {
            if tf.transform(&m)?.is_none() {
                continue;
            }
        }
        if count == Some(0) || last.is_some_and(|l| m.seq > l) {
            break;
        }
        if let Some(c) = &mut count {
            *c -= 1;
        }
        batch.push(m);
        if batch.len() >= BATCH {
            print_batch(
                d,
                out,
                &ctx,
                scheme.as_ref(),
                sh.as_mut(),
                std::mem::take(&mut batch),
            )?;
        }
    }
    print_batch(d, out, &ctx, scheme.as_ref(), sh.as_mut(), batch)?;
    if let Some(e) = r.error() {
        return Err(e);
    }
    Ok(())
}

/// msgid выбранных типов; `None` — типы не отобраны (как в [`crate::data::scan`]).
fn wanted_msgids(scheme: Option<&Scheme>, messages: &[String]) -> Option<HashSet<i32>> {
    if messages.is_empty() {
        return None;
    }
    let mut set = HashSet::new();
    for name in messages {
        if let Some(st) = scheme.and_then(|s| s.messages().find(|st| &st.name == name)) {
            set.insert(st.msgid);
        }
    }
    Some(set)
}

/// Пачка сообщений: диф мутатора — один прогон на пачку, метки правок — по seq.
fn print_batch(
    d: &Dump,
    out: &mut dyn Write,
    ctx: &Context,
    scheme: Option<&Scheme>,
    sh: Option<&mut (crate::data::File, Transformer)>,
    batch: Vec<Msg>,
) -> Result<(), String> {
    if batch.is_empty() {
        return Ok(());
    }
    let mut mutations = crate::data::Mutations::new();
    if let Some((f, tx)) = sh {
        // Сырые сообщения пачки — по её seq: поток dump идёт через мутатор
        // построчно, исходного seq у него нет (смена seq в dump не ловится).
        let seqs: Vec<i64> = batch.iter().map(|m| m.seq).collect();
        let shadowed = crate::data::fetch::by_seqs(ctx, &Open::file(&f.path), &seqs)
            .and_then(|raw| crate::data::shadow(f, tx, &raw, &batch));
        match shadowed {
            Ok(m) => mutations = m,
            Err(e) => eprintln!("mutator: transform: {e}"),
        }
    }
    let w = |e: std::io::Error| e.to_string();
    for m in batch {
        writeln!(out, "- seq: {}", m.seq).map_err(w)?;
        if d.with_size {
            writeln!(out, "  size: {}", m.data.len()).map_err(w)?;
        }
        match scheme.and_then(|s| s.message(m.msgid).map(|st| (s, st))) {
            Some((s, st)) => {
                writeln!(out, "  name: {}", st.name).map_err(w)?;
                let n = decode::decode(s, st, &m.data);
                // Ручная правка важнее: её «в файле» — значение из файла.
                let mut marks: BTreeMap<String, String> =
                    mutations.remove(&m.seq).unwrap_or_default();
                if let Some(me) = d.edits.message(m.seq) {
                    for (p, e) in &me.fields {
                        marks.insert(p.clone(), e.old.clone());
                    }
                }
                write!(out, "{}", yaml_marks(&n, "data", 1, &marks)).map_err(w)?;
            }
            None => {
                writeln!(out, "  msgid: {}", m.msgid).map_err(w)?;
                writeln!(out, "  data: !!binary {}", hex(&m.data)).map_err(w)?;
            }
        }
        writeln!(out).map_err(w)?;
    }
    Ok(())
}

/// Статистика файла: границы seq, всего сообщений и счётчики типов
/// (`agr:dump-stat`).
pub fn stat(path: &Path, out: &mut dyn Write) -> Result<(), String> {
    let ctx = Context::new();
    let r = Reader::open(&ctx, &Open::file(path).meta())?;
    let info = r.info();
    let scheme = r.scheme();
    let s = |v: Option<i64>| v.map(|x| x.to_string()).unwrap_or_default();
    let mut total = 0usize;
    let mut hist: HashMap<i32, usize> = HashMap::new();
    for m in r {
        *hist.entry(m.msgid).or_default() += 1;
        total += 1;
    }
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let w = |e: std::io::Error| e.to_string();
    writeln!(out, "file: {}", path.display()).map_err(w)?;
    writeln!(out, "size: {size} bytes").map_err(w)?;
    writeln!(out, "First seq: {}", s(info.seq_begin)).map_err(w)?;
    writeln!(out, "Last seq: {}", s(info.seq_last)).map_err(w)?;
    writeln!(out, "messages: {total}").map_err(w)?;
    let mut rows: Vec<(usize, i32)> = hist.into_iter().map(|(k, v)| (v, k)).collect();
    rows.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    if !rows.is_empty() {
        writeln!(out, "types:").map_err(w)?;
    }
    for (n, id) in rows {
        let name = scheme
            .as_ref()
            .and_then(|sc| sc.message(id))
            .map(|st| st.name.clone())
            .unwrap_or_else(|| "-".into());
        writeln!(out, "  {n} {id} {name}").map_err(w)?;
    }
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn is_list(n: &Node) -> bool {
    n.value.is_none() && (n.ty.starts_with('*') || n.ty.ends_with(']'))
}

/// Узел как пара `key: value` YAML с отступом `level` (по два пробела).
pub fn yaml(n: &Node, key: &str, level: usize) -> String {
    yaml_marks(n, key, level, &BTreeMap::new())
}

/// То же, но у полей из `marks` (путь → значение в файле) — комментарий
/// `# в файле: <старое>` (`agr:dump-mutations`).
fn yaml_marks(n: &Node, key: &str, level: usize, marks: &BTreeMap<String, String>) -> String {
    let pad = "  ".repeat(level);
    let mut s = String::new();
    if let Some(e) = &n.error {
        s += &format!("{pad}{key}: {}\n", quote(&format!("<{e}>")));
    } else if let Some(Value::Bits(b)) = &n.value {
        s += &format!("{pad}{key}:\n");
        // python-tll читает поле bits одним младшим битом, какой бы ширины
        // оно ни было, — tll-read печатает bool. Вид показывает значение целиком.
        for (k, v, _) in b {
            s += &format!("{pad}  {k}: {}\n", v & 1 == 1);
        }
    } else if let Some(Value::Bytes(b)) = &n.value {
        s += &format!("{pad}{key}: {}\n", bytes(b, &pad));
    } else if let Some(v) = &n.value {
        s += &format!("{pad}{key}: {}\n", scalar(v));
    } else if is_list(n) {
        if n.children.is_empty() {
            s += &format!("{pad}{key}: []\n");
        } else {
            // PyYAML пишет элементы списка на уровне ключа.
            s += &format!("{pad}{key}:\n");
            for c in &n.children {
                s += &item(c, level, marks);
            }
        }
    } else if n.children.is_empty() {
        s += &format!("{pad}{key}: {{}}\n");
    } else {
        s += &format!("{pad}{key}:\n");
        for c in &n.children {
            s += &yaml_marks(c, &c.name, level + 1, marks);
        }
    }
    if let Some(old) = marks.get(&n.path) {
        if let Some(pos) = s.rfind('\n') {
            s.insert_str(pos, &format!("  # в файле: {old}"));
        }
    }
    s
}

/// Элемент списка: `- ` и поля структуры на том же отступе.
fn item(n: &Node, level: usize, marks: &BTreeMap<String, String>) -> String {
    let pad = "  ".repeat(level);
    if n.value.is_some() || n.error.is_some() || is_list(n) || n.children.is_empty() {
        let inner = yaml_marks(n, "x", 0, marks);
        return format!(
            "{pad}- {}",
            inner.trim_start_matches("x: ").trim_start_matches("x:\n")
        );
    }
    let mut s = String::new();
    for (i, c) in n.children.iter().enumerate() {
        let body = yaml_marks(c, &c.name, level + 1, marks);
        if i == 0 {
            s += &format!("{pad}- {}", body.trim_start());
        } else {
            s += &body;
        }
    }
    s
}

/// Сырые байты как у tll-read: ASCII — строкой (с непечатными — в двойных
/// кавычках с экранированием PyYAML), иначе `!!binary` блоком base64.
fn bytes(b: &[u8], pad: &str) -> String {
    if b.iter().all(|&c| c < 0x80) {
        let s: String = b.iter().map(|&c| c as char).collect();
        let printable = b
            .iter()
            .all(|&c| c == b'\t' || c == b'\n' || (0x20..0x7f).contains(&c));
        if printable {
            return quote(&s);
        }
        let mut out = String::from("\"");
        for c in s.chars() {
            match c {
                '\0' => out += "\\0",
                '\x07' => out += "\\a",
                '\x08' => out += "\\b",
                '\t' => out += "\\t",
                '\n' => out += "\\n",
                '\x0b' => out += "\\v",
                '\x0c' => out += "\\f",
                '\r' => out += "\\r",
                '\x1b' => out += "\\e",
                '"' => out += "\\\"",
                '\\' => out += "\\\\",
                c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                    out += &format!("\\x{:02X}", c as u32)
                }
                c => out.push(c),
            }
        }
        out.push('"');
        return out;
    }
    let b64 = base64(b);
    let mut out = String::from("!!binary |");
    for chunk in b64.as_bytes().chunks(76) {
        out += &format!("\n{pad}  {}", std::str::from_utf8(chunk).unwrap());
    }
    out
}

fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn scalar(v: &Value) -> String {
    match v {
        Value::Int(i) => i.to_string(),
        Value::Enum { .. } | Value::Double(_) => v.text(),
        _ => quote(&v.text()),
    }
}

/// Кавычки там, где PyYAML их ставит: пустое, похожее на число/время/bool/null,
/// со служебными знаками.
fn quote(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let special = s.is_empty()
        || yaml_number(s)
        || s.starts_with(|c: char| c.is_ascii_digit() && s.contains('-') && s.contains(':'))
        || matches!(
            lower.as_str(),
            "true" | "false" | "yes" | "no" | "on" | "off" | "null" | "~"
        )
        || s.starts_with(|c: char| "!&*-?:,[]{}#|>@`\"'%".contains(c))
        || s.contains(": ")
        || s.contains(" #")
        || s.ends_with(' ')
        || s.starts_with(' ');
    if special {
        format!("'{}'", s.replace('\'', "''"))
    } else {
        s.to_string()
    }
}

/// Число по регуляркам PyYAML: целое из цифр или float обязательно с точкой
/// (`0E-6176` для PyYAML — строка).
fn yaml_number(s: &str) -> bool {
    let t = s.strip_prefix(['-', '+']).unwrap_or(s);
    if t.is_empty() {
        return false;
    }
    if t.chars().all(|c| c.is_ascii_digit() || c == '_') {
        return true;
    }
    if matches!(t.to_ascii_lowercase().as_str(), ".inf" | ".nan") {
        return true;
    }
    let (mant, exp) = match t.find(['e', 'E']) {
        Some(i) => (&t[..i], Some(&t[i + 1..])),
        None => (t, None),
    };
    let Some((a, b)) = mant.split_once('.') else {
        return false;
    };
    let digits = |x: &str| x.chars().all(|c| c.is_ascii_digit() || c == '_');
    let exp_ok = exp.is_none_or(|e| {
        e.len() > 1 && e.starts_with(['-', '+']) && e[1..].chars().all(|c| c.is_ascii_digit())
    });
    (a.is_empty() || (a.starts_with(|c: char| c.is_ascii_digit()) && digits(a)))
        && digits(b)
        && exp_ok
}

/// Раскраска YAML-вывода ANSI-кодами построчно: [`Write`]-обёртка над любым
/// выходом (`agr:dump-color`).
pub struct Painter<W: Write> {
    inner: W,
    line: Vec<u8>,
}

impl<W: Write> Painter<W> {
    pub fn new(inner: W) -> Self {
        Painter {
            inner,
            line: Vec::new(),
        }
    }
}

impl<W: Write> Write for Painter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.line.extend_from_slice(buf);
        while let Some(pos) = self.line.iter().position(|&b| b == b'\n') {
            let rest: Vec<u8> = self.line.drain(..=pos).collect();
            let s = std::str::from_utf8(&rest[..rest.len() - 1]).unwrap_or_default();
            self.inner.write_all(paint(s).as_bytes())?;
            self.inner.write_all(b"\n")?;
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Одна строка в цвете, как красит tll-read (Pygments TerminalFormatter).
fn paint(s: &str) -> String {
    const KEY: &str = "\x1b[94m";
    const STR: &str = "\x1b[33m";
    const TAG: &str = "\x1b[36m";
    const BIN: &str = "\x1b[31m";
    const NAME: &str = "\x1b[1;36m";
    const PLAIN: &str = "\x1b[37m";
    const RESET: &str = "\x1b[39;49;00m";

    let mut out = String::new();
    let indented = s.len() - s.trim_start().len();
    out += &s[..indented];
    let mut rest = &s[indented..];
    if let Some(r) = rest.strip_prefix("- ") {
        out += "- ";
        rest = r;
    }
    // base64-тело блока `!!binary`: отступ без `key:` — красным.
    if !out.contains("- ") && indented > 0 && !rest.contains(':') && !rest.is_empty() {
        return format!("{out}{BIN}{rest}{RESET}");
    }
    let Some(ci) = rest.find(':') else {
        return format!("{out}{PLAIN}{rest}{RESET}");
    };
    let after = &rest[ci + 1..];
    if !(after.is_empty() || after.starts_with(' ')) {
        return format!("{out}{PLAIN}{rest}{RESET}");
    }
    let key = &rest[..ci];
    out += &format!("{KEY}{key}{RESET}:");
    let (space, v) = match after.strip_prefix(' ') {
        Some(v) => (" ", v),
        None => ("", after),
    };
    if v.is_empty() {
        return out;
    }
    out += space;
    if let Some(r) = v.strip_prefix("!!") {
        let end = r.find(' ').map(|i| i + 2).unwrap_or(v.len());
        out += &format!("{TAG}{}{RESET}", &v[..end]);
        if end < v.len() {
            out += &format!("{PLAIN}{}{RESET}", &v[end..]);
        }
    } else if v.starts_with('\'') || v.starts_with('"') {
        let q = v.as_bytes()[0];
        let b = v.as_bytes();
        let mut j = 1;
        while j < b.len() {
            if q == b'\'' && b[j] == b'\'' && b.get(j + 1) == Some(&b'\'') {
                j += 2;
                continue;
            }
            if q == b'"' && b[j] == b'\\' {
                j += 2;
                continue;
            }
            if b[j] == q {
                break;
            }
            j += 1;
        }
        let end = (j + 1).min(v.len());
        out += &format!("{STR}{}{RESET}", &v[..end]);
    } else if key == "name" {
        out += &format!("{NAME}{v}{RESET}");
    } else {
        out += &format!("{PLAIN}{v}{RESET}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `agr:same-as-tll-read` — строки, которые YAML прочёл бы как число, время
    /// или пусто, — в кавычках, как у PyYAML.
    #[test]
    fn yaml_quotes_like_pyyaml() {
        assert_eq!(quote("3000000.00"), "'3000000.00'");
        assert_eq!(quote(""), "''");
        assert_eq!(
            quote("2026-10-02T13:41:27.562Z"),
            "'2026-10-02T13:41:27.562Z'"
        );
        assert_eq!(quote("Ef3VM0UJzzDUDbb1"), "Ef3VM0UJzzDUDbb1");
        assert_eq!(quote("0E-6176"), "0E-6176");
        assert_eq!(quote("-1.5E+3"), "'-1.5E+3'");
        assert_eq!(quote("42"), "'42'");
        assert_eq!(bytes(b"ab\0\0", ""), "\"ab\\0\\0\"");
        assert_eq!(bytes(&[0x42, 0, 1, 0xff], "  "), "!!binary |\n    QgAB/w==");
        assert_eq!(bytes(b"ok", ""), "ok");
    }

    /// `agr:dump-mutations` — у помеченного поля комментарий со старым значением.
    #[test]
    fn marked_field_gets_a_file_value_comment() {
        let n = Node {
            name: "root".into(),
            path: String::new(),
            ty: String::new(),
            offset: 0,
            size: 0,
            value: None,
            children: vec![Node {
                name: "price".into(),
                path: "price".into(),
                ty: String::new(),
                offset: 0,
                size: 0,
                value: Some(Value::Int(777)),
                children: Vec::new(),
                error: None,
            }],
            error: None,
        };
        let mut marks = BTreeMap::new();
        marks.insert("price".to_string(), "100".to_string());
        assert_eq!(
            yaml_marks(&n, "data", 1, &marks),
            "  data:\n    price: 777  # в файле: 100\n"
        );
        // Без меток — как раньше.
        assert_eq!(yaml(&n, "data", 1), "  data:\n    price: 777\n");
    }

    /// `agr:dump-color` — палитра как у tll-read: ключи, строки, теги, имя.
    #[test]
    fn paint_matches_tll_read_palette() {
        const R: &str = "\x1b[39;49;00m";
        const PLAIN: &str = "\x1b[37m";
        assert_eq!(
            paint("- seq: 42"),
            format!("- \x1b[94mseq{R}: {PLAIN}42{R}")
        );
        assert_eq!(
            paint("  name: Order"),
            format!("  \x1b[94mname{R}: \x1b[1;36mOrder{R}")
        );
        assert_eq!(
            paint("  price: '100.5'"),
            format!("  \x1b[94mprice{R}: \x1b[33m'100.5'{R}")
        );
        assert_eq!(
            paint("  raw: !!binary |"),
            format!("  \x1b[94mraw{R}: \x1b[36m!!binary{R}{PLAIN} |{R}")
        );
        assert_eq!(
            paint("      QgAB/w=="),
            format!("      \x1b[31mQgAB/w=={R}")
        );
        assert_eq!(paint("  data:"), format!("  \x1b[94mdata{R}:"));
        assert_eq!(
            paint("  data: {}"),
            format!("  \x1b[94mdata{R}: {PLAIN}{{}}{R}")
        );
    }
}
