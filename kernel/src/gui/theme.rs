//! Colours and widgets shared by the desktop and the apps, in the style
//! of Windows 11: light surfaces, thin borders and rounded corners.

use super::canvas::{mix, rgb, Canvas, Color, Rect};

/// Window and panel background.
pub const FACE: Color = rgb(0xf3, 0xf3, 0xf3);
pub const LIGHT: Color = rgb(0xff, 0xff, 0xff);
/// Thin borders around controls.
pub const STROKE: Color = rgb(0xdc, 0xdc, 0xdc);
pub const SHADOW: Color = rgb(0xb4, 0xb4, 0xb4);
pub const TEXT: Color = rgb(0x1a, 0x1a, 0x1a);
pub const TEXT_DIM: Color = rgb(0x70, 0x70, 0x70);
pub const ACCENT: Color = rgb(0x00, 0x67, 0xc0);
pub const ACCENT_LIGHT: Color = rgb(0xe3, 0xee, 0xfa);

/// Corner radius of buttons and small controls.
pub const CONTROL_RADIUS: i32 = 5;

/// A push button with a centred label.
pub fn button(c: &mut Canvas, r: Rect, label: &str, pressed: bool) {
    colored_button(c, r, label, rgb(0xfb, 0xfb, 0xfb), pressed);
}

/// A button showing an option that is turned on, such as the chosen tool.
pub fn toggle_button(c: &mut Canvas, r: Rect, label: &str, on: bool) {
    if on {
        c.fill_round(r, CONTROL_RADIUS, ACCENT_LIGHT);
        c.outline_round(r, CONTROL_RADIUS, mix(ACCENT, LIGHT, 120));
        c.text_centered(r, label, ACCENT);
    } else {
        button(c, r, label, false);
    }
}

pub fn colored_button(c: &mut Canvas, r: Rect, label: &str, face: Color, pressed: bool) {
    let face = if pressed { mix(face, SHADOW, 50) } else { face };
    c.fill_round(r, CONTROL_RADIUS, face);
    c.outline_round(r, CONTROL_RADIUS, STROKE);
    // the slightly darker bottom edge Windows 11 buttons have
    if !pressed {
        c.fill_rect(
            r.x + CONTROL_RADIUS,
            r.bottom() - 1,
            r.w - 2 * CONTROL_RADIUS,
            1,
            mix(STROKE, TEXT, 30),
        );
    }
    c.text_centered(r, label, if pressed { TEXT_DIM } else { TEXT });
}

/// The button everything else in a dialog leads to, in the accent colour.
pub fn accent_button(c: &mut Canvas, r: Rect, label: &str, pressed: bool) {
    let face = if pressed {
        mix(ACCENT, LIGHT, 50)
    } else {
        ACCENT
    };
    c.fill_round(r, CONTROL_RADIUS, face);
    c.outline_round(r, CONTROL_RADIUS, mix(ACCENT, TEXT, 40));
    c.text_centered(r, label, LIGHT);
}
