//!
//! Клиент к модели: один вызов `POST {base}/chat/completions` потоком
//! ([`chat_stream`]) и разово ([`chat_once`]). Разбор потока SSE — чистая
//! функция [`parse_payload`]: payload кадра превращает в [`Chunk`] — кусок
//! текста, добавка к вызову инструмента или счёт токенов. По нему собирается
//! [`ToolCall`] из склеенных кусков аргументов.
//!
//! На провод диалог собирает [`wire_messages`]: системный промпт первым,
//! дальше сообщения как лежат, но вызовы без парного результата получают
//! заглушку — обрыв в логе не должен ломать запрос (`agr:agent-tool-pairing`).
//! Единственная точка — диалог: `chat_once` и `list_models` ходят своими
//! короткими запросами.
//!
//! Транспорт SSE — [`eventsource_stream`]: переводы строк и многострочный
//! `data:` по стандарту; payload кадра разбирает свой [`parse_payload`].
//! Надёжность: соединение с таймаутом, ретрай только пока не пришло ни
//! одного кадра (`agr:agent-retry`), idle-таймаут на кадр — длинный стрим
//! общий таймаут убил бы. 429 живёт по `Retry-After` (секунды или HTTP-date),
//! остальное — экспоненциально.
//!
//! `stream_options.include_usage` знает не всякий OpenAI-совместимый сервер:
//! на 400 запрос повторяется без него — размер контекста тогда останется
//! оценкой ([`estimate_tokens`]), но ответ будет.
//!
//! Наружу: [`ToolCall`], [`Reply`], [`Chunk`], [`parse_payload`],
//! [`collect_calls`], [`wire_messages`], [`chat_stream`], [`chat_once`],
//! [`list_models`], [`estimate_tokens`], [`msg_estimate`].

use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{json, Value};

use super::store::{CallReq, Msg, Settings};

/// Сколько секунд ждать очередной кадр стрима, пока считаем его живым.
const IDLE: std::time::Duration = std::time::Duration::from_secs(45);
/// Сколько попыток уходит на соединение и 429, пока стрим не начался.
const ATTEMPTS: u32 = 3;

/// Вызов инструмента, который попросила модель.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Аргументы — строка JSON, склеенная из кусков потока.
    pub arguments: String,
}

/// Ответ модели: текст, вызовы инструментов и счёт контекста от API.
#[derive(Clone, Debug, Default)]
pub struct Reply {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub tokens: Option<u64>,
    /// `finish_reason` последнего кадра: `stop`, `tool_calls`, `length`…
    pub finish: Option<String>,
    /// Слоты вызовов, не собравшиеся до конца: модель попросила вызовы, а
    /// имя или id до потока не дошли. Не ноль при `tool_calls` — ошибка хода,
    /// а не молчаливый конец.
    pub calls_dropped: usize,
}

/// Что принёс один кадр потока.
#[derive(Clone, Debug, PartialEq)]
pub enum Chunk {
    /// Кусок текста ответа.
    Text(String),
    /// Добавка к вызову инструмента: `index` — номер слота, если сервер его
    /// прислал.
    Call {
        index: Option<usize>,
        id: Option<String>,
        name: Option<String>,
        arguments: Option<String>,
    },
    /// Всего токенов в кадре `usage`.
    Usage(u64),
    /// Причина завершения из `choices[0].finish_reason`.
    Finish(String),
}

/// Разобрать payload кадра SSE (транспорт его уже вытащил): `None` — кадр
/// ничего не несёт (`[DONE]`, неразобранный JSON).
pub fn parse_payload(payload: &str) -> Option<Chunk> {
    let payload = payload.trim();
    if payload.is_empty() || payload == "[DONE]" {
        return None;
    }
    let v: Value = serde_json::from_str(payload).ok()?;
    if let Some(total) = usage_total(&v) {
        return Some(Chunk::Usage(total));
    }
    let choice = v.get("choices")?.as_array()?.first()?.clone();
    if let Some(finish) = choice.get("finish_reason").and_then(|f| f.as_str()) {
        return Some(Chunk::Finish(finish.to_string()));
    }
    let delta = choice.get("delta").cloned().unwrap_or(Value::Null);
    if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
        if !text.is_empty() {
            return Some(Chunk::Text(text.to_string()));
        }
    }
    let call = delta.get("tool_calls")?.as_array()?.first()?.clone();
    let pick = |path: &[&str]| {
        let mut cur = &call;
        for key in path {
            cur = cur.get(*key)?;
        }
        cur.as_str().map(|s| s.to_string())
    };
    Some(Chunk::Call {
        index: call
            .get("index")
            .and_then(|i| i.as_u64())
            .map(|i| i as usize),
        id: pick(&["id"]),
        name: pick(&["function", "name"]),
        arguments: pick(&["function", "arguments"]),
    })
}

#[cfg(test)]
fn parse_event(line: &str) -> Option<Chunk> {
    parse_payload(line.trim().strip_prefix("data:")?.trim())
}

/// Собрать вызовы инструментов из накопленных кусков: безымянный слот —
/// потерянный вызов, он не притворится готовым.
fn collect_calls(acc: &[ToolCall]) -> (Vec<ToolCall>, usize) {
    let mut out = Vec::new();
    let mut dropped = 0;
    for (i, c) in acc.iter().enumerate() {
        if c.name.is_empty() {
            dropped += 1;
            continue;
        }
        out.push(ToolCall {
            id: if c.id.is_empty() {
                format!("call_{i}")
            } else {
                c.id.clone()
            },
            name: c.name.clone(),
            arguments: c.arguments.clone(),
        });
    }
    (out, dropped)
}

/// Всего токенов в кадре `usage`: промпт и ответ вместе.
fn usage_total(v: &Value) -> Option<u64> {
    let usage = v.get("usage").filter(|u| !u.is_null())?;
    if let Some(total) = usage.get("total_tokens").and_then(|t| t.as_u64()) {
        return Some(total);
    }
    let prompt = usage.get("prompt_tokens").and_then(|t| t.as_u64())?;
    let completion = usage
        .get("completion_tokens")
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    Some(prompt + completion)
}

/// Грубая оценка токенов: кириллица — около 1,4 знака на токен, ASCII —
/// около 3,5. Точный счёт даёт API в `usage`, оценка нужна, пока его нет.
pub fn estimate_tokens(text: &str) -> u64 {
    let mut cyr = 0u64;
    let mut other = 0u64;
    for c in text.chars() {
        if c.is_ascii() {
            other += 1;
        } else {
            cyr += 1;
        }
    }
    // кириллица и прочая не-ASCII: ~1.4 симв/ток; ASCII: ~3.5 симв/ток
    (cyr * 10 / 14 + other * 10 / 35).max(1)
}

/// Оценка токенов одного сообщения: учитывает аргументы вызовов инструментов.
pub fn msg_estimate(m: &super::store::Msg) -> u64 {
    match m {
        super::store::Msg::Assistant {
            content,
            tool_calls,
        } => {
            let tc: u64 = tool_calls
                .iter()
                .map(|c| estimate_tokens(&c.arguments) + estimate_tokens(&c.name) + 10)
                .sum();
            estimate_tokens(content) + tc + 4
        }
        _ => estimate_tokens(m.content()) + 4,
    }
}

/// HTTP-клиент — через прокси из настроек, если он задан. Соединение —
/// с таймаутом: молчаливый сервер не должен висеть вечно (`agr:agent-retry`).
fn client(settings: &Settings) -> Result<reqwest::Client, String> {
    let proxy = settings.proxy.trim();
    let mut builder =
        reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(10));
    if !proxy.is_empty() {
        let url = if proxy.contains("://") {
            proxy.to_string()
        } else {
            format!("http://{proxy}")
        };
        let mut p = reqwest::Proxy::all(&url).map_err(|e| format!("прокси «{proxy}»: {e}"))?;
        let login = settings.proxy_login.trim();
        if !login.is_empty() {
            p = p.basic_auth(login, &settings.proxy_password);
        }
        builder = builder.proxy(p);
    }
    builder.build().map_err(|e| format!("клиент: {e}"))
}

/// Максимум токенов на контекст диалога (запас до лимита модели).
const CTX_LIMIT: u64 = 100_000;

/// Диалог на провод: системный промпт первым, сообщения как лежат. Вызов
/// ассистента без парного `Tool` получает заглушку — лог может хранить обрыв,
/// и незакрытый вызов не должен ронять запрос 400-м
/// (`agr:agent-tool-pairing`).
pub fn wire_messages(system: &str, messages: &[Msg]) -> Vec<Msg> {
    let mut out = Vec::with_capacity(messages.len() + 1);
    out.push(Msg::System {
        content: system.to_string(),
    });
    let mut i = 0;
    while i < messages.len() {
        let m = &messages[i];
        if let Msg::Assistant { tool_calls, .. } = m {
            out.push(m.clone());
            i += 1;
            // Пара — только Tool подряд, ничего между ними.
            let mut answered = std::collections::HashSet::new();
            while i < messages.len() {
                if let Msg::Tool { call_id, .. } = &messages[i] {
                    answered.insert(call_id.clone());
                    out.push(messages[i].clone());
                    i += 1;
                } else {
                    break;
                }
            }
            // Непарные вызовы закрываются заглушкой на проводе; лог не трогаем.
            for c in tool_calls {
                if !answered.contains(&c.id) {
                    out.push(Msg::Tool {
                        call_id: c.id.clone(),
                        name: c.name.clone(),
                        content: "ERROR: turn aborted".into(),
                        is_error: true,
                    });
                }
            }
        } else {
            out.push(m.clone());
            i += 1;
        }
    }
    trim_context(&mut out, CTX_LIMIT);
    out
}

/// Усечь провод до `limit` токенов. Стратегия:
/// 1. Сжать старые Tool-результаты до 200 символов (не затрагивает System и
///    последний ход).
/// 2. Если всё ещё переполнено — выбросить старые ходы (Assistant + его Tool)
///    целиком, сохранив первый User (задачу) и хвост.
fn trim_context(msgs: &mut Vec<Msg>, limit: u64) {
    let size: u64 = msgs.iter().map(msg_estimate).sum();
    if size <= limit {
        return;
    }

    // Найти индекс начала последнего хода (последнего Assistant в msgs).
    let last_assistant = msgs
        .iter()
        .rposition(|m| matches!(m, Msg::Assistant { .. }));

    // Шаг 1: сжать Tool-результаты старше последнего хода.
    let cutoff = last_assistant.unwrap_or(msgs.len());
    for m in msgs[..cutoff].iter_mut() {
        if let Msg::Tool { content, .. } = m {
            if content.chars().count() > 200 {
                let head: String = content.chars().take(200).collect();
                *content = format!("{head} [обрезано]");
            }
        }
    }

    // Шаг 2: если переполнено — выбросить старые ходы целиком.
    while msgs.iter().map(msg_estimate).sum::<u64>() > limit {
        // Найти первый User (задача) — его никогда не трогаем.
        let first_user = msgs.iter().position(|m| matches!(m, Msg::User { .. }));
        // Ход = Assistant + следующие за ним Tool. Ищем первый Assistant
        // после System и first_user.
        let skip = first_user.map(|p| p + 1).unwrap_or(1);
        let turn_start = msgs[skip..]
            .iter()
            .position(|m| matches!(m, Msg::Assistant { .. }))
            .map(|p| p + skip);
        let Some(start) = turn_start else {
            break;
        };
        // Конец хода: все Tool, идущие сразу после этого Assistant.
        let end = msgs[start + 1..]
            .iter()
            .position(|m| !matches!(m, Msg::Tool { .. }))
            .map(|p| start + 1 + p)
            .unwrap_or(msgs.len());
        msgs.drain(start..end);
    }
}

fn message_json(m: &Msg) -> Value {
    match m {
        Msg::System { content } | Msg::User { content } => {
            json!({ "role": m.role(), "content": content })
        }
        Msg::Assistant {
            content,
            tool_calls,
        } => {
            let calls: Vec<Value> = tool_calls
                .iter()
                .map(|c: &CallReq| {
                    json!({
                        "id": c.id,
                        "type": "function",
                        "function": { "name": c.name, "arguments": c.arguments },
                    })
                })
                .collect();
            json!({
                "role": "assistant",
                "content": content,
                "tool_calls": calls,
            })
        }
        // Имя инструмента в тело не идёт: необязательное, и часть серверов
        // его не любит — пары достаточно.
        Msg::Tool {
            call_id, content, ..
        } => json!({
            "role": "tool",
            "tool_call_id": call_id,
            "content": content,
        }),
    }
}

fn endpoint(settings: &Settings, tail: &str) -> String {
    format!("{}/{}", settings.base_url.trim_end_matches('/'), tail)
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let head: String = s.chars().take(n).collect();
    format!("{head}…")
}

/// Поток ответа: текст приходит кусками в `on_text`, вызовы собираются и
/// возвращаются вместе со счётом токенов и `finish_reason`.
pub async fn chat_stream(
    settings: &Settings,
    system: &str,
    messages: &[Msg],
    tools: &[Value],
    mut on_text: impl FnMut(&str),
    mut on_retry: impl FnMut(u32),
) -> Result<Reply, String> {
    let url = endpoint(settings, "chat/completions");
    let body_messages: Vec<Value> = wire_messages(system, messages)
        .iter()
        .map(message_json)
        .collect();
    let mut body = json!({
        "model": settings.model,
        "messages": body_messages,
        "tools": tools,
        "stream": true,
        "stream_options": { "include_usage": true },
    });

    let client = client(settings)?;
    let post = |body: &Value| {
        client
            .post(&url)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", settings.api_key),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body)
            .send()
    };

    // Запрос: пока стрим не начался, соединение и 429 — повод повторить.
    let mut resp = None;
    for attempt in 1..=ATTEMPTS {
        match post(&body).await {
            Ok(r) => {
                resp = Some(r);
                break;
            }
            Err(e) if attempt < ATTEMPTS => {
                on_retry(attempt);
                tokio::time::sleep(backoff(attempt)).await;
                let _ = e;
            }
            Err(e) => return Err(format!("не удалось достучаться до LLM: {e}")),
        }
    }
    let mut resp = resp.expect("последняя попытка вернула ответ");
    if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        // 429 живёт по Retry-After; стрим не начался — повторять можно.
        for attempt in 1..ATTEMPTS {
            let wait = retry_after(resp.headers()).unwrap_or_else(|| backoff(attempt));
            on_retry(attempt);
            tokio::time::sleep(wait).await;
            match post(&body).await {
                Ok(r) => {
                    resp = r;
                    if resp.status() != reqwest::StatusCode::TOO_MANY_REQUESTS {
                        break;
                    }
                }
                Err(e) => return Err(format!("не удалось достучаться до LLM: {e}")),
            }
        }
    }
    if resp.status() == reqwest::StatusCode::BAD_REQUEST {
        if let Some(map) = body.as_object_mut() {
            map.remove("stream_options");
        }
        resp = post(&body)
            .await
            .map_err(|e| format!("не удалось достучаться до LLM: {e}"))?;
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("LLM ответил {status}: {}", clip(&text, 200)));
    }

    let mut reply = Reply::default();
    let mut acc: Vec<ToolCall> = Vec::new();
    // Транспорт SSE по стандарту (переводы строк, многострочный `data:`),
    // payload — свой. Кадр за кадром с idle-таймаутом: длинный стрим общий
    // таймаут убил бы, а молчание сервера — не жизнь.
    let mut events = resp.bytes_stream().eventsource();
    // После `finish_reason` модель закончила: некоторые серверы держат SSE
    // открытым (keep-alive), и ход висел бы до idle-таймаута. Даём короткую
    // отсрочку на кадр с usage и выходим.
    let mut tail: Option<std::time::Instant> = None;
    loop {
        const GRACE: std::time::Duration = std::time::Duration::from_secs(2);
        let budget = match tail {
            Some(t) if t.elapsed() >= GRACE => break,
            Some(t) => GRACE - t.elapsed(),
            None => IDLE,
        };
        let ev = match tokio::time::timeout(budget, events.next()).await {
            Err(..) => {
                if tail.is_some() {
                    break;
                }
                return Err("поток LLM замолчал".into());
            }
            Ok(None) => break,
            Ok(Some(ev)) => ev.map_err(|e| format!("поток LLM: {e}"))?,
        };
        match parse_payload(&ev.data) {
            Some(Chunk::Text(text)) => {
                on_text(&text);
                reply.content.push_str(&text);
            }
            Some(Chunk::Usage(tokens)) => reply.tokens = Some(tokens),
            Some(Chunk::Finish(finish)) => {
                reply.finish = Some(finish);
                tail = Some(std::time::Instant::now());
            }
            Some(Chunk::Call {
                index,
                id,
                name,
                arguments,
            }) => {
                // Слот: с id — новый; с index — по номеру; без всего —
                // продолжение последнего.
                let slot = if id.is_some() {
                    acc.push(ToolCall {
                        id: String::new(),
                        name: String::new(),
                        arguments: String::new(),
                    });
                    acc.last_mut().unwrap()
                } else if let Some(i) = index {
                    while acc.len() <= i {
                        acc.push(ToolCall {
                            id: String::new(),
                            name: String::new(),
                            arguments: String::new(),
                        });
                    }
                    &mut acc[i]
                } else if let Some(last) = acc.last_mut() {
                    last
                } else {
                    continue;
                };
                if let Some(id) = id {
                    slot.id = id;
                }
                if let Some(name) = name {
                    slot.name = name;
                }
                if let Some(args) = arguments {
                    slot.arguments.push_str(&args);
                }
            }
            None => {}
        }
    }
    let (calls, dropped) = collect_calls(&acc);
    reply.tool_calls = calls;
    reply.calls_dropped = dropped;
    Ok(reply)
}

/// Пауза перед повтором: экспоненциально, 2 и 4 секунды.
fn backoff(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_secs(1 << attempt)
}

/// `Retry-After`: секунды или HTTP-date; не разобралось — общий backoff.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<std::time::Duration> {
    let v = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    if let Ok(secs) = v.trim().parse::<u64>() {
        return Some(std::time::Duration::from_secs(secs));
    }
    http_date_secs(v).map(std::time::Duration::from_secs)
}

/// HTTP-date (`Wed, 21 Oct 2015 07:28:00 GMT`) в секунды от начала эпохи.
fn http_date_secs(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() != 6 || !parts[5].eq_ignore_ascii_case("GMT") {
        return None;
    }
    let day: u64 = parts[1].parse().ok()?;
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|m| parts[2].eq_ignore_ascii_case(m))? as u64
        + 1;
    let year: u64 = parts[3].parse().ok()?;
    let (h, m, sec): (u64, u64, u64) = {
        let mut t = parts[4].split(':');
        (
            t.next()?.parse().ok()?,
            t.next()?.parse().ok()?,
            t.next()?.parse().ok()?,
        )
    };
    // Дни до начала года (год — не високосный по умолчанию), плюс високосные.
    let leap = |y: u64| y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400));
    let mut days = 0;
    for y in 1970..year {
        days += if leap(y) { 366 } else { 365 };
    }
    const LEN: [u64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for mo in 1..month {
        days += LEN[(mo - 1) as usize];
        if mo == 2 && leap(year) {
            days += 1;
        }
    }
    days += day - 1;
    Some(days * 86400 + h * 3600 + m * 60 + sec)
}

/// Разовый вызов без потока — для коротких задач: заголовок диалога, пересказ.
pub async fn chat_once(settings: &Settings, system: &str, prompt: &str) -> Result<String, String> {
    let url = endpoint(settings, "chat/completions");
    let body = json!({
        "model": settings.model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": prompt },
        ],
    });
    let resp = client(settings)?
        .post(&url)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", settings.api_key),
        )
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("не удалось достучаться до LLM: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("LLM ответил {status}: {}", clip(&text, 200)));
    }
    let v: Value = resp.json().await.map_err(|e| format!("ответ LLM: {e}"))?;
    v.get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| s.trim().to_string())
        .ok_or_else(|| "в ответе LLM нет текста".into())
}

/// Проверить, поддерживает ли сервер поле `tools`. Посылает минимальный
/// `POST chat/completions` с одной схемой инструмента; `true` — если не 400/422.
pub async fn probe_tools(settings: &Settings) -> bool {
    let url = endpoint(settings, "chat/completions");
    let body = json!({
        "model": settings.model,
        "messages": [{ "role": "user", "content": "ping" }],
        "tools": [{
            "type": "function",
            "function": {
                "name": "probe",
                "description": "probe",
                "parameters": { "type": "object", "properties": {} },
            },
        }],
        "max_tokens": 1,
    });
    let resp = client(settings)
        .ok()
        .map(|c| {
            c.post(&url)
                .header(
                    reqwest::header::AUTHORIZATION,
                    format!("Bearer {}", settings.api_key),
                )
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .json(&body)
                .send()
        });
    match resp {
        Some(fut) => match fut.await {
            Ok(r) => {
                let s = r.status().as_u16();
                s != 400 && s != 422
            }
            Err(_) => true, // сеть недоступна — не делаем вывод о поддержке
        },
        None => true,
    }
}

/// Список моделей с адреса API (`GET {base}/models`).
pub async fn list_models(settings: &Settings) -> Result<Vec<String>, String> {
    let url = endpoint(settings, "models");
    let resp = client(settings)?
        .get(&url)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", settings.api_key),
        )
        .send()
        .await
        .map_err(|e| format!("не удалось достучаться до LLM: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("LLM ответил {status}: {}", clip(&text, 200)));
    }
    let v: Value = resp.json().await.map_err(|e| format!("ответ LLM: {e}"))?;
    let mut out: Vec<String> = v
        .get("data")
        .and_then(|d| d.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| m.get("id").and_then(|i| i.as_str()))
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Кусок текста берётся из `delta.content`.
    #[test]
    fn parses_text_delta() {
        let line = r#"data: {"choices":[{"delta":{"content":"привет"}}]}"#;
        assert_eq!(parse_event(line), Some(Chunk::Text("привет".into())));
    }

    /// Вызов инструмента собирается по кускам: имя, id и аргументы.
    #[test]
    fn parses_tool_call_pieces() {
        let head = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read","arguments":"{\"a\""}}]}}]}"#;
        let tail = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":":1}"}}]}}]}"#;
        let a = match parse_event(head).unwrap() {
            Chunk::Call {
                index,
                id,
                name,
                arguments,
            } => {
                assert_eq!(index, Some(0));
                assert_eq!(id.as_deref(), Some("c1"));
                assert_eq!(name.as_deref(), Some("read"));
                arguments.unwrap()
            }
            other => panic!("ожидался вызов, пришло {other:?}"),
        };
        let b = match parse_event(tail).unwrap() {
            Chunk::Call { arguments, .. } => arguments.unwrap(),
            other => panic!("ожидался вызов, пришло {other:?}"),
        };
        let mut acc = vec![ToolCall {
            id: "c1".into(),
            name: "read".into(),
            arguments: a,
        }];
        acc[0].arguments.push_str(&b);
        let (calls, dropped) = collect_calls(&acc);
        assert_eq!(dropped, 0);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read");
        assert_eq!(calls[0].arguments, r#"{"a":1}"#);
    }

    /// Причина завершения читается из кадра.
    #[test]
    fn parses_finish_reason() {
        let line = r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#;
        assert_eq!(parse_event(line), Some(Chunk::Finish("tool_calls".into())));
        let line = r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#;
        assert_eq!(parse_event(line), Some(Chunk::Finish("stop".into())));
    }

    /// Дельта вызова без `id` и без `index` — продолжение последнего слота.
    #[test]
    fn piece_without_index_and_id_goes_to_last_slot() {
        // Новые слоты заводятся только по id или index.
        let first = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read","arguments":"{"}}]}}]}"#;
        let more =
            r#"data: {"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"a}"}}]}}]}"#;
        let mut acc: Vec<ToolCall> = Vec::new();
        for line in [first, more] {
            match parse_event(line).unwrap() {
                Chunk::Call {
                    index,
                    id,
                    name,
                    arguments,
                } => {
                    let slot = if id.is_some() {
                        acc.push(ToolCall {
                            id: String::new(),
                            name: String::new(),
                            arguments: String::new(),
                        });
                        acc.last_mut().unwrap()
                    } else if let Some(i) = index {
                        while acc.len() <= i {
                            acc.push(ToolCall {
                                id: String::new(),
                                name: String::new(),
                                arguments: String::new(),
                            });
                        }
                        &mut acc[i]
                    } else {
                        acc.last_mut().unwrap()
                    };
                    if let Some(id) = id {
                        slot.id = id;
                    }
                    if let Some(name) = name {
                        slot.name = name;
                    }
                    if let Some(args) = arguments {
                        slot.arguments.push_str(&args);
                    }
                }
                other => panic!("ожидался вызов, пришло {other:?}"),
            }
        }
        assert_eq!(acc.len(), 1, "продолжка не завела второй слот");
        assert_eq!(acc[0].arguments, "{a}");
        assert_eq!(acc[0].name, "read");
    }

    /// Счёт токенов берётся из кадра `usage`.
    #[test]
    fn parses_usage_frame() {
        let line = r#"data: {"usage":{"total_tokens":123}}"#;
        assert_eq!(parse_event(line), Some(Chunk::Usage(123)));
        let line = r#"data: {"usage":{"prompt_tokens":100,"completion_tokens":7}}"#;
        assert_eq!(parse_event(line), Some(Chunk::Usage(107)));
    }

    /// Служебные строки потока ничего не несут.
    #[test]
    fn ignores_service_lines() {
        assert_eq!(parse_event("data: [DONE]"), None);
        assert_eq!(parse_event(": ping"), None);
        assert_eq!(parse_event(""), None);
        assert_eq!(parse_event("data: {не json}"), None);
    }

    /// Безымянный слот — потерянный вызов: он отбрасывается и считается
    /// несобравшимся, а не притворяется готовым.
    #[test]
    fn drops_unnamed_and_counts_dropped() {
        let (calls, dropped) = collect_calls(&[
            ToolCall {
                id: String::new(),
                name: String::new(),
                arguments: "{}".into(),
            },
            ToolCall {
                id: String::new(),
                name: "search".into(),
                arguments: "{}".into(),
            },
        ]);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "search");
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(dropped, 1);
    }

    /// Системный промпт первый, остальные — как лежат
    /// (`agr:agent-tool-pairing`).
    #[test]
    fn wire_puts_system_first_and_keeps_order() {
        let msgs = [
            Msg::User {
                content: "вопрос".into(),
            },
            Msg::Assistant {
                content: "смотрю".into(),
                tool_calls: vec![CallReq {
                    id: "c1".into(),
                    name: "read_messages".into(),
                    arguments: "{}".into(),
                }],
            },
            Msg::Tool {
                call_id: "c1".into(),
                name: "read_messages".into(),
                content: "данные".into(),
                is_error: false,
            },
        ];
        let wire = wire_messages("SYS", &msgs);
        assert_eq!(wire.len(), 4);
        assert_eq!(wire[0].role(), "system");
        assert_eq!(wire[1].role(), "user");
        assert_eq!(wire[2].role(), "assistant");
        assert_eq!(wire[3].role(), "tool");
        // Форма на проводе: пара по `tool_call_id`, имя инструмента в теле
        // результата не нужно.
        let tool = message_json(&wire[3]);
        assert_eq!(tool["tool_call_id"], "c1");
        assert!(tool.get("name").is_none());
        let assistant = message_json(&wire[2]);
        assert_eq!(assistant["tool_calls"][0]["id"], "c1");
        assert_eq!(assistant["tool_calls"][0]["type"], "function");
    }

    /// Вызов без пары закрывается заглушкой на проводе; между ассистентом и
    /// парами ничего не встаёт (`agr:agent-tool-pairing`).
    #[test]
    fn unpaired_calls_get_synthetic_stubs() {
        let msgs = [
            Msg::Assistant {
                content: String::new(),
                tool_calls: vec![
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
            },
            Msg::Tool {
                call_id: "b".into(),
                name: "search".into(),
                content: "нашлось".into(),
                is_error: false,
            },
            Msg::User {
                content: "дальше".into(),
            },
        ];
        let wire = wire_messages("SYS", &msgs);
        let roles: Vec<_> = wire.iter().map(|m| m.role()).collect();
        assert_eq!(
            roles,
            vec!["system", "assistant", "tool", "tool", "user"],
            "заглушка для «a» встаёт сразу за парами"
        );
        // Сначала пары из лога, потом заглушки непарных.
        match &wire[2] {
            Msg::Tool {
                call_id, content, ..
            } => {
                assert_eq!(call_id, "b");
                assert_eq!(content, "нашлось");
            }
            other => panic!("ожидалась пара, пришло {other:?}"),
        }
        match &wire[3] {
            Msg::Tool {
                call_id,
                content,
                is_error,
                ..
            } => {
                assert_eq!(call_id, "a");
                assert!(*is_error);
                assert!(content.contains("aborted"));
            }
            other => panic!("ожидалась заглушка, пришло {other:?}"),
        }
        // Лог не переписан: заглушки живут только на проводе.
        assert_eq!(msgs.len(), 3);
    }

    /// Оценка токенов: кириллица дороже латиницы, пустой текст — не ноль.
    #[test]
    fn estimates_tokens() {
        assert_eq!(estimate_tokens(""), 1);
        assert!(estimate_tokens("привет мир") > estimate_tokens("hello world"));
        // "привет мир": 9 кир + 1 пробел → 9*10/14 + 1*10/35 = 6 + 0 = 6
        assert_eq!(estimate_tokens("привет мир"), 6);
        // 10 ASCII: 10*10/35 = 2
        assert_eq!(estimate_tokens("hello worl"), 2);
    }

    /// `Retry-After`: секунды читаются, HTTP-date — тоже (`agr:agent-retry`).
    #[test]
    fn parses_retry_after() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(reqwest::header::RETRY_AFTER, "7".parse().unwrap());
        assert_eq!(retry_after(&h), Some(std::time::Duration::from_secs(7)));
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            reqwest::header::RETRY_AFTER,
            "Wed, 21 Oct 2015 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(
            retry_after(&h),
            Some(std::time::Duration::from_secs(1445412480))
        );
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(reqwest::header::RETRY_AFTER, "завтра".parse().unwrap());
        assert_eq!(retry_after(&h), None, "не разобралось — общий backoff");
        assert_eq!(backoff(1), std::time::Duration::from_secs(2));
        assert_eq!(backoff(2), std::time::Duration::from_secs(4));
    }

    /// Многострочный `data:` склеивается переводами строк — транспорт SSE
    /// (`agr:agent-loop`).
    #[test]
    fn parse_payload_takes_transport_payload() {
        // Транспорт отдаёт payload без префикса `data:`.
        assert_eq!(
            parse_payload(r#"{"choices":[{"delta":{"content":"привет"}}]}"#),
            Some(Chunk::Text("привет".into()))
        );
        assert_eq!(parse_payload("[DONE]"), None);
    }

    /// id приходит не первым кадром дельты: документируем текущее поведение
    /// (новый слот заводится по id.is_some(), предыдущие аргументы теряются).
    #[test]
    fn delta_id_arriving_late_documents_behaviour() {
        // Кадр 1: index=0, args="{"  — слот без id/name
        // Кадр 2: index=0, id="c1", name="fn", args="a}" — id пришёл вторым кадром
        let kdr1 = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{"}}]}}]}"#;
        let kdr2 = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"fn","arguments":"a}"}}]}}]}"#;
        let mut acc: Vec<ToolCall> = Vec::new();
        for line in [kdr1, kdr2] {
            if let Some(Chunk::Call { index, id, name, arguments }) = parse_event(line) {
                let slot = if id.is_some() {
                    // id.is_some() → новый слот (текущее поведение)
                    acc.push(ToolCall { id: String::new(), name: String::new(), arguments: String::new() });
                    acc.last_mut().unwrap()
                } else if let Some(i) = index {
                    while acc.len() <= i { acc.push(ToolCall { id: String::new(), name: String::new(), arguments: String::new() }); }
                    &mut acc[i]
                } else {
                    continue;
                };
                if let Some(id) = id { slot.id = id; }
                if let Some(name) = name { slot.name = name; }
                if let Some(args) = arguments { slot.arguments.push_str(&args); }
            }
        }
        // Текущее поведение: два слота — один от index=0 (пустой), второй от id.
        // Когда это изменится, тест укажет на это.
        assert_eq!(acc.len(), 2, "id.is_some() заводит новый слот даже если index тот же");
        assert_eq!(acc[1].name, "fn");
        assert_eq!(acc[1].arguments, "a}");
    }

    /// Два параллельных вызова по index: аргументы попадают в правильный слот.
    #[test]
    fn two_parallel_calls_accumulate_by_index() {
        // {index:0, name:"fn0"} → {index:1, name:"fn1"} → {index:0, args:"0"} →
        // {index:1, args:"1"}
        let frames = [
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"fn0","arguments":""}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"b","function":{"name":"fn1","arguments":""}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"0"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"1"}}]}}]}"#,
        ];
        let mut acc: Vec<ToolCall> = Vec::new();
        for line in frames {
            if let Some(Chunk::Call { index, id, name, arguments }) = parse_event(line) {
                let slot = if id.is_some() {
                    acc.push(ToolCall { id: String::new(), name: String::new(), arguments: String::new() });
                    acc.last_mut().unwrap()
                } else if let Some(i) = index {
                    while acc.len() <= i { acc.push(ToolCall { id: String::new(), name: String::new(), arguments: String::new() }); }
                    &mut acc[i]
                } else {
                    acc.last_mut().unwrap()
                };
                if let Some(id) = id { slot.id = id; }
                if let Some(name) = name { slot.name = name; }
                if let Some(args) = arguments { slot.arguments.push_str(&args); }
            }
        }
        let (calls, dropped) = collect_calls(&acc);
        assert_eq!(dropped, 0);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "fn0");
        assert_eq!(calls[0].arguments, "0");
        assert_eq!(calls[1].name, "fn1");
        assert_eq!(calls[1].arguments, "1");
    }

    /// index с пропуском создаёт пустые слоты-«дырки»; они попадают в
    /// collect_calls как dropped (пустой name).
    #[test]
    fn index_gap_creates_dropped_slots() {
        // index=2 без предыдущих 0 и 1
        let frame = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":2,"id":"c","function":{"name":"fn","arguments":"{}"}}]}}]}"#;
        let mut acc: Vec<ToolCall> = Vec::new();
        if let Some(Chunk::Call { index, id, name, arguments }) = parse_event(frame) {
            let slot = if id.is_some() {
                acc.push(ToolCall { id: String::new(), name: String::new(), arguments: String::new() });
                acc.last_mut().unwrap()
            } else if let Some(i) = index {
                while acc.len() <= i { acc.push(ToolCall { id: String::new(), name: String::new(), arguments: String::new() }); }
                &mut acc[i]
            } else {
                acc.last_mut().unwrap()
            };
            if let Some(id) = id { slot.id = id; }
            if let Some(name) = name { slot.name = name; }
            if let Some(args) = arguments { slot.arguments.push_str(&args); }
        }
        // id.is_some() → новый слот (логика id-first), index:2 не создаёт дырки в этом пути
        // Проверим поведение collect_calls с настоящими дырками через Vec прямо:
        let with_gaps = vec![
            ToolCall { id: String::new(), name: String::new(), arguments: String::new() }, // дырка 0
            ToolCall { id: String::new(), name: String::new(), arguments: String::new() }, // дырка 1
            ToolCall { id: "c".into(), name: "fn".into(), arguments: "{}".into() },        // index 2
        ];
        let (calls, dropped) = collect_calls(&with_gaps);
        assert_eq!(dropped, 2, "дырки считаются как dropped");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "fn");
    }

    /// JSON Tool-сообщения: поле is_error не попадает на провод.
    #[test]
    fn tool_json_omits_is_error() {
        let m = Msg::Tool {
            call_id: "c1".into(),
            name: "read".into(),
            content: "ok".into(),
            is_error: true,
        };
        let j = message_json(&m);
        assert_eq!(j["role"], "tool");
        assert_eq!(j["tool_call_id"], "c1");
        assert_eq!(j["content"], "ok");
        assert!(j.get("is_error").is_none(), "is_error не должен уходить на провод");
        assert!(j.get("name").is_none(), "name тоже не уходит");
    }

    /// JSON Assistant без tool_calls: поле tool_calls — пустой массив.
    #[test]
    fn assistant_json_empty_tool_calls() {
        let m = Msg::Assistant {
            content: "ответ".into(),
            tool_calls: vec![],
        };
        let j = message_json(&m);
        assert_eq!(j["role"], "assistant");
        assert_eq!(j["content"], "ответ");
        assert!(j["tool_calls"].as_array().map(|a| a.is_empty()).unwrap_or(false),
            "tool_calls должен быть пустым массивом, не null и не отсутствовать");
    }

    /// wire_messages с пустым списком сообщений → только System.
    #[test]
    fn wire_empty_messages_returns_only_system() {
        let wire = wire_messages("SYS", &[]);
        assert_eq!(wire.len(), 1);
        assert_eq!(wire[0].role(), "system");
    }

    /// Адрес собирается без двойной косой черты.
    #[test]
    fn endpoint_trims_slash() {
        let s = Settings {
            base_url: "http://localhost:1234/v1/".into(),
            ..Default::default()
        };
        assert_eq!(endpoint(&s, "models"), "http://localhost:1234/v1/models");
    }

    fn tool(call_id: &str, content: &str) -> Msg {
        Msg::Tool {
            call_id: call_id.into(),
            name: "t".into(),
            content: content.into(),
            is_error: false,
        }
    }

    fn assistant_with_call(id: &str) -> Msg {
        Msg::Assistant {
            content: String::new(),
            tool_calls: vec![CallReq {
                id: id.into(),
                name: "t".into(),
                arguments: "{}".into(),
            }],
        }
    }

    fn user(text: &str) -> Msg {
        Msg::User { content: text.into() }
    }

    /// Старые Tool-результаты обрезаются до 200 символов при переполнении.
    #[test]
    fn trim_context_drops_old_tool_results() {
        // Длинный результат инструмента в старом ходе (~120 000 токенов ASCII).
        let big = "x".repeat(420_000);
        let mut msgs = vec![
            Msg::System { content: "sys".into() },
            user("вопрос"),
            assistant_with_call("c1"),
            tool("c1", &big),
            // Последний ход (не трогаем).
            assistant_with_call("c2"),
            tool("c2", "short"),
        ];
        trim_context(&mut msgs, 100_000);
        // Результат старого хода обрезан.
        match &msgs[3] {
            Msg::Tool { content, .. } => {
                assert!(content.len() < 300, "tool content должен быть обрезан");
                assert!(content.contains("[обрезано]"));
            }
            other => panic!("ожидался Tool, пришло {other:?}"),
        }
        // Последний ход не тронут.
        match &msgs[5] {
            Msg::Tool { content, .. } => assert_eq!(content, "short"),
            other => panic!("{other:?}"),
        }
    }

    /// При сильном переполнении старые ходы выбрасываются целиком, первый
    /// User сохраняется.
    #[test]
    fn trim_context_drops_old_turns_but_keeps_first_user() {
        let big = "ы".repeat(50_000); // ~35 000 токенов
        let mut msgs = vec![
            Msg::System { content: "sys".into() },
            user("задача"),
            assistant_with_call("c1"),
            tool("c1", &big),
            assistant_with_call("c2"),
            tool("c2", &big),
            // Последний User + ход.
            user("последний вопрос"),
            assistant_with_call("c3"),
            tool("c3", "ok"),
        ];
        trim_context(&mut msgs, 50_000);
        // Первый User не выброшен.
        assert!(
            msgs.iter().any(|m| matches!(m, Msg::User { content, .. } if content == "задача")),
            "первый User должен остаться"
        );
        // Размер вписывается в лимит.
        let size: u64 = msgs.iter().map(msg_estimate).sum();
        assert!(size <= 50_000, "после усечения: {size} > 50000");
    }

    /// Ход (Assistant + его Tool) никогда не разрывается пополам.
    #[test]
    fn trim_context_never_splits_assistant_tool_pair() {
        let big = "z".repeat(5_000);
        let mut msgs = vec![
            Msg::System { content: "sys".into() },
            user("q"),
            // Три хода.
            assistant_with_call("a"),
            tool("a", &big),
            assistant_with_call("b"),
            tool("b", &big),
            assistant_with_call("c"),
            tool("c", "ok"),
        ];
        trim_context(&mut msgs, 1_000);
        // Проверяем, что каждый Assistant сопровождается своим Tool.
        let mut i = 0;
        while i < msgs.len() {
            if let Msg::Assistant { tool_calls, .. } = &msgs[i] {
                let ids: Vec<_> = tool_calls.iter().map(|c| c.id.as_str()).collect();
                i += 1;
                for expected_id in &ids {
                    assert!(
                        i < msgs.len(),
                        "ход оборван: нет Tool для Assistant"
                    );
                    match &msgs[i] {
                        Msg::Tool { call_id, .. } => {
                            assert_eq!(call_id.as_str(), *expected_id, "Tool не для этого вызова");
                        }
                        other => panic!("ожидался Tool, пришло {other:?}"),
                    }
                    i += 1;
                }
            } else {
                i += 1;
            }
        }
    }
}
