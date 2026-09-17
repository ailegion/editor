use iced::widget::{button, canvas, column, container, row, text, tooltip, Space};
use iced::{Element, Length, Point};

pub struct Info {
    pub provider: String,
    pub model: Option<String>,
    pub used: Option<u64>,
    pub capacity: Option<u64>,
    pub cost: Option<(f64, String)>,
}

fn fraction(used: Option<u64>, capacity: Option<u64>) -> Option<f32> {
    match (used, capacity) {
        (Some(used), Some(max)) if max > 0 => Some((used as f64 / max as f64).clamp(0.0, 1.0) as f32),
        _ => None,
    }
}

pub fn view<'a, M: Clone + 'a>(info: Info, open: bool, toggle: M, dismiss: M) -> Element<'a, M> {
    let ratio = fraction(info.used, info.capacity);
    let percent = ratio.map(|value| format!("{:.0}% context used", value * 100.0))
        .unwrap_or_else(|| "Context usage not reported".into());
    let ring = button(canvas::Canvas::new(Ring(ratio)).width(24).height(24))
        .padding(3).style(crate::flat_button_style).on_press(toggle);
    let trigger = tooltip(ring, container(text(format!("{percent} · Click for details")).size(12)).padding(8).style(container::rounded_box), tooltip::Position::Top);
    let field = |label: &'static str, value: String| row![
        text(label).size(12).style(iced::widget::text::secondary),
        Space::new().width(Length::Fill), text(value).size(12),
    ].spacing(12);
    let tokens = |value: Option<u64>| value.map(|n| format!("{n} tokens")).unwrap_or_else(|| "Not reported".into());
    let details = column![
        row![text("Context & usage").size(14), Space::new().width(Length::Fill),
            crate::icon_control(lucide_icons::Icon::X, "Close usage details", Some(dismiss.clone()), false)].align_y(iced::Alignment::Center),
        text(percent).size(12),
        field("Provider", info.provider),
        field("Model", info.model.unwrap_or_else(|| "Not reported".into())),
        field("Used", tokens(info.used)),
        field("Context limit", tokens(info.capacity.filter(|n| *n > 0))),
        field("Remaining", tokens(info.capacity.filter(|n| *n > 0).zip(info.used).map(|(max, used)| max.saturating_sub(used)))),
        field("Reported cost", info.cost.map(|(amount, currency)| format!("{amount:.2} {currency}")).unwrap_or_else(|| "Not reported".into())),
    ].spacing(10);
    let panel = container(details).padding(14).width(Length::Fill).style(crate::overlay_style);
    iced_aw::DropDown::new(trigger, panel, open).width(300)
        .alignment(iced_aw::drop_down::Alignment::TopEnd).on_dismiss(dismiss).into()
}

struct Ring(Option<f32>);
impl<M> canvas::Program<M> for Ring {
    type State = ();
    fn draw(&self, _: &(), renderer: &iced::Renderer, theme: &iced::Theme, bounds: iced::Rectangle, _: iced::mouse::Cursor) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let center = Point::new(bounds.width / 2.0, bounds.height / 2.0);
        let radius = 8.5;
        let palette = theme.extended_palette();
        frame.stroke(&canvas::Path::circle(center, radius), canvas::Stroke::default().with_width(3.0).with_color(palette.background.strong.color));
        if let Some(fraction) = self.0 {
            if fraction > 0.0 {
                let arc = canvas::Path::new(|path| {
                    let steps = (fraction * 100.0).ceil() as usize;
                    for i in 0..=steps {
                        let angle = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * fraction * i as f32 / steps as f32;
                        let point = Point::new(center.x + radius * angle.cos(), center.y + radius * angle.sin());
                        if i == 0 { path.move_to(point); } else { path.line_to(point); }
                    }
                });
                let color = if fraction >= 0.9 { palette.danger.base.color } else { palette.primary.base.color };
                frame.stroke(&arc, canvas::Stroke::default().with_width(3.0).with_color(color));
            }
        } else {
            frame.fill(&canvas::Path::circle(center, 1.5), palette.background.base.text);
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn usage_ring_handles_unknown_empty_and_overflowing_context() {
        assert_eq!(fraction(Some(79_000), Some(1_000_000)), Some(0.079));
        assert_eq!(fraction(Some(0), Some(100)), Some(0.0));
        assert_eq!(fraction(Some(120), Some(100)), Some(1.0));
        assert_eq!(fraction(Some(20), Some(0)), None);
        assert_eq!(fraction(None, Some(100)), None);
    }
}
