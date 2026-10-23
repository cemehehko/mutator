//! Схема данных в своих типах Rust — копия `tll_scheme_t` после открытия файла.
//!
//! Схему читает libtll ([`crate::io::Reader::scheme`]); здесь — только модель,
//! которую разбор ([`crate::decode`]) и интерфейс читают без FFI и между потоками.
//! Вложенные сообщения лежат в общем списке [`Scheme::structs`], поле ссылается на
//! них номером [`Kind::Message`]: так модель простая и без циклов владения.
//!
//! Наружу: [`Scheme`], [`Struct`], [`Field`], [`Kind`], [`Int`], [`Sub`], [`Enum`],
//! [`Bits`], [`TimeRes`], [`PtrVer`].

use std::collections::HashMap;
use std::sync::Arc;

/// Вся схема файла.
#[derive(Debug, Default)]
pub struct Scheme {
    /// Все сообщения схемы, включая вложенные без msgid.
    pub structs: Vec<Struct>,
    /// msgid → номер в [`Scheme::structs`].
    pub by_id: HashMap<i32, usize>,
    /// Опции верхнего уровня (`schemaClassName`, `version`…).
    pub options: Vec<(String, String)>,
}

/// Сообщение (или вложенная структура).
#[derive(Debug, Clone)]
pub struct Struct {
    pub name: String,
    /// 0 — нет msgid: структура только вложенная.
    pub msgid: i32,
    pub size: usize,
    pub fields: Vec<Field>,
    pub options: Vec<(String, String)>,
    /// Имя поля карты присутствия (`pmap`), если есть.
    pub pmap: Option<String>,
}

/// Поле сообщения.
#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub offset: usize,
    pub size: usize,
    pub kind: Kind,
    pub options: Vec<(String, String)>,
    /// Номер бита в карте присутствия; −1 — поле есть всегда.
    pub index: i32,
}

/// Целочисленные типы tll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Int {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
}

impl Int {
    pub fn name(self) -> &'static str {
        match self {
            Int::I8 => "int8",
            Int::I16 => "int16",
            Int::I32 => "int32",
            Int::I64 => "int64",
            Int::U8 => "uint8",
            Int::U16 => "uint16",
            Int::U32 => "uint32",
            Int::U64 => "uint64",
        }
    }

    /// Размер в байтах.
    pub fn size(self) -> usize {
        match self {
            Int::I8 | Int::U8 => 1,
            Int::I16 | Int::U16 => 2,
            Int::I32 | Int::U32 => 4,
            Int::I64 | Int::U64 => 8,
        }
    }

    /// Знаковый ли тип.
    pub fn signed(self) -> bool {
        matches!(self, Int::I8 | Int::I16 | Int::I32 | Int::I64)
    }
}

/// Разрешение времени `time_point`/`duration`: множитель к наносекундам.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeRes {
    Ns,
    Us,
    Ms,
    S,
    M,
    H,
    D,
}

impl TimeRes {
    pub fn nanos(self) -> i128 {
        match self {
            TimeRes::Ns => 1,
            TimeRes::Us => 1_000,
            TimeRes::Ms => 1_000_000,
            TimeRes::S => 1_000_000_000,
            TimeRes::M => 60_000_000_000,
            TimeRes::H => 3_600_000_000_000,
            TimeRes::D => 86_400_000_000_000,
        }
    }
    pub fn suffix(self) -> &'static str {
        match self {
            TimeRes::Ns => "ns",
            TimeRes::Us => "us",
            TimeRes::Ms => "ms",
            TimeRes::S => "s",
            TimeRes::M => "m",
            TimeRes::H => "h",
            TimeRes::D => "d",
        }
    }
}

/// Перечисление: имя и значения.
#[derive(Debug)]
pub struct Enum {
    pub name: String,
    pub values: Vec<(String, i64)>,
    /// Значения — биты (опция схемы `flags`): число — их сумма.
    pub flags: bool,
}

impl Enum {
    pub fn lookup(&self, v: i64) -> Option<&str> {
        self.values
            .iter()
            .find(|(_, x)| *x == v)
            .map(|(n, _)| n.as_str())
    }

    /// Имя значения; у флагов — выставленные биты через ` | ` по возрастанию.
    /// `None` — имени нет (или у флагов остались биты без имени).
    pub fn name_of(&self, v: i64) -> Option<String> {
        if let Some(n) = self.lookup(v) {
            return Some(n.to_string());
        }
        if !self.flags || v == 0 {
            return None;
        }
        let mut bits: Vec<_> = self
            .values
            .iter()
            .filter(|(_, x)| *x != 0 && v & x == *x)
            .collect();
        bits.sort_by_key(|(_, x)| *x);
        let covered = bits.iter().fold(0, |a, (_, x)| a | x);
        (covered == v).then(|| {
            bits.iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(" | ")
        })
    }
}

/// Набор битовых полей.
#[derive(Debug)]
pub struct Bits {
    pub name: String,
    /// (имя, смещение в битах, ширина).
    pub fields: Vec<(String, u32, u32)>,
}

/// Подтип числа: как его читать человеку.
#[derive(Debug, Clone)]
pub enum Sub {
    None,
    Enum(Arc<Enum>),
    Fixed(u32),
    TimePoint(TimeRes),
    Duration(TimeRes),
    Bits(Arc<Bits>),
}

/// Версия offset-ptr: раскладка заголовка списка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtrVer {
    /// `u32 offset, u24 size, u8 entity` (entity 0xff — размер элемента в начале данных).
    Default,
    /// `u16 offset, u16 size`, размер элемента — из схемы.
    LegacyShort,
    /// `u32 offset, u16 size, u16 entity`.
    LegacyLong,
}

/// Тип поля.
#[derive(Debug, Clone)]
pub enum Kind {
    Int(Int, Sub),
    Double(Sub),
    Decimal128,
    /// Фиксированные байты; `string` — строка до первого нуля (`byteN` с `type: string`).
    Bytes {
        string: bool,
    },
    /// Вложенное сообщение — номер в [`Scheme::structs`].
    Message(usize),
    /// Массив фиксированной вместимости со счётчиком.
    Array {
        count: Box<Field>,
        elem: Box<Field>,
        capacity: usize,
    },
    /// Список по offset-ptr; `string` — строка (`type: string` у указателя).
    Pointer {
        elem: Box<Field>,
        ver: PtrVer,
        string: bool,
    },
    /// Объединение: поле-тег и варианты по его значению.
    Union {
        tag: Box<Field>,
        variants: Arc<Vec<Field>>,
    },
}

impl Field {
    /// Значение опции поля.
    pub fn option(&self, name: &str) -> Option<&str> {
        self.options
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Короткое описание типа для детали: `int64 fixed8`, `*Leg`, `byte17 string`.
    pub fn type_name(&self, scheme: &Scheme) -> String {
        match &self.kind {
            Kind::Int(i, sub) => format!("{}{}", i.name(), sub_name(sub)),
            Kind::Double(sub) => format!("double{}", sub_name(sub)),
            Kind::Decimal128 => "decimal128".into(),
            Kind::Bytes { string } => {
                format!("byte{}{}", self.size, if *string { " string" } else { "" })
            }
            Kind::Message(m) => scheme.structs[*m].name.clone(),
            Kind::Array { elem, capacity, .. } => {
                format!("{}[{}]", elem.type_name(scheme), capacity)
            }
            Kind::Pointer { string: true, .. } => "string".into(),
            Kind::Pointer { elem, .. } => format!("*{}", elem.type_name(scheme)),
            Kind::Union { .. } => "union".into(),
        }
    }
}

fn sub_name(sub: &Sub) -> String {
    match sub {
        Sub::None => String::new(),
        Sub::Enum(e) => format!(" enum {}", e.name),
        Sub::Fixed(p) => format!(" fixed{p}"),
        Sub::TimePoint(r) => format!(" time_point {}", r.suffix()),
        Sub::Duration(r) => format!(" duration {}", r.suffix()),
        Sub::Bits(b) => format!(" bits {}", b.name),
    }
}

impl Scheme {
    /// Сообщение по msgid.
    pub fn message(&self, msgid: i32) -> Option<&Struct> {
        self.by_id.get(&msgid).map(|&i| &self.structs[i])
    }

    /// Сообщения с msgid в порядке схемы — для выбора типов.
    pub fn messages(&self) -> impl Iterator<Item = &Struct> {
        self.structs.iter().filter(|s| s.msgid != 0)
    }

    /// Опция схемы верхнего уровня.
    pub fn option(&self, name: &str) -> Option<&str> {
        self.options
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Найти поле по пути `header.status` в сообщении: `None`, если такого нет.
    /// Через массивы и списки путь не идёт — колонки показывают скаляры.
    pub fn field_path<'a>(&'a self, msg: &'a Struct, path: &str) -> Option<(usize, &'a Field)> {
        let mut st = msg;
        let mut base = 0;
        let mut parts = path.split('.').peekable();
        while let Some(p) = parts.next() {
            let f = st.fields.iter().find(|f| f.name == p)?;
            if parts.peek().is_none() {
                return Some((base + f.offset, f));
            }
            match f.kind {
                Kind::Message(m) => {
                    base += f.offset;
                    st = &self.structs[m];
                }
                _ => return None,
            }
        }
        None
    }

    /// Все скалярные пути полей сообщения (`header.status`, `price`) — для выбора колонок.
    pub fn scalar_paths(&self, msg: &Struct) -> Vec<String> {
        let mut out = Vec::new();
        self.walk_paths(msg, "", &mut out);
        out
    }

    fn walk_paths(&self, st: &Struct, prefix: &str, out: &mut Vec<String>) {
        for f in &st.fields {
            let path = if prefix.is_empty() {
                f.name.clone()
            } else {
                format!("{prefix}.{}", f.name)
            };
            match f.kind {
                Kind::Message(m) => self.walk_paths(&self.structs[m], &path, out),
                Kind::Array { .. } | Kind::Pointer { string: false, .. } | Kind::Union { .. } => {}
                _ => out.push(path),
            }
        }
    }
}
