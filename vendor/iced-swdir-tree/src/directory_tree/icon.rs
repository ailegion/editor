//! Icon abstraction: [`IconRole`], [`IconSpec`], [`IconTheme`], and
//! the two stock themes the crate ships.
//!
//! The widget renders icons through an [`IconTheme`] — a trait that
//! returns an [`IconSpec`] (glyph + optional font + optional size)
//! for each logical [`IconRole`]. Two stock themes are provided:
//!
//! * [`UnicodeTheme`] — always available. Renders short Unicode
//!   symbols (📁 📂 📄 ⚠ ▸ ▾) that work in any system font.
//! * [`LucideTheme`] — available with the `icons` feature flag.
//!   Renders real lucide vector glyphs via the bundled
//!   [`crate::LUCIDE_FONT_BYTES`] font.
//!
//! Which theme is the default depends on the feature flag: with
//! `icons` on, it's [`LucideTheme`]; with `icons` off, it's
//! [`UnicodeTheme`]. Applications can plug in their own theme via
//! [`DirectoryTree::with_icon_theme`]:
//!
//! ```ignore
//! use std::sync::Arc;
//! use iced_swdir_tree::{DirectoryTree, IconRole, IconSpec, IconTheme};
//!
//! #[derive(Debug)]
//! struct MyTheme;
//!
//! impl IconTheme for MyTheme {
//!     fn glyph(&self, role: IconRole) -> IconSpec {
//!         match role {
//!             IconRole::FolderClosed => IconSpec::new("📂"),
//!             IconRole::FolderOpen => IconSpec::new("📁"),
//!             IconRole::File => IconSpec::new("·"),
//!             _ => IconSpec::new("?"),
//!         }
//!     }
//! }
//!
//! let tree = DirectoryTree::new(".".into())
//!     .with_icon_theme(Arc::new(MyTheme));
//! ```
//!
//! [`DirectoryTree::with_icon_theme`]: crate::DirectoryTree::with_icon_theme

use std::borrow::Cow;

use iced::Element;

/// Semantic icon identifiers the widget renders.
///
/// The widget asks the configured [`IconTheme`] for an [`IconSpec`]
/// per-role whenever it needs to render a row. Themes are
/// responsible for producing a reasonable visual for every role.
///
/// **This enum is `#[non_exhaustive]`** so future versions can add
/// roles (`Symlink`, `Hidden`, `Loading`, …) without breaking
/// external themes' `match` exhaustiveness. External themes should
/// provide a `_ =>` fallback arm when matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum IconRole {
    /// A directory that is currently collapsed.
    FolderClosed,
    /// A directory that is currently expanded.
    FolderOpen,
    /// A regular file.
    File,
    /// A directory we could not list (permission denied, etc.).
    Error,
    /// The caret pointing right (collapsed indicator for folders).
    CaretRight,
    /// The caret pointing down (expanded indicator for folders).
    CaretDown,
}

/// Description of how to render an icon for a particular
/// [`IconRole`].
///
/// Constructed by an [`IconTheme`]. The widget takes the spec and
/// emits an `iced` text element using `glyph`, optionally setting
/// the specified `font` and `size`. When `font` is `None`, the
/// glyph renders in the default iced font. When `size` is `None`,
/// the widget picks its own default (currently 14).
///
/// Use [`IconSpec::new`] + [`IconSpec::with_font`] / [`IconSpec::with_size`]
/// for ergonomic construction:
///
/// ```
/// # use iced_swdir_tree::IconSpec;
/// let spec = IconSpec::new("\u{E89C}").with_size(16.0);
/// assert_eq!(spec.glyph.as_ref(), "\u{E89C}");
/// ```
///
/// Fields are `pub` so `const` themes can construct specs
/// literally — but note that adding fields here is a breaking
/// change, so we don't expect to add new fields before a
/// hypothetical 2.0.
#[derive(Debug, Clone, PartialEq)]
pub struct IconSpec {
    /// The text to render. A single `char` for typical icons;
    /// longer strings are supported (e.g. ligatures, emoji
    /// sequences, labels like `"DIR"`).
    ///
    /// `Cow<'static, str>` so themes can use `&'static str`
    /// literals at no allocation cost, while still accepting
    /// owned `String`s for dynamically constructed glyphs.
    pub glyph: Cow<'static, str>,
    /// The font to render `glyph` in, or `None` for the iced
    /// default. Themes using icon-font packs
    /// ([lucide-icons](https://lucide.dev),
    /// [Material Design Icons](https://pictogrammers.com/library/mdi/),
    /// ...) must set this or the glyph codepoints will render
    /// as tofu.
    pub font: Option<iced::Font>,
    /// The point size to render at, or `None` to let the widget
    /// pick (currently 14). Themes using larger/smaller glyphs
    /// than the default can set this to match their intended
    /// visual balance.
    pub size: Option<f32>,
}

impl IconSpec {
    /// Build a spec from a glyph string. Font and size are `None`
    /// by default — the widget will render in the iced default
    /// font at its default size.
    pub fn new(glyph: impl Into<Cow<'static, str>>) -> Self {
        Self {
            glyph: glyph.into(),
            font: None,
            size: None,
        }
    }

    /// Set the font used to render the glyph. Required for
    /// icon-font packs where the codepoint is only valid in a
    /// specific font.
    pub fn with_font(mut self, font: iced::Font) -> Self {
        self.font = Some(font);
        self
    }

    /// Set the point size. `None` (the default) means the widget
    /// picks — currently 14.
    pub fn with_size(mut self, size: f32) -> Self {
        self.size = Some(size);
        self
    }
}

/// How to render each [`IconRole`] for a given visual design.
///
/// Implementers return an [`IconSpec`] per role. The widget calls
/// [`glyph`](IconTheme::glyph) during view rendering, so the
/// method should be cheap and pure — build a table at construction
/// time rather than computing per call if your theme is complex.
///
/// `Send + Sync + Debug` because the widget holds the theme in an
/// `Arc<dyn IconTheme>` and the tree itself is `Debug`-derived.
///
/// # Implementing a custom theme
///
/// ```
/// use std::borrow::Cow;
/// use iced_swdir_tree::{IconRole, IconSpec, IconTheme};
///
/// #[derive(Debug)]
/// struct EmojiTheme;
///
/// impl IconTheme for EmojiTheme {
///     fn glyph(&self, role: IconRole) -> IconSpec {
///         let s: &'static str = match role {
///             IconRole::FolderClosed => "📁",
///             IconRole::FolderOpen => "📂",
///             IconRole::File => "📄",
///             IconRole::Error => "⚠",
///             IconRole::CaretRight => "▸",
///             IconRole::CaretDown => "▾",
///             _ => "?",
///         };
///         IconSpec::new(Cow::Borrowed(s))
///     }
/// }
/// ```
///
/// Note the `_ =>` arm: [`IconRole`] is `#[non_exhaustive]` so new
/// variants may be added in future minor releases; always provide
/// a fallback.
pub trait IconTheme: Send + Sync + std::fmt::Debug {
    /// Produce the rendering description for `role`.
    fn glyph(&self, role: IconRole) -> IconSpec;

    /// File-specific icon; defaults to the generic file icon.
    fn file_glyph(&self, _path: &std::path::Path) -> IconSpec { self.glyph(IconRole::File) }

    /// Optional accent color for a file icon.
    fn file_color(&self, _path: &std::path::Path) -> Option<iced::Color> { None }
}

/// Stock theme that renders short Unicode symbols available in any
/// system font. Always available.
///
/// This is the default theme when the `icons` feature is disabled,
/// and serves as a dependency-free fallback that still looks
/// reasonable out of the box.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnicodeTheme;

impl IconTheme for UnicodeTheme {
    fn glyph(&self, role: IconRole) -> IconSpec {
        // `IconRole` is `#[non_exhaustive]` but that only affects
        // *external* crates — inside the defining crate we can (and
        // must, to silence unreachable-pattern warnings) match every
        // variant exhaustively. External themes need a `_ =>`
        // fallback; see the trait's rustdoc.
        let s: &'static str = match role {
            IconRole::FolderClosed => "\u{1F4C1}", // 📁
            IconRole::FolderOpen => "\u{1F4C2}",   // 📂
            IconRole::File => "\u{1F4C4}",         // 📄
            IconRole::Error => "\u{26A0}",         // ⚠
            IconRole::CaretRight => "\u{25B8}",    // ▸
            IconRole::CaretDown => "\u{25BE}",     // ▾
        };
        IconSpec::new(Cow::Borrowed(s))
    }
}

/// Stock theme that renders real [lucide](https://lucide.dev)
/// vector glyphs.
///
/// Available only when the `icons` feature is enabled. The
/// application is responsible for registering the bundled
/// [`crate::LUCIDE_FONT_BYTES`] font:
///
/// ```ignore
/// iced::application(App::new, App::update, App::view)
///     .font(iced_swdir_tree::LUCIDE_FONT_BYTES)
///     .run()
/// ```
///
/// Without the font registered, lucide codepoints render as tofu
/// squares — the widget still compiles and the selection/drag/etc.
/// state all works, the icons just look wrong.
#[cfg(feature = "icons")]
#[cfg_attr(docsrs, doc(cfg(feature = "icons")))]
#[derive(Debug, Clone, Copy, Default)]
pub struct LucideTheme;

#[cfg(feature = "icons")]
impl IconTheme for LucideTheme {
    fn glyph(&self, role: IconRole) -> IconSpec {
        // lucide-icons exposes an `Icon` enum with codepoints reachable
        // via `char::from(icon)`. We map each role to the corresponding
        // enum variant so future lucide updates flow through.
        use lucide_icons::Icon as LIcon;
        let lucide_icon: LIcon = match role {
            IconRole::FolderClosed => LIcon::Folder,
            IconRole::FolderOpen => LIcon::FolderOpen,
            IconRole::File => LIcon::File,
            IconRole::Error => LIcon::AlertCircle,
            IconRole::CaretRight => LIcon::ChevronRight,
            IconRole::CaretDown => LIcon::ChevronDown,
            // `IconRole` is `#[non_exhaustive]` but only externally;
            // inside this crate every arm must be named. If a new
            // variant is added, the compile error will point here
            // and remind whoever adds it to also update the stock
            // themes. External themes should add a `_ =>` fallback.
        };
        let c: char = lucide_icon.into();
        let mut s = String::with_capacity(c.len_utf8());
        s.push(c);
        IconSpec::new(s)
            .with_font(iced::Font::with_name("lucide"))
            .with_size(14.0)
    }
}

/// Build the default [`IconTheme`] for the current feature set.
///
/// * With `icons` feature: [`LucideTheme`].
/// * Without `icons` feature: [`UnicodeTheme`].
///
/// Used internally by [`DirectoryTree::new`](crate::DirectoryTree::new);
/// applications that want a different default call
/// [`with_icon_theme`](crate::DirectoryTree::with_icon_theme).
pub(crate) fn default_theme() -> std::sync::Arc<dyn IconTheme> {
    #[cfg(feature = "icons")]
    {
        std::sync::Arc::new(LucideTheme)
    }
    #[cfg(not(feature = "icons"))]
    {
        std::sync::Arc::new(UnicodeTheme)
    }
}

/// Render a role to an `iced::Element` by consulting `theme`.
///
/// This is the one call site view code uses — it takes the theme,
/// asks for the spec, and produces the element. Keeps feature-flag
/// and theme-dispatch concerns out of the view layer.
pub(crate) fn render<'a, Message: 'a>(
    theme: &dyn IconTheme,
    role: IconRole,
) -> Element<'a, Message> {
    use iced::widget::text;
    let spec = theme.glyph(role);
    // Cow -> String ownership: text() wants a `text::IntoFragment`
    // which accepts owned Strings; build one unconditionally. For
    // typical `Cow::Borrowed(&'static str)` themes this is one
    // small allocation per icon per render, which matches the
    // pre-0.7 cost of `text("📁")` constructing its own text
    // element.
    let mut t = text(spec.glyph.into_owned()).size(spec.size.unwrap_or(14.0));
    if let Some(font) = spec.font {
        t = t.font(font);
    }
    t.into()
}

pub(crate) fn render_file<'a, Message: 'a>(theme: &dyn IconTheme, path: &std::path::Path) -> Element<'a, Message> {
    let spec = theme.file_glyph(path);
    let mut label = iced::widget::text(spec.glyph.into_owned()).size(spec.size.unwrap_or(14.0));
    if let Some(font) = spec.font { label = label.font(font); }
    if let Some(color) = theme.file_color(path) { label = label.color(color); }
    iced::widget::container(label).width(20).center_x(20).into()
}

#[cfg(test)]
mod tests;
