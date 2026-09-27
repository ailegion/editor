//! The conversation history menu shared by the AI panels: most recently active first,
//! searchable, with rename and delete, and "Show all" past the first [`RECENT`].
use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::{Element, Length};

pub const RECENT: usize = 10;

#[derive(Default)]
pub struct Menu {
    pub open: bool,
    query: String,
    show_all: bool,
    /// The conversation being renamed and the name typed so far.
    renaming: Option<(usize, String)>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Toggle,
    Query(String),
    ShowAll,
    Switch(usize),
    Delete(usize),
    /// Start renaming conversation `usize`, whose current title is given.
    Rename(usize, String),
    RenameInput(String),
    RenameDone,
    RenameCancel,
}

/// A change the owning panel applies to its conversations.
#[derive(Debug, PartialEq)]
pub enum Action {
    Switch(usize),
    Delete(usize),
    Rename(usize, String),
}

/// One conversation as the menu lists it.
pub struct Item {
    pub index: usize,
    pub title: String,
    /// Unix seconds of the last message; 0 when unknown (conversations saved before this).
    pub updated: u64,
    /// Message text searched in addition to the title.
    pub text: String,
}

impl Menu {
    pub fn update(&mut self, message: Message) -> Option<Action> {
        match message {
            Message::Toggle => {
                self.open = !self.open;
                self.query.clear();
                self.show_all = false;
                self.renaming = None;
            }
            Message::Query(query) => self.query = query,
            Message::ShowAll => self.show_all = true,
            Message::Switch(index) => {
                self.open = false;
                self.renaming = None;
                return Some(Action::Switch(index));
            }
            Message::Delete(index) => {
                // Indices shift after a delete, so a rename in progress would hit another row.
                self.renaming = None;
                return Some(Action::Delete(index));
            }
            Message::Rename(index, title) => self.renaming = Some((index, title)),
            Message::RenameInput(name) => {
                if let Some((_, current)) = &mut self.renaming { *current = name; }
            }
            Message::RenameDone => {
                let (index, name) = self.renaming.take()?;
                let name = name.trim();
                return (!name.is_empty()).then(|| Action::Rename(index, name.to_string()));
            }
            Message::RenameCancel => self.renaming = None,
        }
        None
    }
}

/// Newest activity first (ties: newest created first), matching `query` in the title or text.
/// Returns what to list and how many more matches `show_all` would reveal.
fn visible(mut items: Vec<Item>, query: &str, show_all: bool) -> (Vec<Item>, usize) {
    let query = query.trim().to_lowercase();
    if !query.is_empty() {
        items.retain(|item| item.title.to_lowercase().contains(&query) || item.text.to_lowercase().contains(&query));
    }
    items.sort_by(|a, b| (b.updated, b.index).cmp(&(a.updated, a.index)));
    let hidden = if show_all { 0 } else { items.len().saturating_sub(RECENT) };
    items.truncate(items.len() - hidden);
    (items, hidden)
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs())
}

/// Short relative age such as "5m" or "3d"; empty when `then` is unknown.
pub fn ago(now: u64, then: u64) -> String {
    if then == 0 { return String::new(); }
    let seconds = now.saturating_sub(then);
    match seconds {
        0..60 => "now".into(),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        86_400..604_800 => format!("{}d", seconds / 86_400),
        _ => format!("{}w", seconds / 604_800),
    }
}

/// Untitled conversations are listed by this name.
pub fn title_or_default(title: &str) -> String {
    if title.trim().is_empty() { "New conversation".into() } else { title.to_string() }
}

pub fn view<'a>(menu: &'a Menu, items: Vec<Item>, active: usize, enabled: bool) -> Element<'a, Message> {
    let now = now();
    let total = items.len();
    let (items, hidden) = visible(items, &menu.query, menu.show_all);
    let shown = items.len();
    let mut list = column![].spacing(2);
    for item in items {
        let index = item.index;
        if let Some((_, name)) = menu.renaming.as_ref().filter(|(renaming, _)| *renaming == index) {
            list = list.push(row![
                text_input("Conversation name", name).size(13).padding([3, 6])
                    .on_input(Message::RenameInput).on_submit(Message::RenameDone),
                crate::icon_control(lucide_icons::Icon::Check, "Save name", Some(Message::RenameDone), false),
                crate::icon_control(lucide_icons::Icon::X, "Cancel rename", Some(Message::RenameCancel), false),
            ].spacing(4).align_y(iced::Alignment::Center));
            continue;
        }
        let title = title_or_default(&item.title);
        let label = if index == active { format!("✓ {title}") } else { title.clone() };
        list = list.push(row![
            button(row![
                text(label).size(13).width(Length::Fill),
                text(ago(now, item.updated)).size(11).style(text::secondary),
            ].spacing(6).align_y(iced::Alignment::Center))
                .width(Length::Fill).padding([4, 8]).style(crate::flat_button_style)
                .on_press_maybe(enabled.then_some(Message::Switch(index))),
            crate::icon_control(lucide_icons::Icon::Pencil, "Rename conversation", Some(Message::Rename(index, item.title)), false),
            crate::icon_control(lucide_icons::Icon::Trash2, "Delete conversation", enabled.then_some(Message::Delete(index)), false),
        ].spacing(2).align_y(iced::Alignment::Center));
    }
    if total > 0 && shown == 0 {
        list = list.push(container(text("No conversations match").size(12).style(text::secondary)).padding([4, 8]));
    }
    if hidden > 0 {
        list = list.push(button(text(format!("Show all ({hidden} more)")).size(12)).padding([4, 8])
            .style(crate::flat_button_style).on_press(Message::ShowAll));
    }
    let search = text_input("Search conversations", &menu.query).size(13).padding([4, 8]).on_input(Message::Query);
    container(column![search, scrollable(list).height(Length::Shrink)].spacing(6)).padding(4).max_height(360)
        .style(|theme: &iced::Theme| iced::widget::container::Style {
            background: Some(theme.extended_palette().background.weak.color.into()),
            border: iced::Border::default().rounded(6.0),
            ..Default::default()
        })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(index: usize, title: &str, updated: u64) -> Item {
        Item { index, title: title.into(), updated, text: format!("body of {title}") }
    }

    #[test]
    fn lists_recent_activity_first_and_hides_the_rest_until_asked() {
        let items: Vec<_> = (0..15).map(|i| item(i, &format!("t{i}"), 0)).collect();
        let (shown, hidden) = visible(items, "", false);
        assert_eq!(shown.len(), RECENT);
        assert_eq!(hidden, 5);
        assert_eq!((shown[0].index, shown[9].index), (14, 5), "undated: newest created first");

        let items = vec![item(0, "old but active", 500), item(1, "newer", 100), item(2, "undated", 0)];
        let order: Vec<_> = visible(items, "", false).0.into_iter().map(|item| item.index).collect();
        assert_eq!(order, [0, 1, 2]);

        let items: Vec<_> = (0..15).map(|i| item(i, &format!("t{i}"), 0)).collect();
        let (shown, hidden) = visible(items, "", true);
        assert_eq!((shown.len(), hidden), (15, 0));
    }

    #[test]
    fn search_matches_titles_and_message_text() {
        let items = || vec![item(0, "Fix login bug", 1), item(1, "Refactor", 2)];
        let titles = |query| visible(items(), query, false).0.into_iter().map(|item| item.title).collect::<Vec<_>>();
        assert_eq!(titles("LOGIN"), ["Fix login bug"]);
        assert_eq!(titles("body of refactor"), ["Refactor"]);
        assert!(titles("nothing").is_empty());
    }

    #[test]
    fn menu_reports_actions_and_validates_names() {
        let mut menu = Menu::default();
        assert_eq!(menu.update(Message::Toggle), None);
        assert!(menu.open);
        assert_eq!(menu.update(Message::Rename(2, "Old".into())), None);
        menu.update(Message::RenameInput("  New name ".into()));
        assert_eq!(menu.update(Message::RenameDone), Some(Action::Rename(2, "New name".into())));
        menu.update(Message::Rename(2, "Old".into()));
        menu.update(Message::RenameInput("   ".into()));
        assert_eq!(menu.update(Message::RenameDone), None, "blank names are ignored");
        menu.update(Message::Rename(1, "x".into()));
        assert_eq!(menu.update(Message::Delete(0)), Some(Action::Delete(0)));
        assert_eq!(menu.update(Message::RenameDone), None, "deleting cancels a rename");
        assert_eq!(menu.update(Message::Switch(3)), Some(Action::Switch(3)));
        assert!(!menu.open);
    }

    #[test]
    fn ages_are_short_and_unknown_is_blank() {
        assert_eq!(ago(1_000, 0), "");
        assert_eq!(ago(1_000, 990), "now");
        assert_eq!(ago(10_000, 10_000 - 300), "5m");
        assert_eq!(ago(100_000, 100_000 - 7_200), "2h");
        assert_eq!(ago(1_000_000, 1_000_000 - 259_200), "3d");
        assert_eq!(ago(10_000_000, 10_000_000 - 1_209_600), "2w");
        assert_eq!(ago(5, 10), "now", "clock skew never panics");
    }
}
