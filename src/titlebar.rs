//! The title bar on Windows, drawn by the app like Zed's: the menu shares the row with the
//! minimize, maximize and close buttons, in the theme's colors. Windows still manages the
//! window: a hook in front of winit's window procedure removes the system title bar
//! (`WM_NCCALCSIZE`) and tells Windows which parts of the row are caption and which are the
//! window buttons (`WM_NCHITTEST`), so dragging, snapping, double-click to maximize, the
//! system menu, resizing and Snap Layouts stay native. Other platforms keep their title bar.
#![cfg_attr(not(windows), allow(dead_code))]

use iced::advanced::{layout, mouse, overlay, renderer, widget::{self, Tree}, Clipboard, Layout, Shell, Widget};
use iced::widget::{container, row, text};
use iced::{Element, Event, Length, Point, Rectangle, Renderer, Size, Theme, Vector};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;

/// Height of the title bar row, like Windows' own caption buttons.
pub const HEIGHT: f32 = 32.0;
const BUTTON_WIDTH: f32 = 46.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Minimize = 1,
    Maximize = 2,
    Close = 3,
}

impl Button {
    fn from_code(code: u8) -> Option<Self> {
        [Button::Minimize, Button::Maximize, Button::Close].into_iter().find(|button| *button as u8 == code)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    Caption,
    Button(Button),
}

/// What is under the mouse, in Windows' terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// The app's own widgets: menus, icons, everything below the title bar.
    Client,
    Caption,
    Button(Button),
    Top,
    TopLeft,
    TopRight,
}

/// Where the caption and the buttons were last drawn, in logical pixels of the window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Regions {
    caption: Option<Rectangle>,
    buttons: [Option<Rectangle>; 3],
}

impl Regions {
    const fn new() -> Self { Self { caption: None, buttons: [None; 3] } }

    fn set(&mut self, region: Region, bounds: Rectangle) {
        match region {
            Region::Caption => self.caption = Some(bounds),
            Region::Button(button) => self.buttons[button as usize - 1] = Some(bounds),
        }
    }
}

static REGIONS: Mutex<Regions> = Mutex::new(Regions::new());
/// The button under the mouse, as `Button as u8`; 0 for none.
static HOVERED: AtomicU8 = AtomicU8::new(0);

/// Decides what `point` is over. Within `border` of the top edge the window resizes, unless
/// maximized (then the buttons reach the screen edge, where they are easiest to hit).
pub fn hit_test(point: Point, width: f32, border: f32, maximized: bool, regions: &Regions) -> Hit {
    if !maximized && point.y < border {
        return if point.x < border { Hit::TopLeft } else if point.x >= width - border { Hit::TopRight } else { Hit::Top };
    }
    for (index, bounds) in regions.buttons.iter().enumerate() {
        if bounds.is_some_and(|bounds| bounds.contains(point)) {
            return Hit::Button(Button::from_code(index as u8 + 1).expect("three buttons"));
        }
    }
    if regions.caption.is_some_and(|bounds| bounds.contains(point)) { Hit::Caption } else { Hit::Client }
}

pub fn hovered() -> Option<Button> { Button::from_code(HOVERED.load(Ordering::Relaxed)) }

/// Whether the title bar replaces the system one (Windows only).
pub fn custom() -> bool { cfg!(windows) }

/// Empty title bar space: dragging it moves the window, double-clicking maximizes.
pub fn caption<'a, M: 'a>(content: impl Into<Element<'a, M>>) -> Element<'a, M> {
    Element::new(Marker { region: Region::Caption, content: content.into() })
}

/// Minimize, maximize/restore and close. Their clicks go to Windows (see [`native`]), so they
/// are drawn here but have no press handlers; hover comes from the hook as well.
pub fn buttons<'a, M: 'a>(maximized: bool) -> Element<'a, M> {
    if !custom() { return row![].into(); }
    let hovered = hovered();
    let lucide = iced::Font::with_name("lucide");
    let button = move |button: Button, icon: lucide_icons::Icon| -> Element<'a, M> {
        let hover = hovered == Some(button);
        let glyph: char = icon.into();
        Element::new(Marker {
            region: Region::Button(button),
            content: container(text(glyph).font(lucide).size(14))
                .center_x(BUTTON_WIDTH).center_y(HEIGHT)
                .style(move |theme: &Theme| {
                    let palette = theme.extended_palette();
                    let (background, color) = match (hover, button) {
                        // Windows' own close-button red.
                        (true, Button::Close) => (Some(iced::Color::from_rgb8(0xC4, 0x2B, 0x1C)), iced::Color::WHITE),
                        (true, _) => (Some(palette.background.strong.color), palette.background.base.text),
                        (false, _) => (None, palette.background.base.text),
                    };
                    container::Style { background: background.map(Into::into), text_color: Some(color), ..Default::default() }
                })
                .into(),
        })
    };
    row![
        button(Button::Minimize, lucide_icons::Icon::Minus),
        button(Button::Maximize, if maximized { lucide_icons::Icon::Copy } else { lucide_icons::Icon::Square }),
        button(Button::Close, lucide_icons::Icon::X),
    ].height(Length::Fixed(HEIGHT)).into()
}

/// Records where its content is drawn, for the hook's hit test; otherwise transparent.
struct Marker<'a, M> {
    region: Region,
    content: Element<'a, M>,
}

impl<M> Widget<M, Theme, Renderer> for Marker<'_, M> {
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
        self.content.as_widget_mut().update(&mut tree.children[0], event, layout, cursor, renderer, clipboard, shell, viewport);
    }
    fn mouse_interaction(&self, tree: &Tree, layout: Layout<'_>, cursor: mouse::Cursor, viewport: &Rectangle, renderer: &Renderer) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
    }
    fn draw(&self, tree: &Tree, renderer: &mut Renderer, theme: &Theme, style: &renderer::Style, layout: Layout<'_>, cursor: mouse::Cursor, viewport: &Rectangle) {
        if let Ok(mut regions) = REGIONS.lock() { regions.set(self.region, layout.bounds()); }
        self.content.as_widget().draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport);
    }
    fn overlay<'b>(&'b mut self, tree: &'b mut Tree, layout: Layout<'b>, renderer: &Renderer, viewport: &Rectangle, translation: Vector) -> Option<overlay::Element<'b, M, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }
}

#[cfg(windows)]
pub use native::{install, maximized};

#[cfg(not(windows))]
pub fn maximized() -> bool { false }

#[cfg(windows)]
mod native {
    use super::{hit_test, Button, Hit, HOVERED, REGIONS};
    use iced::Point;
    use std::sync::atomic::{AtomicIsize, AtomicU8, Ordering};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
    use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT};
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetClientRect, IsZoomed, PostMessageW, SetWindowPos, HTCAPTION, HTCLIENT, HTCLOSE, HTMAXBUTTON, HTMINBUTTON,
        HTTOP, HTTOPLEFT, HTTOPRIGHT, NCCALCSIZE_PARAMS, SC_MAXIMIZE, SC_MINIMIZE, SC_RESTORE, SM_CXPADDEDBORDER,
        SM_CYFRAME, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WM_CLOSE, WM_MOUSEMOVE,
        WM_NCCALCSIZE, WM_NCDESTROY, WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP,
        WM_NCMOUSELEAVE, WM_NCMOUSEMOVE, WM_SYSCOMMAND,
    };

    const SUBCLASS_ID: usize = 1;
    static WINDOW: AtomicIsize = AtomicIsize::new(0);
    /// The button pressed (as `Button as u8`), clicked once released over the same button.
    static PRESSED: AtomicU8 = AtomicU8::new(0);

    /// Hooks the main window. Must run on the window's thread, as Iced's `update` does.
    pub fn install(raw: u64) -> Result<(), String> {
        let hwnd = raw as usize as HWND;
        // SAFETY: `hwnd` is Iced's live main window on this thread; the subclass procedure
        // stays valid for the program's lifetime and is removed on `WM_NCDESTROY`.
        unsafe {
            if SetWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID, 0) == 0 {
                return Err("could not hook the window for the custom title bar".into());
            }
            WINDOW.store(hwnd as isize, Ordering::Relaxed);
            // Have Windows recompute the frame now that `WM_NCCALCSIZE` drops the title bar.
            SetWindowPos(hwnd, std::ptr::null_mut(), 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
        Ok(())
    }

    pub fn maximized() -> bool {
        let hwnd = WINDOW.load(Ordering::Relaxed) as HWND;
        // SAFETY: IsZoomed only reads window state and accepts any handle, including null.
        !hwnd.is_null() && unsafe { IsZoomed(hwnd) } != 0
    }

    fn button(code: WPARAM) -> Option<Button> {
        match code as u32 {
            HTMINBUTTON => Some(Button::Minimize),
            HTMAXBUTTON => Some(Button::Maximize),
            HTCLOSE => Some(Button::Close),
            _ => None,
        }
    }

    /// Thickness of the resize frame at the window's DPI, in physical pixels.
    unsafe fn frame(hwnd: HWND) -> (u32, i32) {
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        (dpi, unsafe { GetSystemMetricsForDpi(SM_CYFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi) })
    }

    unsafe extern "system" fn subclass(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM, _: usize, _: usize) -> LRESULT {
        // SAFETY (whole body): called by Windows with a live `hwnd` and message parameters
        // whose meaning is fixed by `message`, as documented for each case.
        unsafe {
            match message {
                // Keep the system's left, right and bottom resize frame, but start the client
                // area at the window's top, so the app draws where the title bar was.
                WM_NCCALCSIZE if wparam != 0 => {
                    let params = lparam as *mut NCCALCSIZE_PARAMS;
                    let top = (*params).rgrc[0].top;
                    let result = DefSubclassProc(hwnd, message, wparam, lparam);
                    if result != 0 { return result; }
                    // A maximized window hangs its frame past the screen edge; keep content on screen.
                    (*params).rgrc[0].top = top + if IsZoomed(hwnd) != 0 { frame(hwnd).1 } else { 0 };
                    0
                }
                WM_NCHITTEST => {
                    let result = DefSubclassProc(hwnd, message, wparam, lparam);
                    if result != HTCLIENT as LRESULT { return result; }
                    let mut point = POINT { x: (lparam & 0xFFFF) as i16 as i32, y: ((lparam >> 16) & 0xFFFF) as i16 as i32 };
                    ScreenToClient(hwnd, &mut point);
                    let mut client = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                    GetClientRect(hwnd, &mut client);
                    let (dpi, border) = frame(hwnd);
                    let scale = dpi as f32 / 96.0;
                    let Ok(regions) = REGIONS.lock() else { return result };
                    let hit = hit_test(Point::new(point.x as f32 / scale, point.y as f32 / scale), client.right as f32 / scale,
                        border as f32 / scale, IsZoomed(hwnd) != 0, &regions);
                    (match hit {
                        Hit::Client => HTCLIENT,
                        Hit::Caption => HTCAPTION,
                        Hit::Button(Button::Minimize) => HTMINBUTTON,
                        Hit::Button(Button::Maximize) => HTMAXBUTTON,
                        Hit::Button(Button::Close) => HTCLOSE,
                        Hit::Top => HTTOP,
                        Hit::TopLeft => HTTOPLEFT,
                        Hit::TopRight => HTTOPRIGHT,
                    }) as LRESULT
                }
                WM_NCMOUSEMOVE => {
                    HOVERED.store(button(wparam).map_or(0, |button| button as u8), Ordering::Relaxed);
                    let mut track = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE | TME_NONCLIENT, hwndTrack: hwnd, dwHoverTime: 0 };
                    TrackMouseEvent(&mut track);
                    // The system would paint its own caption buttons over ours.
                    if button(wparam).is_some() { 0 } else { DefSubclassProc(hwnd, message, wparam, lparam) }
                }
                WM_NCMOUSELEAVE | WM_MOUSEMOVE => {
                    HOVERED.store(0, Ordering::Relaxed);
                    DefSubclassProc(hwnd, message, wparam, lparam)
                }
                WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if button(wparam).is_some() => {
                    PRESSED.store(button(wparam).map_or(0, |button| button as u8), Ordering::Relaxed);
                    0
                }
                WM_NCLBUTTONUP if button(wparam).is_some() => {
                    let pressed = PRESSED.swap(0, Ordering::Relaxed);
                    match button(wparam).filter(|button| *button as u8 == pressed) {
                        Some(Button::Minimize) => { PostMessageW(hwnd, WM_SYSCOMMAND, SC_MINIMIZE as WPARAM, 0); }
                        Some(Button::Maximize) => {
                            let command = if IsZoomed(hwnd) != 0 { SC_RESTORE } else { SC_MAXIMIZE };
                            PostMessageW(hwnd, WM_SYSCOMMAND, command as WPARAM, 0);
                        }
                        // Same close request as the system button: the app saves unsaved edits to
                        // its recovery session, then exits.
                        Some(Button::Close) => { PostMessageW(hwnd, WM_CLOSE, 0, 0); }
                        None => {}
                    }
                    0
                }
                WM_NCDESTROY => {
                    RemoveWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID);
                    WINDOW.store(0, Ordering::Relaxed);
                    DefSubclassProc(hwnd, message, wparam, lparam)
                }
                _ => DefSubclassProc(hwnd, message, wparam, lparam),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regions() -> Regions {
        let mut regions = Regions::new();
        regions.set(Region::Caption, Rectangle::new(Point::new(200.0, 0.0), Size::new(400.0, HEIGHT)));
        for (index, button) in [Button::Minimize, Button::Maximize, Button::Close].into_iter().enumerate() {
            regions.set(Region::Button(button), Rectangle::new(Point::new(862.0 + index as f32 * BUTTON_WIDTH, 0.0), Size::new(BUTTON_WIDTH, HEIGHT)));
        }
        regions
    }

    #[test]
    fn hit_test_finds_edges_buttons_caption_and_client() {
        let (regions, width, border) = (regions(), 1000.0, 8.0);
        let hit = |x, y, maximized| hit_test(Point::new(x, y), width, border, maximized, &regions);
        assert_eq!(hit(500.0, 2.0, false), Hit::Top);
        assert_eq!(hit(2.0, 2.0, false), Hit::TopLeft);
        assert_eq!(hit(998.0, 2.0, false), Hit::TopRight, "the close button's corner still resizes");
        assert_eq!(hit(990.0, 2.0, true), Hit::Button(Button::Close), "maximized: buttons reach the screen edge");
        assert_eq!(hit(500.0, 2.0, true), Hit::Caption);
        assert_eq!(hit(870.0, 16.0, false), Hit::Button(Button::Minimize));
        assert_eq!(hit(930.0, 16.0, false), Hit::Button(Button::Maximize));
        assert_eq!(hit(990.0, 16.0, false), Hit::Button(Button::Close));
        assert_eq!(hit(500.0, 16.0, false), Hit::Caption);
        assert_eq!(hit(100.0, 16.0, false), Hit::Client, "the menu stays clickable");
        assert_eq!(hit(700.0, 16.0, false), Hit::Client, "icons between caption and buttons");
        assert_eq!(hit(500.0, 300.0, false), Hit::Client, "below the title bar");
        assert_eq!(hit_test(Point::new(500.0, 16.0), width, border, false, &Regions::new()), Hit::Client, "nothing drawn yet");
    }

    #[test]
    fn button_codes_round_trip() {
        for button in [Button::Minimize, Button::Maximize, Button::Close] {
            assert_eq!(Button::from_code(button as u8), Some(button));
        }
        assert_eq!(Button::from_code(0), None);
        assert_eq!(Button::from_code(4), None);
    }
}
