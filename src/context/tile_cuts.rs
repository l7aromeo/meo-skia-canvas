//! Where a page's draws would come out otherwise in a tile than in the page
//! -- see `page::RecordingSurface::read_tiled`.
//!
//! A tile is a surface of its own, offset from the page, and two things
//! change a draw in it. Its sides clip what is drawn into it: on the CPU
//! that changes an anti-aliased path where it crosses a side the page does
//! not have, since Skia clips a path's edges to the device before it
//! scan-converts them and an edge clipped at a tile's side is re-quantised
//! along its whole length inside the tile. A shallow triangle across two
//! tiles came out 1,671 pixels apart, up to 128 levels, where a path wholly
//! inside a tile, or across the page's own edge, came out the same. On the
//! GPU a tile's raster is not local to its clip: a path crossing only the
//! page's edge came out a pixel apart, and so did 3 of 250 paths wholly
//! inside one tile.
//!
//! And its offset is folded into the transform, which a shader or an image
//! is sampled through the inverse of. Where that offset is not exact in
//! single precision, or the transform scales, the sample coordinates round
//! otherwise than in the page, on either engine and wherever the draw
//! touches the tile. Over 40 sub-pixel placements, a gradient filling the
//! page came out apart in 37 on the CPU, a translated pattern in 4 on the
//! CPU and 23 on the GPU, an image moved part of a pixel in 13 on the GPU,
//! and an image scaled wholly inside one tile in all 40 on the CPU.
//!
//! What came out alike in every placement is made pixel by pixel from
//! device coordinates with nothing sampled: a rectangle or a glyph run
//! filled with a colour under a scale and a translation, and an image moved
//! by whole pixels and not scaled.

use std::sync::Arc;

use skia_safe::Rect;

/// The most rectangles a page keeps: past that a new one is merged into the
/// one it grows least, which only ever widens what keeps a read off the
/// tiles, and a draw costs a scan of at most this many.
const KEPT: usize = 16;

/// The device rectangles of a page's draws that a tile would make
/// otherwise. Shared as the recorder's layers are: taking a page clones the
/// handle, and the recorder's next note copies the list, of at most
/// [`KEPT`], before changing it. A page none of whose draws needs one holds
/// no list at all.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TileCuts {
    cuts: Arc<Vec<Cut>>,
}

/// One draw's rectangle, in the page's own pixels before any density.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Cut {
    rect: Rect,
    kind: Kind,
}

/// How a tile would come to make a draw otherwise -- see [`TileReach`].
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Copied,
    Edges,
    Sampled,
}

impl Kind {
    /// The kind that keeps a read off every tile either would: alike, or
    /// sampled, which keeps it off every tile it touches on either engine.
    fn join(self, other: Self) -> Self {
        match self == other {
            true => self,
            false => Self::Sampled,
        }
    }
}

/// What a draw is, for the tiles: one a tile makes as the page does, or the
/// device rectangle within which a tile would make it otherwise, and when.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum TileReach {
    /// A tile makes it as the page does, on either engine and at any
    /// density: a colour filling a rectangle or glyphs under a scale and a
    /// translation, or pixels copied in place.
    Safe,
    /// Copied onto whole pixels: a tile makes it as the page does where the
    /// page is read at a density of 1, and samples it otherwise.
    Copied(Rect),
    /// A path's edge: a CPU tile makes it otherwise where it crosses one of
    /// the tile's sides the page does not share, a GPU tile wherever it
    /// touches the tile.
    Cut(Rect),
    /// Sampled through the inverse of its transform: a tile on either
    /// engine makes it otherwise wherever it touches the tile.
    Sampled(Rect),
}

/// Which sides of a tile the page does not share, and so would cut a path
/// the page draws whole.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct InteriorSides {
    pub left: bool,
    pub top: bool,
    pub right: bool,
    pub bottom: bool,
}

impl TileCuts {
    /// Notes a draw as `reach` says.
    pub fn note(&mut self, reach: TileReach) {
        match reach {
            TileReach::Safe => {}
            TileReach::Copied(rect) => self.add(rect, Kind::Copied),
            TileReach::Cut(rect) => self.add(rect, Kind::Edges),
            TileReach::Sampled(rect) => self.add(rect, Kind::Sampled),
        }
    }

    fn add(&mut self, rect: Rect, kind: Kind) {
        // A rectangle that is not finite stands for what it cannot bound:
        // the whole plane.
        let rect = match rect.is_finite() {
            true => rect,
            false => Rect::new(-f32::MAX, -f32::MAX, f32::MAX, f32::MAX),
        };
        if rect.is_empty() {
            return;
        }
        let cuts = Arc::make_mut(&mut self.cuts);
        if cuts.len() < KEPT {
            cuts.push(Cut { rect, kind });
            return;
        }
        let area = |r: &Rect| r.width() * r.height();
        let grows = |cut: &Cut| {
            let mut joined = cut.rect;
            joined.join(rect);
            area(&joined) - area(&cut.rect)
        };
        if let Some(kept) =
            cuts.iter_mut().min_by(|a, b| grows(a).total_cmp(&grows(b)))
        {
            kept.rect.join(rect);
            kept.kind = kept.kind.join(kind);
        }
    }

    /// Whether a read of `tile`, in the page's own pixels, must be served
    /// by the page, on the GPU where `gpu` and read at a density other than
    /// 1 where `scaled` -- see [`TileReach`] for each kind of draw.
    pub fn keep_off(
        &self,
        tile: Rect,
        interior: InteriorSides,
        gpu: bool,
        scaled: bool,
    ) -> bool {
        self.cuts.iter().any(|cut| {
            let r = cut.rect;
            let overlaps = r.left < tile.right
                && r.right > tile.left
                && r.top < tile.bottom
                && r.bottom > tile.top;
            let across_x = |x: f32| r.left < x && r.right > x;
            let across_y = |y: f32| r.top < y && r.bottom > y;
            overlaps
                && match cut.kind {
                    Kind::Copied => scaled,
                    Kind::Sampled => true,
                    Kind::Edges => {
                        gpu || (interior.left && across_x(tile.left))
                            || (interior.right && across_x(tile.right))
                            || (interior.top && across_y(tile.top))
                            || (interior.bottom && across_y(tile.bottom))
                    }
                }
        })
    }

    /// How many rectangles are kept.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.cuts.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: InteriorSides = InteriorSides {
        left: true,
        top: true,
        right: true,
        bottom: true,
    };

    fn tile() -> Rect {
        Rect::new(256.0, 256.0, 512.0, 512.0)
    }

    /// Whether `cuts` keeps `tile()` off on the CPU and on the GPU, read at
    /// a density of 1, with every side interior.
    fn kept(cuts: &TileCuts) -> (bool, bool) {
        (
            cuts.keep_off(tile(), ALL, false, false),
            cuts.keep_off(tile(), ALL, true, false),
        )
    }

    #[test]
    fn a_safe_draw_keeps_nothing() {
        let mut cuts = TileCuts::default();
        cuts.note(TileReach::Safe);
        assert_eq!(cuts.len(), 0);
        assert_eq!(kept(&cuts), (false, false));
        assert!(!cuts.keep_off(tile(), ALL, true, true));
    }

    #[test]
    fn a_cut_inside_a_tile_keeps_only_the_gpu_off() {
        let mut cuts = TileCuts::default();
        cuts.note(TileReach::Cut(Rect::new(300.0, 300.0, 400.0, 400.0)));
        assert_eq!(kept(&cuts), (false, true));
    }

    #[test]
    fn a_cut_across_an_interior_side_keeps_both_off() {
        let mut cuts = TileCuts::default();
        cuts.note(TileReach::Cut(Rect::new(200.0, 300.0, 300.0, 400.0)));
        assert_eq!(kept(&cuts), (true, true));
    }

    #[test]
    fn a_cut_across_the_pages_own_side_keeps_only_the_gpu_off() {
        let mut cuts = TileCuts::default();
        cuts.note(TileReach::Cut(Rect::new(200.0, 300.0, 300.0, 400.0)));
        let page_left = InteriorSides { left: false, ..ALL };
        assert!(!cuts.keep_off(tile(), page_left, false, false));
        assert!(cuts.keep_off(tile(), page_left, true, false));
    }

    #[test]
    fn a_draw_elsewhere_keeps_nothing_off() {
        for reach in [TileReach::Copied, TileReach::Cut, TileReach::Sampled] {
            let mut cuts = TileCuts::default();
            cuts.note(reach(Rect::new(600.0, 600.0, 700.0, 700.0)));
            assert_eq!(kept(&cuts), (false, false));
            assert!(!cuts.keep_off(tile(), ALL, true, true));
        }
    }

    #[test]
    fn a_sampled_draw_inside_a_tile_keeps_both_off() {
        let mut cuts = TileCuts::default();
        cuts.note(TileReach::Sampled(Rect::new(300.0, 300.0, 400.0, 400.0)));
        assert_eq!(kept(&cuts), (true, true));
    }

    #[test]
    fn a_copied_draw_keeps_off_only_a_scaled_read() {
        let mut cuts = TileCuts::default();
        cuts.note(TileReach::Copied(Rect::new(200.0, 300.0, 300.0, 400.0)));
        assert_eq!(kept(&cuts), (false, false));
        assert!(cuts.keep_off(tile(), ALL, false, true));
        assert!(cuts.keep_off(tile(), ALL, true, true));
    }

    #[test]
    fn a_full_list_merges_and_still_keeps_every_cut_off() {
        let mut cuts = TileCuts::default();
        let rect = |i: usize| {
            let x = (i % 8) as f32 * 100.0;
            let y = (i / 8) as f32 * 100.0;
            Rect::new(x + 10.0, y + 10.0, x + 90.0, y + 90.0)
        };
        for i in 0..40 {
            cuts.note(TileReach::Cut(rect(i)));
        }
        assert_eq!(cuts.len(), KEPT);
        for i in 0..40 {
            let r = rect(i);
            let tight = Rect::new(
                r.left + 1.0,
                r.top + 1.0,
                r.right - 1.0,
                r.bottom - 1.0,
            );
            let none = InteriorSides::default();
            assert!(cuts.keep_off(tight, none, true, false), "cut {i} lost");
        }
    }

    #[test]
    fn a_merge_of_two_kinds_keeps_off_what_either_would() {
        let mut cuts = TileCuts::default();
        for i in 0..KEPT {
            let x = i as f32 * 1000.0;
            cuts.note(TileReach::Copied(Rect::new(x, 0.0, x + 10.0, 10.0)));
        }
        // Merged into the first: alone, an edge inside one tile keeps only
        // the GPU off, and a copy neither at a density of 1. Joined, they
        // keep off what a sampled draw does, wider than either.
        cuts.note(TileReach::Cut(Rect::new(1.0, 1.0, 9.0, 9.0)));
        let around = Rect::new(0.0, 0.0, 256.0, 256.0);
        assert!(cuts.keep_off(around, InteriorSides::default(), true, false));
        assert!(cuts.keep_off(around, InteriorSides::default(), false, false));
    }

    #[test]
    fn an_unbounded_cut_keeps_every_tile_off() {
        let mut cuts = TileCuts::default();
        cuts.note(TileReach::Cut(Rect::new(0.0, 0.0, f32::INFINITY, 10.0)));
        assert_eq!(kept(&cuts), (true, true));
    }
}
