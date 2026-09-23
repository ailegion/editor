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

/// Longest title shown as-is; agents sometimes put a whole file or patch in the title.
const TITLE_CHARS: usize = 160;

pub fn view<'a, M: Clone + 'a>(card: Card<M>) -> Element<'a, M> {
    let lucide = iced::Font::with_name("lucide");
    let shield: char = lucide_icons::Icon::ShieldCheck.into();
    let header = row![
        text(shield).font(lucide).size(16),
        text("Permission request").size(12),
        Space::new().width(Length::Fill),
    ].spacing(8).align_y(iced::Alignment::Center);
    // Keep the title to one short line; an oversized one moves into the scrollable
    // details so it can't push the buttons off the bottom of the panel.
    let (title, details) = split_title(&card.title, card.details);
    let mut body = column![header, text(title).size(14)].spacing(10);
    if !details.is_empty() {
        let height = ((details.lines().count() as f32 + 1.0) * 17.0).clamp(64.0, 160.0);
        let preview = column![
            row![text("REQUEST DETAILS").size(10).style(iced::widget::text::secondary), Space::new().width(Length::Fill),
                crate::icon_control(lucide_icons::Icon::Copy, "Copy request details", Some(card.copy), false)].align_y(iced::Alignment::Center),
            scrollable(text(details).size(12).font(iced::Font::MONOSPACE)
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
        let mut label = row![].spacing(6).align_y(iced::Alignment::Center);
        // Green check / red X so the two decisions read at a glance.
        let glyph: Option<char> = match kind {
            ChoiceKind::Allow => Some(lucide_icons::Icon::Check.into()),
            ChoiceKind::Reject => Some(lucide_icons::Icon::X.into()),
            ChoiceKind::Other => None,
        };
        if let Some(glyph) = glyph {
            label = label.push(text(glyph).font(lucide).size(14).style(move |theme: &iced::Theme| iced::widget::text::Style {
                color: (kind == ChoiceKind::Reject).then(|| theme.extended_palette().danger.base.color),
            }));
        }
        label = label.push(text(choice.label).size(12));
        let control = button(container(label).center_x(Length::Fill)).padding([7, 12])
            .width(Length::Fill).style(move |theme: &iced::Theme, status| {
                let mut style = if kind == ChoiceKind::Allow { button::success(theme, status) }
                    else { crate::flat_button_style(theme, status) };
                if kind != ChoiceKind::Allow {
                    style.border = iced::Border { color: theme.extended_palette().background.strong.color, width: 1.0, radius: 5.0.into() };
                }
                style
            }).on_press(choice.message);
        if kind == ChoiceKind::Other { extra = extra.push(control); }
        else { actions = actions.push(control); }
    }
    // Everything above the buttons scrolls within a bounded height, so the decision
    // stays reachable however much the request carries.
    let body = column![container(scrollable(body)).max_height(320), actions, extra].spacing(10);
    container(body).padding(12).width(Length::Fill).style(|theme: &iced::Theme| {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(palette.background.weak.color.into()),
            border: iced::Border { color: palette.background.strong.color, width: 1.0, radius: 8.0.into() },
            ..Default::default()
        }
    }).into()
}

/// First line of `title`, cut to [`TITLE_CHARS`]; when anything was cut, the full title is
/// prepended to `details` so nothing is hidden.
fn split_title(title: &str, details: String) -> (String, String) {
    let first = title.lines().next().unwrap_or_default();
    let short: String = first.chars().take(TITLE_CHARS).collect();
    if short.len() == title.len() {
        return (short, details);
    }
    let short = format!("{}…", short.trim_end());
    let details = if details.is_empty() { title.to_string() } else { format!("{title}\n\n{details}") };
    (short, details)
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

    #[test]
    fn oversized_titles_move_into_details() {
        assert_eq!(split_title("Write main.py", "x".into()), ("Write main.py".into(), "x".into()));
        let (title, details) = split_title("Create main.py\nprint('hi')\n", String::new());
        assert_eq!(title, "Create main.py…");
        assert_eq!(details, "Create main.py\nprint('hi')\n");
        let long = "a".repeat(TITLE_CHARS + 5);
        let (title, details) = split_title(&long, "args".into());
        assert_eq!(title.chars().count(), TITLE_CHARS + 1);
        assert!(details.starts_with(&long) && details.ends_with("\n\nargs"));
    }
}
