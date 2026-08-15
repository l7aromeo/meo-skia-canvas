//! ICO, written by hand around the PNG encoder that is already here.
//!
//! The container is a six-byte header, one sixteen-byte entry per image,
//! and the payloads. The `ico` crate would do it, and would bring a second
//! copy of `png` into the tree beside the one APNG uses -- for a header this
//! size that is a poor trade.
//!
//! The payloads are PNG rather than the older DIB form. Every Windows since
//! Vista reads PNG-in-ICO, it is what any icon above 48 pixels uses in
//! practice, and it means the alpha channel needs no explaining.

use png::{BitDepth, ColorType, Encoder};

use super::{
    Frame, FrameEncoder, FrameSink, SequenceSpec, Sink, color::ColorProfile,
};

/// The largest an icon may be. The dimension fields are one byte each.
const MAX: u32 = 256;

/// The `ICONDIR` header: two bytes of reserved, two naming the file's kind,
/// two counting the images.
const HEADER_LEN: u32 = 6;

/// One `ICONDIRENTRY`: width, height, palette size and a reserved byte, then
/// two-byte plane and bit counts, then the payload's length and offset.
const ENTRY_LEN: u32 = 4 + 2 + 2 + 4 + 4;

/// The value `idType` takes for an icon. Two would make it a cursor, which
/// differs by carrying a hotspot where an icon carries plane and bit counts.
const TYPE_ICON: u16 = 1;

/// `bReserved`, and `bColorCount` for a truecolour image: there is no
/// palette to count.
const NO_PALETTE: u8 = 0;

/// `wPlanes`. One, as in BMP, and as meaningless.
const COLOUR_PLANES: u16 = 1;

/// `wBitCount`. Eight bits each of red, green, blue and alpha.
const BITS_PER_PIXEL: u16 = 32;

pub(crate) struct Ico;

impl FrameEncoder for Ico {
    fn start<'a>(
        &self,
        spec: &SequenceSpec,
        out: &'a mut dyn Sink,
    ) -> Result<Box<dyn FrameSink + 'a>, String> {
        if spec.frames > u16::MAX as usize {
            return Err(format!("An icon holds at most {} images", u16::MAX));
        }
        Ok(Box::new(IcoSink {
            out,
            images: Vec::with_capacity(spec.frames),
            color: spec.color,
        }))
    }

    fn uniform_frames(&self) -> bool {
        // The whole point of the container: one icon at several sizes, so
        // the pages are meant to differ.
        false
    }
}

struct IcoSink<'a> {
    out: &'a mut dyn Sink,
    /// Each image's dimensions and its encoded PNG.
    ///
    /// Held rather than streamed because every entry in the directory
    /// carries the offset and length of a payload, and neither is known
    /// until that payload has been compressed. An icon is a handful of
    /// images of at most 256 pixels, so this is bounded by the format.
    images: Vec<(u32, u32, Vec<u8>)>,
    color: ColorProfile,
}

impl FrameSink for IcoSink<'_> {
    fn write_frame(&mut self, frame: &Frame) -> Result<(), String> {
        if frame.width > MAX || frame.height > MAX {
            return Err(format!(
                "An icon is at most {MAX}x{MAX} (got {}x{})",
                frame.width, frame.height
            ));
        }
        if frame.width == 0 || frame.height == 0 {
            return Err("An icon cannot be empty".to_string());
        }

        let mut png = Vec::new();
        {
            let mut encoder = Encoder::new(&mut png, frame.width, frame.height);
            encoder.set_color(ColorType::Rgba);
            encoder.set_depth(BitDepth::Eight);
            // The same pre-2022 description an APNG gets, for the same
            // reason: an icon reader older than `cICP` should still be told
            // the gamut rather than left to assume sRGB.
            if !self.color.is_srgb() {
                super::apng::describe_the_old_way(&mut encoder, &self.color);
            }
            let mut writer = encoder.write_header().map_err(|e| {
                format!("Could not write an icon's PNG header: {e}")
            })?;
            // The payload is a PNG, so it names its colour space the way any
            // other PNG this crate writes does. An icon is unlikely to be
            // wide-gamut and the container has no opinion either way -- what
            // matters is that a P3 page does not become an untagged PNG that
            // every reader takes for sRGB.
            super::apng::write_cicp(&mut writer, &self.color)?;
            writer
                .write_image_data(&frame.eight())
                .map_err(|e| format!("Could not write an icon image: {e}"))?;
            writer
                .finish()
                .map_err(|e| format!("Could not finish an icon image: {e}"))?;
        }
        self.images.push((frame.width, frame.height, png));
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<(), String> {
        let this = *self;
        let count = this.images.len() as u16;
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&0u16.to_le_bytes()); // reserved
        bytes.extend_from_slice(&TYPE_ICON.to_le_bytes());
        bytes.extend_from_slice(&count.to_le_bytes());

        // Payloads begin after the directory, and each entry names where its
        // own starts.
        let mut offset = HEADER_LEN + ENTRY_LEN * u32::from(count);
        for (width, height, png) in &this.images {
            // Zero means 256: the field is one byte and 256 does not fit.
            bytes.push(if *width == MAX { 0 } else { *width as u8 });
            bytes.push(if *height == MAX { 0 } else { *height as u8 });
            bytes.push(NO_PALETTE); // colour count
            bytes.push(NO_PALETTE); // reserved
            bytes.extend_from_slice(&COLOUR_PLANES.to_le_bytes());
            bytes.extend_from_slice(&BITS_PER_PIXEL.to_le_bytes());
            let length = u32::try_from(png.len()).map_err(|_| {
                "An icon image is too large to describe".to_string()
            })?;
            bytes.extend_from_slice(&length.to_le_bytes());
            bytes.extend_from_slice(&offset.to_le_bytes());
            offset = offset.checked_add(length).ok_or_else(|| {
                "An icon's images do not fit in one file".to_string()
            })?;
        }
        for (_, _, png) in &this.images {
            bytes.extend_from_slice(png);
        }

        this.out
            .write_all(&bytes)
            .map_err(|e| format!("Could not write the icon: {e}"))?;
        this.out
            .flush()
            .map_err(|e| format!("Could not finish the icon: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        encode::{FrameDepth, Pixels},
        export::ChromaSampling,
    };
    use std::io::Cursor;

    use super::*;
    use crate::{
        encode::{color::ColorProfile, start},
        export::ImageFormat,
        pixels::PixelColorSpace,
    };

    fn frame(size: u32) -> Frame {
        Frame {
            pixels: Pixels::Eight(
                [200, 30, 40, 255].repeat((size * size) as usize),
            ),
            width: size,
            height: size,
            delay_ms: 0,
        }
    }

    fn encoded(sizes: &[u32]) -> Vec<u8> {
        let spec = SequenceSpec {
            chroma: ChromaSampling::Full,
            lossless: false,
            width: sizes[0],
            height: sizes[0],
            frames: sizes.len(),
            loops: None,
            quality: 90.0,
            density: 1.0,
            color: ColorProfile::of(PixelColorSpace::Srgb),
            space: PixelColorSpace::Srgb,
            depth: FrameDepth::Eight,
            bits: None,
        };
        let mut bytes = Cursor::new(Vec::new());
        {
            let mut sink = start(ImageFormat::Ico, &spec, &mut bytes)
                .expect("the spec is well formed");
            for size in sizes {
                sink.write_frame(&frame(*size))
                    .expect("a well formed image");
            }
            sink.finish().expect("the encoder closes");
        }
        bytes.into_inner()
    }

    #[test]
    fn the_pages_may_differ_in_size_because_that_is_the_point() {
        // Every other format here declares one canvas and holds every frame
        // to it. An icon is the same picture at several sizes, so the check
        // is relaxed for this one encoder rather than for all of them.
        let b = encoded(&[16, 32, 48]);
        assert_eq!(u16::from_le_bytes([b[0], b[1]]), 0, "reserved");
        assert_eq!(
            u16::from_le_bytes([b[2], b[3]]),
            TYPE_ICON,
            "an icon, not a cursor"
        );
        assert_eq!(u16::from_le_bytes([b[4], b[5]]), 3, "three images");

        for (nth, size) in [16u8, 32, 48].into_iter().enumerate() {
            let at = (HEADER_LEN as usize) + nth * ENTRY_LEN as usize;
            assert_eq!(b[at], size, "image {nth} width");
            assert_eq!(b[at + 1], size, "image {nth} height");
        }
    }

    #[test]
    fn every_entry_points_at_its_own_payload() {
        let b = encoded(&[16, 32]);
        let u32at = |i: usize| {
            u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as usize
        };
        let mut expected = (HEADER_LEN + ENTRY_LEN * 2) as usize;
        for nth in 0..2 {
            let at = (HEADER_LEN as usize) + nth * ENTRY_LEN as usize;
            let (length, offset) = (u32at(at + 8), u32at(at + 12));
            assert_eq!(offset, expected, "image {nth} starts where it says");
            assert!(offset + length <= b.len(), "and ends inside the file");
            // The payloads are PNG, which is what any icon past 48 pixels
            // uses and what carries the alpha channel without explaining.
            assert_eq!(&b[offset..offset + 4], b"\x89PNG", "image {nth}");
            expected += length;
        }
        assert_eq!(expected, b.len(), "nothing left over");
    }

    #[test]
    fn the_largest_icon_is_written_as_a_zero() {
        // The dimension fields are one byte, so 256 does not fit and the
        // format spells it as zero.
        let b = encoded(&[MAX]);
        assert_eq!(b[HEADER_LEN as usize], 0, "256 wide");
        assert_eq!(b[HEADER_LEN as usize + 1], 0, "256 tall");
    }

    #[test]
    fn an_icon_larger_than_the_format_allows_is_refused() {
        let spec = SequenceSpec {
            chroma: ChromaSampling::Full,
            lossless: false,
            width: 512,
            height: 512,
            frames: 1,
            loops: None,
            quality: 90.0,
            density: 1.0,
            color: ColorProfile::of(PixelColorSpace::Srgb),
            space: PixelColorSpace::Srgb,
            depth: FrameDepth::Eight,
            bits: None,
        };
        let mut bytes = Cursor::new(Vec::new());
        let mut sink = start(ImageFormat::Ico, &spec, &mut bytes)
            .expect("the spec is well formed");
        let refused = sink
            .write_frame(&frame(512))
            .expect_err("512 is past what an icon can describe");
        assert!(refused.contains("at most 256x256"), "{refused}");
    }
}
