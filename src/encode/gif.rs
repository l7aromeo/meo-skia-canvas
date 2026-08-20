//! GIF, which Skia can read and cannot write.

use std::slice;

use rayon::prelude::*;

use gif::{DisposalMethod, Encoder, Frame as GifFrame, Repeat};
use quantette::{
    ImageRef, PaletteSize, Pipeline, QuantizeMethod, deps::palette::Srgb,
};

use super::{
    Frame, FrameEncoder, FrameSink, SequenceSpec, Sink, changed_region,
    crop_bytes,
};

/// Alpha at or above which a pixel is drawn rather than left transparent.
///
/// GIF has one bit of alpha: a palette index is either the transparent one
/// or it is not, so every partly transparent pixel has to be rounded one way
/// or the other and the only question is where.
///
/// The midpoint, which is not what the `gif` crate this module writes
/// through does on its own -- its own conversion keeps any pixel with a
/// non-zero alpha, so a barely-there edge comes back fully opaque. Rounding
/// at the halfway mark instead puts the hard edge where the source was half
/// faded, which is nearer to where a viewer would say the shape ends.
/// Neither is free: a soft edge becomes a hard one either way.
const OPAQUE_AT: u8 = 128;

/// The palette index reserved for transparency, in a frame that needs one.
///
/// The last, so the quantizer is asked for the other 255 and its own indices
/// need no shifting.
const TRANSPARENT: u8 = 255;

/// Milliseconds in the hundredth of a second GIF89a counts a frame delay in.
///
/// The whole reason [`Tick`] exists: most frame rates are not a whole number
/// of these, so the remainder has to be carried rather than dropped.
const MS_PER_CENTISECOND: u64 = 10;

/// Added before the division so a delay lands on the nearest centisecond
/// rather than the one below it.
///
/// Half of [`MS_PER_CENTISECOND`], which is what makes it round-half-up:
/// 33ms is nearer 30 than 40, and truncating ran every default-rate
/// animation twenty percent slow.
const ROUND_HALF_UP: u64 = MS_PER_CENTISECOND / 2;

/// Bytes in one pixel of the frames handed to this module.
const BYTES_PER_PIXEL: usize = 4;

/// The multiple a frame's origin has to land on.
///
/// One, meaning none: GIF's image descriptor stores both offsets as whole
/// pixels, as APNG does and unlike WebP, which halves them.
const FRAME_ORIGIN_GRAIN: u32 = 1;

/// The rectangle a frame carries when nothing changed at all.
///
/// A still passage of an animation is two identical frames, and an image
/// descriptor of no width or height describes nothing, so the frame still has
/// to carry a pixel. Any pixel will do, and the one at the origin is already
/// the right colour.
const SMALLEST_FRAME: (u32, u32, u32, u32) = (0, 0, 1, 1);

pub(crate) struct Gif;

impl FrameEncoder for Gif {
    fn start<'a>(
        &self,
        spec: &SequenceSpec,
        out: &'a mut dyn Sink,
    ) -> Result<Box<dyn FrameSink + 'a>, String> {
        let width = dimension(spec.width, "width")?;
        let height = dimension(spec.height, "height")?;

        let mut encoder = Encoder::new(out, width, height, &[])
            .map_err(|e| format!("Could not start a GIF: {e}"))?;
        encoder
            .set_repeat(repeat(spec.loops))
            .map_err(|e| format!("Could not set the GIF loop count: {e}"))?;

        Ok(Box::new(GifSink {
            encoder,
            width,
            height,
            tick: Tick::default(),
            pending: None,
        }))
    }
}

struct GifSink<'a> {
    encoder: Encoder<&'a mut dyn Sink>,
    width: u16,
    height: u16,
    /// The running clock, so a rate that does not divide by GIF's tick
    /// still averages out across the animation.
    tick: Tick,
    /// The frame accepted but not yet written.
    ///
    /// One frame is always held back, because a frame's disposal is a fact
    /// about the frame *after* it -- see [`erases`]. It is held as pixels
    /// rather than as palette indices because the rectangle it carries can
    /// still widen, and quantizing before that is settled would mean
    /// quantizing twice.
    pending: Option<Pending>,
}

/// A frame accepted, placed, and waiting to learn its disposal.
struct Pending {
    eight: Vec<u8>,
    width: u32,
    delay: u16,
    /// The rectangle it differs from the frame before it in.
    region: (u32, u32, u32, u32),
}

/// Whether going from `previous` to `current` stops a pixel being drawn.
///
/// This is what GIF cannot express inside a frame. A transparent index means
/// "leave what is underneath", not "clear this", so a rectangle laid over the
/// canvas can add pixels and change them and can never take one away. The
/// only eraser the format has is disposing a frame to the background, which
/// happens *after* that frame is shown and clears the rectangle it covered.
/// So erasing is something the frame before has to have arranged, and knowing
/// whether to arrange it means looking one frame ahead.
///
/// This is the same fact the disposal comment used to record as a reason the
/// encoder could not do better: it was handed one frame at a time and could
/// not look ahead. It is handed a batch now.
fn erases(previous: &[u8], current: &[u8]) -> bool {
    previous
        .as_chunks::<BYTES_PER_PIXEL>()
        .0
        .iter()
        .zip(current.as_chunks::<BYTES_PER_PIXEL>().0.iter())
        .any(|(was, now)| was[3] >= OPAQUE_AT && now[3] < OPAQUE_AT)
}

impl GifSink<'_> {
    /// The whole canvas, as a rectangle.
    fn whole(&self) -> (u32, u32, u32, u32) {
        (0, 0, u32::from(self.width), u32::from(self.height))
    }

    /// Writes the held-back frame, now that its disposal is known.
    ///
    /// `cleared` says the frame after this one needs a clear canvas, which is
    /// the one case a rectangle cannot serve: disposing to the background
    /// clears only what this frame covered, so to clear the canvas this frame
    /// has to have been the canvas.
    fn flush(&mut self, cleared: bool) -> Result<(), String> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        let region = match cleared {
            true => self.whole(),
            false => pending.region,
        };
        let (x, y, width, height) = region;

        // Quantized against the rectangle rather than the page. A palette
        // chosen for the part that moved describes it better than one
        // averaged over a page that mostly did not, and it is the smaller
        // job by the ratio of their areas.
        let cropped = match region == self.whole() {
            true => pending.eight,
            false => crop_bytes(
                &pending.eight,
                pending.width,
                BYTES_PER_PIXEL,
                region,
            ),
        };
        let (palette, indices, transparent) = quantize(width, height, &cropped);

        let mut written = GifFrame {
            left: x as u16,
            top: y as u16,
            width: width as u16,
            height: height as u16,
            palette: Some(palette),
            buffer: indices.into(),
            delay: pending.delay,
            // Keep what is under this frame, so that everything outside its
            // rectangle survives into the next one. That is the whole point
            // of sending a rectangle, and the same answer `apng.rs` and
            // `webp.rs` give.
            //
            // Background only where the next frame has a pixel to erase and
            // no way to erase it. Every frame took this branch once, when
            // each covered the whole canvas and the choice cost nothing:
            // clearing a canvas about to be painted over completely is the
            // same picture.
            dispose: match cleared {
                true => DisposalMethod::Background,
                false => DisposalMethod::Keep,
            },
            ..GifFrame::default()
        };
        written.transparent = transparent;
        self.encoder
            .write_frame(&written)
            .map_err(|e| format!("Could not write a GIF frame: {e}"))
    }
}

impl FrameSink for GifSink<'_> {
    fn write_frame(&mut self, frame: &Frame) -> Result<(), String> {
        self.write_batch(slice::from_ref(frame))
    }

    /// Places the batch's frames, then writes them a frame behind.
    ///
    /// Placing is where the work went: a frame carries the rectangle it
    /// differs from its predecessor in, so the quantizing and the LZW that
    /// follow are over that rectangle instead of over the page. Both shrink
    /// with it, and so does the file.
    ///
    /// The writing runs one frame behind because a frame's disposal belongs
    /// to the frame after it, and the last frame of a batch has not met its
    /// successor yet. It waits for the next batch, or for `finish`.
    fn write_batch(&mut self, frames: &[Frame]) -> Result<(), String> {
        // Narrowed once. This reached `frame.eight()` four times a frame --
        // twice inside `quantize`, once for the transparent index, and once
        // more in the rewrite loop -- and on a float canvas each one converts
        // and allocates the whole page, about 8 MB at 1080p. The alpha scan
        // behind the transparent index ran twice over the same bytes for the
        // same reason.
        let narrowed = frames
            .par_iter()
            .map(|frame| frame.eight().into_owned())
            .collect::<Vec<_>>();

        for (frame, eight) in frames.iter().zip(narrowed) {
            let whole = (0, 0, frame.width, frame.height);

            // The frame before this one is exactly the one being held back,
            // so there is no second copy of it to keep. Nothing precedes the
            // animation's first frame, which is therefore the whole canvas
            // and erases nothing.
            let (region, cleared) = match self.pending.as_ref() {
                None => (whole, false),
                Some(held) => (
                    changed_region(
                        &held.eight,
                        &eight,
                        frame.width,
                        frame.height,
                        BYTES_PER_PIXEL,
                        FRAME_ORIGIN_GRAIN,
                    )
                    .unwrap_or(SMALLEST_FRAME),
                    erases(&held.eight, &eight),
                ),
            };

            self.flush(cleared)?;
            self.pending = Some(Pending {
                eight,
                width: frame.width,
                delay: self.tick.next(frame.delay_ms),
                // A frame whose predecessor just cleared the canvas for it
                // has to repaint all of it, not the part that changed.
                region: match cleared {
                    true => whole,
                    false => region,
                },
            });
        }
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<(), String> {
        // Nothing follows the last frame, so nothing needs a clear canvas
        // after it.
        self.flush(false)?;
        // GIF's trailer is written when the encoder drops, and dropping is
        // where a write error would otherwise be swallowed. `into_inner`
        // writes it now and hands the error back.
        self.encoder
            .into_inner()
            .map(|_| ())
            .map_err(|e| format!("Could not finish the GIF: {e}"))
    }
}

/// The running clock that turns millisecond delays into GIF's hundredths.
///
/// GIF's tick is ten milliseconds and most frame rates are not a whole
/// number of them. Rounding each frame on its own drops the remainder every
/// time: 30fps became three hundredths a frame, which plays at 33.3fps, so
/// an animation ran eleven percent fast and a ten-second one finished more
/// than a second early. Keeping the running total and handing out the
/// difference spends the remainder instead -- the frames come out 3, 4, 3 --
/// and twelve frames of 30fps last exactly 400ms.
#[derive(Default)]
struct Tick {
    elapsed_ms: u64,
    written_cs: u64,
}

impl Tick {
    fn next(&mut self, delay_ms: u32) -> u16 {
        self.elapsed_ms += u64::from(delay_ms);
        let target = (self.elapsed_ms + ROUND_HALF_UP) / MS_PER_CENTISECOND;
        let mut delay = target.saturating_sub(self.written_cs);
        // A zero delay does not play fast. Browsers clamp it up -- Firefox
        // renders any frame of 10ms or less at 100ms, Chrome the same -- so
        // the honest rounding of a 4ms frame produced the slowest playback
        // the format has, and asking for 240fps got 10. One hundredth is
        // the shortest delay that survives a browser; past that a caller
        // wants a format other than GIF.
        if delay == 0 && delay_ms > 0 {
            delay = 1;
            self.elapsed_ms = self
                .elapsed_ms
                .max((self.written_cs + 1) * MS_PER_CENTISECOND);
        }
        // Credit the clock with what the file actually carries, not with
        // what was asked for. GIF stores a delay in sixteen bits, so a frame
        // past 655.35s is written short -- and adding the unclamped figure
        // here told the clock time had passed that no reader will ever wait
        // through. Every later frame then found `target` already behind
        // `written_cs`, took the zero-floor branch, and was written as one
        // hundredth of a second whatever it asked for.
        let written = delay.min(u64::from(u16::MAX));
        self.written_cs += written;
        if written < delay {
            // The time the format could not carry is gone -- no reader will
            // ever wait it out -- so the clock forgives it rather than
            // charging it to the frames that follow.
            //
            // Two bugs met here. The total used to be credited with the
            // unclamped delay, so it ran ahead of the file. Fixing only that
            // left the debt in `elapsed_ms` instead, and the clock spent
            // every later frame trying to pay it: after one 700-second
            // frame, the next asked for a second and was written as 45.65.
            // Both ways, one overlong frame distorted the whole rest of the
            // animation.
            self.elapsed_ms = self.written_cs * MS_PER_CENTISECOND;
        }
        written as u16
    }
}

/// GIF measures the canvas in sixteen bits.
fn dimension(value: u32, axis: &str) -> Result<u16, String> {
    u16::try_from(value).map_err(|_| {
        format!("A GIF's {axis} must be at most {} (got {value})", u16::MAX)
    })
}

/// The transparent palette index, if this frame has a pixel that needs one.
///
/// Takes the narrowed bytes rather than the `Frame`, so a caller that already
/// has them does not convert the page again to ask.
fn transparent_index(eight: &[u8]) -> Option<u8> {
    eight
        .as_chunks::<4>()
        .0
        .iter()
        .any(|pixel| pixel[3] < OPAQUE_AT)
        .then_some(TRANSPARENT)
}

/// GIF counts repeats after the first play; this crate counts plays.
///
/// One play is the case GIF cannot state. The loop count lives in a
/// NETSCAPE application block whose zero means "forever", so there is no
/// number that means "once" -- leaving the block out is the convention for
/// it, and what the `gif` crate does when asked for `Finite(0)`.
///
/// The cost is that a file asking for one play declares nothing, so the
/// answer depends on the decoder and on when it is asked. Skia's reports
/// forever before it has decoded any frames and once afterwards, both from
/// the same bytes.
fn repeat(loops: Option<u32>) -> Repeat {
    match loops {
        None => Repeat::Infinite,
        // `saturating_sub` rather than `- 1`: `Some(0)` is not a number of
        // plays anyone means, and reading it as "play once" is the only
        // answer that is not a panic or an accidental eternity.
        Some(plays) => {
            Repeat::Finite(plays.saturating_sub(1).min(u16::MAX as u32) as u16)
        }
    }
}

/// A palette and one index per pixel, in k-means-refined Oklab.
///
/// The transparent pixels keep whatever colour they carry while the palette
/// is chosen, and are overwritten with [`TRANSPARENT`] afterwards. Taking
/// them out first would mean remapping the survivors back into place
/// afterwards, which is more code than the entries it saves.
///
/// How many entries that is depends on what made them transparent, and the
/// two cases are further apart than they look. Pixels nothing was drawn on
/// read back as transparent black throughout -- measured: one distinct
/// colour across a whole untouched canvas -- so they cost a single cluster.
/// Pixels drawn *at* a low alpha do not: a gradient filled at `alpha 0.3`
/// measured 79 distinct colours below the threshold, and each of those can
/// take a cluster from the opaque part of the picture. So a drawing that
/// fades out costs real palette, and one on a clear background costs one
/// entry.
fn quantize(
    width: u32,
    height: u32,
    eight: &[u8],
) -> (Vec<u8>, Vec<u8>, Option<u8>) {
    let transparent = transparent_index(eight);
    let opaque: Vec<Srgb<u8>> = eight
        .as_chunks::<BYTES_PER_PIXEL>()
        .0
        .iter()
        .map(|pixel| Srgb::new(pixel[0], pixel[1], pixel[2]))
        .collect();

    let image = ImageRef::new(width, height, &opaque).unwrap_or_default();

    let quantized = Pipeline::new()
        .palette_size(match transparent {
            Some(_) => PaletteSize::from_u8_clamped(TRANSPARENT),
            None => PaletteSize::MAX,
        })
        // k-means, which the pipeline runs in Oklab.
        //
        // Wu's method alone is the fast path, and "visibly coarser on a
        // gradient" was the whole of the reason given for not taking it.
        // Measured since, on three 1200x900 pages against the pixels that
        // were drawn -- root-mean-square error a channel, and the file the
        // choice produces:
        //
        //           k-means            Wu
        //   grad    18.5ms  73.9KB  rms 2.25    16.7ms  73.4KB  rms 2.85
        //   chart   16.0    18.1        0.56    11.9    18.1        0.53
        //   photo   22.1   201.7        8.19    14.8   218.8        9.07
        //
        // So the coarseness is real and it is not only the gradient: the
        // photographic page is both further from its pixels and 17 KB
        // larger under Wu. Wu is 35% to 50% faster and marginally better on
        // a flat chart, which is the one drawing that has few enough colours
        // for the refinement to have nothing to do.
        //
        // The sample budget is left at the crate's own 262144 pixels. It is
        // already at the knee: doubling it changes no error in the second
        // decimal and costs 22% to 34% of the time, and halving it is 6% to
        // 16% faster for the photographic page's 8.10 becoming 8.19 -- a cut
        // on the one axis k-means is here for.
        //
        // No ditherer, which is a choice rather than an omission. Floyd-
        // Steinberg roughly doubles the file -- measured on the same three
        // drawings at 600x450, the gradient 30.8 KB to 65.7 and the
        // photographic page 58.8 to 134.5 -- and raises the error on all
        // three at once, 2.17 to 2.58, 1.13 to 1.32 and 8.19 to 9.99. It
        // gives LZW noise to encode and moves every pixel rather than the
        // ones that needed it.
        .quantize_method(QuantizeMethod::kmeans())
        .parallel(true)
        .input_image(image)
        .output_srgb8_indexed_image();

    let mut palette: Vec<u8> = quantized
        .palette()
        .iter()
        .flat_map(|color| [color.red, color.green, color.blue])
        .collect();
    let mut indices = quantized.indices().to_vec();

    if transparent.is_some() {
        // The reserved entry has to exist for the index to name it, and its
        // colour is never drawn.
        palette.resize((TRANSPARENT as usize + 1) * 3, 0);
        for (index, pixel) in
            indices.iter_mut().zip(eight.as_chunks::<4>().0.iter())
        {
            if pixel[3] < OPAQUE_AT {
                *index = TRANSPARENT;
            }
        }
    }

    // Handed back rather than recomputed by the caller, which was the second
    // full alpha scan.
    (palette, indices, transparent)
}

#[cfg(test)]
mod tests {
    use crate::{
        encode::{FrameDepth, Pixels},
        export::ChromaSampling,
    };
    use gif::DecodeOptions;
    use std::io::Cursor;

    use super::*;
    use crate::{
        encode::color::ColorProfile, export::ImageFormat,
        pixels::PixelColorSpace,
    };

    /// Three frames, two pixels wide, each a solid primary.
    fn frames() -> Vec<Frame> {
        [([255u8, 0, 0], 100), ([0, 255, 0], 200), ([0, 0, 255], 350)]
            .into_iter()
            .map(|([r, g, b], delay_ms)| Frame {
                pixels: Pixels::Eight(vec![r, g, b, 255, r, g, b, 255]),
                width: 2,
                height: 1,
                delay_ms,
            })
            .collect()
    }

    /// Encodes `frames` through the sink seam, as a caller would.
    fn encoded(frames: &[Frame], loops: Option<u32>) -> Vec<u8> {
        // SAFETY: every frame in these tests is well formed, so the checks
        // in `start` cannot fire.
        let first = &frames[0];
        let spec = SequenceSpec {
            chroma: ChromaSampling::Full,
            lossless: false,
            width: first.width,
            height: first.height,
            frames: frames.len(),
            loops,
            quality: 90.0,
            density: 1.0,
            color: ColorProfile::of(PixelColorSpace::Srgb),
            space: PixelColorSpace::Srgb,
            depth: FrameDepth::Eight,
            bits: None,
        };
        let mut bytes = Cursor::new(Vec::new());
        {
            let mut sink =
                super::super::start(ImageFormat::Gif, &spec, &mut bytes)
                    .expect("the spec is well formed");
            for frame in frames {
                sink.write_frame(frame).expect("a well formed frame");
            }
            sink.finish().expect("the encoder closes");
        }
        bytes.into_inner()
    }

    /// The disposal method recorded for every frame of `bytes`.
    fn disposals(bytes: &[u8]) -> Vec<DisposalMethod> {
        let mut decoder = DecodeOptions::new()
            .read_info(bytes)
            .expect("a GIF this crate wrote");
        let mut out = Vec::new();
        while let Some(frame) =
            decoder.read_next_frame().expect("a decodable frame")
        {
            out.push(frame.dispose);
        }
        out
    }

    /// Every frame of `bytes`, as its delay and its pixels in RGBA.
    fn decoded(bytes: &[u8]) -> Vec<(u16, Vec<u8>)> {
        let mut options = DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::RGBA);
        let mut decoder =
            options.read_info(bytes).expect("a GIF this crate wrote");
        let mut out = Vec::new();
        while let Some(frame) =
            decoder.read_next_frame().expect("a decodable frame")
        {
            out.push((frame.delay, frame.buffer.to_vec()));
        }
        out
    }

    #[test]
    fn an_animation_reads_back_with_the_colours_and_delays_it_was_given() {
        let written = frames();
        let read = decoded(&encoded(&written, None));
        assert_eq!(read.len(), 3);

        for (frame, (delay, pixels)) in written.iter().zip(read) {
            // Hundredths of a second, so 100ms is 10 and 350ms is 35.
            assert_eq!(u32::from(delay) * 10, frame.delay_ms);
            // Three solid primaries fit in any palette, so quantizing them
            // is expected to be exact rather than merely close.
            assert_eq!(pixels.as_slice(), &frame.eight()[..]);
        }
    }

    #[test]
    fn a_rate_that_does_not_divide_by_the_tick_still_averages_out() {
        // GIF stores hundredths, and most rates are not a whole number of
        // them. Rounding each frame alone dropped the remainder every time:
        // 30fps became 3 hundredths a frame, which plays at 33.3fps, so an
        // animation ran eleven percent fast and a ten second one finished
        // more than a second early. The delays now come from the difference
        // between running totals, so the remainder is spent rather than
        // lost.
        for (fps, frames) in [(30.0f64, 12usize), (24.0, 12), (60.0, 12)] {
            let written: Vec<Frame> = (0..frames)
                .map(|i| {
                    let at = |f: usize| (f as f64 * 1000.0 / fps).round();
                    Frame {
                        pixels: Pixels::Eight(vec![
                            255, 0, 0, 255, 0, 255, 0, 255,
                        ]),
                        width: 2,
                        height: 1,
                        delay_ms: (at(i + 1) - at(i)) as u32,
                    }
                })
                .collect();

            let read = decoded(&encoded(&written, None));
            let total: u32 =
                read.iter().map(|(cs, _)| u32::from(*cs) * 10).sum();
            let played = frames as f64 / (f64::from(total) / 1000.0);
            assert!(
                (played - fps).abs() < 0.01,
                "{fps}fps came out at {played:.2}fps over {frames} frames"
            );
        }
    }

    #[test]
    fn a_frame_too_short_for_the_format_is_floored_rather_than_zeroed() {
        // A zero delay does not play fast. Browsers clamp it up -- Firefox
        // renders any frame of 10ms or less at 100ms -- so the honest
        // rounding of a 4ms frame produced the slowest playback the format
        // has: asking for 240fps got 10.
        assert_eq!(Tick::default().next(4), 1, "not zero");
        assert_eq!(Tick::default().next(1), 1);
        // A frame that asked for no time at all still gets none.
        assert_eq!(Tick::default().next(0), 0);

        // And through the sink, where the running total could otherwise
        // hand out a zero of its own.
        let quick: Vec<Frame> = (0..8)
            .map(|_| Frame {
                pixels: Pixels::Eight(vec![255, 0, 0, 255, 0, 255, 0, 255]),
                width: 2,
                height: 1,
                delay_ms: 4,
            })
            .collect();
        for (delay, _) in decoded(&encoded(&quick, None)) {
            assert!(delay >= 1, "no frame is written as an instant one");
        }
    }

    /// A frame too long for sixteen bits must not poison the ones after it.
    ///
    /// The clock credited itself with the delay it was asked for rather than
    /// the one it wrote, so after a frame past 655.35s every later frame
    /// found the running total already ahead of the target, took the
    /// zero-floor branch, and was written as a single hundredth.
    #[test]
    fn a_frame_too_long_for_the_format_does_not_starve_the_rest() {
        let mut tick = Tick::default();

        // 700 seconds, which the format cannot hold: it is written short.
        assert_eq!(tick.next(700_000), u16::MAX);

        // The frames after it still get what they asked for. Before the fix
        // these came back as 1, because the clock thought 700s had passed
        // while the file said 655.35.
        assert_eq!(tick.next(1000), 100, "a one-second frame is 100cs");
        assert_eq!(tick.next(500), 50);
        assert_eq!(tick.next(1000), 100);
    }

    #[test]
    fn a_delay_lands_on_the_nearest_hundredth_rather_than_the_next_one() {
        // 30fps is 33ms a frame, which is nearer 30ms than 40ms. Rounding it
        // up would have run every default-rate animation 20% slow.
        assert_eq!(Tick::default().next(33), 3);
        assert_eq!(Tick::default().next(35), 4);
        assert_eq!(Tick::default().next(100), 10);
        // Under half a hundredth would round to zero, which browsers read
        // as the *longest* delay, so it is floored to one instead.
        assert_eq!(Tick::default().next(4), 1);
        assert_eq!(Tick::default().next(0), 0, "no time asked, none given");
        // And a delay past what sixteen bits hold stops there rather than
        // wrapping to something short.
        assert_eq!(Tick::default().next(u32::MAX), u16::MAX);
    }

    /// Plays `bytes` back the way a viewer does: each frame laid into a
    /// running canvas at its own offset, honouring transparency and disposal.
    ///
    /// The decoder hands back each frame as it was stored -- a rectangle at
    /// an offset -- and composing them is the reader's job. Which is exactly
    /// why this exists: a test that only reads the rectangles back would pass
    /// on an animation that plays as nonsense.
    fn played(bytes: &[u8], width: usize, height: usize) -> Vec<Vec<u8>> {
        let mut options = DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::RGBA);
        let mut decoder =
            options.read_info(bytes).expect("a GIF this crate wrote");

        let mut canvas = vec![0u8; width * height * BYTES_PER_PIXEL];
        let mut out = Vec::new();
        while let Some(frame) =
            decoder.read_next_frame().expect("a decodable frame")
        {
            for row in 0..frame.height as usize {
                for column in 0..frame.width as usize {
                    let from = (row * frame.width as usize + column) * 4;
                    let pixel = &frame.buffer[from..from + 4];
                    // A transparent pixel leaves what is underneath, which is
                    // the rule that makes disposal necessary at all.
                    if pixel[3] == 0 {
                        continue;
                    }
                    let x = frame.left as usize + column;
                    let y = frame.top as usize + row;
                    let at = (y * width + x) * 4;
                    canvas[at..at + 4].copy_from_slice(pixel);
                }
            }
            out.push(canvas.clone());

            if frame.dispose == DisposalMethod::Background {
                for row in 0..frame.height as usize {
                    let y = frame.top as usize + row;
                    let at = (y * width + frame.left as usize) * 4;
                    let span = frame.width as usize * 4;
                    canvas[at..at + span].fill(0);
                }
            }
        }
        out
    }

    #[test]
    fn an_animation_of_rectangles_plays_back_as_the_pages_it_was_given() {
        // The check dirty rectangles actually need. Every other test here
        // hands the encoder frames that differ everywhere, so the rectangle
        // is the whole canvas and the placement is never exercised; this one
        // moves a dot across a background that does not change, which is the
        // case the rectangles exist for and the case that breaks if an offset
        // is wrong, a disposal is wrong, or the canvas is not carried from
        // one frame to the next.
        const W: usize = 8;
        const H: usize = 4;
        // Three colours, so the quantizer is exact and any difference read
        // back is this encoder's rather than k-means' -- the same reason the
        // primaries test uses three. A checkerboard rather than a flat fill,
        // so a frame written at the wrong offset lands on the wrong square
        // and says so.
        let page = |dot: usize| {
            let mut pixels = Vec::with_capacity(W * H * BYTES_PER_PIXEL);
            for y in 0..H {
                for x in 0..W {
                    let lit = y == H / 2 && x == dot;
                    pixels.extend_from_slice(&match (lit, (x + y) % 2 == 0) {
                        (true, _) => [255, 0, 0, 255],
                        (false, true) => [0, 0, 200, 255],
                        (false, false) => [0, 200, 0, 255],
                    });
                }
            }
            Frame {
                pixels: Pixels::Eight(pixels),
                width: W as u32,
                height: H as u32,
                delay_ms: 100,
            }
        };

        let pages: Vec<Frame> = (0..W).map(page).collect();
        let played = played(&encoded(&pages, None), W, H);
        assert_eq!(played.len(), pages.len());
        for (nth, (shown, drawn)) in played.iter().zip(&pages).enumerate() {
            assert_eq!(
                shown.as_slice(),
                &drawn.eight()[..],
                "frame {nth} played back as something else"
            );
        }
    }

    #[test]
    fn an_animation_that_erases_plays_back_with_the_pixel_gone() {
        // The disposal test beside this one reads the byte; this one reads the
        // picture. They are not the same check, and the byte is the weaker of
        // the two: it says the encoder asked for the canvas to be cleared, not
        // that clearing it produces the frame that was drawn.
        //
        // Erasing is the one thing a GIF frame cannot do for itself -- a
        // transparent index means "leave what is underneath" -- so it is the
        // part of the rectangle work most likely to be silently wrong, and it
        // was the part with no picture-level coverage at all.
        const W: usize = 6;
        const H: usize = 3;
        let page = |lit: bool| {
            let mut pixels = Vec::with_capacity(W * H * BYTES_PER_PIXEL);
            for y in 0..H {
                for x in 0..W {
                    let on = lit && y == 1 && (2..4).contains(&x);
                    pixels.extend_from_slice(&match on {
                        true => [255, 0, 0, 255],
                        // Nothing drawn: transparent, and it has to come back
                        // transparent.
                        false => [0, 0, 0, 0],
                    });
                }
            }
            Frame {
                pixels: Pixels::Eight(pixels),
                width: W as u32,
                height: H as u32,
                delay_ms: 100,
            }
        };

        // Drawn, held, then taken away. The third frame is what needs the
        // second to have cleared the canvas for it.
        let pages = vec![page(true), page(true), page(false)];
        let played = played(&encoded(&pages, None), W, H);
        assert_eq!(played.len(), pages.len());
        for (nth, (shown, drawn)) in played.iter().zip(&pages).enumerate() {
            assert_eq!(
                shown.as_slice(),
                &drawn.eight()[..],
                "frame {nth} played back as something else"
            );
        }
    }

    #[test]
    fn a_frame_clears_the_one_before_it_only_where_that_erases() {
        // The disposal byte, read back rather than assumed.
        //
        // Every frame used to clear the canvas after itself, because every
        // frame covered the canvas and the alternative was wrong: with "keep"
        // a transparent pixel reveals the frame underneath, so an animation
        // on a clear background accumulated instead of moving, and a dot
        // crossing a strip came out `#...`, `##..`, `###.`, `####`.
        //
        // Frames carry rectangles now, and a rectangle *must* keep what is
        // under it or there is nothing outside it left to see. So clearing
        // becomes the exception, and the rule is what decides it: a frame
        // clears only when the frame after it needs a pixel erased, which is
        // the one thing a rectangle cannot do for itself.
        let opaque = encoded(&frames(), None);
        assert_eq!(
            disposals(&opaque),
            vec![DisposalMethod::Keep; 3],
            "nothing is ever erased, so nothing needs clearing"
        );

        // A pixel drawn, then not drawn. The frame that has to clear is the
        // *first* one -- the one before the erasure -- and testing only the
        // frame that erases would miss that entirely.
        let solid = |alpha| Frame {
            pixels: Pixels::Eight(vec![255, 0, 0, 255, 0, 0, 255, alpha]),
            width: 2,
            height: 1,
            delay_ms: 100,
        };
        assert_eq!(
            disposals(&encoded(&[solid(255), solid(0), solid(0)], None)),
            vec![
                DisposalMethod::Background,
                DisposalMethod::Keep,
                DisposalMethod::Keep
            ],
        );
    }

    #[test]
    fn a_transparent_pixel_stays_transparent_through_the_round_trip() {
        let frame = Frame {
            // Opaque red, then a pixel nothing was drawn on.
            pixels: Pixels::Eight(vec![255, 0, 0, 255, 0, 0, 0, 0]),
            width: 2,
            height: 1,
            delay_ms: 100,
        };
        let read = decoded(&encoded(&[frame], None));
        let (_, pixels) = &read[0];
        assert_eq!(&pixels[0..4], [255, 0, 0, 255]);
        assert_eq!(pixels[7], 0, "the untouched pixel stays transparent");
    }

    #[test]
    fn a_reserved_transparent_index_has_a_palette_entry_to_name() {
        // The index is 255, so the colour table has to be 256 entries long
        // for it to name anything -- and a two-colour frame quantizes to a
        // two-entry table. Without the padding the file indexes past the end
        // of its own palette.
        //
        // Checked on the bytes rather than through a decoder, because the
        // `gif` crate's reader tolerates the out-of-range index and hands
        // back the right pixels anyway. A stricter decoder need not.
        let frame = Frame {
            pixels: Pixels::Eight(vec![255, 0, 0, 255, 0, 0, 0, 0]),
            width: 2,
            height: 1,
            delay_ms: 100,
        };
        let (palette, indices, _) =
            quantize(frame.width, frame.height, &frame.eight());
        assert_eq!(
            palette.len(),
            (TRANSPARENT as usize + 1) * 3,
            "the table reaches the reserved index"
        );
        assert!(indices.contains(&TRANSPARENT));

        // And an opaque frame is not padded: nothing indexes past what the
        // quantizer returned.
        let opaque = Frame {
            pixels: Pixels::Eight(vec![255, 0, 0, 255, 0, 0, 255, 255]),
            ..frame
        };
        let (palette, indices, _) =
            quantize(opaque.width, opaque.height, &opaque.eight());
        let highest = *indices.iter().max().expect("two pixels");
        assert!(
            (highest as usize + 1) * 3 <= palette.len(),
            "every index names an entry ({highest} in {} bytes)",
            palette.len()
        );
    }

    #[test]
    fn only_a_frame_with_transparency_reserves_an_index() {
        // Reserved where something needs it and not otherwise, so an opaque
        // frame is quantized against the whole palette rather than 255 of
        // it. What the palette then comes back holding is quantette's
        // business, and this does not claim to check it.
        let opaque = Frame {
            pixels: Pixels::Eight(vec![1, 2, 3, 255, 4, 5, 6, 255]),
            width: 2,
            height: 1,
            delay_ms: 100,
        };
        assert_eq!(transparent_index(&opaque.eight()), None);

        let translucent = Frame {
            pixels: Pixels::Eight(vec![1, 2, 3, 255, 4, 5, 6, 127]),
            ..opaque
        };
        assert_eq!(transparent_index(&translucent.eight()), Some(TRANSPARENT));

        // Pinned from both sides. Alpha 127 is below the threshold and 128
        // is not, so the boundary is where it is documented to be rather
        // than anywhere in the range the two cases above leave open.
        let barely = Frame {
            pixels: Pixels::Eight(vec![1, 2, 3, 255, 4, 5, 6, OPAQUE_AT]),
            ..opaque
        };
        assert_eq!(
            transparent_index(&barely.eight()),
            None,
            "alpha 128 is drawn"
        );
        let under = Frame {
            pixels: Pixels::Eight(vec![1, 2, 3, 255, 4, 5, 6, OPAQUE_AT - 1]),
            ..opaque
        };
        assert_eq!(transparent_index(&under.eight()), Some(TRANSPARENT));
    }

    #[test]
    fn plays_are_counted_as_the_format_counts_repeats() {
        // This crate counts plays; GIF counts the repeats after the first.
        assert_eq!(repeat(None), Repeat::Infinite);
        assert_eq!(
            repeat(Some(1)),
            Repeat::Finite(0),
            "one play is no repeats"
        );
        assert_eq!(repeat(Some(4)), Repeat::Finite(3));
        // Nobody means "zero plays", and reading it as one beats a panic.
        assert_eq!(repeat(Some(0)), Repeat::Finite(0));
    }
}
