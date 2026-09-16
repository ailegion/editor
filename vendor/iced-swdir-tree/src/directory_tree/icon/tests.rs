//! Unit tests for the [`super::IconTheme`] trait and stock themes.

use super::*;

#[test]
fn unicode_theme_produces_expected_glyphs() {
    let t = UnicodeTheme;
    assert_eq!(t.glyph(IconRole::FolderClosed).glyph.as_ref(), "\u{1F4C1}");
    assert_eq!(t.glyph(IconRole::FolderOpen).glyph.as_ref(), "\u{1F4C2}");
    assert_eq!(t.glyph(IconRole::File).glyph.as_ref(), "\u{1F4C4}");
    assert_eq!(t.glyph(IconRole::Error).glyph.as_ref(), "\u{26A0}");
    assert_eq!(t.glyph(IconRole::CaretRight).glyph.as_ref(), "\u{25B8}");
    assert_eq!(t.glyph(IconRole::CaretDown).glyph.as_ref(), "\u{25BE}");
}

#[test]
fn unicode_theme_does_not_set_font() {
    // The whole point of UnicodeTheme is that it uses glyphs in the
    // default system font — setting a specific font would defeat
    // that.
    let t = UnicodeTheme;
    assert!(t.glyph(IconRole::FolderClosed).font.is_none());
    assert!(t.glyph(IconRole::File).font.is_none());
}

#[test]
fn unicode_theme_does_not_set_size() {
    // Theme defers sizing to the widget so row heights stay
    // consistent across configurations.
    let t = UnicodeTheme;
    assert!(t.glyph(IconRole::FolderClosed).size.is_none());
}

#[cfg(feature = "icons")]
#[test]
fn lucide_theme_sets_the_lucide_font() {
    let t = LucideTheme;
    let spec = t.glyph(IconRole::FolderClosed);
    assert!(
        spec.font.is_some(),
        "LucideTheme must set a font so the glyph codepoints render"
    );
}

#[cfg(feature = "icons")]
#[test]
fn lucide_theme_glyph_length_is_single_char() {
    // Lucide codepoints live in the Private Use Area (single chars).
    let t = LucideTheme;
    for role in [
        IconRole::FolderClosed,
        IconRole::FolderOpen,
        IconRole::File,
        IconRole::Error,
        IconRole::CaretRight,
        IconRole::CaretDown,
    ] {
        let spec = t.glyph(role);
        let s = spec.glyph.as_ref();
        assert_eq!(s.chars().count(), 1, "role {role:?} produced {s:?}");
    }
}

#[test]
fn iconspec_builder_sets_font_and_size() {
    let spec = IconSpec::new("x")
        .with_font(iced::Font::DEFAULT)
        .with_size(22.0);
    assert_eq!(spec.glyph.as_ref(), "x");
    assert_eq!(spec.font, Some(iced::Font::DEFAULT));
    assert_eq!(spec.size, Some(22.0));
}

#[test]
fn iconspec_new_accepts_static_str_and_string() {
    // `Cow<'static, str>` over `Into<Cow<_, _>>` accepts both
    // without forcing callers to choose.
    let from_static: IconSpec = IconSpec::new("static");
    let from_owned: IconSpec = IconSpec::new(String::from("owned"));
    assert_eq!(from_static.glyph.as_ref(), "static");
    assert_eq!(from_owned.glyph.as_ref(), "owned");
}

#[test]
fn custom_theme_is_consulted_per_role() {
    // A tiny fake theme demonstrates that the trait contract is
    // enough for a downstream implementor to plug in.
    #[derive(Debug)]
    struct FakeTheme;

    impl IconTheme for FakeTheme {
        fn glyph(&self, role: IconRole) -> IconSpec {
            // In-crate so exhaustive; external themes need `_ =>`.
            let s: &'static str = match role {
                IconRole::FolderClosed => "C",
                IconRole::FolderOpen => "O",
                IconRole::File => "F",
                IconRole::Error => "E",
                IconRole::CaretRight => ">",
                IconRole::CaretDown => "v",
            };
            IconSpec::new(s)
        }
    }

    let t = FakeTheme;
    assert_eq!(t.glyph(IconRole::FolderClosed).glyph.as_ref(), "C");
    assert_eq!(t.glyph(IconRole::FolderOpen).glyph.as_ref(), "O");
    assert_eq!(t.glyph(IconRole::File).glyph.as_ref(), "F");
    assert_eq!(t.glyph(IconRole::Error).glyph.as_ref(), "E");
    assert_eq!(t.glyph(IconRole::CaretRight).glyph.as_ref(), ">");
    assert_eq!(t.glyph(IconRole::CaretDown).glyph.as_ref(), "v");
}

#[test]
fn icon_theme_is_object_safe() {
    // If this compiles, `IconTheme` is object-safe — which it must
    // be, because the widget holds it in `Arc<dyn IconTheme>`.
    let _boxed: Box<dyn IconTheme> = Box::new(UnicodeTheme);
    let _arc: std::sync::Arc<dyn IconTheme> = std::sync::Arc::new(UnicodeTheme);
}

#[test]
fn default_theme_produces_reasonable_folder_glyph() {
    // Regardless of feature flag, the default theme must produce a
    // non-empty glyph for every role — the widget depends on this
    // for layout stability.
    let t = default_theme();
    for role in [
        IconRole::FolderClosed,
        IconRole::FolderOpen,
        IconRole::File,
        IconRole::Error,
        IconRole::CaretRight,
        IconRole::CaretDown,
    ] {
        let spec = t.glyph(role);
        assert!(
            !spec.glyph.as_ref().is_empty(),
            "default theme returned empty glyph for {role:?}"
        );
    }
}
