//! Lua-обработчик без чтения: тела прогоняются через пару каналов
//! `lua+direct` + direct-slave постом, синхронно и без второй читки файла
//! (`agr:transform-shadow`, `agr:filter-post-eval`).
//!
//! `direct://` — синхронный канал: пост в slave зовёт колбэки противоположной
//! стороны прямо внутри `tll_channel_post`, без `process` (он у direct вообще
//! запрещён). Дочерний direct-master получает сообщение, lua-канал его
//! обрабатывает, а `tll_callback` из Lua доставляет результат в колбэк на самом
//! lua-канале — всё в том же стеке. Выброшенное сообщение — `None` из
//! [`Transformer::transform`]; несколько `tll_callback` на одно сообщение —
//! берём первое. Канал в Error (битый код, имя вне схемы — tll-lua по умолчанию
//! fragile) — ошибка каждого следующего прогона: текст из журнала libtll.
//!
//! Два режима, различаются только `lua.preset` (управляет настройками
//! [`tll::lua::Settings`] кодека, см. `tll-lua/src/tll/lua/base.h`):
//!
//! - [`Transformer::new`] — transform (`tll_on_data`), без пресета: fixed и
//!   decimal128 едут объектами, чтобы мутатор мог менять поля;
//! - [`Transformer::filter`] — фильтр (`tll_filter`), `lua.preset=filter`, как у
//!   `tll-read -f`: fixed/decimal128 сравниваются как числа (float), enum — как
//!   строки. Выражение `f`/`t` видит те же значения, что в консоли
//!   (`agr:lua-like-tll-read`).
//!
//! Режим выбирается самим tll-lua по наличию `tll_filter` или `tll_on_data` в
//! коде (см. `tll-lua/src/prefix.cc`); пресет — только кодек.
//!
//! Наружу: [`Transformer`].

use super::*;

/// Пара каналов для прогона тел через Lua-transform.
pub struct Transformer {
    lua: *mut tll_channel_t,
    slave: *mut tll_channel_t,
    pump: Box<Pump>,
}

unsafe impl Send for Transformer {}

impl Transformer {
    /// Transform-пара: `tll_on_data`, без `lua.preset` — fixed/decimal128 едут
    /// объектами. Контекст живёт у вызывающего потока (как у [`Reader`]).
    pub fn new(ctx: &Context, code: &str, scheme_url: &str) -> Result<Transformer, String> {
        Self::build(ctx, code, scheme_url, None)
    }

    /// Фильтр-пара: `tll_filter`, `lua.preset=filter` — выражение видит те же
    /// значения (fixed/decimal128 как float, enum как строки), что `tll-read -f`
    /// (`agr:lua-like-tll-read`).
    pub fn filter(ctx: &Context, code: &str, scheme_url: &str) -> Result<Transformer, String> {
        Self::build(ctx, code, scheme_url, Some("filter"))
    }

    /// Собрать и открыть пару: `lua+direct` (`lua.code`, схема для имён) и
    /// direct-slave `master=<lua-канал>`.
    fn build(
        ctx: &Context,
        code: &str,
        scheme_url: &str,
        preset: Option<&str>,
    ) -> Result<Transformer, String> {
        if !ctx.ensure_lua() {
            return Err("Lua недоступен: нет модуля tll-lua (MUTATOR_LUA)".into());
        }
        log::install();
        log::take();
        let name = format!("tp{}", next_name());
        let fail = |what: &str| {
            log::take()
                .map(|e| log::explain(&e))
                .unwrap_or_else(|| format!("{what} (lua+direct)"))
        };
        let _g = lifecycle();
        let mut kv: Vec<(&'static str, String)> = vec![
            ("name", name.clone()),
            ("dump", "no".into()),
            ("lua.code", code.to_string()),
            ("scheme", scheme_url.to_string()),
            ("tll.proto", "lua+direct".into()),
        ];
        if let Some(p) = preset {
            kv.push(("lua.preset", p.to_string()));
        }
        let lua = unsafe { new_channel(ctx, &kv) };
        if lua.is_null() {
            return Err(fail("не создался канал"));
        }
        let slave = unsafe {
            new_channel(
                ctx,
                &[
                    ("name", format!("{name}-in")),
                    ("dump", "no".into()),
                    ("master", name),
                    ("tll.proto", "direct".into()),
                ],
            )
        };
        if slave.is_null() {
            let e = fail("не создался direct-slave");
            unsafe { tll_channel_free(lua) };
            return Err(e);
        }
        let mut t = Transformer {
            lua,
            slave,
            pump: Box::default(),
        };
        let user = (&mut *t.pump) as *mut Pump as *mut c_void;
        unsafe {
            tll_channel_callback_add(
                lua,
                on_msg,
                user,
                TLL_MESSAGE_MASK_DATA | TLL_MESSAGE_MASK_STATE,
            );
            let rc = tll_channel_open(lua, std::ptr::null(), 0);
            let active = rc == 0 && tll_channel_state(lua) == TLL_STATE_ACTIVE;
            if active {
                let rc = tll_channel_open(slave, std::ptr::null(), 0);
                t.pump.failed = rc != 0 || tll_channel_state(slave) == TLL_STATE_ERROR;
            } else {
                t.pump.failed = true;
            }
        }
        if t.pump.failed {
            let e = fail("не открылся канал transform");
            // Замок снимается до `t`: его `Drop` берёт тот же замок, и ранний
            // возврат с ошибкой иначе заблокировал бы поток сам на себе.
            drop(_g);
            drop(t);
            return Err(e);
        }
        Ok(t)
    }

    /// Прогнать тело через обработчик: результат `tll_callback` из Lua, `None` —
    /// сообщение выброшено (в transform — без `tll_callback`, в filter —
    /// `tll_filter` вернул false). Ошибка канала (в том числе sticky после
    /// Error) — `Err`.
    pub fn transform(&mut self, m: &Msg) -> Result<Option<Msg>, String> {
        if self.pump.failed {
            return Err(self.error_text());
        }
        self.pump.queue.clear();
        let um = tll_msg_t {
            type_: TLL_MESSAGE_DATA,
            flags: 0,
            msgid: m.msgid,
            seq: m.seq,
            pad0: std::ptr::null(),
            data: m.data.as_ptr() as *const c_void,
            size: m.data.len(),
            addr: 0,
            time: 0,
            reserved: 0,
        };
        let rc = unsafe { tll_channel_post(self.slave, &um, 0) };
        if rc != 0 {
            self.pump.failed = true;
            return Err(self.error_text());
        }
        if self.pump.failed {
            return Err(self.error_text());
        }
        Ok(self.pump.queue.pop_front())
    }

    fn error_text(&self) -> String {
        log::take()
            .map(|e| log::explain(&e))
            .unwrap_or_else(|| "канал transform оборвался ошибкой".into())
    }
}

impl Drop for Transformer {
    fn drop(&mut self) {
        let _g = lifecycle();
        unsafe {
            // Сначала slave: он держит указатель на общий master.
            tll_channel_free(self.slave);
            tll_channel_free(self.lua);
        }
    }
}
