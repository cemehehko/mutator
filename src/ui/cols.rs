//! Раскладка колонок таблицы в знаках — одна на окно и TUI: ширина по видимым
//! строкам, seq и name всегда, поля — с прокрутки [`Desk::hscroll`], что не
//! влезло справа — отброшено.
//!
//! Наружу: [`Col`], [`ColKind`], [`layout`], [`cell`], [`fit`].

use crate::desk::{Desk, RowView};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColKind {
    Seq,
    Name,
    /// Номер в [`Desk::columns`].
    Field(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Col {
    pub kind: ColKind,
    pub title: String,
    /// Ширина в знаках без промежутка.
    pub width: usize,
    /// Прижимать вправо (числа).
    pub right: bool,
}

/// Самая широкая колонка поля: длинное обрезается с `…`.
const MAX_FIELD: usize = 32;
const MAX_NAME: usize = 24;

/// Текст ячейки.
pub fn cell(row: &RowView, kind: ColKind) -> String {
    match kind {
        ColKind::Seq => row.seq.to_string(),
        ColKind::Name => row.name.clone(),
        ColKind::Field(i) => row.cells.get(i).cloned().flatten().unwrap_or_default(),
    }
}

/// Текст в ширину: обрезка с `…` и выравнивание.
pub fn fit(text: &str, width: usize, right: bool) -> String {
    let n = text.chars().count();
    if n > width {
        let mut s: String = text.chars().take(width.saturating_sub(1)).collect();
        s.push('…');
        return s;
    }
    let pad = " ".repeat(width - n);
    if right {
        format!("{pad}{text}")
    } else {
        format!("{text}{pad}")
    }
}

/// Колонки в ширину `avail` знаков (промежуток — 1 знак). Второе — есть ли
/// колонки правее показанных.
pub fn layout(desk: &Desk, avail: usize) -> (Vec<Col>, bool) {
    let vis = desk.rows.iter().skip(desk.top).take(desk.height.max(1));
    let rows: Vec<&RowView> = vis.collect();
    let width = |kind: ColKind, title: &str, max: usize| {
        let w = rows
            .iter()
            .map(|r| cell(r, kind).chars().count())
            .max()
            .unwrap_or(0);
        w.max(title.chars().count()).clamp(1, max)
    };
    let mut cols = vec![
        Col {
            kind: ColKind::Seq,
            title: "seq".into(),
            width: width(ColKind::Seq, "seq", 20),
            right: true,
        },
        Col {
            kind: ColKind::Name,
            title: "name".into(),
            width: width(ColKind::Name, "name", MAX_NAME),
            right: false,
        },
    ];
    let mut used: usize = cols.iter().map(|c| c.width + 1).sum();
    let mut more = false;
    for (i, path) in desk.columns.iter().enumerate().skip(desk.hscroll) {
        let title = path.rsplit('.').next().unwrap_or(path).to_string();
        let w = width(ColKind::Field(i), &title, MAX_FIELD);
        let numeric = rows.iter().all(|r| {
            let t = cell(r, ColKind::Field(i));
            t.is_empty() || t.parse::<f64>().is_ok()
        });
        if used + w + 1 > avail {
            // Хоть часть первой колонки поля — лучше, чем ничего.
            if used + 6 < avail && cols.len() == 2 {
                cols.push(Col {
                    kind: ColKind::Field(i),
                    title,
                    width: avail - used - 1,
                    right: numeric,
                });
            }
            more = true;
            break;
        }
        used += w + 1;
        cols.push(Col {
            kind: ColKind::Field(i),
            title,
            width: w,
            right: numeric,
        });
    }
    (cols, more)
}
