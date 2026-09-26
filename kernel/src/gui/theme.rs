//! Colours and widgets shared by the desktop and the apps.

use super::canvas::{mix, rgb, Canvas, Color, Rect};

/// Window and button background.
pub const FACE: Color = rgb(0xe4, 0xe6, 0xec);
pub const LIGHT: Color = rgb(0xff, 0xff, 0xff);
pub const SHADOW: Color = rgb(0x9a, 0x9e, 0xaa);
pub const DARK: Color = rgb(0x30, 0x34, 0x40);
pub const TEXT: Color = rgb(0x18, 0x1a, 0x22);
pub const ACCENT: Color = rgb(0x3a, 0x5c, 0xd8);
pub const ACCENT_2: Color = rgb(0x7a, 0x3c, 0xc8);

/// A push button with a centred label.
pub fn button(c: &mut Canvas, r: Rect, label: &str, pressed: bool) {
    colored_button(c, r, label, FACE, pressed);
}

pub fn colored_button(c: &mut Canvas, r: Rect, label: &str, face: Color, pressed: bool) {
    let face = if pressed {
        mix(face, SHADOW, 110)
    } else {
        face
    };
    c.vertical_gradient(r, mix(face, LIGHT, 90), face);
    c.outline(r, SHADOW);
    c.bevel(
        r.inset(1),
        if pressed { SHADOW } else { LIGHT },
        face,
        false,
    );
    let r = if pressed { r.offset(1, 1) } else { r };
    c.text_centered(r, label, TEXT);
}

/// The button everything else in a dialog leads to, in the accent colour.
pub fn accent_button(c: &mut Canvas, r: Rect, label: &str, pressed: bool) {
    let (top, bottom) = if pressed {
        (mix(ACCENT, DARK, 60), mix(ACCENT_2, DARK, 60))
    } else {
        (mix(ACCENT, LIGHT, 40), ACCENT)
    };
    c.vertical_gradient(r, top, bottom);
    c.outline(r, mix(ACCENT, DARK, 120));
    let r = if pressed { r.offset(1, 1) } else { r };
    c.text_centered(r, label, LIGHT);
}
