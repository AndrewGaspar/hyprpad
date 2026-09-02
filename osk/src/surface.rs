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

use crate::layout::{Keyboard, LayoutEngine, LayoutMode, PanelRole};
use crate::theme::Geom;

/// The one layer tier the OSK ever uses.
pub const LAYER: Layer = Layer::Overlay;
/// The OSK never takes keyboard focus.
pub const INTERACTIVITY: KeyboardInteractivity = KeyboardInteractivity::None;
/// Layer namespace; the anchor for compositor-side layer rules and for
/// `hyprctl layers` identification.
pub const NAMESPACE: &str = "hyprpad-osk";

/// The anchoring + sizing + exclusive-zone spec for one layer surface.
#[derive(Clone, Copy, Debug)]
pub struct PanelSpec {
    pub role: PanelRole,
    pub anchor: Anchor,
    /// Requested width in px — the panel's **content size** (only as wide as the
    /// keys need). Always non-zero now: the surface is sized to content, never
    /// spanned across the screen.
    pub width: u32,
    /// Requested height in px — the panel's content size. Always non-zero.
    pub height: u32,
    /// Exclusive zone in px along the anchored edge. `>0` reflows workspace
    /// content; `0` floats over it (osk-technology.md §2.3 / task Mode A vs B).
    /// Bottom mode reserves the surface's *height*; a side column reserves its
    /// *width*.
    pub exclusive_zone: i32,
}

/// Compute the panel spec(s) for a show request. Every panel is **sized to
/// content** — its `width`/`height` come from
/// [`LayoutEngine::content_size`] over `keyboard` + `geom`, a pure function of
/// the key grid and the theme's geometry tokens (never the screen size).
///
/// The docking uses a **single-edge anchor** so the compositor centres the
/// surface on the perpendicular axis:
///
/// * [`LayoutMode::BottomDeck`] → **one** panel anchored to the bottom edge
///   only, so it is centred horizontally on the bottom (a narrow, content-wide
///   deck rather than a full-width bar). Reflow reserves its *height* as the
///   bottom exclusive zone; overlay reserves `0`.
/// * [`LayoutMode::SideSplit`] → **two** panels, each anchored to one vertical
///   edge only, so each is docked to its edge and centred *vertically* (a short
///   content-tall column rather than a full-height rail). Reflow reserves each
///   column's *width* as its edge's exclusive zone. Two surfaces (not one with a
///   transparent hole) because an exclusive zone is a single scalar along one
///   edge — you cannot carve a gap in the middle of one surface, so genuine
///   centre reflow *requires* an independent exclusive zone on each edge. The
///   cost is two surfaces to keep in sync; the benefit is real two-sided reflow
///   and each thumb owning its own physical surface.
///
/// `strip` is how many candidate slots the prediction strip holds (0 for none);
/// it is part of the content, so it grows the panel's height and, in reflow
/// mode, the bottom exclusive zone with it.
pub fn panels_for(
    mode: LayoutMode,
    reflow: bool,
    keyboard: &Keyboard,
    geom: &Geom,
    strip: usize,
) -> Vec<PanelSpec> {
    let eng = LayoutEngine::new(keyboard, mode).with_strip(strip);
    let size = |role: PanelRole| {
        let (w, h) = eng.content_size(role, geom);
        (w.ceil() as u32, h.ceil() as u32)
    };
    match mode {
        LayoutMode::BottomDeck => {
            let (w, h) = size(PanelRole::Bottom);
            vec![PanelSpec {
                role: PanelRole::Bottom,
                // Bottom edge only → centred horizontally on the bottom anchor.
                anchor: Anchor::BOTTOM,
                width: w,
                height: h,
                exclusive_zone: if reflow { h as i32 } else { 0 },
            }]
        }
        LayoutMode::SideSplit => {
            let (lw, lh) = size(PanelRole::LeftColumn);
            let (rw, rh) = size(PanelRole::RightColumn);
            vec![
                PanelSpec {
                    role: PanelRole::LeftColumn,
                    // Left edge only → docked left, centred vertically.
                    anchor: Anchor::LEFT,
                    width: lw,
                    height: lh,
                    exclusive_zone: if reflow { lw as i32 } else { 0 },
                },
                PanelSpec {
                    role: PanelRole::RightColumn,
                    // Right edge only → docked right, centred vertically.
                    anchor: Anchor::RIGHT,
                    width: rw,
                    height: rh,
                    exclusive_zone: if reflow { rw as i32 } else { 0 },
                },
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn kb() -> Keyboard {
        Keyboard::qwerty()
    }
    fn geom() -> Geom {
        Theme::default().geom
    }

    #[test]
    fn bottom_deck_is_one_bottom_centered_content_sized_panel() {
        let p = panels_for(LayoutMode::BottomDeck, true, &kb(), &geom(), 0);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].role, PanelRole::Bottom);
        // Bottom edge only → compositor centres it horizontally.
        assert!(p[0].anchor.contains(Anchor::BOTTOM));
        assert!(!p[0].anchor.contains(Anchor::LEFT));
        assert!(!p[0].anchor.contains(Anchor::RIGHT));
        assert!(!p[0].anchor.contains(Anchor::TOP));
        // Content-sized: a fixed, non-zero width far below a 2048 screen.
        assert!(p[0].width > 0 && p[0].height > 0);
        assert!(p[0].width < 1600, "bottom width {} should be content-sized", p[0].width);
        // Reflow reserves the panel HEIGHT (bottom exclusive zone).
        assert_eq!(p[0].exclusive_zone, p[0].height as i32);
    }

    #[test]
    fn overlay_mode_sets_zero_exclusive_zone() {
        let p = panels_for(LayoutMode::BottomDeck, false, &kb(), &geom(), 0);
        assert_eq!(p[0].exclusive_zone, 0);
        // Same surface/anchor as reflow — only the zone differs.
        assert!(p[0].anchor.contains(Anchor::BOTTOM));
        assert!(p[0].width > 0 && p[0].height > 0);
    }

    #[test]
    fn side_split_is_two_opposite_edge_content_sized_columns() {
        let full_h: u32 = 1254; // this machine's usable height; columns must be shorter
        let p = panels_for(LayoutMode::SideSplit, true, &kb(), &geom(), 0);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].role, PanelRole::LeftColumn);
        assert!(p[0].anchor.contains(Anchor::LEFT));
        assert!(!p[0].anchor.contains(Anchor::RIGHT));
        // Single vertical edge → NOT stretched top-to-bottom; centred vertically.
        assert!(!p[0].anchor.contains(Anchor::TOP));
        assert!(!p[0].anchor.contains(Anchor::BOTTOM));
        assert_eq!(p[1].role, PanelRole::RightColumn);
        assert!(p[1].anchor.contains(Anchor::RIGHT));
        assert!(!p[1].anchor.contains(Anchor::LEFT));
        // Each column reserves its own WIDTH on its own edge → centre reflow.
        assert_eq!(p[0].exclusive_zone, p[0].width as i32);
        assert_eq!(p[1].exclusive_zone, p[1].width as i32);
        // Content-tall, not full-height.
        assert!(p[0].height < full_h && p[1].height < full_h);
        assert!(p[0].width > 0 && p[1].width > 0);
    }

    #[test]
    fn tier_is_overlay_never_top() {
        assert!(matches!(LAYER, Layer::Overlay));
    }

    #[test]
    fn a_candidate_strip_grows_the_panel_and_its_exclusive_zone() {
        let g = geom();
        let plain = panels_for(LayoutMode::BottomDeck, true, &kb(), &g, 0);
        let with = panels_for(LayoutMode::BottomDeck, true, &kb(), &g, 3);
        assert_eq!(plain[0].width, with[0].width, "the strip changes no width");
        let grew = with[0].height - plain[0].height;
        assert!(
            grew >= g.strip_height as u32 && grew <= (g.strip_height + g.gap).ceil() as u32 + 1,
            "the strip adds its own band plus the row gap, got {grew}"
        );
        // Reflow still reserves exactly the panel height, strip included, so
        // content is not left behind the suggestions.
        assert_eq!(with[0].exclusive_zone, with[0].height as i32);
    }
}
