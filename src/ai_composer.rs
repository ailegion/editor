use std::path::PathBuf;
use iced::advanced::{layout, mouse, overlay, renderer, widget::{self, Tree}, Clipboard, Layout, Shell, Widget};
use iced::widget::{column, container, row, scrollable, text, Space};
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Theme, Vector};
use crate::ai_context::Attachment;

#[derive(Debug, Clone)]
pub enum Action {
    Attach,
    Reference,
    Remove(usize),
    Drop(Vec<PathBuf>),
}

#[derive(Clone, Copy)]
pub struct Context<'a> {
    pub sources: &'a [PathBuf],
    pub can_reference: bool,
}

pub fn view<'a, M: Clone + 'a>(
    input: Element<'a, M>, attachments: &'a [Attachment], context: Context<'a>,
    on_action: impl Fn(Action) -> M + Copy + 'a,
) -> Element<'a, M> {
    let mut chips = column![].spacing(4);
    for (index, attachment) in attachments.iter().enumerate() {
        let name = std::path::Path::new(&attachment.name).file_name()
            .unwrap_or_default().to_string_lossy().into_owned();
        let chip = row![
            text(if attachment.mime.is_some() { "Image" } else { "File" }).size(10),
            text(name).size(12).width(Length::Fill),
            crate::icon_control(lucide_icons::Icon::X, "Remove attachment", Some(on_action(Action::Remove(index))), false),
        ].spacing(6).align_y(iced::Alignment::Center);
        chips = chips.push(iced::widget::tooltip(
            container(chip).padding([2, 6]).style(container::rounded_box),
            text(&attachment.name).size(12), iced::widget::tooltip::Position::Top,
        ));
    }
    let mut content = column![].spacing(6);
    if !attachments.is_empty() {
        content = content.push(container(scrollable(chips)).max_height(110));
    }
    content = content.push(input).push(row![
        crate::icon_control(lucide_icons::Icon::Paperclip, "Attach image or file", Some(on_action(Action::Attach)), false),
        crate::icon_control(lucide_icons::Icon::Code, "Reference selection or active file", context.can_reference.then(|| on_action(Action::Reference)), false),
        text(if context.sources.is_empty() { "Drop files or images here" } else { "Drop to add to this message" }).size(11).style(iced::widget::text::secondary),
        Space::new().width(Length::Fill),
    ].spacing(4).align_y(iced::Alignment::Center));
    Element::new(DropTarget {
        content: container(content).padding(6).width(Length::Fill).into(),
        sources: context.sources,
        on_drop: Box::new(move |paths| on_action(Action::Drop(paths))),
    })
}

/// Intercept drops before the text editor consumes mouse releases. All normal
/// typing, selection, scrolling, focus and overlay behavior passes through.
struct DropTarget<'a, M> {
    content: Element<'a, M>,
    sources: &'a [PathBuf],
    on_drop: Box<dyn Fn(Vec<PathBuf>) -> M + 'a>,
}

#[derive(Default)]
struct State { external_drag: bool }

fn dropped_paths(event: &Event, over: bool, sources: &[PathBuf]) -> Option<Vec<PathBuf>> {
    if !over { return None; }
    match event {
        Event::Window(iced::window::Event::FileDropped(path)) => Some(vec![path.clone()]),
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if !sources.is_empty() => Some(sources.to_vec()),
        _ => None,
    }
}

impl<M> Widget<M, Theme, Renderer> for DropTarget<'_, M> {
    fn tag(&self) -> widget::tree::Tag { widget::tree::Tag::of::<State>() }
    fn state(&self) -> widget::tree::State { widget::tree::State::new(State::default()) }
    fn children(&self) -> Vec<Tree> { vec![Tree::new(&self.content)] }
    fn diff(&self, tree: &mut Tree) { tree.diff_children(std::slice::from_ref(&self.content)); }
    fn size(&self) -> Size<Length> { self.content.as_widget().size() }
    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        self.content.as_widget_mut().layout(&mut tree.children[0], renderer, limits)
    }
    fn operate(&mut self, tree: &mut Tree, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn widget::Operation) {
        self.content.as_widget_mut().operate(&mut tree.children[0], layout, renderer, operation);
    }
    fn update(&mut self, tree: &mut Tree, event: &Event, layout: Layout<'_>, cursor: mouse::Cursor, renderer: &Renderer, clipboard: &mut dyn Clipboard, shell: &mut Shell<'_, M>, viewport: &Rectangle) {
        let state = tree.state.downcast_mut::<State>();
        match event {
            Event::Window(iced::window::Event::FileHovered(_)) => { state.external_drag = true; shell.request_redraw(); }
            Event::Window(iced::window::Event::FilesHoveredLeft | iced::window::Event::FileDropped(_)) => { state.external_drag = false; shell.request_redraw(); }
            _ => {}
        }
        if let Some(paths) = dropped_paths(event, cursor.is_over(layout.bounds()) && cursor.is_over(*viewport), self.sources) {
            shell.publish((self.on_drop)(paths));
            shell.capture_event();
            return;
        }
        self.content.as_widget_mut().update(&mut tree.children[0], event, layout, cursor, renderer, clipboard, shell, viewport);
    }
    fn mouse_interaction(&self, tree: &Tree, layout: Layout<'_>, cursor: mouse::Cursor, viewport: &Rectangle, renderer: &Renderer) -> mouse::Interaction {
        if !self.sources.is_empty() && cursor.is_over(layout.bounds()) { mouse::Interaction::Copy }
        else { self.content.as_widget().mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer) }
    }
    fn draw(&self, tree: &Tree, renderer: &mut Renderer, theme: &Theme, style: &renderer::Style, layout: Layout<'_>, cursor: mouse::Cursor, viewport: &Rectangle) {
        self.content.as_widget().draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport);
        if (tree.state.downcast_ref::<State>().external_drag || !self.sources.is_empty()) && cursor.is_over(layout.bounds()) {
            use iced::advanced::Renderer as _;
            renderer.fill_quad(renderer::Quad {
                bounds: layout.bounds(),
                border: iced::Border { color: theme.extended_palette().primary.base.color, width: 2.0, radius: 6.0.into() },
                ..Default::default()
            }, iced::Color::TRANSPARENT);
        }
    }
    fn overlay<'b>(&'b mut self, tree: &'b mut Tree, layout: Layout<'b>, renderer: &Renderer, viewport: &Rectangle, translation: Vector) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_drops_inside_the_composer_attach_files() {
        let sources = vec![PathBuf::from("src/main.rs"), PathBuf::from("image.png")];
        let release = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        assert_eq!(dropped_paths(&release, true, &sources), Some(sources.clone()));
        assert_eq!(dropped_paths(&release, false, &sources), None);
        assert_eq!(dropped_paths(&release, true, &[]), None);
        let external = Event::Window(iced::window::Event::FileDropped(sources[1].clone()));
        assert_eq!(dropped_paths(&external, true, &[]), Some(vec![sources[1].clone()]));
        assert_eq!(dropped_paths(&external, false, &[]), None);
        assert_eq!(dropped_paths(&Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)), true, &sources), None);
    }
}
