//!
//! Инструменты агента: реестр JSON-схем ([`build_tools`]) и исполнение
//! ([`execute`]). Читающие инструменты работают здесь же, в потоке агента: обзор
//! файла и схемы, типы сообщений, чтение и поиск по seq, сборка Lua-фильтра.
//! Тяжёлое отдаётся не целиком, а сводкой плюс выборкой окнами — как в black-d.
//!
//! Пачковый инструмент ([`needs_confirm`]) не трогает файл: [`prepare`]
//! собирает все правки в кандидата — поля и состав потока (удаление, смена
//! типа, вставка; `agr:stream-ops`), — проверяет записью по схеме и отдаёт
//! человеку одну сводку с прогнозом последнего seq (`agr:seq-forecast`); [`apply_batch`] применяет пачку целиком одним
//! [`Act::Sync`]. Фиксацию агент не делает вовсе (`agr:agent-no-write`): про
//! `w` ему напоминает системный промпт, инструмента для этого нет.
//!
//! Промпт и описания инструментов — по-английски: так модель устойчивее
//! понимает схемы; ответы пользователю агент даёт по-русски.
//!
//! Наружу: [`build_tools`], [`execute`], [`needs_confirm`], [`prepare`],
//! [`apply_batch`], [`restore`], [`Batch`].
//!
//! Ещё: тестовый контекст `tests_ctx` на эталонном датнике.

use serde_json::{json, Value};

use super::{Act, ToolCtx};
use crate::data::fetch;
use crate::decode;
use crate::dump;
use crate::io::{Msg, Reader, Transformer};
use crate::lua::Filter;
use crate::scheme::{Kind, Scheme};

/// Сколько сообщений отдаём по умолчанию и не больше скольких — по просьбе.
const READ_DEFAULT: usize = 10;
const READ_MAX: usize = 50;
/// Сколько сообщений читает за раз скан `search_messages`; между кусками
/// смотрится флаг прерывания.
const SCAN_CHUNK: usize = 1024;

/// Реестр инструментов для запроса к модели.
pub fn build_tools() -> Vec<Value> {
    let tools = vec![
        func(
            "file_overview",
            "What file is open: path, size in bytes, first and last seq, number of \
             messages and message types (when the index is ready), schema name.",
            json!({ "type": "object", "properties": {} }),
        ),
        func(
            "message_types",
            "Message types in the file with how many messages each has, most \
             frequent first. Use it before filtering by type.",
            json!({
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "description": "How many types to return (default 30)" }
                }
            }),
        ),
        func(
            "schema_fields",
            "Fields of a message type (name, type, offset, size, enum or bits values). \
             With no `type`, lists the message types and their field counts.",
            json!({
                "type": "object",
                "properties": {
                    "type": { "type": "string", "description": "Message type name or msgid" }
                }
            }),
        ),
        func(
            "column_presets",
            "Column presets of the viewer and the columns they show; field paths are \
             dotted paths (header.status).",
            json!({ "type": "object", "properties": {} }),
        ),
        func(
            "read_messages",
            "Read messages as YAML, starting from seq `from` (default: first in the \
             file). `fields` limits the output to those field paths (default: all \
             scalar fields of the message).",
            json!({
                "type": "object",
                "properties": {
                    "from": { "type": "integer", "description": "First seq to read (inclusive)" },
                    "count": { "type": "integer", "description": "How many messages, up to 50 (default 10)" },
                    "fields": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Field paths to show, e.g. [\"header.status\", \"price\"]"
                    }
                }
            }),
        ),
        func(
            "message_detail",
            "Full field tree of one message with values, types and offsets.",
            json!({
                "type": "object",
                "properties": {
                    "seq": { "type": "integer", "description": "Message seq" }
                },
                "required": ["seq"]
            }),
        ),
        func(
            "search_messages",
            "Find messages matching a Lua expression, forward from `from`. The \
             expression sees seq, name, data, msgid, addr, time. Returns seq and type \
             of the first matches, not the bodies.",
            json!({
                "type": "object",
                "properties": {
                    "expr": { "type": "string", "description": "Lua boolean expression" },
                    "from": { "type": "integer", "description": "First seq to scan (default: first in the file)" },
                    "count": { "type": "integer", "description": "How many matches, up to 50 (default 10)" }
                },
                "required": ["expr"]
            }),
        ),
        func(
            "count_messages",
            "How many messages match a type and/or a Lua expression. Scans the whole \
             file, so it is slow on big files.",
            json!({
                "type": "object",
                "properties": {
                    "type": { "type": "string", "description": "Message type name" },
                    "expr": { "type": "string", "description": "Lua boolean expression" }
                }
            }),
        ),
        func(
            "make_filter",
            "Build the Lua filter code the viewer would run for these conditions, and \
             show it. Use it to check the expression without touching the viewer.",
            json!({
                "type": "object",
                "properties": {
                    "expr": { "type": "string", "description": "Lua boolean expression" },
                    "types": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Message type names to keep"
                    }
                }
            }),
        ),
        func(
            "apply_filter",
            "Set the viewer filter: keep only these message types and messages matching \
             the expression. An empty call clears the filter.",
            json!({
                "type": "object",
                "properties": {
                    "expr": { "type": "string", "description": "Lua boolean expression" },
                    "types": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Message type names to keep"
                    }
                }
            }),
        ),
        func(
            "get_lua_transform",
            "Read the current Lua transform code and whether it is enabled. The transform \
             runs in tll-lua NORMAL mode: `tll_on_data` sees every message and may change \
             its fields.",
            json!({ "type": "object", "properties": {} }),
        ),
        func(
            "set_lua_transform",
            "Set the Lua transform code the viewer runs over every message (F4 screen), and \
             whether it is enabled. This changes the view only, never the file on disk.\n\
             \n\
             HOW TO WRITE TLL-LUA TRANSFORM CODE:\n\
             The code runs in tll-lua NORMAL mode. Required hook:\n\
               function tll_on_data(seq, name, data) ... end\n\
             `data` is a read-only reflection object of the message body; fields are read as \
             `data.field.subfield` (`nil` if absent or if the message has no scheme).\n\
             To change a field, first copy the message and the submessage you modify \
             (reflections are read-only):\n\
               msg = tll_msg_copy(data)\n\
               msg.header = tll_msg_copy(msg.header)\n\
               msg.header.status = 5\n\
             To forward the (possibly modified) message upstream to the viewer use \
             `tll_callback` — NOT `tll_child_post` (that posts into the child channel, i.e. \
             the source file itself, which is read-only and fails):\n\
               tll_callback(seq, name, msg)\n\
             Optional hooks: `tll_on_open(cfg)` and `tll_on_close()` (may be empty).\n\
             Filtering is NOT done here — dropping a message means simply not calling \
             `tll_callback` for it. Other helpers: `tll_msg_deepcopy(data)` (recursive copy), \
             `tll_callback({ type = \"Data\", seq = 100, name = \"Msg\", data = {...} })` \
             (table form), `tll_time_point(...)`, `tll_msg_pmap_check(msg, field)`.",
            json!({
                "type": "object",
                "properties": {
                    "code": { "type": "string", "description": "Full Lua code (tll_on_data etc.). Empty string disables the transform." },
                    "enabled": { "type": "boolean", "description": "Whether the code is applied (false keeps the code but does not run it)" }
                },
                "required": ["code"]
            }),
        ),
        func(
            "search",
            "Search in the viewer, within the current filter, from the selected \
             message. Sets the selection to the first match.",
            json!({
                "type": "object",
                "properties": {
                    "expr": { "type": "string", "description": "Lua boolean expression" },
                    "back": { "type": "boolean", "description": "Search backwards (default false)" }
                },
                "required": ["expr"]
            }),
        ),
        func(
            "goto",
            "Move the viewer to a message: a seq number, `+N` — row number from the \
             start, `-N` — from the end.",
            json!({
                "type": "object",
                "properties": {
                    "where": { "type": "string", "description": "seq, +N or -N" }
                },
                "required": ["where"]
            }),
        ),
        func(
            "view_state",
            "Current viewer state: open file, generation, total messages, active filter, \
             visible columns, selected position, unsynchronised edits. Call at session \
             start or after being told the file was reopened.",
            json!({ "type": "object", "properties": {} }),
        ),
        func(
            "set_columns",
            "Set the table columns shown in the viewer. Each element is a field path \
             (e.g. \"header.status\"), a preset name, or \"#data\" for the message body \
             size in bytes.",
            json!({
                "type": "object",
                "properties": {
                    "columns": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Field paths, preset names, or \"#data\" (message body size)"
                    }
                },
                "required": ["columns"]
            }),
        ),
        func(
            "edit_messages",
            "Change messages in one batch: fields (`op` field, default), delete \
             (`remove` shifts later seqs down by one, `clear` leaves a gap; \
             `till` makes a range [seq, till)), change type (`retype`), insert a \
             new message before seq (`insert`, a copy of `copy_of` or of seq, \
             then `fields` on top), or undo (`restore`). An entry without \
             `value` drops that pending field edit. All seqs are file seqs: \
             renumbering happens only on commit. Removing K and inserting K \
             keeps the last seq. Find the messages first (`search_messages`, \
             `count_messages`), then send the whole batch in one call. The batch \
             is checked, shown to the human as one summary (first line: whether \
             the last seq changes) and confirmed once; the file is not written \
             until the human commits it.",
            json!({
                "type": "object",
                "properties": {
                    "edits": {
                        "type": "array",
                        "description": "One entry per field",
                        "items": {
                            "type": "object",
                            "properties": {
                                "op": {
                                    "type": "string",
                                    "enum": ["field", "remove", "clear", "retype", "insert", "restore"],
                                    "description": "What to do; default field"
                                },
                                "seq": { "type": "integer", "description": "Message seq (insert: before this seq)" },
                                "till": { "type": "integer", "description": "remove/clear/retype: end of range, exclusive; a retyped range gets one body: `type` plus `fields`, the rest default" },
                                "path": { "type": "string", "description": "field: path, e.g. header.status" },
                                "value": { "type": "string", "description": "field: new value as text; omit to drop the pending edit" },
                                "type": { "type": "string", "description": "retype/insert: message type name" },
                                "copy_of": { "type": "integer", "description": "insert: seq whose body is copied; default seq" },
                                "fields": {
                                    "type": "object",
                                    "additionalProperties": { "type": "string" },
                                    "description": "retype/insert: field path -> value on top of the body"
                                }
                            },
                            "required": ["seq"]
                        }
                    }
                },
                "required": ["edits"]
            }),
        ),
    ];
    tools
}

/// Пачковая правка меняет правки — нужен один подтверждающий набор на всю
/// пачку (`agr:agent-batch`).
pub fn needs_confirm(name: &str) -> bool {
    name == "edit_messages"
}

fn func(name: &str, description: &str, parameters: Value) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": parameters,
        }
    })
}

/// Исполнить инструмент. Аргументы — строка JSON от модели.
pub fn execute(ctx: &mut ToolCtx, name: &str, arguments: &str) -> Result<String, String> {
    let args: Value = if arguments.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(arguments).map_err(|e| format!("аргументы {name}: {e}"))?
    };
    match name {
        "file_overview" => overview(ctx),
        "message_types" => types(ctx, int(&args, "limit").unwrap_or(30) as usize),
        "schema_fields" => fields(ctx, args.get("type").and_then(|t| t.as_str())),
        "column_presets" => presets(ctx),
        "read_messages" => read(ctx, &args),
        "message_detail" => detail(ctx, &args),
        "search_messages" => find(ctx, &args),
        "count_messages" => count(ctx, &args),
        "make_filter" => make_filter(&args),
        "apply_filter" => apply_filter(ctx, &args),
        "get_lua_transform" => get_lua_transform(ctx),
        "set_lua_transform" => set_lua_transform(ctx, &args),
        "view_state" => view_state(ctx),
        "set_columns" => set_columns(ctx, &args),
        "search" => search(ctx, &args),
        "goto" => go_to(ctx, &args),
        // `edit_messages` всегда перехватывает `needs_confirm`: напрямую его
        // не исполняют — пачку применяет [`apply_batch`] после подтверждения.
        "edit_messages" => Err("edit_messages исполняется только через подтверждение".into()),
        other => Err(format!("нет инструмента {other}")),
    }
}

fn int(args: &Value, key: &str) -> Option<i64> {
    args.get(key).and_then(|v| v.as_i64())
}

fn strings(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn scheme_of(ctx: &ToolCtx) -> &Scheme {
    &ctx.file.scheme
}

fn overview(ctx: &ToolCtx) -> Result<String, String> {
    let info = &ctx.file.info;
    let bytes: u64 = ctx
        .file
        .parts
        .iter()
        .map(|p| std::fs::metadata(&p.path).map(|m| m.len()).unwrap_or(0))
        .sum();
    let mut out = String::new();
    if let Some(l) = &ctx.file.link {
        // Линк: что за канал и его состояние; файл ниже — его спул.
        out.push_str(&format!(
            "live link (read-only, grows by itself): {}\nlink state: {}\n",
            l.url(),
            l.status().line()
        ));
    }
    out += &format!(
        "file: {}\nparts: {}\nsize: {bytes} bytes\nfirst seq: {}\nlast seq: {}\n",
        ctx.file.name,
        ctx.file.parts.len(),
        info.seq_begin.map_or("-".into(), |s| s.to_string()),
        info.seq_last.map_or("-".into(), |s| s.to_string()),
    );
    match ctx.total {
        Some(total) => out.push_str(&format!("messages: {total}\n")),
        None => out.push_str("messages: index not ready yet\n"),
    }
    if let Some(version) = &info.version {
        out.push_str(&format!("format version: {version}\n"));
    }
    let s = scheme_of(ctx);
    out.push_str(&format!(
        "schema: {} types, {} structs\n",
        s.messages().count(),
        s.structs.len()
    ));
    for (k, v) in s.options.iter().take(6) {
        out.push_str(&format!("{k}: {v}\n"));
    }
    Ok(out)
}

fn types(ctx: &ToolCtx, limit: usize) -> Result<String, String> {
    let s = scheme_of(ctx);
    let Some(index) = &ctx.index else {
        let names: Vec<String> = s.messages().map(|m| m.name.clone()).collect();
        return Ok(format!(
            "index not ready: {} message types in the schema\n{}",
            names.len(),
            names.join("\n")
        ));
    };
    let mut hist: Vec<(i32, usize)> = index.hist.iter().map(|(k, v)| (*k, *v)).collect();
    hist.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut out = String::new();
    for (msgid, n) in hist.into_iter().take(limit.max(1)) {
        let name = s.message(msgid).map_or("?".to_string(), |m| m.name.clone());
        out.push_str(&format!("{name} (msgid {msgid}): {n}\n"));
    }
    Ok(out)
}

fn fields(ctx: &ToolCtx, ty: Option<&str>) -> Result<String, String> {
    let s = scheme_of(ctx);
    let Some(ty) = ty else {
        let mut out = String::new();
        for m in s.messages() {
            out.push_str(&format!(
                "{} (msgid {}): {} fields, {} bytes\n",
                m.name,
                m.msgid,
                m.fields.len(),
                m.size
            ));
        }
        return Ok(out);
    };
    let msg = match ty.parse::<i32>().ok().and_then(|id| s.message(id)) {
        Some(m) => m,
        None => s
            .messages()
            .find(|m| m.name == ty)
            .ok_or_else(|| format!("нет типа {ty}"))?,
    };
    let mut out = format!("{}: {} bytes\n", msg.name, msg.size);
    if let Some(pmap) = &msg.pmap {
        out.push_str(&format!("presence map: {pmap}\n"));
    }
    for f in &msg.fields {
        out.push_str(&format!(
            "  {} : {} @{}+{}\n",
            f.name,
            f.type_name(s),
            f.offset,
            f.size
        ));
        if let Kind::Int(_, crate::scheme::Sub::Enum(e)) = &f.kind {
            for (name, v) in e.values.iter().take(12) {
                out.push_str(&format!("      {v} = {name}\n"));
            }
        }
        if let Kind::Int(_, crate::scheme::Sub::Bits(b)) = &f.kind {
            for (name, off, w) in b.fields.iter().take(12) {
                out.push_str(&format!("      bit {off}+{w} = {name}\n"));
            }
        }
    }
    Ok(out)
}

fn presets(ctx: &ToolCtx) -> Result<String, String> {
    let user = crate::columns::load(scheme_of(ctx));
    let mut out = String::new();
    for p in user {
        out.push_str(&format!("{}: {}\n", p.name, p.columns.join(", ")));
    }
    Ok(out)
}

fn read(ctx: &mut ToolCtx, args: &Value) -> Result<String, String> {
    let from = int(args, "from")
        .or(ctx.file.info.seq_begin)
        .ok_or("файл пуст")?;
    let count = (int(args, "count").unwrap_or(READ_DEFAULT as i64).max(1) as usize).min(READ_MAX);
    let only = strings(args, "fields");
    let msgs = fetch::from_seq(&ctx.ctx, &ctx.src(), from, count)?;
    if msgs.is_empty() {
        return Ok(format!("нет сообщений с seq ≥ {from}"));
    }
    let s = scheme_of(ctx);
    let mut out = String::new();
    for m in &msgs {
        match s.message(m.msgid) {
            Some(st) => {
                out.push_str(&format!("- seq: {}\n  name: {}\n  data:\n", m.seq, st.name));
                let node = decode::decode(s, st, &m.data);
                for (_, n) in node.walk() {
                    if n.value.is_none() || n.error.is_some() {
                        continue;
                    }
                    if !only.is_empty() && !only.contains(&n.path) {
                        continue;
                    }
                    out.push_str(&format!("    {}: {}\n", n.path, n.text()));
                }
            }
            None => out.push_str(&format!(
                "- seq: {}\n  msgid: {}\n  data: !!binary {}\n",
                m.seq,
                m.msgid,
                m.data
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            )),
        }
    }
    Ok(out)
}

fn detail(ctx: &mut ToolCtx, args: &Value) -> Result<String, String> {
    let seq = int(args, "seq").ok_or("нужен seq")?;
    let msgs = fetch::from_seq(&ctx.ctx, &ctx.src(), seq, 1)?;
    let m = msgs
        .into_iter()
        .find(|m| m.seq == seq)
        .ok_or_else(|| format!("нет сообщения seq {seq}"))?;
    let s = scheme_of(ctx);
    let Some(st) = s.message(m.msgid) else {
        return Ok(format!(
            "seq {seq}: msgid {} unknown in schema, {} bytes",
            m.msgid,
            m.data.len()
        ));
    };
    let node = decode::decode(s, st, &m.data);
    let mut out = format!("seq: {}\nname: {}\n", m.seq, st.name);
    out.push_str(&dump::yaml(&node, "data", 0));
    Ok(out)
}

fn find(ctx: &mut ToolCtx, args: &Value) -> Result<String, String> {
    let expr = args
        .get("expr")
        .and_then(|e| e.as_str())
        .ok_or("нужен expr")?;
    let from = int(args, "from")
        .or(ctx.file.info.seq_begin)
        .ok_or("файл пуст")?;
    let count = (int(args, "count").unwrap_or(READ_DEFAULT as i64).max(1) as usize).min(READ_MAX);
    let last = ctx.file.info.seq_last.unwrap_or(from);
    ctx.ctx.ensure_lua(ctx.file.info.backend);
    let filter = Filter::expr(expr);
    // Выражение гоняем пост-эвалом поверх transform+правок (`agr:filter-post-eval`).
    let code = filter
        .expr_code_for(ctx.file.info.backend)
        .expect("есть expr");
    let mut tf = Transformer::filter(
        &ctx.ctx,
        ctx.file.info.backend,
        &code,
        ctx.file
            .scheme_url
            .as_deref()
            .ok_or("схема файла не читается — нечем оценить выражение")?,
    )?;
    let s = scheme_of(ctx);
    let mut out = String::new();
    let mut found = 0;
    let mut at = from;
    // Кусками: на большом файле один скан целиком занял бы минуты. Между
    // кусками смотрим флаг прерывания (`agr:agent-loop`).
    let mut truncated = false;
    while found < count && at <= last {
        if ctx.stop.load(std::sync::atomic::Ordering::SeqCst) {
            truncated = true;
            break;
        }
        let msgs = fetch::from_seq(&ctx.ctx, &ctx.src(), at, SCAN_CHUNK)?;
        if msgs.is_empty() {
            break;
        }
        for m in &msgs {
            // Позиция идёт по каждому прочитанному, а не по совпавшему: кусок
            // без совпадений иначе читался бы заново без конца.
            at = m.seq + 1;
            if tf.transform(m)?.is_none() {
                continue;
            }
            let name = s.message(m.msgid).map_or("?", |st| st.name.as_str());
            out.push_str(&format!("seq {} {}\n", m.seq, name));
            found += 1;
            if found == count {
                break;
            }
        }
    }
    if truncated {
        let total = ctx.total.unwrap_or(0);
        let matches: Vec<&str> = out.lines().collect();
        return Ok(serde_json::to_string(&json!({
            "matches": matches,
            "truncated": true,
            "scanned": at - from,
            "total": total
        }))
        .unwrap());
    }
    if out.is_empty() {
        return Ok(format!("нет совпадений с seq ≥ {from}"));
    }
    Ok(out)
}

fn count(ctx: &mut ToolCtx, args: &Value) -> Result<String, String> {
    let types = args
        .get("type")
        .and_then(|t| t.as_str())
        .map(|t| vec![t.to_string()])
        .unwrap_or_default();
    let expr = args
        .get("expr")
        .and_then(|e| e.as_str())
        .unwrap_or("")
        .to_string();
    let filter = Filter {
        messages: types,
        expr,
    };
    if filter.is_empty() {
        return match ctx.total {
            Some(total) => Ok(format!("{total} messages")),
            None => Err("нужен type или expr: индекс ещё не готов".into()),
        };
    }
    ctx.ctx.ensure_lua(ctx.file.info.backend);
    // Скан по флагу воркера: прерванный ход останавливает и его
    // (`agr:agent-loop`).
    let matches = crate::data::Matches::default();
    crate::data::filter(&ctx.file, &filter, &ctx.stop, &matches, || {});
    if let Some(e) = matches.error.lock().unwrap().clone() {
        return Err(e);
    }
    let n = matches.len();
    let truncated = ctx.stop.load(std::sync::atomic::Ordering::SeqCst);
    if truncated {
        let total = ctx.total.unwrap_or(0);
        Ok(serde_json::to_string(&json!({
            "matches": n,
            "truncated": true,
            "total": total
        }))
        .unwrap())
    } else {
        Ok(format!("{n} messages match"))
    }
}

fn filter_of(args: &Value) -> Filter {
    Filter {
        messages: strings(args, "types"),
        expr: args
            .get("expr")
            .and_then(|e| e.as_str())
            .unwrap_or("")
            .trim()
            .to_string(),
    }
}

fn make_filter(args: &Value) -> Result<String, String> {
    let f = filter_of(args);
    match f.code() {
        None => Ok("filter is empty: the viewer shows every message".into()),
        Some(code) => Ok(format!("summary: {}\n\n{code}", f.summary())),
    }
}

fn apply_filter(ctx: &mut ToolCtx, args: &Value) -> Result<String, String> {
    let f = filter_of(args);
    let summary = f.summary();
    ctx.act(Act::Filter(f));
    Ok(if summary.is_empty() {
        "filter cleared: every message is shown".into()
    } else {
        format!("filter set: {summary}")
    })
}

fn view_state(ctx: &mut ToolCtx) -> Result<String, String> {
    let file = ctx.file.name.clone();
    let gen = ctx.gen.load(std::sync::atomic::Ordering::SeqCst);
    let total = ctx.total.map(|n| n.to_string()).unwrap_or("?".into());
    let filter = ctx.filter.summary();
    let filter = if filter.is_empty() { "none" } else { &filter };
    let columns = if ctx.columns.is_empty() {
        "default".into()
    } else {
        ctx.columns.join(", ")
    };
    let pos = match (ctx.sel_row, ctx.sel_seq) {
        (Some(row), Some(seq)) => format!("row {row}, seq {seq}"),
        (None, Some(seq)) => format!("seq {seq}"),
        (Some(row), None) => format!("row {row}"),
        (None, None) => "unknown".into(),
    };
    let edits = if ctx.edits.is_empty() {
        "none".into()
    } else {
        format!(
            "{} messages, {} fields, {} total incl. deletes/inserts/retypes",
            ctx.edits.messages(),
            ctx.edits.fields(),
            ctx.edits.count()
        )
    };
    if !ctx.edits.is_empty() {
        ctx.act(Act::MarkSynced);
    }
    Ok(format!(
        "file: {file}\ngen: {gen}\ntotal: {total}\nfilter: {filter}\ncolumns: {columns}\nposition: {pos}\nunsync edits: {edits}"
    ))
}

fn set_columns(ctx: &mut ToolCtx, args: &Value) -> Result<String, String> {
    let cols = strings(args, "columns");
    if cols.is_empty() {
        return Err("columns list is empty".into());
    }
    ctx.columns = cols.clone();
    ctx.act(Act::SetColumns(cols.clone()));
    Ok(format!("columns set: {}", cols.join(", ")))
}

/// Текущий transform-код и его состояние (агент хранит последнее, что сам
/// установил или что было на старте).
fn get_lua_transform(ctx: &ToolCtx) -> Result<String, String> {
    if ctx.lua_code.is_empty() {
        return Ok("no transform: there is no Lua transform code".into());
    }
    Ok(format!("enabled: {}\n\n{}", ctx.lua_enabled, ctx.lua_code))
}

/// Установить transform-код: обновляет и вид, и копию агента. Пустой код —
/// снять transform; `enabled=false` — код остаётся, но не применяется.
fn set_lua_transform(ctx: &mut ToolCtx, args: &Value) -> Result<String, String> {
    let code = args
        .get("code")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    let enabled = args
        .get("enabled")
        .and_then(|e| e.as_bool())
        .unwrap_or(true);
    ctx.lua_code = code.clone();
    ctx.lua_enabled = enabled;
    ctx.act(Act::LuaTransform { code, enabled });
    if ctx.lua_code.is_empty() {
        Ok("transform cleared".into())
    } else if enabled {
        Ok(format!(
            "transform set (enabled): {} lines",
            ctx.lua_code.lines().count()
        ))
    } else {
        Ok(format!(
            "transform set (disabled): {} lines kept, not applied",
            ctx.lua_code.lines().count()
        ))
    }
}

fn search(ctx: &mut ToolCtx, args: &Value) -> Result<String, String> {
    let expr = args
        .get("expr")
        .and_then(|e| e.as_str())
        .ok_or("нужен expr")?;
    let back = args.get("back").and_then(|b| b.as_bool()).unwrap_or(false);
    ctx.act(Act::Search {
        filter: Filter::expr(expr),
        back,
    });
    Ok(format!("searching for {expr}"))
}

fn go_to(ctx: &mut ToolCtx, args: &Value) -> Result<String, String> {
    let where_ = args
        .get("where")
        .and_then(|w| w.as_str())
        .ok_or("нужен where")?;
    ctx.act(Act::Goto(where_.to_string()));
    Ok(format!("moved to {where_}"))
}

/// Пачка правок, собранная [`prepare`] и ждущая человека; применяет её целиком
/// [`apply_batch`] после единственного подтверждения (`agr:agent-batch`).
#[derive(Debug, Default)]
pub struct Batch {
    pub edits: crate::edit::Edits,
    /// Прогноз последнего seq после записи (`agr:seq-forecast`).
    pub forecast: Option<crate::edit::Forecast>,
}

impl Batch {
    /// Сводка для подтверждения: первой строкой — прогноз последнего seq.
    pub fn summary(&self) -> crate::edit::Summary {
        let mut s = self.edits.summary();
        if let Some(f) = &self.forecast {
            s.lines.insert(0, f.line());
        }
        s
    }
}

/// `agr:agent-batch` — собрать пачку правок одним вызовом: каждая запись
/// проверяется по текущему значению поля, кандидат целиком записывается по
/// схеме. Неверная пачка валится целиком и до человека не доходит — модель
/// получает отказ и чинит записи. Чтение агента на время проверки идёт через
/// кандидата; отказ от подтверждения откатывает [`restore`].
pub fn prepare(ctx: &mut ToolCtx, arguments: &str) -> Result<Batch, String> {
    if ctx.file.link.is_some() {
        // `agr:link-read-only`: линк только смотрят — отказ модели, человека не тревожим.
        return Err(
            "this is a live link, not a file: it is read-only — edits are not possible; \
             read, filter and search instead"
                .into(),
        );
    }
    let args: Value = if arguments.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(arguments).map_err(|e| format!("аргументы edit_messages: {e}"))?
    };
    let list = args
        .get("edits")
        .and_then(|v| v.as_array())
        .ok_or("нужен массив edits")?;
    if list.is_empty() {
        return Err("пустая пачка: нечего подтверждать".into());
    }
    let mut edits = ctx.edits.clone();
    let mut touched = std::collections::BTreeSet::new();
    let mut retyped: Vec<(i64, String)> = Vec::new();
    for e in list {
        let seq = e
            .get("seq")
            .and_then(|v| v.as_i64())
            .ok_or("в записи нужен seq")?;
        let op = e.get("op").and_then(|v| v.as_str()).unwrap_or("field");
        if op != "field" {
            stream_op(ctx, &mut edits, &mut retyped, op, seq, e)?;
            continue;
        }
        if edits.deleted(seq) {
            return Err(format!("seq {seq} удалён в пачке — поле не правится"));
        }
        let path = e
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or("в записи нужен path")?
            .to_string();
        match e.get("value").and_then(|v| v.as_str()) {
            Some(value) => {
                let msgs = fetch::from_seq(&ctx.ctx, &ctx.src(), seq, 1)?;
                let m = msgs
                    .into_iter()
                    .find(|m| m.seq == seq)
                    .ok_or_else(|| format!("нет сообщения seq {seq}"))?;
                let scheme = ctx.file.scheme.clone();
                let st = scheme
                    .message(m.msgid)
                    .ok_or_else(|| format!("тип msgid {} неизвестен", m.msgid))?;
                let current = decode::field(&scheme, st, &m.data, &path)
                    .ok_or_else(|| format!("нет поля {path} в {}", st.name))?;
                edits.set(seq, &st.name, &path, &current, value);
                touched.insert(seq);
            }
            // Без значения — снять правку поля.
            None => {
                edits.unset(seq, &path);
            }
        }
    }
    if let Err(e) = verify(ctx, &edits, &touched).and_then(|()| verify_stream(ctx, &retyped)) {
        restore(ctx);
        return Err(e);
    }
    let forecast = forecast(ctx, &edits)?;
    Ok(Batch { edits, forecast })
}

/// Сырые seq и msgid файла в `[from, to]` и следующий за ними.
fn file_seqs(ctx: &ToolCtx, from: i64, to: i64) -> Result<fetch::FileSeqs, String> {
    fetch::seqs_in(&ctx.ctx, &ctx.file.raw_sources(), from, to)
}

/// Тело сообщения `seq` в виде (с правками) и его тип.
fn body_at(ctx: &ToolCtx, seq: i64) -> Result<(String, Msg), String> {
    let m = fetch::from_seq(&ctx.ctx, &ctx.src(), seq, 1)?
        .into_iter()
        .find(|m| m.seq == seq)
        .ok_or_else(|| format!("нет сообщения seq {seq}"))?;
    let name = ctx
        .file
        .scheme
        .message(m.msgid)
        .map(|s| s.name.clone())
        .ok_or_else(|| format!("тип msgid {} неизвестен", m.msgid))?;
    Ok((name, m))
}

/// Записи состава потока (`agr:stream-ops`): удаление, смена типа, вставка,
/// снятие. Негодная запись валит пачку — модель чинит её сама.
fn stream_op(
    ctx: &mut ToolCtx,
    edits: &mut crate::edit::Edits,
    retyped: &mut Vec<(i64, String)>,
    op: &str,
    seq: i64,
    e: &Value,
) -> Result<(), String> {
    use crate::edit::{stream, Insert};
    let scheme = ctx.file.scheme.clone();
    let fields: Vec<(String, String)> = e
        .get("fields")
        .and_then(|v| v.as_object())
        .map(|o| {
            o.iter()
                .map(|(k, v)| {
                    let v = v
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v.to_string());
                    (k.clone(), v)
                })
                .collect()
        })
        .unwrap_or_default();
    match op {
        "remove" | "clear" => {
            let to = match e.get("till").and_then(|v| v.as_i64()) {
                Some(t) if t <= seq => return Err(format!("пустой диапазон [{seq}, {t})")),
                Some(t) => t - 1,
                None => seq,
            };
            let (seqs, after) = file_seqs(ctx, seq, to)?;
            if seqs.is_empty() {
                return Err(format!("нет сообщений в [{seq}, {}]", to));
            }
            for (i, (s, msgid)) in seqs.iter().enumerate() {
                let next = seqs.get(i + 1).map(|(n, _)| *n).or(after);
                let name = scheme
                    .message(*msgid)
                    .map(|st| st.name.clone())
                    .unwrap_or_default();
                edits.delete(*s, &name, op == "remove", next);
            }
        }
        "retype" if e.get("till").is_some() => {
            // Диапазон — одним телом: тип и `fields`, остальное по умолчанию.
            let to = e
                .get("type")
                .and_then(|v| v.as_str())
                .ok_or("retype: нужен type")?;
            if !scheme.messages().any(|m| m.name == to) {
                return Err(format!("типа {to} нет в схеме"));
            }
            let till = e.get("till").and_then(|v| v.as_i64()).unwrap_or(seq);
            if till <= seq {
                return Err(format!("пустой диапазон [{seq}, {till})"));
            }
            let (seqs, after) = file_seqs(ctx, seq, till - 1)?;
            let msgs: Vec<(i64, String, Option<i64>)> = seqs
                .iter()
                .enumerate()
                .map(|(i, (s, msgid))| {
                    let next = seqs.get(i + 1).map(|(n, _)| *n).or(after);
                    let name = scheme
                        .message(*msgid)
                        .map(|st| st.name.clone())
                        .unwrap_or_default();
                    (*s, name, next)
                })
                .collect();
            let body: stream::Body = fields.into_iter().collect();
            if edits.retype_many(&msgs, to, &body) == 0 {
                return Err(format!(
                    "[{seq}, {till}): менять нечего — все уже {to} или удалены"
                ));
            }
            if let Some(first) = msgs
                .iter()
                .find(|m| matches!(edits.op(m.0), Some(crate::edit::Op::Retype { .. })))
            {
                retyped.push((first.0, to.to_string()));
            }
        }
        "retype" => {
            let to = e
                .get("type")
                .and_then(|v| v.as_str())
                .ok_or("retype: нужен type")?;
            let to_st = scheme
                .messages()
                .find(|m| m.name == to)
                .ok_or_else(|| format!("типа {to} нет в схеме"))?;
            let (from, m) = body_at(ctx, seq)?;
            let from_st = scheme.message(m.msgid).ok_or("тип неизвестен")?;
            let mut body: stream::Body =
                stream::body_of(&decode::decode(&scheme, from_st, &m.data), &m.data)
                    .into_iter()
                    .filter(|(path, _)| {
                        !path.contains('[')
                            && match (
                                scheme.field_path(from_st, path),
                                scheme.field_path(to_st, path),
                            ) {
                                (Some((_, a)), Some((_, b))) => {
                                    format!("{:?}", a.kind) == format!("{:?}", b.kind)
                                }
                                _ => false,
                            }
                    })
                    .collect();
            body.extend(fields);
            edits.retype(seq, &from, to, body)?;
            retyped.push((seq, to.to_string()));
        }
        "insert" => {
            let last = ctx.file.info.seq_last.unwrap_or(-1);
            if seq > last {
                return Err(format!(
                    "вставка за последним seq {last} не поддерживается — вставьте перед ним"
                ));
            }
            let src = e.get("copy_of").and_then(|v| v.as_i64()).unwrap_or(seq);
            let occupied = file_seqs(ctx, seq, seq)?.0.iter().any(|(s, _)| *s == seq);
            let (name, mut body) = match body_at(ctx, src) {
                Ok((name, m)) => {
                    let st = scheme.message(m.msgid).ok_or("тип неизвестен")?;
                    (
                        name,
                        stream::body_of(&decode::decode(&scheme, st, &m.data), &m.data),
                    )
                }
                Err(_) if e.get("type").is_some() => (String::new(), stream::Body::new()),
                Err(x) => return Err(format!("insert: копировать нечего — {x}")),
            };
            let name = match e.get("type").and_then(|v| v.as_str()) {
                Some(t) if t != name => {
                    // Другой тип — тело только из fields.
                    body.clear();
                    t.to_string()
                }
                _ => name,
            };
            body.extend(fields);
            let mut ins = Insert {
                name,
                body,
                occupied,
                data: Vec::new(),
            };
            ins.data = check_insert(ctx, seq, &ins)?;
            edits.insert(seq, ins)?;
        }
        "restore" => {
            let mut any = edits.restore(seq);
            while edits.drop_insert(seq, 0) {
                any = true;
            }
            if !any {
                return Err(format!("seq {seq}: снимать нечего"));
            }
        }
        _ => return Err(format!("неизвестная op {op}")),
    }
    Ok(())
}

/// Собрать тело вставки по схеме: тело или отказ.
fn check_insert(ctx: &mut ToolCtx, seq: i64, ins: &crate::edit::Insert) -> Result<Vec<u8>, String> {
    if fetch::Src::part_of(&ctx.file.raw_sources(), seq).is_none() {
        return Err(format!("вставка перед {seq}: seq за концом файла"));
    }
    crate::edit::encode::compose(&ctx.file.scheme, &ins.name, &ins.body)
        .map(|(_, data)| data)
        .map_err(|e| format!("вставка перед {seq}: {e}"))
}

/// Смена типа записалась: сообщение читается с новым типом.
fn verify_stream(ctx: &mut ToolCtx, retyped: &[(i64, String)]) -> Result<(), String> {
    for (seq, to) in retyped {
        let m = check(ctx, *seq).map_err(|e| format!("смена типа seq {seq}: {e}"))?;
        let got = ctx.file.scheme.message(m.msgid).map(|s| s.name.as_str());
        if got != Some(to.as_str()) {
            return Err(format!("смена типа seq {seq} не записалась"));
        }
    }
    Ok(())
}

/// Прогноз последнего seq по пачке: без прохода, удалён хвост — чтением назад.
fn forecast(
    ctx: &ToolCtx,
    edits: &crate::edit::Edits,
) -> Result<Option<crate::edit::Forecast>, String> {
    let Some(before) = ctx.file.info.seq_last else {
        return Ok(None);
    };
    let map = edits.seq_map();
    let alive = fetch::alive_before(
        &ctx.ctx,
        &ctx.file.raw_sources(),
        ctx.file.begin(),
        before,
        |s| map.deleted(s),
    )?;
    Ok(Some(crate::edit::Forecast {
        before,
        after: map.last(before, alive),
        transform: ctx.lua_enabled && !ctx.lua_code.is_empty(),
    }))
}

/// Записать кандидата по схеме: каждое правленое сообщение читается с
/// наложенными правками — значение, которое не записать, обрывает чтение.
fn verify(
    ctx: &mut ToolCtx,
    edits: &crate::edit::Edits,
    touched: &std::collections::BTreeSet<i64>,
) -> Result<(), String> {
    ctx.write_patch(edits);
    let mut seqs: std::collections::BTreeSet<i64> = edits.iter().map(|(s, _)| s).collect();
    seqs.extend(touched);
    for seq in seqs {
        check(ctx, seq).map_err(|e| format!("правки не приняты (seq {seq}): {e}"))?;
    }
    Ok(())
}

/// `agr:agent-batch` — применить подтверждённую пачку: один [`Act::Sync`] на
/// всю пачку, вид перечитывает файл с правками один раз.
pub fn apply_batch(ctx: &mut ToolCtx, batch: Batch) -> String {
    ctx.edits = batch.edits;
    ctx.sync();
    format!(
        "правки применены: {} сообщений, {} полей, всего правок {}",
        ctx.edits.messages(),
        ctx.edits.fields(),
        ctx.edits.count()
    )
}

/// Откатить patch кандидата: чтение агента снова идёт по подтверждённым
/// правкам — человек пачку не принял (`agr:agent-batch`).
pub fn restore(ctx: &mut ToolCtx) {
    if ctx.edits.is_empty() {
        ctx.patch = None;
        ctx.file.patch = None;
    } else {
        let edits = ctx.edits.clone();
        ctx.write_patch(&edits);
    }
}

/// Прочитать сообщение `seq` с наложенными правками: правка, которую не
/// записать, обрывает чтение (как `data::check`).
fn check(ctx: &ToolCtx, seq: i64) -> Result<Msg, String> {
    let srcs = ctx.src();
    let Some(j) = fetch::Src::part_of(&srcs, seq) else {
        return Err(format!("seq {seq} за концом файла"));
    };
    let mut r = Reader::open(&ctx.ctx, &srcs[j].open.clone().at(seq))?;
    let m = r.next();
    if let Some(e) = r.error() {
        return Err(e);
    }
    match m {
        Some(m) if m.seq == seq => Ok(m),
        _ => Err(r
            .error()
            .unwrap_or_else(|| format!("seq {seq} не прочитался"))),
    }
}

/// Контекст инструментов на эталонном файле — для тестов этого модуля и
/// проверок хода агента.
#[cfg(all(test, feature = "tll"))]
pub(crate) fn tests_ctx() -> ToolCtx {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/plain.dat");
    let file = crate::data::File::open(&path).unwrap();
    ToolCtx {
        file,
        gen: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        patch: None,
        ctx: crate::io::Context::new(),
        total: None,
        index: None,
        acts: None,
        edits: crate::edit::Edits::default(),
        lua_code: String::new(),
        lua_enabled: true,
        debug: false,
        filter: crate::lua::Filter::default(),
        columns: Vec::new(),
        sel_seq: None,
        sel_row: None,
        last_gen: 0,
    }
}

#[cfg(all(test, feature = "tll"))]
mod tests {
    use super::*;
    use crate::agent::ToolCtx;

    fn ctx() -> ToolCtx {
        tests_ctx()
    }

    /// Подтверждение требует только пачковый инструмент.
    #[test]
    fn only_the_batch_tool_needs_confirm() {
        assert!(needs_confirm("edit_messages"));
        assert!(!needs_confirm("read_messages"));
        assert!(!needs_confirm("apply_filter"));
        assert!(!needs_confirm("goto"));
    }

    /// Каждая схема инструмента — правильной формы для OpenAI.
    #[test]
    fn every_schema_is_well_formed() {
        for t in build_tools() {
            assert_eq!(t["type"], "function");
            let f = &t["function"];
            assert!(f["name"].as_str().is_some_and(|s| !s.is_empty()));
            assert!(f["description"].as_str().is_some_and(|s| !s.is_empty()));
            assert_eq!(f["parameters"]["type"], "object");
        }
    }

    /// Обзор файла называет границы seq и схему.
    #[test]
    fn overview_reports_bounds() {
        let mut c = ctx();
        let text = execute(&mut c, "file_overview", "{}").unwrap();
        assert!(text.contains("first seq:"));
        assert!(text.contains("schema:"));
    }

    /// Чтение сообщений даёт seq, имя типа и значения полей.
    #[test]
    fn read_messages_lists_fields() {
        let mut c = ctx();
        let text = execute(&mut c, "read_messages", r#"{"count":1}"#).unwrap();
        assert!(text.contains("- seq:"));
        assert!(text.contains("name:"));
    }

    /// Ограничение по полям работает: лишние пути в вывод не попадают.
    #[test]
    fn read_messages_honours_field_filter() {
        let mut c = ctx();
        let all = execute(&mut c, "read_messages", r#"{"count":1}"#).unwrap();
        let paths: Vec<String> = all
            .lines()
            .filter_map(|l| l.strip_prefix("    "))
            .filter_map(|l| l.split(':').next())
            .map(|s| s.to_string())
            .collect();
        assert!(
            paths.len() >= 2,
            "в сообщении меньше двух скалярных полей: {paths:?}"
        );
        let one = execute(
            &mut c,
            "read_messages",
            &format!(r#"{{"count":1,"fields":["{}"]}}"#, paths[0]),
        )
        .unwrap();
        assert!(one.contains(&format!("    {}:", paths[0])));
        assert!(!one.contains(&format!("    {}:", paths[1])));
    }

    /// Неизвестный инструмент — понятная ошибка, а не паника.
    #[test]
    fn unknown_tool_is_an_error() {
        let mut c = ctx();
        assert!(execute(&mut c, "nope", "{}").unwrap_err().contains("nope"));
    }

    /// Сломанные аргументы ловятся до исполнения.
    #[test]
    fn bad_arguments_are_reported() {
        let mut c = ctx();
        let e = execute(&mut c, "read_messages", "{не json").unwrap_err();
        assert!(e.contains("аргументы"), "{e}");
    }

    /// `make_filter` печатает тот же код, что запустил бы просмотрщик.
    #[test]
    fn make_filter_shows_lua_code() {
        let mut c = ctx();
        let text = execute(&mut c, "make_filter", r#"{"expr":"seq > 5","types":["A"]}"#).unwrap();
        assert!(text.contains("function tll_filter"));
        assert!(text.contains("name ~= \"A\""));
        assert!(text.contains("return seq > 5"));
    }

    /// Действия уходят в канал просмотрщика, а не меняют состояние сами.
    #[test]
    fn actions_go_to_the_viewer() {
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        let mut c = ctx();
        c.acts = Some(tx);
        execute(&mut c, "apply_filter", r#"{"expr":"seq > 1"}"#).unwrap();
        execute(&mut c, "goto", r#"{"where":"+5"}"#).unwrap();
        execute(&mut c, "search", r#"{"expr":"seq > 2"}"#).unwrap();
        let acts: Vec<Act> = rx.try_iter().collect();
        assert_eq!(acts.len(), 3);
        assert!(matches!(acts[0], Act::Filter(_)));
        assert!(matches!(acts[1], Act::Goto(ref w) if w == "+5"));
        assert!(matches!(acts[2], Act::Search { back: false, .. }));
    }

    /// Без канала действий инструменты всё равно отвечают, а не падают.
    #[test]
    fn actions_without_viewer_are_safe() {
        let mut c = ctx();
        assert!(execute(&mut c, "goto", r#"{"where":"1"}"#)
            .unwrap()
            .contains("1"));
    }

    /// Пачка из двух сообщений собирается и применяется одним `Act::Sync`
    /// (`agr:agent-batch`).
    #[test]
    fn batch_of_two_messages_applies_with_one_sync() {
        let mut c = ctx();
        let seq = c.file.info.seq_begin.unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        c.acts = Some(tx);
        let b = prepare(
            &mut c,
            &format!(
                r#"{{"edits":[{{"seq":{seq},"path":"header.order_id","value":"777"}},{{"seq":{},"path":"header.order_id","value":"778"}}]}}"#,
                seq + 1
            ),
        )
        .unwrap();
        assert_eq!(b.edits.messages(), 2, "два сообщения в пачке");
        assert_eq!(b.edits.fields(), 2);
        assert!(c.patch.is_some(), "кандидат проверен через patch");
        let out = apply_batch(&mut c, b);
        assert!(out.contains("2 сообщений"), "{out}");
        assert!(matches!(rx.try_recv(), Ok(Act::Sync { .. })));
        assert!(rx.try_recv().is_err(), "один Act::Sync на всю пачку");
        assert!(rx.try_recv().is_err(), "агент не пишет файл сам");
    }

    /// Запись без значения снимает правку поля.
    #[test]
    fn entry_without_value_drops_the_edit() {
        let mut c = ctx();
        let seq = c.file.info.seq_begin.unwrap();
        let b = prepare(
            &mut c,
            &format!(r#"{{"edits":[{{"seq":{seq},"path":"header.order_id","value":"777"}}]}}"#),
        )
        .unwrap();
        c.edits = b.edits;
        let b = prepare(
            &mut c,
            &format!(r#"{{"edits":[{{"seq":{seq},"path":"header.order_id"}}]}}"#),
        )
        .unwrap();
        assert!(b.edits.is_empty(), "правка снята");
    }

    /// Значение, которое поле не вместит, валит всю пачку и не доходит
    /// до человека.
    #[test]
    fn refused_value_fails_the_whole_batch() {
        let mut c = ctx();
        let seq = c.file.info.seq_begin.unwrap();
        let e = prepare(
            &mut c,
            &format!(
                r#"{{"edits":[{{"seq":{seq},"path":"header.order_id","value":"777"}},{{"seq":{seq},"path":"header.order_id","value":"abc"}}]}}"#
            ),
        )
        .unwrap_err();
        assert!(e.contains("не приняты") || e.contains("не принята"), "{e}");
        assert!(c.patch.is_none(), "кандидат откачен");
    }

    /// Правка несуществующего поля отклоняется с понятной ошибкой.
    #[test]
    fn edit_unknown_field_is_rejected() {
        let mut c = ctx();
        let seq = c.file.info.seq_begin.unwrap();
        let e = prepare(
            &mut c,
            &format!(r#"{{"edits":[{{"seq":{seq},"path":"нет.такого","value":"1"}}]}}"#),
        )
        .unwrap_err();
        assert!(e.contains("нет поля"), "{e}");
    }

    /// `agr:link-read-only` — у линка пачка правок — отказ модели, до
    /// человека она не доходит.
    #[test]
    fn link_refuses_edit_batches() {
        let mut c = ctx();
        let link = crate::io::link::Link::start(
            "resolve://nosuch/x",
            "",
            None,
            None,
            std::sync::Arc::new(|| {}),
        )
        .unwrap();
        let _ = link.ready(|_| {});
        c.file.link = Some(std::sync::Arc::new(link));
        let e = prepare(&mut c, r#"{"edits":[{"seq":1}]}"#).unwrap_err();
        assert!(e.contains("read-only"), "{e}");
        let o = execute(&mut c, "file_overview", "{}").unwrap();
        assert!(o.contains("live link") && o.contains("Error"), "{o}");
    }

    /// Пустая пачка и сломанные записи не доходят до подтверждения.
    #[test]
    fn empty_or_broken_batch_is_an_error() {
        let mut c = ctx();
        assert!(prepare(&mut c, r#"{"edits":[]}"#)
            .unwrap_err()
            .contains("пустая"));
        assert!(prepare(&mut c, r#"{"edits":[{}]}"#)
            .unwrap_err()
            .contains("seq"));
        assert!(prepare(&mut c, "не json")
            .unwrap_err()
            .contains("аргументы"));
    }

    /// `agr:agent-batch` `agr:stream-ops` `agr:seq-forecast` — одна пачка
    /// смешивает поле, удаление диапазона, вставку и смену типа; сводка
    /// первой строкой говорит про последний seq.
    #[test]
    fn batch_mixes_field_remove_insert_and_retype() {
        let mut c = ctx();
        let b = prepare(
            &mut c,
            r#"{"edits":[
                {"seq":0,"path":"header.order_id","value":"777"},
                {"op":"remove","seq":10,"till":13},
                {"op":"insert","seq":20,"copy_of":2},
                {"op":"insert","seq":20,"copy_of":3},
                {"op":"insert","seq":20,"copy_of":7},
                {"op":"retype","seq":6,"type":"Cancel"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(b.edits.deleted_seqs(), vec![10, 11, 12]);
        assert_eq!(b.edits.inserts(20).len(), 3);
        assert!(b.edits.inserts(20).iter().all(|i| !i.data.is_empty()));
        assert!(b
            .edits
            .yaml()
            .contains("- cmd: Remove\n  from: 10\n  till: 13\n"));
        assert!(b.edits.yaml().contains("cmd: Update"));
        let s = b.summary();
        assert_eq!(s.lines[0], "последний seq 399 — не меняется", "{s:?}");
    }

    /// `agr:agent-batch` `agr:stream-ops` — смена типа диапазона: одна
    /// команда `Update` одним телом, уже нужного типа не трогаются.
    #[test]
    fn batch_retypes_a_range_with_one_body() {
        let mut c = ctx();
        let b = prepare(
            &mut c,
            r#"{"edits":[{"op":"retype","seq":10,"till":20,"type":"Ping"}]}"#,
        )
        .unwrap();
        // 14 и 19 — уже Ping (каждое пятое): диапазон рвётся на них.
        let y = b.edits.yaml();
        assert!(
            y.contains("- cmd: Update\n  from: 10\n  till: 14\n  name: 'Ping'\n  fields: {}\n"),
            "{y}"
        );
        assert!(y.contains("- cmd: Update\n  from: 15\n  till: 19\n"), "{y}");
        assert_eq!(y.matches("cmd: Update").count(), 2, "{y}");
    }

    /// `agr:agent-batch` — противоречивые записи валят пачку: поле удалённого,
    /// вставка за последним seq, неизвестная операция.
    #[test]
    fn conflicting_ops_on_one_seq_fail_the_batch() {
        let mut c = ctx();
        let e = prepare(
            &mut c,
            r#"{"edits":[{"op":"remove","seq":5},{"seq":5,"path":"header.order_id","value":"1"}]}"#,
        )
        .unwrap_err();
        assert!(e.contains("удалён"), "{e}");
        let e = prepare(
            &mut c,
            r#"{"edits":[{"op":"insert","seq":100000,"copy_of":2}]}"#,
        )
        .unwrap_err();
        assert!(e.contains("за последним"), "{e}");
        let e = prepare(&mut c, r#"{"edits":[{"op":"swap","seq":5}]}"#).unwrap_err();
        assert!(e.contains("неизвестная op"), "{e}");
        assert!(c.edits.is_empty());
    }

    /// `agr:agent-no-write` — состав потока от агента ложится в вид одним
    /// `Act::Sync`, файл не меняется.
    #[test]
    fn stream_ops_from_agent_do_not_write_the_file() {
        let mut c = ctx();
        let before = std::fs::read(&c.file.parts[0].path).unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        c.acts = Some(tx);
        let b = prepare(&mut c, r#"{"edits":[{"op":"clear","seq":30,"till":40}]}"#).unwrap();
        apply_batch(&mut c, b);
        assert!(matches!(rx.try_recv(), Ok(Act::Sync { .. })));
        assert!(rx.try_recv().is_err());
        assert_eq!(
            std::fs::read(&c.file.parts[0].path).unwrap(),
            before,
            "файл не тронут"
        );
        assert_eq!(c.edits.deleted_seqs().len(), 10);
    }

    /// Типы сообщений считаются по индексу, если он есть; иначе перечисляют схемные.
    #[test]
    fn message_types_counts_by_index() {
        let mut c = ctx();
        let without = execute(&mut c, "message_types", "{}").unwrap();
        assert!(
            without.contains("Data") || without.contains("Order"),
            "{without}"
        );
    }

    /// Схема полей возвращает список типов без аргумента.
    #[test]
    fn schema_fields_lists_types_when_no_arg() {
        let mut c = ctx();
        let text = execute(&mut c, "schema_fields", "{}").unwrap();
        assert!(text.contains("fields"), "{text}");
    }

    /// Колонки читаются из файла presets, даже если их нет.
    #[test]
    fn column_presets_are_reported() {
        let mut c = ctx();
        let text = execute(&mut c, "column_presets", "{}").unwrap();
        // Ответ или пуст, или содержит имена пресетов.
        assert!(!text.starts_with("error"));
    }

    /// Поиск сообщений по Lua-выражению возвращает seq и имя.
    #[test]
    fn search_messages_finds_matches() {
        let mut c = ctx();
        let text = execute(&mut c, "search_messages", r#"{"expr":"seq > 0","count":2}"#).unwrap();
        assert!(text.contains("seq "), "{text}");
    }

    /// Поиск идёт дальше куска без совпадений: единственное совпадение в
    /// последнем сообщении находится, скан не крутится на месте.
    #[test]
    fn search_messages_skips_chunks_without_matches() {
        let mut c = ctx();
        let last = c.file.info.seq_last.unwrap();
        assert!(
            last > c.file.info.seq_begin.unwrap(),
            "нужно больше одного сообщения"
        );
        let args = format!(r#"{{"expr":"seq == {last}","count":1}}"#);
        let text = execute(&mut c, "search_messages", &args).unwrap();
        assert!(text.starts_with(&format!("seq {last} ")), "{text}");
    }

    /// Подсчёт сообщений работает и без индекса, если задано выражение.
    #[test]
    fn count_messages_with_expr_works() {
        let mut c = ctx();
        let text = execute(&mut c, "count_messages", r#"{"expr":"seq > 0"}"#).unwrap();
        assert!(text.contains(" messages match"), "{text}");
    }
}
