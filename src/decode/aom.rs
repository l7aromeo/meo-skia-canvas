//! A safe surface over libaom's AV1 decoder.
//!
//! This is the only `unsafe` in the decoders, and it exists because the crate
//! that used to wrap libaom for us could not be built without pulling in an
//! MPL-2.0 container parser we no longer use.
//!
//! `aom-decode` 0.2.13 declares its `avif` feature optional, but the feature
//! does not switch off: its `error.rs` names `avif_parse::Error` inside a
//! `quick_error!` invocation, and that macro drops the `#[cfg]` on the
//! variant, so a build with `default-features = false` fails to compile the
//! dependency itself. With [`super::avif`] reading the container, the only
//! thing left to want from that crate was the decoder handle -- ten C
//! functions -- so those are bound here and the crate is gone.
//!
//! # What the C API requires of a caller
//!
//! Two rules shape the types below. `aom_codec_get_frame` hands back a
//! pointer into the decoder's own storage, valid only until the next
//! `aom_codec_decode` on the same context, so [`Picture`] borrows the
//! [`Decoder`] mutably and cannot outlive the frame it describes. And a
//! context that was initialised must be destroyed exactly once, which is
//! [`Drop`]'s job rather than any caller's.

use std::{os::raw::c_int, ptr};

use libaom_sys::{
    AOM_CODEC_OK, AOM_DECODER_ABI_VERSION, AOM_IMG_FMT_HIGHBITDEPTH,
    aom_codec_av1_dx, aom_codec_ctx_t, aom_codec_dec_cfg_t,
    aom_codec_dec_init_ver, aom_codec_decode, aom_codec_destroy,
    aom_codec_get_frame, aom_image_t, aom_img_plane_height,
    aom_img_plane_width,
};

/// The plane indices AV1 stores a picture in, in the order libaom lists them.
///
/// Named rather than written as 0, 1, 2 at the call sites because the second
/// and third are the two chroma differences and swapping them is a mistake
/// that still produces a picture.
const PLANE_Y: c_int = libaom_sys::AOM_PLANE_Y as c_int;
const PLANE_U: c_int = libaom_sys::AOM_PLANE_U as c_int;
const PLANE_V: c_int = libaom_sys::AOM_PLANE_V as c_int;

/// The width in bytes of a sample in a high-bit-depth plane.
///
/// libaom hands every plane back as bytes. Above eight bits the samples are
/// sixteen-bit in the host's own order, so a row is half as many samples as
/// it is bytes.
pub(crate) const DEEP_SAMPLE_BYTES: usize = 2;

/// libaom's AV1 decoder, for one stream.
///
/// `Debug` prints nothing but the name: the context is libaom's, and there
/// is no state here worth showing.
///
/// Frames of a sequence must be fed in coded order through the same decoder,
/// because a frame after the first is coded against those before it.
pub(crate) struct Decoder {
    context: aom_codec_ctx_t,
}

impl std::fmt::Debug for Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Decoder")
    }
}

impl Decoder {
    /// Starts a decoder that will use `threads` threads.
    pub(crate) fn new(threads: u32) -> Result<Self, String> {
        let config = aom_codec_dec_cfg_t {
            threads,
            // Zero lets libaom take the dimensions from the sequence header,
            // which is the only place this crate knows them from either.
            w: 0,
            h: 0,
            allow_lowbitdepth: 1,
        };

        // SAFETY: `context` is initialised by this call and by nothing else;
        // it is only read afterwards if the call reported success, and the
        // interface pointer comes from libaom's own accessor. The ABI
        // version is the constant the header requires, which is what makes
        // the struct layouts agree.
        let mut context = unsafe { std::mem::zeroed::<aom_codec_ctx_t>() };
        // SAFETY: as above. `config` outlives the call, and libaom copies
        // what it needs from it.
        let status = unsafe {
            aom_codec_dec_init_ver(
                &mut context,
                aom_codec_av1_dx(),
                &config,
                0,
                AOM_DECODER_ABI_VERSION as c_int,
            )
        };
        match status {
            AOM_CODEC_OK => Ok(Self { context }),
            code => Err(format!("Could not start the AV1 decoder: {code}")),
        }
    }

    /// Decodes one coded frame.
    ///
    /// The returned picture borrows the decoder: libaom owns the pixels and
    /// reuses them on the next call.
    pub(crate) fn decode(
        &mut self,
        sample: &[u8],
    ) -> Result<Picture<'_>, String> {
        // SAFETY: the pointer and length describe `sample`, which outlives
        // the call; libaom does not retain it. A null `user_priv` is what
        // the header documents for "no application data".
        let status = unsafe {
            aom_codec_decode(
                &mut self.context,
                sample.as_ptr(),
                sample.len(),
                ptr::null_mut(),
            )
        };
        if status != AOM_CODEC_OK {
            return Err(format!("Could not decode the AV1 frame: {status}"));
        }

        // The iterator must start null, and a null return means the stream
        // held no displayable frame -- which for a still image is a file
        // whose primary item codes nothing.
        let mut iterator = ptr::null();
        // SAFETY: `iterator` is initialised to null as the header requires.
        // The returned pointer is libaom's, valid until the next decode on
        // this context, which the borrow on `self` is what enforces.
        let image =
            unsafe { aom_codec_get_frame(&mut self.context, &mut iterator) };
        match unsafe { image.as_ref() } {
            Some(image) => Ok(Picture { image }),
            None => Err("The AV1 stream held no frame".to_string()),
        }
    }
}

// SAFETY: libaom keeps no thread-local state for a decoder context. Its
// documented thread-safety caveat is about two threads using *one* context
// at once, which every method here already prevents by taking `&mut self`.
// Moving a context between threads is sound, and is what lets an `Image`
// hold one across calls so a sequential walk does not restart from zero.
unsafe impl Send for Decoder {}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: the context was initialised in `new`, which is the only
        // way to build this type, and this runs once.
        unsafe {
            aom_codec_destroy(&mut self.context);
        }
    }
}

/// One decoded picture, borrowed from the decoder that produced it.
pub(crate) struct Picture<'a> {
    image: &'a aom_image_t,
}

impl Picture<'_> {
    /// Bits per sample, which is eight, ten or twelve for AVIF.
    pub(crate) fn depth(&self) -> u32 {
        self.image.bit_depth
    }

    /// Whether the samples are sixteen bits wide in memory.
    ///
    /// Not the same question as [`depth`](Self::depth): a ten-bit picture is
    /// stored in sixteen-bit samples, and libaom says so with a format flag
    /// rather than with the depth.
    pub(crate) fn deep(&self) -> bool {
        self.image.fmt & AOM_IMG_FMT_HIGHBITDEPTH != 0
    }

    /// Whether the picture carries no chroma, as an alpha plane does.
    pub(crate) fn monochrome(&self) -> bool {
        self.image.monochrome != 0
    }

    /// How far chroma is subsampled against luma, as a shift per axis.
    ///
    /// Zero and zero is 4:4:4; one and zero is 4:2:2; one and one is 4:2:0.
    pub(crate) fn chroma_shifts(&self) -> (u32, u32) {
        (self.image.x_chroma_shift, self.image.y_chroma_shift)
    }

    /// One plane's rows, each already trimmed to the plane's own width.
    ///
    /// Rows are returned as bytes. A caller reading a deep picture pairs
    /// them into sixteen-bit samples itself, because that is the only place
    /// the host's byte order matters.
    fn plane(&self, index: c_int) -> Vec<&[u8]> {
        // SAFETY: `index` is one of libaom's own plane constants and the
        // image is the one it came with, which is what these two accessors
        // require. They read the descriptor and allocate nothing.
        let (width, height) = unsafe {
            (
                aom_img_plane_width(self.image, index) as usize,
                aom_img_plane_height(self.image, index) as usize,
            )
        };
        let plane = self.image.planes[index as usize];
        let stride = self.image.stride[index as usize] as usize;
        if plane.is_null() || width == 0 {
            return Vec::new();
        }

        // A plane's rows are `stride` apart, which is at least its width and
        // usually more -- the tail is padding libaom aligned to, and is not
        // part of the picture.
        let scale = match self.deep() {
            true => DEEP_SAMPLE_BYTES,
            false => 1,
        };
        (0..height)
            .map(|row| {
                // SAFETY: libaom guarantees `height` rows of `stride` bytes
                // from `plane`, so every slice below is inside that
                // allocation, and each borrows for as long as the picture
                // does. The slice is cut to the plane's width so the padding
                // past it is never read.
                unsafe {
                    std::slice::from_raw_parts(
                        plane.add(row * stride),
                        width * scale,
                    )
                }
            })
            .collect()
    }

    /// The luma plane's rows.
    pub(crate) fn luma(&self) -> Vec<&[u8]> {
        self.plane(PLANE_Y)
    }

    /// The blue-difference plane's rows, empty for a monochrome picture.
    pub(crate) fn cb(&self) -> Vec<&[u8]> {
        match self.monochrome() {
            true => Vec::new(),
            false => self.plane(PLANE_U),
        }
    }

    /// The red-difference plane's rows, empty for a monochrome picture.
    pub(crate) fn cr(&self) -> Vec<&[u8]> {
        match self.monochrome() {
            true => Vec::new(),
            false => self.plane(PLANE_V),
        }
    }
}
