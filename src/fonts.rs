//! The bundled Monaspace Neon font (https://github.com/githubnext/monaspace), the font of the
//! whole app: iced's default, the code editor and the terminal. The files are the unmodified
//! v1.400 static OTFs, under the SIL Open Font License 1.1 (`assets/fonts/Monaspace-LICENSE.txt`).

pub const NAME: &str = "Monaspace Neon";

pub const FONT: iced::Font = iced::Font::with_name(NAME);

pub const FILES: [&[u8]; 4] = [
    include_bytes!("../assets/fonts/MonaspaceNeon-Regular.otf"),
    include_bytes!("../assets/fonts/MonaspaceNeon-Bold.otf"),
    include_bytes!("../assets/fonts/MonaspaceNeon-Italic.otf"),
    include_bytes!("../assets/fonts/MonaspaceNeon-BoldItalic.otf"),
];

/// Adds the bundled files to a font database of the code editor's own and makes Monaspace Neon
/// the family its text asks for (the default, sans-serif).
pub fn configure(db: &mut cosmic_text::fontdb::Database) {
    for file in FILES {
        db.load_font_data(file.to_vec());
    }
    db.set_sans_serif_family(NAME);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_files_are_the_four_styles_of_monaspace_neon() {
        let mut db = cosmic_text::fontdb::Database::new();
        for file in FILES {
            db.load_font_data(file.to_vec());
        }
        let mut styles: Vec<(u16, bool)> = db.faces().map(|face| {
            assert!(face.families.iter().any(|(family, _)| family == NAME), "{:?}", face.families);
            assert!(face.monospaced);
            (face.weight.0, face.style == cosmic_text::fontdb::Style::Italic)
        }).collect();
        styles.sort();
        assert_eq!(styles, vec![(400, false), (400, true), (700, false), (700, true)]);
    }
}
