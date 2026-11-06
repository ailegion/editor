//! Scrollbars matching the canvas editor: inset, rounded, neutral overlay thumbs.
use iced::{Color, Element, Theme};
use iced::widget::{scrollable as scroll, Scrollable};

pub fn rail() -> scroll::Scrollbar {
    scroll::Scrollbar::new().width(8).scroller_width(8).margin(2)
}

pub fn scrollable<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Scrollable<'a, M> {
    iced::widget::scrollable(content)
        .direction(scroll::Direction::Vertical(rail()))
        .style(style)
}

pub fn style(theme: &Theme, status: scroll::Status) -> scroll::Style {
    use scroll::Status;
    let (horizontal, vertical) = match status {
        Status::Active { .. } => (false, false),
        Status::Hovered { is_horizontal_scrollbar_hovered, is_vertical_scrollbar_hovered, .. } =>
            (is_horizontal_scrollbar_hovered, is_vertical_scrollbar_hovered),
        Status::Dragged { is_horizontal_scrollbar_dragged, is_vertical_scrollbar_dragged, .. } =>
            (is_horizontal_scrollbar_dragged, is_vertical_scrollbar_dragged),
    };
    let rail = |active| scroll::Rail {
        background: None,
        border: iced::Border::default(),
        scroller: scroll::Scroller {
            background: Color { a: if active { 0.5 } else { 0.25 }, ..theme.palette().text }.into(),
            border: iced::Border::default().rounded(4),
        },
    };
    let mut style = scroll::default(theme, status);
    style.horizontal_rail = rail(horizontal);
    style.vertical_rail = rail(vertical);
    style.gap = None;
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbs_match_editor_opacity_shape_and_axis_feedback() {
        for theme in [Theme::Light, Theme::Dark] {
            for disabled in [false, true] {
                let states = [
                    (scroll::Status::Active { is_horizontal_scrollbar_disabled: disabled, is_vertical_scrollbar_disabled: disabled }, false, false),
                    (scroll::Status::Hovered { is_horizontal_scrollbar_hovered: false, is_vertical_scrollbar_hovered: false, is_horizontal_scrollbar_disabled: disabled, is_vertical_scrollbar_disabled: disabled }, false, false),
                    (scroll::Status::Hovered { is_horizontal_scrollbar_hovered: true, is_vertical_scrollbar_hovered: false, is_horizontal_scrollbar_disabled: disabled, is_vertical_scrollbar_disabled: disabled }, true, false),
                    (scroll::Status::Hovered { is_horizontal_scrollbar_hovered: false, is_vertical_scrollbar_hovered: true, is_horizontal_scrollbar_disabled: disabled, is_vertical_scrollbar_disabled: disabled }, false, true),
                    (scroll::Status::Dragged { is_horizontal_scrollbar_dragged: true, is_vertical_scrollbar_dragged: false, is_horizontal_scrollbar_disabled: disabled, is_vertical_scrollbar_disabled: disabled }, true, false),
                    (scroll::Status::Dragged { is_horizontal_scrollbar_dragged: false, is_vertical_scrollbar_dragged: true, is_horizontal_scrollbar_disabled: disabled, is_vertical_scrollbar_disabled: disabled }, false, true),
                ];
                for (status, horizontal, vertical) in states {
                    let actual = style(&theme, status);
                    assert_eq!(actual.gap, None);
                    assert_eq!(actual.container, scroll::default(&theme, status).container);
                    for (rail, active) in [(actual.horizontal_rail, horizontal), (actual.vertical_rail, vertical)] {
                        assert_eq!(rail.background, None);
                        assert_eq!(rail.border.width, 0.0);
                        assert_eq!(rail.scroller.background, iced::Background::Color(Color { a: if active { 0.5 } else { 0.25 }, ..theme.palette().text }));
                        assert_eq!(rail.scroller.border, iced::Border::default().rounded(4));
                    }
                }
            }
        }
    }
}
