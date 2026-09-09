//! Layout tokens for the visual overhaul (U1/U2).
//!
//! Single source for shell spacing and the sidebar width. Radius, type, and
//! color ride the installed component theme (`cx.theme()`), so this module
//! does not duplicate them. Pure constants, so tests prove the scale without
//! launching GPUI.

/// Sidebar width in logical pixels. Single source for `app.rs`.
pub const SIDEBAR_WIDTH: f32 = 232.0;

/// Spacing rhythm in logical pixels. Monotonic by construction. Render code
/// uses these for gaps and padding; off-scale literals elsewhere are
/// pre-existing outliers for a later pass, not a second scale.
pub const SPACE_XS: f32 = 4.0;
pub const SPACE_SM: f32 = 8.0;
pub const SPACE_MD: f32 = 12.0;
pub const SPACE_LG: f32 = 16.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_width_stays_in_usable_range() {
        assert!((200.0..=260.0).contains(&SIDEBAR_WIDTH));
    }

    #[test]
    fn spacing_scale_is_monotonic_with_distinct_steps() {
        let steps = [SPACE_XS, SPACE_SM, SPACE_MD, SPACE_LG];
        for pair in steps.windows(2) {
            assert!(pair[0] < pair[1]);
        }
        assert_eq!(SPACE_SM, 2.0 * SPACE_XS);
        assert_eq!(SPACE_MD, 3.0 * SPACE_XS);
        assert_eq!(SPACE_LG, 4.0 * SPACE_XS);
    }
}
