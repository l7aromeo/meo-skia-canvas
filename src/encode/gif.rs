//! GIF, which Skia can read and cannot write.

use gif::{DisposalMethod, Encoder, Frame as GifFrame, Repeat};
use quantette::{
    ImageRef, PaletteSize, Pipeline, QuantizeMethod, deps::palette::Srgb,
};

use super::{Frame, FrameEncoder, FrameSink, SequenceSpec, Sink};

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
}

impl FrameSink for GifSink<'_> {
    fn write_frame(&mut self, frame: &Frame) -> Result<(), String> {
        let (palette, indices) = quantize(frame);
        let mut written = GifFrame {
            width: self.width,
            height: self.height,
            palette: Some(palette),
            buffer: indices.into(),
            delay: self.tick.next(frame.delay_ms),
            // Clear the canvas between frames rather than leaving the last
            // one under this one.
            //
            // Every frame here covers the whole canvas, which was taken to
            // mean nothing needed restoring -- true of colour and false of
            // alpha. A transparent index says "do not touch this pixel", so
            // with no disposal the frame before shows through wherever this
            // one is transparent, and an animation on a clear background
            // accumulates instead of moving: a dot crossing a strip came out
            // as `#...`, `##..`, `###.`, `####`.
            //
            // Unconditional rather than only on frames that carry a
            // transparent index, for two reasons. What matters is the
            // disposal of the frame *before* a transparent one, so a
            // per-frame test would have to look ahead -- and this encoder is
            // handed one frame at a time and cannot. On a fully opaque frame
            // it changes nothing anyway: clearing a canvas that is about to
            // be painted over completely is the same picture.
            dispose: DisposalMethod::Background,
            ..GifFrame::default()
        };
        written.transparent = transparent_index(frame);
        self.encoder
            .write_frame(&written)
            .map_err(|e| format!("Could not write a GIF frame: {e}"))
    }

    fn finish(self: Box<Self>) -> Result<(), String> {
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
fn transparent_index(frame: &Frame) -> Option<u8> {
    frame
        .eight()
        .chunks_exact(4)
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
fn quantize(frame: &Frame) -> (Vec<u8>, Vec<u8>) {
    let transparent = transparent_index(frame);
    let opaque: Vec<Srgb<u8>> = frame
        .eight()
        .chunks_exact(4)
        .map(|pixel| Srgb::new(pixel[0], pixel[1], pixel[2]))
        .collect();

    let image =
        ImageRef::new(frame.width, frame.height, &opaque).unwrap_or_default();

    let quantized = Pipeline::new()
        .palette_size(match transparent {
            Some(_) => PaletteSize::from_u8_clamped(TRANSPARENT),
            None => PaletteSize::MAX,
        })
        // k-means, which the pipeline runs in Oklab. Wu's method alone is
        // the fast path and visibly coarser on a gradient, which is most of
        // what a canvas draws.
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
            indices.iter_mut().zip(frame.eight().chunks_exact(4))
        {
            if pixel[3] < OPAQUE_AT {
                *index = TRANSPARENT;
            }
        }
    }

    (palette, indices)
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

    #[test]
    fn every_frame_clears_the_one_before_it() {
        // The disposal byte, read back rather than assumed: with "no
        // disposal" a transparent pixel reveals the previous frame, so an
        // animation on a clear background accumulates instead of moving.
        // Asserted on both an opaque and a transparent animation, because
        // the frame that needs clearing is the one *before* the transparent
        // one -- testing only transparent frames would miss it.
        let opaque = encoded(&frames(), None);
        assert_eq!(disposals(&opaque), vec![DisposalMethod::Background; 3]);

        let clear = Frame {
            pixels: Pixels::Eight(vec![255, 0, 0, 255, 0, 0, 0, 0]),
            width: 2,
            height: 1,
            delay_ms: 100,
        };
        assert_eq!(
            disposals(&encoded(&[clear], None)),
            vec![DisposalMethod::Background]
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
        let (palette, indices) = quantize(&frame);
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
        let (palette, indices) = quantize(&opaque);
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
        assert_eq!(transparent_index(&opaque), None);

        let translucent = Frame {
            pixels: Pixels::Eight(vec![1, 2, 3, 255, 4, 5, 6, 127]),
            ..opaque
        };
        assert_eq!(transparent_index(&translucent), Some(TRANSPARENT));

        // Pinned from both sides. Alpha 127 is below the threshold and 128
        // is not, so the boundary is where it is documented to be rather
        // than anywhere in the range the two cases above leave open.
        let barely = Frame {
            pixels: Pixels::Eight(vec![1, 2, 3, 255, 4, 5, 6, OPAQUE_AT]),
            ..opaque
        };
        assert_eq!(transparent_index(&barely), None, "alpha 128 is drawn");
        let under = Frame {
            pixels: Pixels::Eight(vec![1, 2, 3, 255, 4, 5, 6, OPAQUE_AT - 1]),
            ..opaque
        };
        assert_eq!(transparent_index(&under), Some(TRANSPARENT));
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
