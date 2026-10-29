//! Форма «Отправить в линк» (`agr:link-post`): тип сообщения, seq и все поля
//! типа строками — те же тексты, что показывает деталь (`agr:edit-encode`).
//!
//! У каждого поля есть значение с самого начала: форма открывается копией
//! выбранного сообщения, а смена типа даёт поля нового типа со значениями по
//! умолчанию — разбором тела из нулей. Правится только нужное. Элементы
//! списков — те, что были в копии: добавить элемент форма не умеет.
//!
//! Форма view-agnostic, как `OpenForm`: клавиши — [`SendForm::key`], рисуют
//! окно, TUI и браузер. Готовое сообщение отдаётся хозяину ([`SendAction`]);
//! шлёт его `Desk` через линк файла.
//!
//! Наружу: [`SendForm`], [`SendField`], [`SendAction`], [`fields`].

use crate::browse::Line;
use crate::desk::Key;
use crate::edit::Body;
use crate::io::Msg;
use crate::scheme::Scheme;

/// Поле формы: путь, тип для подсказки и значение.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendField {
    pub path: String,
    pub ty: String,
    pub value: Line,
}

/// Что сделать с формой.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendAction {
    Send(Msg),
    Cancel,
}

/// Листовые поля сообщения `name` с телом `data`: (путь, тип, текст). Тело
/// `None` — из нулей: значения по умолчанию.
pub fn fields(scheme: &Scheme, name: &str, data: Option<&[u8]>) -> Vec<(String, String, String)> {
    let Some(st) = scheme.messages().find(|m| m.name == name) else {
        return Vec::new();
    };
    let zero;
    let data = match data {
        Some(d) => d,
        None => {
            zero = crate::edit::encode::compose(scheme, name, &Body::new())
                .map(|(_, d)| d)
                .unwrap_or_else(|_| vec![0; st.size]);
            &zero
        }
    };
    let root = crate::decode::decode(scheme, st, data);
    root.walk()
        .into_iter()
        .filter(|(_, n)| n.value.is_some())
        .map(|(_, n)| (n.path.clone(), n.ty.clone(), n.text()))
        .collect()
}

/// Форма отправки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendForm {
    /// Типы сообщений схемы.
    pub types: Vec<String>,
    pub ty: usize,
    pub seq: Line,
    pub fields: Vec<SendField>,
    /// Выбранная строка: 0 — тип, 1 — seq, дальше — поля.
    pub sel: usize,
    /// Первая видимая строка полей.
    pub top: usize,
    /// Отказ кодировщика или канала.
    pub error: Option<String>,
}

impl SendForm {
    /// Форма копией сообщения `name` с телом `data` (`None` — по умолчанию).
    pub fn new(scheme: &Scheme, name: &str, seq: i64, data: Option<&[u8]>) -> SendForm {
        let types: Vec<String> = scheme.messages().map(|m| m.name.clone()).collect();
        let ty = types.iter().position(|t| t == name).unwrap_or(0);
        let mut f = SendForm {
            types,
            ty,
            seq: Line::new(&seq.to_string()),
            fields: Vec::new(),
            sel: 2,
            top: 0,
            error: None,
        };
        f.fill(scheme, data);
        f
    }

    /// Имя выбранного типа.
    pub fn name(&self) -> &str {
        self.types.get(self.ty).map(String::as_str).unwrap_or("")
    }

    fn fill(&mut self, scheme: &Scheme, data: Option<&[u8]>) {
        let name = self.name().to_string();
        self.fields = fields(scheme, &name, data)
            .into_iter()
            .map(|(path, ty, text)| SendField {
                path,
                ty,
                value: Line::new(&text),
            })
            .collect();
        self.sel = self.sel.min(self.fields.len() + 1);
        self.top = 0;
    }

    /// Сменить тип на соседний: поля — по умолчанию.
    fn retype(&mut self, scheme: &Scheme, step: isize) {
        let n = self.types.len();
        if n == 0 {
            return;
        }
        self.ty = (self.ty as isize + step).rem_euclid(n as isize) as usize;
        self.fill(scheme, None);
        self.error = None;
    }

    /// Собрать сообщение из полей.
    pub fn compose(&self, scheme: &Scheme) -> Result<Msg, String> {
        let seq: i64 = self
            .seq
            .text
            .trim()
            .parse()
            .map_err(|_| format!("seq: не число: {}", self.seq.text))?;
        let body: Body = self
            .fields
            .iter()
            .map(|f| (f.path.clone(), f.value.text.clone()))
            .collect();
        crate::io::link::compose(scheme, self.name(), seq, &body)
    }

    /// Строк в форме: тип, seq и поля.
    pub fn rows(&self) -> usize {
        self.fields.len() + 2
    }

    /// Держать выбранное поле в окне из `h` строк полей.
    pub fn scroll_to(&mut self, h: usize) {
        let h = h.max(1);
        let at = self.sel.saturating_sub(2);
        if self.sel >= 2 {
            if at < self.top {
                self.top = at;
            } else if at >= self.top + h {
                self.top = at + 1 - h;
            }
        }
        self.top = self.top.min(self.fields.len().saturating_sub(h));
    }

    /// Клавиша формы; `scheme` — для смены типа и сборки.
    pub fn key(&mut self, k: Key, scheme: &Scheme) -> Option<SendAction> {
        let last = self.rows() - 1;
        match k {
            Key::Esc => return Some(SendAction::Cancel),
            Key::Enter => match self.compose(scheme) {
                Ok(m) => return Some(SendAction::Send(m)),
                Err(e) => self.error = Some(e),
            },
            Key::Up => self.sel = self.sel.saturating_sub(1),
            Key::Down | Key::Tab => self.sel = (self.sel + 1).min(last),
            Key::BackTab => self.sel = self.sel.saturating_sub(1),
            Key::PageUp => self.sel = self.sel.saturating_sub(10),
            Key::PageDown => self.sel = (self.sel + 10).min(last),
            Key::Left if self.sel == 0 => self.retype(scheme, -1),
            Key::Right if self.sel == 0 => self.retype(scheme, 1),
            k => {
                let line = match self.sel {
                    0 => return None,
                    1 => &mut self.seq,
                    i => &mut self.fields[i - 2].value,
                };
                if line.key(k) {
                    self.error = None;
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scheme() -> std::sync::Arc<Scheme> {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/plain.dat");
        crate::data::File::open(std::path::Path::new(p))
            .unwrap()
            .scheme
    }

    fn value<'a>(f: &'a SendForm, path: &str) -> &'a str {
        &f.fields.iter().find(|x| x.path == path).unwrap().value.text
    }

    /// `agr:link-post` — форма — копия выбранного: тот же тип и все поля с его
    /// значениями; Enter собирает то же тело, правка поля его меняет.
    #[test]
    fn send_form_copies_the_selected_message() {
        let s = scheme();
        let body = [("header.order_id".to_string(), "1000".to_string())]
            .into_iter()
            .collect();
        let m = crate::io::link::compose(&s, "Order", 7, &body).unwrap();
        let mut f = SendForm::new(&s, "Order", 7, Some(&m.data));
        assert_eq!(f.name(), "Order");
        assert_eq!(value(&f, "header.order_id"), "1000");
        assert!(f.fields.len() > 5, "все листовые поля: {:?}", f.fields);
        // Тело — то же по значениям (пустая строка может лечь не нулевым
        // указателем, а пустой строкой).
        match f.key(Key::Enter, &s) {
            Some(SendAction::Send(got)) => {
                assert_eq!((got.seq, got.msgid), (m.seq, m.msgid));
                assert_eq!(
                    fields(&s, "Order", Some(&got.data)),
                    fields(&s, "Order", Some(&m.data))
                );
            }
            o => panic!("{o:?}"),
        }
        f.sel = 2 + f
            .fields
            .iter()
            .position(|x| x.path == "header.order_id")
            .unwrap();
        f.key(Key::Ctrl('u'), &s);
        for c in "777".chars() {
            f.key(Key::Char(c), &s);
        }
        let Some(SendAction::Send(got)) = f.key(Key::Enter, &s) else {
            panic!("не собралось: {:?}", f.error)
        };
        let st = s.message(got.msgid).unwrap();
        assert_eq!(
            crate::decode::field(&s, st, &got.data, "header.order_id").as_deref(),
            Some("777")
        );
    }

    /// `agr:link-post` — смена типа даёт все поля нового типа со значениями по
    /// умолчанию; значение, которое не записать, — ошибка в форме, форма
    /// остаётся.
    #[test]
    fn send_form_retypes_with_defaults_and_keeps_errors() {
        let s = scheme();
        let mut f = SendForm::new(&s, "Order", 0, None);
        f.sel = 0;
        f.key(Key::Right, &s);
        assert_ne!(f.name(), "Order");
        assert!(
            f.fields
                .iter()
                .all(|x| !x.value.text.is_empty() || x.ty.contains("string")),
            "у полей есть значения: {:?}",
            f.fields
        );
        let mut f = SendForm::new(&s, "Order", 0, None);
        f.sel = 2 + f
            .fields
            .iter()
            .position(|x| x.path == "header.order_id")
            .unwrap();
        f.key(Key::Ctrl('u'), &s);
        for c in "abc".chars() {
            f.key(Key::Char(c), &s);
        }
        assert_eq!(f.key(Key::Enter, &s), None, "не отправилось");
        assert!(
            f.error.as_deref().unwrap_or("").contains("order_id"),
            "{:?}",
            f.error
        );
        assert_eq!(f.key(Key::Esc, &s), Some(SendAction::Cancel));
    }
}
