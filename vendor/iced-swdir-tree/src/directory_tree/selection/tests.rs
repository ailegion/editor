//! Unit tests for [`super::SelectionMode`].

use super::*;

#[test]
fn default_is_replace() {
    assert_eq!(SelectionMode::default(), SelectionMode::Replace);
}

#[test]
fn from_modifiers_maps_all_four_cases() {
    let plain = Modifiers::default();
    assert_eq!(SelectionMode::from_modifiers(plain), SelectionMode::Replace);

    let shift = Modifiers::SHIFT;
    assert_eq!(
        SelectionMode::from_modifiers(shift),
        SelectionMode::ExtendRange
    );

    let ctrl = Modifiers::CTRL;
    assert_eq!(SelectionMode::from_modifiers(ctrl), SelectionMode::Toggle);

    let logo = Modifiers::LOGO;
    assert_eq!(SelectionMode::from_modifiers(logo), SelectionMode::Toggle);
}

#[test]
fn shift_wins_over_ctrl() {
    let shift_ctrl = Modifiers::SHIFT | Modifiers::CTRL;
    assert_eq!(
        SelectionMode::from_modifiers(shift_ctrl),
        SelectionMode::ExtendRange
    );
}

#[test]
fn alt_alone_is_replace() {
    // Alt doesn't carry multi-select semantics in any major
    // platform's file manager; treat it as plain.
    let alt = Modifiers::ALT;
    assert_eq!(SelectionMode::from_modifiers(alt), SelectionMode::Replace);
}
