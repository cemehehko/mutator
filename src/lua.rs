//! Фильтр как у `tll-read -f`: выражение видит те же переменные
//! (`seq, name, data, msgid, addr, time`) и ту же семантику, что в консоли
//! (`agr:lua-like-tll-read`). Выбор типов — то же, что `--message`.
//!
//! [`Filter::code`] строит полный скрипт `tll_filter` (типы + выражение) для
//! префикса `lua+` — им пользуется `--dump`, читающий файл как `tll-read`.
//! [`Filter::expr_code`] — только выражение, без отбора типов: его гоняют
//! пост-эвалом через пару `lua+direct` поверх уже прочитанных (и прогоных
//! transform'ом) сообщений (`agr:filter-post-eval`); типы при этом отбираются в
//! Rust по `msgid`.
//!
//! Ещё — [`transform_template`]: шаблон кода для режима модификации сообщений
//! (Normal-режим tll-lua, `tll_on_data` вместо `tll_filter`).
//!
//! Наружу: [`Filter`], [`transform_template`].

/// Условие отбора сообщений.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// Имена типов (`--message`); пусто — все типы.
    pub messages: Vec<String>,
    /// Lua-выражение (`-f`); пусто — без условия.
    pub expr: String,
}

impl Filter {
    pub fn expr(e: impl Into<String>) -> Self {
        Filter {
            messages: Vec::new(),
            expr: e.into(),
        }
    }

    /// Ничего не отсеивает.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty() && self.expr.trim().is_empty()
    }

    /// Код для `lua.code`; `None` — фильтра нет, читать файл напрямую.
    pub fn code(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut code = vec![
            "function tll_on_open(cfg)".to_string(),
            "end".into(),
            "function tll_on_close()".into(),
            "end".into(),
            "function tll_filter(seq, name, data, msgid, addr, time)".into(),
        ];
        if !self.messages.is_empty() {
            let conds: Vec<_> = self
                .messages
                .iter()
                .map(|s| format!("name ~= \"{s}\""))
                .collect();
            code.push(format!(
                "    if {} then return false end",
                conds.join(" and ")
            ));
        }
        if self.expr.trim().is_empty() {
            code.push("    return true".into());
        } else {
            code.push(format!("    return {}", self.expr.trim()));
        }
        code.push("end".into());
        Some(code.join("\n"))
    }

    /// Код только из выражения (`f`/`t`), без отбора типов: его прогоняют
    /// пост-эвалом поверх уже прочитанных сообщений, когда типы отобраны в Rust
    /// по `msgid` (`agr:filter-post-eval`). `None` — выражения нет.
    pub fn expr_code(&self) -> Option<String> {
        if self.expr.trim().is_empty() {
            return None;
        }
        let code = vec![
            "function tll_on_open(cfg)".to_string(),
            "end".into(),
            "function tll_on_close()".into(),
            "end".into(),
            "function tll_filter(seq, name, data, msgid, addr, time)".into(),
            format!("    return {}", self.expr.trim()),
            "end".into(),
        ];
        Some(code.join("\n"))
    }

    /// Пересечение: типы этого фильтра и оба выражения (поиск внутри фильтра).
    pub fn and(&self, other: &Filter) -> Filter {
        let mut messages = self.messages.clone();
        if !other.messages.is_empty() {
            messages = if messages.is_empty() {
                other.messages.clone()
            } else {
                messages
                    .into_iter()
                    .filter(|m| other.messages.contains(m))
                    .collect()
            };
            if messages.is_empty() {
                // Типы не пересеклись: пустой список значил бы «все типы».
                return Filter::expr("false");
            }
        }
        let exprs: Vec<_> = [&self.expr, &other.expr]
            .into_iter()
            .map(|e| e.trim())
            .filter(|e| !e.is_empty())
            .collect();
        let expr = match exprs.as_slice() {
            [] => String::new(),
            [one] => one.to_string(),
            many => many
                .iter()
                .map(|e| format!("({e})"))
                .collect::<Vec<_>>()
                .join(" and "),
        };
        Filter { messages, expr }
    }

    /// Коротко для строки состояния.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.messages.is_empty() {
            parts.push(self.messages.join(","));
        }
        if !self.expr.trim().is_empty() {
            parts.push(self.expr.trim().to_string());
        }
        parts.join(" · ")
    }
}

/// Шаблон кода для режима Transform (Normal-режим tll-lua): `tll_on_data`
/// получает каждое сообщение, может менять поля и отдаёт его наверх в
/// просмотрщик через `tll_callback`. Для нового пользователя — код-заготовка.
///
/// Подписи — как у tll-lua: `tll_on_data(seq, name, data[, msgid, addr, time])`.
/// `tll_callback(seq, name, data)` генерирует сообщение из lua-канала наверх;
/// `tll_child_post` шлёт в дочерний канал (сам файл — только чтение) и потому
/// для чтения не годится.
pub fn transform_template() -> String {
    "\
function tll_on_open(cfg)\n\
end\n\
\n\
function tll_on_close()\n\
end\n\
\n\
function tll_on_data(seq, name, data)\n\
    -- data — объект сообщения: data.header.status = 5\n\
    -- наверх в просмотрщик: tll_callback(seq, name, data)\n\
    tll_callback(seq, name, data)\n\
end\n\
"
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `agr:lua-like-tll-read` — код фильтра тот же, что строит `tll-read
    /// --message A --message B -f EXPR`.
    #[test]
    fn filter_code_is_the_same_as_tll_read_builds() {
        let f = Filter {
            messages: vec!["A".into(), "B".into()],
            expr: "seq > 5".into(),
        };
        assert_eq!(
            f.code().unwrap(),
            "function tll_on_open(cfg)\nend\nfunction tll_on_close()\nend\n\
             function tll_filter(seq, name, data, msgid, addr, time)\n\
             \x20   if name ~= \"A\" and name ~= \"B\" then return false end\n\
             \x20   return seq > 5\nend"
        );
        assert_eq!(Filter::default().code(), None);
        assert!(Filter::expr("x")
            .code()
            .unwrap()
            .contains("    return x\nend"));
    }

    /// Код только из выражения не содержит отбора типов, а поиск внутри
    /// фильтра — пересечение условий.
    #[test]
    fn expr_code_and_search_within_filter() {
        let c = Filter::expr("x").expr_code().unwrap();
        assert!(c.contains(
            "function tll_filter(seq, name, data, msgid, addr, time)\n    return x\nend"
        ));
        assert_eq!(Filter::default().expr_code(), None);
        assert!(Filter::expr("x")
            .expr_code()
            .unwrap()
            .contains("    return x\nend"));
        let f = Filter {
            messages: vec!["A".into(), "B".into()],
            expr: "a".into(),
        };
        let s = Filter {
            messages: vec!["B".into()],
            expr: "b".into(),
        };
        assert_eq!(
            f.and(&s),
            Filter {
                messages: vec!["B".into()],
                expr: "(a) and (b)".into()
            }
        );
        assert_eq!(Filter::default().and(&Filter::expr("b")), Filter::expr("b"));
        let c = Filter {
            messages: vec!["C".into()],
            expr: String::new(),
        };
        assert_eq!(f.and(&c), Filter::expr("false"));
    }

    /// Шаблон transform-кода — заготовка модификации сообщений: `tll_on_data`
    /// и передача наверх в просмотрщик через `tll_callback`.
    #[test]
    fn transform_template_is_a_full_modify_example() {
        let t = transform_template();
        assert!(
            t.contains("function tll_on_data(seq, name, data)"),
            "обработчик сообщения"
        );
        assert!(
            t.contains("tll_callback(seq, name, data)"),
            "передача наверх в просмотрщик"
        );
        assert!(t.contains("function tll_on_open(cfg)"), "открытие");
        assert!(t.contains("function tll_on_close()"), "закрытие");
    }
}
