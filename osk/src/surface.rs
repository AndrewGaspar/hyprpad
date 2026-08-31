//! The layer-shell surface geometry — anchoring and exclusive-zone policy for
//! both presentation modes. This module is pure geometry (no live Wayland
//! objects), so it is unit-testable; [`crate::app`] turns each [`PanelSpec`]
//! into an actual `zwlr_layer_surface_v1`.
//!
//! # Non-negotiable rules baked in here (osk-technology.md §2, §8)
//!
//! * **OVERLAY tier only** ([`LAYER`]). TOP is faded to alpha 0 under a
//!   fullscreen client; OVERLAY keeps alpha 1 and draws over XWayland games too
//!   (§2.1). Never TOP.
//! * **No keyboard focus** ([`INTERACTIVITY`] = `None`). The OSK is driven
//!   entirely over the control channel; it must not steal keyboard focus from
//!   the app being typed into.
//! * **Destroy on dismiss, never hide** — enforced by [`crate::app`] dropping
//!   the surfaces; an idle OVERLAY surface permanently disables direct scanout
//!   and tearing for every fullscreen game on that monitor (§2.3).
//! * **Stable namespace** ([`NAMESPACE`]) so the compositor-side layer rules
//!   `no_screen_share` (keep the OSK out of screen captures) and, for the lock
//!   screen, `above_lock` match it (§2.4). Those rules live in Hyprland config
//!   keyed on this namespace — see the crate README.
//!
//! # The two orthogonal axes
//!
//! Presentation is the product of two independent choices:
//!
//! * **Layout** ([`crate::layout::LayoutMode`]): `BottomDeck` → one bottom
//!   panel; `SideSplit` → two edge columns.
//! * **Reflow vs overlay** (`reflow: bool`): set an exclusive zone so Hyprland
//!   shrinks the tiled area and content slides out of the OSK's way (the
//!   pan-up / side-squeeze feel), OR set a zero exclusive zone to float over a
//!   fullscreen game without reflowing it. Same OVERLAY surface either way —
//!   only the exclusive zone differs.

use smithay_client_toolkit::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer};

use crate::layout::{LayoutMode, PanelRole};

/// The one layer tier the OSK ever uses.
pub const LAYER: Layer = Layer::Overlay;
/// The OSK never takes keyboard focus.
pub const INTERACTIVITY: KeyboardInteractivity = KeyboardInteractivity::None;
/// Layer namespace; the anchor for compositor-side layer rules and for
/// `hyprctl layers` identification.
pub const NAMESPACE: &str = "hyprpad-osk";

// Thickness heuristics (kickoff). Final ergonomics — especially the side
// columns — are DEFERRED (see README / osk-technology.md §4).
const DEFAULT_OUT_W: u32 = 1920;
const DEFAULT_OUT_H: u32 = 1080;

fn bottom_height(out_h: u32) -> u32 {
    let h = if out_h == 0 { DEFAULT_OUT_H } else { out_h };
    ((h as f32 * 0.34) as u32).clamp(240, 520)
}

fn column_width(out_w: u32) -> u32 {
    let w = if out_w == 0 { DEFAULT_OUT_W } else { out_w };
    ((w as f32 * 0.22) as u32).clamp(220, 480)
}

/// The anchoring + sizing + exclusive-zone spec for one layer surface.
#[derive(Clone, Copy, Debug)]
pub struct PanelSpec {
    pub role: PanelRole,
    pub anchor: Anchor,
    /// Requested width; `0` means "span the anchored axis" (compositor fills in
    /// the real width in its configure).
    pub width: u32,
    /// Requested height; `0` means "span".
    pub height: u32,
    /// Exclusive zone in px along the anchored edge. `>0` reflows workspace
    /// content; `0` floats over it (osk-technology.md §2.3 / task Mode A vs B).
    pub exclusive_zone: i32,
}

/// Compute the panel spec(s) for a show request.
///
/// * [`LayoutMode::BottomDeck`] → **one** panel anchored bottom, spanning the
///   full width, reserving `bottom_height` px (reflow) or `0` (overlay).
/// * [`LayoutMode::SideSplit`] → **two** panels, one anchored to each vertical
///   edge, each spanning full height and reserving `column_width` px. Two
///   surfaces (not one full-width surface with a transparent hole) because an
///   exclusive zone is a single scalar along one edge — you cannot carve a gap
///   in the middle of one surface, so genuine centre reflow *requires* an
///   independent exclusive zone on each edge. The cost is two surfaces to keep
///   in sync; the benefit is real two-sided reflow and each thumb owning its
///   own physical surface.
pub fn panels_for(mode: LayoutMode, reflow: bool, out_w: u32, out_h: u32) -> Vec<PanelSpec> {
    match mode {
        LayoutMode::BottomDeck => {
            let h = bottom_height(out_h);
            vec![PanelSpec {
                role: PanelRole::Bottom,
                anchor: Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                width: 0,
                height: h,
                exclusive_zone: if reflow { h as i32 } else { 0 },
            }]
        }
        LayoutMode::SideSplit => {
            let w = column_width(out_w);
            let ez = if reflow { w as i32 } else { 0 };
            vec![
                PanelSpec {
                    role: PanelRole::LeftColumn,
                    anchor: Anchor::LEFT | Anchor::TOP | Anchor::BOTTOM,
                    width: w,
                    height: 0,
                    exclusive_zone: ez,
                },
                PanelSpec {
                    role: PanelRole::RightColumn,
                    anchor: Anchor::RIGHT | Anchor::TOP | Anchor::BOTTOM,
                    width: w,
                    height: 0,
                    exclusive_zone: ez,
                },
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bottom_deck_is_one_bottom_anchored_panel() {
        let p = panels_for(LayoutMode::BottomDeck, true, 1920, 1080);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].role, PanelRole::Bottom);
        assert!(p[0].anchor.contains(Anchor::BOTTOM));
        assert!(p[0].anchor.contains(Anchor::LEFT | Anchor::RIGHT));
        assert!(!p[0].anchor.contains(Anchor::TOP));
        // Reflow reserves the panel height.
        assert_eq!(p[0].exclusive_zone, p[0].height as i32);
        assert!(p[0].exclusive_zone > 0);
    }

    #[test]
    fn overlay_mode_sets_zero_exclusive_zone() {
        let p = panels_for(LayoutMode::BottomDeck, false, 1920, 1080);
        assert_eq!(p[0].exclusive_zone, 0);
        // Same surface/anchor as reflow — only the zone differs.
        assert!(p[0].anchor.contains(Anchor::BOTTOM));
    }

    #[test]
    fn side_split_is_two_opposite_edge_columns() {
        let p = panels_for(LayoutMode::SideSplit, true, 1920, 1080);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].role, PanelRole::LeftColumn);
        assert!(p[0].anchor.contains(Anchor::LEFT));
        assert!(!p[0].anchor.contains(Anchor::RIGHT));
        assert_eq!(p[1].role, PanelRole::RightColumn);
        assert!(p[1].anchor.contains(Anchor::RIGHT));
        assert!(!p[1].anchor.contains(Anchor::LEFT));
        // Both columns reserve their width on their own edge → centre reflow.
        assert!(p[0].exclusive_zone > 0 && p[0].exclusive_zone == p[1].exclusive_zone);
        assert!(p[0].anchor.contains(Anchor::TOP | Anchor::BOTTOM));
    }

    #[test]
    fn tier_is_overlay_never_top() {
        assert!(matches!(LAYER, Layer::Overlay));
    }
}
