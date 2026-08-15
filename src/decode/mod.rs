//! Decoders for the formats Skia cannot read.
//!
//! The mirror of [`encode`](crate::encode), and much smaller, because Skia
//! reads nearly everything it is handed. There is one gap, and this crate
//! walks straight into it: Skia encodes no APNG *and decodes none either*.
//! `SkCodec` opens an APNG as the still image its `IDAT` holds and reports
//! one frame, so an animation this crate wrote came back as its first frame
//! with no timing -- while the GIF and WebP beside it round-tripped.
//!
//! That made the promise in the README false for one of the three formats it
//! names. An `Image` reports `frames` and `delays` so an animation can be
//! read in and written back out; for APNG it reported `1` and `[0]`, and
//! re-encoding one produced a single frame. Nothing said so, which is the
//! part that mattered: the call succeeded.
//!
//! So APNG is demuxed here, using the same `png` crate the encoder writes it
//! with -- no new dependency, and the two halves are the same author's
//! reading of the same specification.

pub(crate) mod aom;
pub(crate) mod apng;
pub(crate) mod avif;

/// A decoder part-way through an animation.
///
/// Both formats here code a frame against the ones before it -- APNG as a
/// sub-rectangle drawn over what came before, AVIF as a coded difference --
/// so reaching frame `n` means working through every frame up to it. Doing
/// that from zero on every call makes playing an animation quadratic, and
/// the loop the documentation shows is exactly that: one frame per output
/// frame.
///
/// An [`Image`](crate::image::Image) keeps one of these. It is per format
/// because the state is: APNG holds a `png` reader and a composited canvas,
/// AVIF holds a libaom decoder. A slot holding the wrong one is simply
/// replaced, which costs the run-up once and is never wrong.
pub(crate) enum Playback {
    /// Boxed because it is the larger by far -- a `png` reader and a whole
    /// composited canvas against a libaom handle -- and an enum is as big as
    /// its widest variant wherever it is stored.
    Apng(Box<apng::Playback>),
    Avif(avif::Playback),
}
