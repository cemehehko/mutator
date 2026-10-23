//! Перенос `tll_scheme_t` в [`Scheme`]: обход связных списков C, вложенные
//! сообщения — по адресу в общий список, перечисления и биты — общие `Arc`.

use std::collections::HashMap;
use std::ffi::CStr;
use std::os::raw::c_char;
use std::sync::Arc;

use super::sys::*;
use crate::scheme::{Bits, Enum, Field, Int, Kind, PtrVer, Scheme, Struct, Sub, TimeRes};

const F_INT8: i32 = 0;
const F_INT16: i32 = 1;
const F_INT32: i32 = 2;
const F_INT64: i32 = 3;
const F_UINT8: i32 = 4;
const F_UINT16: i32 = 5;
const F_UINT32: i32 = 6;
const F_DOUBLE: i32 = 7;
const F_DECIMAL128: i32 = 8;
const F_BYTES: i32 = 9;
const F_MESSAGE: i32 = 10;
const F_ARRAY: i32 = 11;
const F_POINTER: i32 = 12;
const F_UNION: i32 = 13;
const F_UINT64: i32 = 14;

const S_ENUM: i32 = 1;
const S_BYTE_STRING: i32 = 2;
const S_FIXED: i32 = 3;
const S_TIME_POINT: i32 = 4;
const S_DURATION: i32 = 5;
const S_BITS: i32 = 6;

#[derive(Default)]
struct Conv {
    out: Scheme,
    structs: HashMap<usize, usize>,
    enums: HashMap<usize, Arc<Enum>>,
    bits: HashMap<usize, Arc<Bits>>,
    unions: HashMap<usize, Arc<Vec<Field>>>,
}

pub(super) unsafe fn scheme(s: &tll_scheme_t) -> Scheme {
    let mut c = Conv::default();
    c.out.options = options(s.options);
    let mut m = s.messages;
    while !m.is_null() {
        let i = c.structure(m);
        if (*m).msgid != 0 {
            c.out.by_id.insert((*m).msgid, i);
        }
        m = (*m).next;
    }
    c.out
}

unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

unsafe fn options(mut o: *mut tll_scheme_option_t) -> Vec<(String, String)> {
    let mut r = Vec::new();
    while !o.is_null() {
        r.push((cstr((*o).name), cstr((*o).value)));
        o = (*o).next;
    }
    r
}

fn low32(v: usize) -> u32 {
    (v & 0xffff_ffff) as u32
}

impl Conv {
    unsafe fn structure(&mut self, m: *mut tll_scheme_message_t) -> usize {
        if let Some(&i) = self.structs.get(&(m as usize)) {
            return i;
        }
        let i = self.out.structs.len();
        self.structs.insert(m as usize, i);
        let pmap = if (*m).pmap.is_null() {
            None
        } else {
            Some(cstr((*(*m).pmap).name))
        };
        self.out.structs.push(Struct {
            name: cstr((*m).name),
            msgid: (*m).msgid,
            size: (*m).size,
            fields: Vec::new(),
            options: options((*m).options),
            pmap,
        });
        let fields = self.fields((*m).fields);
        self.out.structs[i].fields = fields;
        i
    }

    unsafe fn fields(&mut self, mut f: *mut tll_scheme_field_t) -> Vec<Field> {
        let mut r = Vec::new();
        while !f.is_null() {
            r.push(self.field(f));
            f = (*f).next;
        }
        r
    }

    unsafe fn field(&mut self, f: *mut tll_scheme_field_t) -> Field {
        let f = &*f;
        let kind = match f.type_ {
            F_INT8 => Kind::Int(Int::I8, self.sub(f)),
            F_INT16 => Kind::Int(Int::I16, self.sub(f)),
            F_INT32 => Kind::Int(Int::I32, self.sub(f)),
            F_INT64 => Kind::Int(Int::I64, self.sub(f)),
            F_UINT8 => Kind::Int(Int::U8, self.sub(f)),
            F_UINT16 => Kind::Int(Int::U16, self.sub(f)),
            F_UINT32 => Kind::Int(Int::U32, self.sub(f)),
            F_UINT64 => Kind::Int(Int::U64, self.sub(f)),
            F_DOUBLE => Kind::Double(self.sub(f)),
            F_DECIMAL128 => Kind::Decimal128,
            F_BYTES => Kind::Bytes {
                string: f.sub_type == S_BYTE_STRING,
            },
            F_MESSAGE => Kind::Message(self.structure(f.u.a as *mut tll_scheme_message_t)),
            F_ARRAY => Kind::Array {
                elem: Box::new(self.field(f.u.a as *mut tll_scheme_field_t)),
                count: Box::new(self.field(f.u.b as *mut tll_scheme_field_t)),
                capacity: f.u.c,
            },
            F_POINTER => Kind::Pointer {
                elem: Box::new(self.field(f.u.a as *mut tll_scheme_field_t)),
                ver: match low32(f.u.b) {
                    1 => PtrVer::LegacyShort,
                    2 => PtrVer::LegacyLong,
                    _ => PtrVer::Default,
                },
                string: f.sub_type == S_BYTE_STRING,
            },
            F_UNION => {
                let u = f.u.a as *mut tll_scheme_union_t;
                Kind::Union {
                    tag: Box::new(self.field((*u).type_ptr)),
                    variants: self.union(u),
                }
            }
            other => panic!("неизвестный тип поля tll: {other}"),
        };
        Field {
            name: cstr(f.name),
            offset: f.offset,
            size: f.size,
            kind,
            options: options(f.options),
            index: f.index,
        }
    }

    unsafe fn union(&mut self, u: *mut tll_scheme_union_t) -> Arc<Vec<Field>> {
        if let Some(v) = self.unions.get(&(u as usize)) {
            return v.clone();
        }
        // Варианты union — массив `fields_size` штук, а не связный список.
        let v: Arc<Vec<Field>> = Arc::new(
            (0..(*u).fields_size)
                .map(|i| self.field((*u).fields.add(i)))
                .collect(),
        );
        self.unions.insert(u as usize, v.clone());
        v
    }

    unsafe fn sub(&mut self, f: &tll_scheme_field_t) -> Sub {
        match f.sub_type {
            S_ENUM => Sub::Enum(self.enumeration(f.u.a as *mut tll_scheme_enum_t)),
            S_FIXED => Sub::Fixed(low32(f.u.a as usize)),
            S_TIME_POINT => Sub::TimePoint(res(low32(f.u.a as usize))),
            S_DURATION => Sub::Duration(res(low32(f.u.a as usize))),
            S_BITS => Sub::Bits(self.bitset(f)),
            _ => Sub::None,
        }
    }

    unsafe fn enumeration(&mut self, e: *mut tll_scheme_enum_t) -> Arc<Enum> {
        if let Some(v) = self.enums.get(&(e as usize)) {
            return v.clone();
        }
        let mut values = Vec::new();
        let mut v = (*e).values;
        while !v.is_null() {
            values.push((cstr((*v).name), (*v).value));
            v = (*v).next;
        }
        let r = Arc::new(Enum {
            name: cstr((*e).name),
            values,
            flags: false,
        });
        self.enums.insert(e as usize, r.clone());
        r
    }

    unsafe fn bitset(&mut self, f: &tll_scheme_field_t) -> Arc<Bits> {
        let key = f.u.a as usize;
        if let Some(v) = self.bits.get(&key) {
            return v.clone();
        }
        let tb = f.u.b as *mut tll_scheme_bits_t;
        let name = if tb.is_null() {
            cstr(f.name)
        } else {
            cstr((*tb).name)
        };
        let mut fields = Vec::new();
        let mut b = f.u.a as *mut tll_scheme_bit_field_t;
        while !b.is_null() {
            fields.push((cstr((*b).name), (*b).offset, (*b).size));
            b = (*b).next;
        }
        let r = Arc::new(Bits { name, fields });
        self.bits.insert(key, r.clone());
        r
    }
}

fn res(v: u32) -> TimeRes {
    match v {
        0 => TimeRes::Ns,
        1 => TimeRes::Us,
        2 => TimeRes::Ms,
        3 => TimeRes::S,
        4 => TimeRes::M,
        5 => TimeRes::H,
        _ => TimeRes::D,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tll::sys::{tll_scheme_load, tll_scheme_unref};

    fn load(yaml: &str) -> Scheme {
        let url = format!("yamls://{yaml}");
        unsafe {
            let p = tll_scheme_load(url.as_ptr().cast(), url.len() as i32);
            assert!(!p.is_null(), "схема не загрузилась");
            let s = scheme(&*p);
            tll_scheme_unref(p);
            s
        }
    }

    /// Раскладка структур FFI совпадает с libtll: смещения, размеры и
    /// подтипы полей тестовой схемы — те, что посчитала сама библиотека.
    #[test]
    fn ffi_layout_matches_libtll_offsets() {
        let s = load(
            r#"
- name: Sub
  fields:
    - {name: a, type: int16}
    - {name: b, type: byte8, options.type: string}
- name: Msg
  id: 10
  enums:
    E: {type: int8, enum: {A: 1, B: 2}}
  bits:
    F: {type: uint8, bits: [{name: x, offset: 0}, {name: y, offset: 1, size: 2}]}
  fields:
    - {name: i8, type: int8}
    - {name: e, type: E}
    - {name: f, type: F}
    - {name: px, type: int64, options.type: fixed8}
    - {name: ts, type: int64, options.type: time_point, options.resolution: us}
    - {name: d, type: decimal128}
    - {name: sub, type: Sub}
    - {name: arr, type: 'int32[4]'}
    - {name: s, type: string}
    - {name: l, type: '*Sub', list-options.offset-ptr-type: legacy-short}
    - {name: u, type: union, union: [{name: ui, type: int32}, {name: us, type: string}]}
"#,
        );
        let m = s.message(10).expect("Msg");
        assert_eq!(m.name, "Msg");
        let names: Vec<_> = m
            .fields
            .iter()
            .map(|f| (f.name.as_str(), f.offset))
            .collect();
        assert_eq!(
            names,
            [
                ("i8", 0),
                ("e", 1),
                ("f", 2),
                ("px", 3),
                ("ts", 11),
                ("d", 19),
                ("sub", 35),
                ("arr", 45),
                ("s", 62),
                ("l", 70),
                ("u", 74)
            ]
        );
        let get = |n: &str| m.fields.iter().find(|f| f.name == n).unwrap();
        assert!(
            matches!(&get("e").kind, Kind::Int(Int::I8, Sub::Enum(e)) if e.lookup(2) == Some("B"))
        );
        assert!(
            matches!(&get("f").kind, Kind::Int(Int::U8, Sub::Bits(b)) if b.fields[1] == ("y".into(), 1, 2))
        );
        assert!(matches!(get("px").kind, Kind::Int(Int::I64, Sub::Fixed(8))));
        assert!(matches!(
            get("ts").kind,
            Kind::Int(Int::I64, Sub::TimePoint(TimeRes::Us))
        ));
        assert!(
            matches!(&get("arr").kind, Kind::Array { capacity: 4, count, .. } if count.size == 1)
        );
        assert!(matches!(
            get("s").kind,
            Kind::Pointer {
                string: true,
                ver: PtrVer::Default,
                ..
            }
        ));
        assert!(
            matches!(&get("l").kind, Kind::Pointer { ver: PtrVer::LegacyShort, elem, .. } if elem.size == 10)
        );
        assert!(matches!(&get("u").kind, Kind::Union { variants, .. } if variants.len() == 2));
        let sub = match get("sub").kind {
            Kind::Message(i) => &s.structs[i],
            _ => panic!("sub"),
        };
        assert!(matches!(sub.fields[1].kind, Kind::Bytes { string: true }));
    }
}
