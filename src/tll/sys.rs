//! Сырые объявления C API libtll — ровно то, чем пользуется [`super`].
//!
//! Написаны руками по `tll/channel.h`, `tll/channel/impl.h`, `tll/scheme.h`,
//! `tll/config.h`, `tll/logger.h`: нужно немного, а bindgen потянул бы libclang в
//! каждую сборку. Раскладку структур держат тесты `convert.rs` на настоящих
//! схемах — ошибка в смещении сразу ломает разбор.
//!
//! Функции не слинкованы: libtll грузится при первом [`api`]
//! (`agr:backend-dlopen`, [`crate::io::dynlib`]); обёртки носят имена функций C.
//! Путь к библиотеке можно задать в `MUTATOR_LIBTLL`.
//!
//! Caps канала — [`TLL_CAPS_PROXY`], [`TLL_CAPS_INPUT`], [`TLL_CAPS_OUTPUT`]:
//! по ним линк решает, принимает ли канал сообщения (`agr:link-post`).

#![allow(non_camel_case_types, dead_code)]

use std::os::raw::{c_char, c_int, c_long, c_longlong, c_short, c_uint, c_void};

pub const TLL_STATE_CLOSED: c_int = 0;
pub const TLL_STATE_ACTIVE: c_int = 2;
pub const TLL_STATE_ERROR: c_int = 4;
pub const TLL_STATE_DESTROY: c_int = 5;

pub const TLL_MESSAGE_DATA: c_short = 0;
pub const TLL_MESSAGE_STATE: c_short = 2;

pub const TLL_MESSAGE_MASK_DATA: c_uint = 1;
pub const TLL_MESSAGE_MASK_STATE: c_uint = 1 << 2;

pub const TLL_CAPS_PROXY: c_uint = 0x2;
pub const TLL_CAPS_INPUT: c_uint = 0x4;
pub const TLL_CAPS_OUTPUT: c_uint = 0x8;
pub const TLL_DCAPS_PROCESS: c_uint = 0x10;
pub const TLL_DCAPS_SUSPEND: c_uint = 0x40;

pub const TLL_LOGGER_ERROR: c_int = 4;
pub const TLL_LOGGER_CRITICAL: c_int = 5;

#[repr(C)]
pub struct tll_channel_context_t {
    _p: [u8; 0],
}
#[repr(C)]
pub struct tll_config_t {
    _p: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct tll_msg_t {
    pub type_: c_short,
    pub flags: c_short,
    pub msgid: c_int,
    pub seq: c_longlong,
    pub pad0: *const c_void,
    pub data: *const c_void,
    pub size: usize,
    pub addr: u64,
    pub time: c_longlong,
    pub reserved: c_longlong,
}

#[repr(C)]
pub struct tll_channel_list_t {
    pub channel: *mut tll_channel_t,
    pub next: *mut tll_channel_list_t,
}

#[repr(C)]
pub struct tll_channel_t {
    pub impl_: *const c_void,
    pub data: *mut c_void,
    pub internal: *mut tll_channel_internal_t,
    pub context: *mut tll_channel_context_t,
    pub parent: *mut tll_channel_t,
}

/// Начало `tll_channel_internal_t` — дальше `dcaps` нам ничего не нужно.
#[repr(C)]
pub struct tll_channel_internal_t {
    pub state: c_int,
    pub version: c_int,
    pub self_: *mut tll_channel_t,
    pub name: *const c_char,
    pub caps: c_uint,
    pub dcaps: c_uint,
}

pub type tll_channel_callback_t = unsafe extern "C" fn(
    channel: *const tll_channel_t,
    msg: *const tll_msg_t,
    user: *mut c_void,
) -> c_int;

#[repr(C)]
pub struct tll_scheme_option_t {
    pub next: *mut tll_scheme_option_t,
    pub name: *const c_char,
    pub value: *const c_char,
}

#[repr(C)]
pub struct tll_scheme_enum_value_t {
    pub next: *mut tll_scheme_enum_value_t,
    pub name: *const c_char,
    pub value: c_longlong,
}

#[repr(C)]
pub struct tll_scheme_enum_t {
    pub next: *mut tll_scheme_enum_t,
    pub name: *const c_char,
    pub type_: c_int,
    pub size: usize,
    pub values: *mut tll_scheme_enum_value_t,
    pub options: *mut tll_scheme_option_t,
}

#[repr(C)]
pub struct tll_scheme_union_t {
    pub next: *mut tll_scheme_union_t,
    pub name: *const c_char,
    pub type_ptr: *mut tll_scheme_field_t,
    pub fields: *mut tll_scheme_field_t,
    pub fields_size: usize,
    pub union_size: usize,
    pub options: *mut tll_scheme_option_t,
}

#[repr(C)]
pub struct tll_scheme_bit_field_t {
    pub next: *mut tll_scheme_bit_field_t,
    pub name: *const c_char,
    pub offset: c_uint,
    pub size: c_uint,
}

#[repr(C)]
pub struct tll_scheme_bits_t {
    pub next: *mut tll_scheme_bits_t,
    pub name: *const c_char,
    pub type_: c_int,
    pub size: usize,
    pub values: *mut tll_scheme_bit_field_t,
    pub options: *mut tll_scheme_option_t,
}

/// Безымянный union из `tll_scheme_field_t`: три слова, смысл зависит от типа.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct tll_scheme_field_u {
    pub a: *mut c_void,
    pub b: usize,
    pub c: usize,
}

#[repr(C)]
pub struct tll_scheme_field_t {
    pub next: *mut tll_scheme_field_t,
    pub options: *mut tll_scheme_option_t,
    pub name: *const c_char,
    pub offset: usize,
    pub type_: c_int,
    pub sub_type: c_int,
    pub size: usize,
    pub u: tll_scheme_field_u,
    pub user: *mut c_void,
    pub user_free: *mut c_void,
    pub index: c_int,
    pub reserved: [c_longlong; 4],
}

#[repr(C)]
pub struct tll_scheme_message_t {
    pub next: *mut tll_scheme_message_t,
    pub options: *mut tll_scheme_option_t,
    pub msgid: c_int,
    pub name: *const c_char,
    pub size: usize,
    pub fields: *mut tll_scheme_field_t,
    pub enums: *mut tll_scheme_enum_t,
    pub unions: *mut tll_scheme_union_t,
    pub user: *mut c_void,
    pub user_free: *mut c_void,
    pub bits: *mut tll_scheme_bits_t,
    pub pmap: *mut tll_scheme_field_t,
    pub reserved: [c_longlong; 4],
}

#[repr(C)]
pub struct tll_scheme_t {
    pub internal: *mut c_void,
    pub options: *mut tll_scheme_option_t,
    pub messages: *mut tll_scheme_message_t,
    pub enums: *mut tll_scheme_enum_t,
    pub aliases: *mut tll_scheme_field_t,
    pub unions: *mut tll_scheme_union_t,
    pub user: *mut c_void,
    pub user_free: *mut c_void,
    pub bits: *mut tll_scheme_bits_t,
    pub imports: *mut c_void,
    pub reserved: [c_longlong; 4],
}

/// Реализация журнала libtll (`tll/logger/impl.h`).
#[repr(C)]
pub struct tll_logger_impl_t {
    pub log: Option<
        unsafe extern "C" fn(
            ts: c_longlong,
            category: *const c_char,
            level: c_int,
            data: *const c_char,
            size: usize,
            obj: *mut c_void,
        ) -> c_int,
    >,
    pub log_new: Option<
        unsafe extern "C" fn(imp: *mut tll_logger_impl_t, category: *const c_char) -> *mut c_void,
    >,
    pub log_free: Option<
        unsafe extern "C" fn(
            imp: *mut tll_logger_impl_t,
            category: *const c_char,
            obj: *mut c_void,
        ),
    >,
    pub configure: Option<
        unsafe extern "C" fn(imp: *mut tll_logger_impl_t, cfg: *const tll_config_t) -> c_int,
    >,
    pub release: Option<unsafe extern "C" fn(imp: *mut tll_logger_impl_t)>,
    pub user: *mut c_void,
}

/// Где искать libtll: полный путь в `MUTATOR_LIBTLL`, каталог pkg-config со
/// сборки, Homebrew, системный поиск.
fn load_lib() -> Result<libloading::Library, String> {
    #[cfg(target_os = "macos")]
    const NAMES: &[&str] = &["libtll.0.dylib", "libtll.dylib"];
    #[cfg(not(target_os = "macos"))]
    const NAMES: &[&str] = &["libtll.so.0", "libtll.so"];
    crate::io::dynlib::open("MUTATOR_LIBTLL", option_env!("MUTATOR_TLL_LIBDIR"), NAMES)
}

crate::dynamic_api! {
    loader = load_lib;
    pub fn tll_logger_register(imp: *mut tll_logger_impl_t) -> c_int;
    pub fn tll_logger_impl_get() -> *const tll_logger_impl_t;
    pub fn tll_channel_context_new(defaults: *mut tll_config_t) -> *mut tll_channel_context_t;
    pub fn tll_channel_context_free(ctx: *mut tll_channel_context_t);
    pub fn tll_channel_context_ref(ctx: *mut tll_channel_context_t) -> *mut tll_channel_context_t;
    pub fn tll_channel_module_load_cfg(
        ctx: *mut tll_channel_context_t,
        module: *const c_char,
        symbol: *const c_char,
        cfg: *const tll_config_t,
    ) -> c_int;
    pub fn tll_channel_impl_get(
        ctx: *const tll_channel_context_t,
        name: *const c_char,
    ) -> *const c_void;

    pub fn tll_channel_new(
        ctx: *mut tll_channel_context_t,
        s: *const c_char,
        len: usize,
        master: *mut tll_channel_t,
        impl_: *const c_void,
    ) -> *mut tll_channel_t;
    pub fn tll_channel_new_url(
        ctx: *mut tll_channel_context_t,
        url: *const tll_config_t,
        master: *mut tll_channel_t,
        impl_: *const c_void,
    ) -> *mut tll_channel_t;
    pub fn tll_channel_free(c: *mut tll_channel_t);
    pub fn tll_channel_open(c: *mut tll_channel_t, s: *const c_char, len: usize) -> c_int;
    pub fn tll_channel_close(c: *mut tll_channel_t, force: c_int) -> c_int;
    pub fn tll_channel_process(c: *mut tll_channel_t, flags: c_long, reserved: c_int) -> c_int;
    pub fn tll_channel_state(c: *const tll_channel_t) -> c_int;
    pub fn tll_channel_callback_add(
        c: *mut tll_channel_t,
        cb: tll_channel_callback_t,
        user: *mut c_void,
        mask: c_uint,
    ) -> c_int;
    pub fn tll_channel_config(c: *mut tll_channel_t) -> *mut tll_config_t;
    pub fn tll_channel_children(c: *const tll_channel_t) -> *const tll_channel_list_t;
    pub fn tll_channel_scheme(c: *const tll_channel_t, type_: c_int) -> *const tll_scheme_t;

    pub fn tll_config_get_copy(
        cfg: *const tll_config_t,
        path: *const c_char,
        plen: c_int,
        vlen: *mut c_int,
    ) -> *mut c_char;
    pub fn tll_config_value_free(v: *const c_char);
    pub fn tll_config_unref(cfg: *const tll_config_t);
    pub fn tll_config_new() -> *mut tll_config_t;
    pub fn tll_config_set(
        cfg: *mut tll_config_t,
        path: *const c_char,
        plen: c_int,
        value: *const c_char,
        vlen: c_int,
    ) -> c_int;

    pub fn tll_scheme_load(url: *const c_char, ulen: c_int) -> *mut tll_scheme_t;
    pub fn tll_scheme_unref(s: *const tll_scheme_t);
    /// Схема текстом (`yamls+gz://…`); строку освобождает [`free`].
    pub fn tll_scheme_dump(s: *const tll_scheme_t, format: *const c_char) -> *mut c_char;

    pub fn tll_channel_post(c: *mut tll_channel_t, msg: *const tll_msg_t, flags: c_int) -> c_int;
    /// Схема через кэш контекста: так в него попадают схемы по хешу.
    pub fn tll_channel_context_scheme_load(
        ctx: *mut tll_channel_context_t,
        url: *const c_char,
        len: c_int,
        cache: c_int,
    ) -> *const tll_scheme_t;

    pub fn tll_logger_set(name: *const c_char, len: c_int, level: c_int, subtree: c_int) -> c_int;
}

extern "C" {
    /// libc: освободить строку, выделенную libtll ([`tll_scheme_dump`]).
    pub fn free(p: *mut c_void);
}
