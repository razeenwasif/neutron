//! A single-texture atlas for system file icons.
//!
//! # Why one texture rather than one per icon
//!
//! egui batches draw calls by texture. Uploading each icon as its own
//! `TextureHandle` would break the batch at every row whose icon differs from
//! its neighbour's, turning one draw call for the whole listing into dozens.
//! It also costs a GPU allocation and a bind-group per distinct file type,
//! which for a directory with a hundred extensions is a hundred of each.
//!
//! Instead every icon occupies one cell of a fixed grid in a single texture,
//! and a row draws by sampling its cell's UV rect. All rows share one texture,
//! so the whole listing remains a single batch no matter how varied it is.
//!
//! # Growth
//!
//! The atlas never shrinks and never evicts. That is a deliberate bound rather
//! than an oversight: cells are keyed by *extension*, not by file, so the
//! population is the number of distinct file types the user has looked at. A
//! very busy session might reach a few hundred; [`CAPACITY`] allows 1024, which
//! at 32px is a 4MB texture. Eviction would add a whole cache-invalidation
//! problem to save nothing that matters.

use egui::{Color32, ColorImage, Rect, TextureHandle, TextureOptions, pos2};

/// Edge of one cell, in pixels. Must match the shell's requested icon size.
pub const CELL_PX: usize = 32;

/// Cells per row and column. 32×32 cells of 32px each — a 1024×1024 texture,
/// which is within the guaranteed minimum on every GPU wgpu will run on.
const GRID: usize = 32;

/// Maximum distinct icons.
pub const CAPACITY: usize = GRID * GRID;

/// Where an icon lives in the atlas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot(u16);

/// The atlas texture and its free-slot cursor.
pub struct IconAtlas {
    texture: Option<TextureHandle>,
    /// Whole-atlas pixel buffer, kept so a new icon can be written into its
    /// cell and the texture re-uploaded.
    ///
    /// 4MB resident. The alternative — a partial GPU update — is not something
    /// egui's texture API exposes, and re-uploading on the handful of frames
    /// where an icon actually arrives is cheaper than the machinery to avoid
    /// it.
    pixels: ColorImage,
    next: usize,
    /// Set when `pixels` has changed and the texture needs re-uploading.
    dirty: bool,
}

impl Default for IconAtlas {
    fn default() -> Self {
        Self::new()
    }
}

impl IconAtlas {
    pub fn new() -> Self {
        Self {
            texture: None,
            pixels: ColorImage::filled(
                [GRID * CELL_PX, GRID * CELL_PX],
                Color32::TRANSPARENT,
            ),
            next: 0,
            dirty: false,
        }
    }

    /// Claims a slot and writes `rgba` into it.
    ///
    /// `rgba` must be exactly `CELL_PX × CELL_PX × 4` bytes; the shell's
    /// resolver guarantees this by rescaling. Returns `None` when the atlas is
    /// full, which callers treat as "draw the fallback glyph" rather than as an
    /// error.
    pub fn insert(&mut self, rgba: &[u8]) -> Option<Slot> {
        if rgba.len() != CELL_PX * CELL_PX * 4 || self.next >= CAPACITY {
            return None;
        }

        let slot = Slot(self.next as u16);
        let (ox, oy) = origin(slot);

        for y in 0..CELL_PX {
            for x in 0..CELL_PX {
                let s = (y * CELL_PX + x) * 4;
                // Straight (non-premultiplied) RGBA in, which is what
                // `Color32::from_rgba_unmultiplied` expects — the shell's
                // resolver un-premultiplies precisely so this is the case.
                self.pixels[(ox + x, oy + y)] = Color32::from_rgba_unmultiplied(
                    rgba[s],
                    rgba[s + 1],
                    rgba[s + 2],
                    rgba[s + 3],
                );
            }
        }

        self.next += 1;
        self.dirty = true;
        Some(slot)
    }

    pub fn is_full(&self) -> bool {
        self.next >= CAPACITY
    }

    pub fn len(&self) -> usize {
        self.next
    }

    pub fn is_empty(&self) -> bool {
        self.next == 0
    }

    /// Uploads pending changes and returns the texture to sample from.
    ///
    /// Call once per frame before drawing rows. Returns `None` only before the
    /// first icon has ever landed, when there is nothing to draw.
    pub fn texture(&mut self, ctx: &egui::Context) -> Option<&TextureHandle> {
        if self.next == 0 {
            return None;
        }

        if self.dirty || self.texture.is_none() {
            let options = TextureOptions {
                // Linear, because icons are drawn smaller than their 32px
                // source. Nearest would alias the diagonals badly at 20pt.
                magnification: egui::TextureFilter::Linear,
                minification: egui::TextureFilter::Linear,
                ..Default::default()
            };

            match &mut self.texture {
                Some(t) => t.set(self.pixels.clone(), options),
                None => {
                    self.texture =
                        Some(ctx.load_texture("neutron-icons", self.pixels.clone(), options))
                }
            }
            self.dirty = false;
        }

        self.texture.as_ref()
    }

    /// The UV rectangle for `slot`, in 0..1 texture coordinates.
    pub fn uv(&self, slot: Slot) -> Rect {
        uv_of(slot)
    }
}

/// Top-left pixel of a slot's cell.
fn origin(slot: Slot) -> (usize, usize) {
    let i = slot.0 as usize;
    ((i % GRID) * CELL_PX, (i / GRID) * CELL_PX)
}

/// A slot's UV rectangle.
///
/// Inset by half a texel on every side. Sampling exactly on the cell boundary
/// with linear filtering blends in the neighbouring cell, which shows up as a
/// sliver of an unrelated icon along one edge — the classic atlas bleed.
fn uv_of(slot: Slot) -> Rect {
    let (ox, oy) = origin(slot);
    let full = (GRID * CELL_PX) as f32;
    let half_texel = 0.5 / full;

    Rect::from_min_max(
        pos2(ox as f32 / full + half_texel, oy as f32 / full + half_texel),
        pos2(
            (ox + CELL_PX) as f32 / full - half_texel,
            (oy + CELL_PX) as f32 / full - half_texel,
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(value: u8) -> Vec<u8> {
        vec![value; CELL_PX * CELL_PX * 4]
    }

    #[test]
    fn slots_are_handed_out_in_order_and_tile_the_grid() {
        let mut atlas = IconAtlas::new();
        let a = atlas.insert(&cell(1)).expect("first slot");
        let b = atlas.insert(&cell(2)).expect("second slot");

        assert_eq!(origin(a), (0, 0));
        assert_eq!(origin(b), (CELL_PX, 0));
        // The row must wrap rather than running off the right edge.
        assert_eq!(origin(Slot(GRID as u16)), (0, CELL_PX));
        assert_eq!(atlas.len(), 2);
    }

    #[test]
    fn pixels_land_in_the_right_cell() {
        let mut atlas = IconAtlas::new();
        atlas.insert(&cell(0)).unwrap();
        let second = atlas.insert(&cell(200)).unwrap();

        let (ox, oy) = origin(second);
        assert_eq!(atlas.pixels[(ox, oy)].a(), 200);
        assert_eq!(
            atlas.pixels[(ox + CELL_PX - 1, oy + CELL_PX - 1)].a(),
            200,
            "the cell's far corner was not written"
        );
        // And must not have spilled into the neighbour.
        assert_eq!(atlas.pixels[(0, 0)].a(), 0);
    }

    #[test]
    fn a_wrongly_sized_image_is_refused() {
        // Accepting it would write past the cell and corrupt every icon after
        // it — a failure that shows up as garbage in unrelated rows.
        let mut atlas = IconAtlas::new();
        assert!(atlas.insert(&[0u8; 16]).is_none());
        assert!(atlas.insert(&cell(1)[..CELL_PX * CELL_PX * 4 - 1]).is_none());
        assert_eq!(atlas.len(), 0, "a refused insert must not consume a slot");
    }

    #[test]
    fn a_full_atlas_refuses_rather_than_wrapping() {
        // Wrapping would silently overwrite slot 0, so every row still holding
        // that slot would start drawing someone else's icon.
        let mut atlas = IconAtlas::new();
        atlas.next = CAPACITY;
        assert!(atlas.insert(&cell(1)).is_none());
        assert!(atlas.is_full());
    }

    #[test]
    fn uvs_stay_inside_their_cell() {
        // Bleed guard: a slot's UV rect must not reach into a neighbour, or a
        // sliver of an unrelated icon appears along the edge.
        let a = uv_of(Slot(0));
        let b = uv_of(Slot(1));
        assert!(a.max.x < b.min.x, "adjacent cells overlap in u");

        let below = uv_of(Slot(GRID as u16));
        assert!(a.max.y < below.min.y, "stacked cells overlap in v");

        // And within the texture.
        assert!(a.min.x > 0.0 && a.min.y > 0.0);
        let last = uv_of(Slot((CAPACITY - 1) as u16));
        assert!(last.max.x < 1.0 && last.max.y < 1.0);
    }

    #[test]
    fn an_empty_atlas_has_no_texture_to_draw() {
        // Uploading a 4MB fully transparent texture on the first frame, before
        // any icon has been resolved, would be pure waste.
        let mut atlas = IconAtlas::new();
        assert!(atlas.is_empty());
        let ctx = egui::Context::default();
        assert!(atlas.texture(&ctx).is_none());
    }

    /// Keyed by extension, so the population is distinct file types seen, not
    /// files. If this ever needs raising it is a sign the key changed.
    /// Checked at compile time, since it is a constant.
    const _: () = assert!(CAPACITY >= 1024);
}
