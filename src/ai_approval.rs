//! Approval presentation shared by local tools and ACP agents.
use iced::widget::{button, checkbox, column, container, row, scrollable, text, Space};
use iced::{Element, Length};

#[derive(Clone, Copy, PartialEq)]
pub enum ChoiceKind { Allow, Reject, Other }

pub struct Choice<M> {
    pub label: String,
    pub message: M,
    pub kind: ChoiceKind,
}

pub struct Card<M> {
    pub title: String,
    pub details: String,
    pub choices: Vec<Choice<M>>,
    pub remember: Option<(bool, fn(bool) -> M)>,
    pub copy: M,
}

pub fn view<'a, M: Clone + 'a>(card: Card<M>) -> Element<'a, M> {
    let shield: char = lucide_icons::Icon::ShieldCheck.into();
    let header = row![
        text(shield).font(iced::Font::with_name("lucide")).size(16),
        text("Permission request").size(12),
        Space::new().width(Length::Fill),
    ].spacing(8).align_y(iced::Alignment::Center);
    let mut body = column![header, text(card.title).size(14)].spacing(10);
    if !card.details.is_empty() {
        let height = ((card.details.lines().count() as f32 + 1.0) * 17.0).clamp(64.0, 160.0);
        let preview = column![
            row![text("REQUEST DETAILS").size(10).style(iced::widget::text::secondary), Space::new().width(Length::Fill),
                crate::icon_control(lucide_icons::Icon::Copy, "Copy request details", Some(card.copy), false)].align_y(iced::Alignment::Center),
            scrollable(text(card.details).size(12).font(iced::Font::MONOSPACE)
                .wrapping(iced::widget::text::Wrapping::None))
                .direction(scrollable::Direction::Both { vertical: Default::default(), horizontal: Default::default() })
                .height(height).width(Length::Fill),
        ].spacing(4);
        body = body.push(container(preview).padding(8).width(Length::Fill).style(|theme: &iced::Theme| {
            let palette = theme.extended_palette();
            container::Style {
                background: Some(palette.background.base.color.into()),
                border: iced::Border::default().rounded(5), ..Default::default()
            }
        }));
    }
    if let Some((checked, on_toggle)) = card.remember {
        body = body.push(iced::widget::tooltip(
            checkbox(checked).label("Remember for this session").size(14).text_size(12).on_toggle(on_toggle),
            container(text("Allows only identical tool input. Reset session approvals to revoke.").size(12)).padding(8).style(container::rounded_box),
            iced::widget::tooltip::Position::Top,
        ));
    }
    let mut actions = row![].spacing(8);
    let mut extra = column![].spacing(6);
    // Persistent or agent-specific choices remain available without competing
    // visually with the ordinary reject / allow-once decision.
    for choice in card.choices {
        let kind = choice.kind;
        let control = button(text(choice.label).size(12)).padding([7, 12])
            .width(Length::Fill).style(move |theme: &iced::Theme, status| {
                let mut style = if kind == ChoiceKind::Allow { button::primary(theme, status) }
                    else { crate::flat_button_style(theme, status) };
                if kind != ChoiceKind::Allow {
                    style.border = iced::Border { color: theme.extended_palette().background.strong.color, width: 1.0, radius: 5.0.into() };
                }
                style
            }).on_press(choice.message);
        if kind == ChoiceKind::Other { extra = extra.push(control); }
        else { actions = actions.push(control); }
    }
    body = body.push(actions).push(extra);
    container(body).padding(12).width(Length::Fill).style(|theme: &iced::Theme| {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(palette.background.weak.color.into()),
            border: iced::Border { color: palette.background.strong.color, width: 1.0, radius: 8.0.into() },
            ..Default::default()
        }
    }).into()
}

/// Display decoded values rather than a wall of escaped JSON. Preserve all
/// arguments, including unknown fields, so the review does not hide tool input.
pub fn format_input(input: &serde_json::Value) -> String {
    match input {
        serde_json::Value::Object(fields) => fields.iter().map(|(key, value)| {
            let value = value.as_str().map(str::to_owned)
                .unwrap_or_else(|| serde_json::to_string_pretty(value).unwrap_or_default());
            format!("{key}\n{value}")
        }).collect::<Vec<_>>().join("\n\n"),
        serde_json::Value::String(value) => value.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_preview_decodes_strings_and_preserves_every_argument() {
        let input = serde_json::json!({"command": "cat \"a file\"\nGet-ChildItem C:\\project", "timeout": 3000, "extra": {"flag":true}});
        let preview = format_input(&input);
        assert!(preview.contains("cat \"a file\"\nGet-ChildItem C:\\project"));
        assert!(preview.contains("timeout\n3000"));
        assert!(preview.contains("\"flag\": true"));
    }
}
