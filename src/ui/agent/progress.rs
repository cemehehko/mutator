//! Индикатор хода агента (`agr:agent-progress`). Фаза [`Wait`] и её время
//! превращаются в [`Indicator`] чистой функцией [`indicator`]: пока ждём
//! модель — три точки переливаются волной, пока идут данные или работает
//! инструмент — крутится спиннер. Часы читает только вызывающий, поэтому
//! тесты гоняют функцию на заданных длительностях.
//!
//! Окно рисует индикатор кругами ([`show`]), TUI — знаками с теми же
//! яркостями ([`dot_color`]); мигающий курсор потока — [`caret_on`].
//!
//! Наружу: [`Wait`], [`Look`], [`Indicator`], [`indicator`], [`caret_on`],
//! [`dot_color`], [`show`].

use std::time::Duration;

use crate::ui::theme::{ACCENT, DIM, OK};

/// Фаза хода, как её видит вид.
#[derive(Clone, Debug, PartialEq)]
pub enum Wait {
    /// Модель вызвана, данных нет.
    Thinking,
    /// Идут куски ответа.
    Writing,
    /// Работает инструмент с этим именем.
    Tool(String),
}

/// Как рисовать индикатор.
#[derive(Clone, Debug, PartialEq)]
pub enum Look {
    /// Три точки, яркость каждой от 0 до 1.
    Dots([f32; 3]),
    /// Кадр спиннера.
    Spin(char),
}

/// Что показать в строке состояния.
#[derive(Clone, Debug, PartialEq)]
pub struct Indicator {
    pub look: Look,
    pub text: String,
}

/// Сколько тишины после куска ещё считается «пишет».
const STALE: Duration = Duration::from_millis(1500);
/// Период волны точек.
const WAVE_MS: f32 = 1200.0;
/// Кадры спиннера и длительность одного.
const SPIN: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const SPIN_MS: u128 = 80;

/// Индикатор фазы: `elapsed` — с начала фазы, `since_delta` — с последнего
/// куска, `chars` — сколько знаков ответа уже пришло, `last_tool` — итог
/// инструмента, на который модель сейчас отвечает.
pub fn indicator(
    phase: &Wait,
    elapsed: Duration,
    since_delta: Option<Duration>,
    chars: usize,
    last_tool: Option<&str>,
) -> Indicator {
    let mut ind = match phase {
        Wait::Writing if since_delta.is_some_and(|d| d < STALE) => Indicator {
            look: spin(elapsed),
            text: format!("модель пишет · {chars} знаков"),
        },
        // Куски перестали идти: снова ждём, время — с последнего куска.
        Wait::Writing => thinking(since_delta.unwrap_or(elapsed)),
        Wait::Thinking => thinking(elapsed),
        Wait::Tool(name) => Indicator {
            look: spin(elapsed),
            text: format!("инструмент {name}{}", secs(elapsed, 1)),
        },
    };
    // Ждём модель после инструмента: видно, что ей ушло.
    if let (Look::Dots(_), Some(t)) = (&ind.look, last_tool) {
        ind.text = format!("{} · {t}", ind.text);
    }
    ind
}

fn thinking(elapsed: Duration) -> Indicator {
    Indicator {
        look: Look::Dots(dots(elapsed)),
        text: format!("модель думает{}", secs(elapsed, 3)),
    }
}

/// « · N с», если прошло не меньше `from` секунд.
fn secs(elapsed: Duration, from: u64) -> String {
    let s = elapsed.as_secs();
    if s >= from {
        format!(" · {s} с")
    } else {
        String::new()
    }
}

/// Волна по трём точкам: у каждой своя фаза, яркость бежит слева направо.
fn dots(elapsed: Duration) -> [f32; 3] {
    let t = elapsed.as_millis() as f32 / WAVE_MS;
    let mut out = [0.0; 3];
    for (i, b) in out.iter_mut().enumerate() {
        let phase = (t - i as f32 / 4.0) * std::f32::consts::TAU;
        *b = 0.5 + 0.5 * phase.cos();
    }
    out
}

fn spin(elapsed: Duration) -> Look {
    Look::Spin(SPIN[(elapsed.as_millis() / SPIN_MS) as usize % SPIN.len()])
}

/// Курсор потока мигает раз в полсекунды.
pub fn caret_on(elapsed: Duration) -> bool {
    (elapsed.as_millis() / 500).is_multiple_of(2)
}

/// Цвет точки по яркости: от тусклого к акценту.
pub fn dot_color(b: f32) -> egui::Color32 {
    let b = b.clamp(0.0, 1.0);
    let mix = |a: u8, c: u8| (a as f32 + (c as f32 - a as f32) * b).round() as u8;
    egui::Color32::from_rgb(
        mix(DIM.r(), ACCENT.r()),
        mix(DIM.g(), ACCENT.g()),
        mix(DIM.b(), ACCENT.b()),
    )
}

/// Индикатор в окне: точки — кругами, спиннер — свой у egui.
pub fn show(ui: &mut egui::Ui, ind: &Indicator) {
    ui.horizontal(|ui| {
        match &ind.look {
            Look::Dots(b) => {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(36.0, 14.0), egui::Sense::hover());
                for (i, b) in b.iter().enumerate() {
                    let c = rect.left_center() + egui::vec2(6.0 + 12.0 * i as f32, 0.0);
                    ui.painter().circle_filled(c, 3.0 + 1.0 * b, dot_color(*b));
                }
            }
            Look::Spin(_) => {
                ui.spinner();
            }
        }
        ui.label(egui::RichText::new(&ind.text).color(OK));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// `agr:agent-progress`
    /// Пока ждём, точки переливаются: яркости меняются со временем и у
    /// точек разные.
    #[test]
    fn thinking_dots_shimmer() {
        let at = |t| match indicator(&Wait::Thinking, ms(t), None, 0, None).look {
            Look::Dots(b) => b,
            other => panic!("ждали точки, пришло {other:?}"),
        };
        let (a, b) = (at(0), at(300));
        assert_ne!(a, b, "точки двигаются");
        assert!(a[0] != a[1] && a[1] != a[2], "фазы точек сдвинуты: {a:?}");
        assert!(a.iter().chain(&b).all(|x| (0.0..=1.0).contains(x)));
    }

    /// `agr:agent-progress`
    /// Секунды ожидания видны с третьей.
    #[test]
    fn thinking_shows_seconds_after_three() {
        let text = |t| indicator(&Wait::Thinking, ms(t), None, 0, None).text;
        assert_eq!(text(1000), "модель думает");
        assert_eq!(text(8200), "модель думает · 8 с");
    }

    /// `agr:agent-progress`
    /// Пока куски идут — спиннер и счёт знаков; тишина дольше 1,5 с — снова
    /// точки.
    #[test]
    fn writing_spins_while_data_flows_and_falls_back_to_dots() {
        let live = indicator(&Wait::Writing, ms(5000), Some(ms(200)), 42, None);
        assert!(matches!(live.look, Look::Spin(_)));
        assert_eq!(live.text, "модель пишет · 42 знаков");
        let stale = indicator(&Wait::Writing, ms(9000), Some(ms(4000)), 42, None);
        assert!(matches!(stale.look, Look::Dots(_)));
        assert_eq!(stale.text, "модель думает · 4 с");
    }

    /// `agr:agent-progress`
    /// Ждём модель после инструмента — в строке его итог; спиннер инструмента
    /// итог прошлого не тащит.
    #[test]
    fn thinking_after_tool_shows_its_summary() {
        let t = indicator(
            &Wait::Thinking,
            ms(500),
            None,
            0,
            Some("schema_fields → 25 строк"),
        );
        assert_eq!(t.text, "модель думает · schema_fields → 25 строк");
        let tool = Wait::Tool("goto".into());
        let g = indicator(&tool, ms(500), None, 0, Some("schema_fields → 25 строк"));
        assert_eq!(g.text, "инструмент goto");
    }

    /// `agr:agent-progress`
    /// Инструмент — спиннер, имя и секунды.
    #[test]
    fn tool_spins_with_name_and_seconds() {
        let t = Wait::Tool("search_messages".into());
        let a = indicator(&t, ms(12_300), None, 0, None);
        assert_eq!(a.text, "инструмент search_messages · 12 с");
        let (Look::Spin(x), Look::Spin(y)) =
            (a.look, indicator(&t, ms(12_380), None, 0, None).look)
        else {
            panic!("ждали спиннер");
        };
        assert_ne!(x, y, "спиннер крутится");
    }

    /// Курсор мигает, цвет точки — между тусклым и акцентом.
    #[test]
    fn caret_blinks_and_dot_color_spans_dim_to_accent() {
        assert!(caret_on(ms(100)));
        assert!(!caret_on(ms(600)));
        assert_eq!(dot_color(0.0), DIM);
        assert_eq!(dot_color(1.0), ACCENT);
    }
}
