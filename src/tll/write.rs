//! Запись датника: `file://` с `dir=w` — та же схема, размер блока, сжатие и
//! версия, что у исходника. Нужен фиксации правок: исходник читается с
//! наложенными правками, каждое сообщение уходит сюда.
//!
//! Наружу: [`Writer`].

use std::os::raw::c_int;
use std::path::Path;

use super::sys::*;
use super::{lifecycle, log, new_channel, next_name, Context, Info, Msg};

/// Открытый канал записи файла.
pub struct Writer {
    ctx: *mut tll_channel_context_t,
    ch: *mut tll_channel_t,
}

unsafe impl Send for Writer {}

impl Writer {
    /// Открыть файл `path` на запись со схемой `scheme` (url,
    /// [`super::Reader::scheme_url`]) и параметрами исходника `like`. `overwrite`
    /// (`true`) — писать заново, существующий заменяется; `false` — дописывать к
    /// хвосту (так растущий файл в тесте `agr:reload` растёт после открытия).
    pub fn open(
        ctx: &Context,
        path: &Path,
        scheme: &str,
        like: &Info,
        overwrite: bool,
    ) -> Result<Writer, String> {
        log::install();
        log::take();
        let p = path.to_str().ok_or("путь не в UTF-8")?;
        let mut kv: Vec<(&str, String)> = vec![
            ("tll.proto", "file".into()),
            ("tll.host", p.into()),
            ("name", format!("tw{}", next_name())),
            ("dump", "no".into()),
            ("dir", "w".into()),
            ("scheme", scheme.into()),
        ];
        for (k, v) in [
            ("block", &like.block),
            ("compression", &like.compression),
            ("version", &like.version),
        ] {
            if let Some(v) = v {
                kv.push((k, v.clone()));
            }
        }
        let g = lifecycle();
        let ch = unsafe { new_channel(ctx, &kv) };
        if ch.is_null() {
            drop(g);
            return Err(log::take().unwrap_or_else(|| format!("не создался канал записи {p}")));
        }
        let w = Writer {
            ctx: unsafe { tll_channel_context_ref(ctx.ptr) },
            ch,
        };
        let params = if overwrite { "overwrite=yes" } else { "" };
        let rc = unsafe { tll_channel_open(ch, params.as_ptr().cast(), params.len()) };
        let state = unsafe { tll_channel_state(ch) };
        drop(g);
        if rc != 0 || state != TLL_STATE_ACTIVE {
            return Err(log::take().unwrap_or_else(|| format!("не открылся на запись {p}")));
        }
        Ok(w)
    }

    /// Дописать сообщение.
    pub fn post(&mut self, m: &Msg) -> Result<(), String> {
        let msg = tll_msg_t {
            type_: TLL_MESSAGE_DATA,
            flags: 0,
            msgid: m.msgid,
            seq: m.seq,
            pad0: std::ptr::null(),
            data: m.data.as_ptr().cast(),
            size: m.data.len(),
            addr: 0,
            time: 0,
            reserved: 0,
        };
        let rc = unsafe { tll_channel_post(self.ch, &msg, 0 as c_int) };
        if rc != 0 {
            return Err(log::take().unwrap_or_else(|| format!("запись seq {}: код {rc}", m.seq)));
        }
        Ok(())
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        let _g = lifecycle();
        unsafe {
            tll_channel_close(self.ch, 0);
            tll_channel_free(self.ch);
            tll_channel_context_free(self.ctx);
        }
    }
}
