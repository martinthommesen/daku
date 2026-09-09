//! Layout tokens for shell spacing and sidebar width.
//!
//! Single source for shell spacing and the sidebar width. Radius, type, and
//! color ride the installed component theme (`cx.theme()`), so this module
//! does not duplicate them. Pure constants, so tests prove the scale without
//! launching GPUI.

/// Sidebar width in logical pixels. Pinned to the pre-overhaul value so the
/// shell layout does not shift under the token adoption.
pub const SIDEBAR_WIDTH: f32 = 220.0;

/// Spacing rhythm in logical pixels. Monotonic by construction. Render code
/// adopts these incrementally; remaining literals are pre-existing outliers.
pub const SPACE_XS: f32 = 4.0;
pub const SPACE_SM: f32 = 8.0;
pub const SPACE_MD: f32 = 12.0;
pub const SPACE_LG: f32 = 16.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_width_stays_pinned() {
        assert_eq!(SIDEBAR_WIDTH, 220.0);
    }

    #[test]
    fn spacing_scale_is_monotonic_with_distinct_steps() {
        let steps = [SPACE_XS, SPACE_SM, SPACE_MD, SPACE_LG];
        assert!(steps.iter().all(|step| *step > 0.0));
        for pair in steps.windows(2) {
            assert!(pair[0] < pair[1]);
        }
    }
}
