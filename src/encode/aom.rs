//! A safe surface over libaom's AV1 encoder.
//!
//! The counterpart of [`decode::aom`](crate::decode::aom), and here for a
//! reason that module is not: rav1e, which encoded AVIF before this, does not
//! implement lossless. Its source says so outright -- the lossless block is
//! `not yet supported` -- and that is a coding tool rather than a setting, so
//! no arrangement of its dial reaches it.
//!
//! libaom does, through `AV1E_SET_LOSSLESS`, and libaom was already linked
//! into this binary to decode. So the encoder and the decoder are now one
//! library's reading of the specification rather than two, which is the same
//! argument [`decode`](crate::decode) makes about writing APNG with the crate
//! that reads it.
//!
//! # What the C API requires of a caller
//!
//! Three rules shape the types below.
//!
//! `aom_codec_control` is variadic, so every control has to be called with
//! the argument width its identifier expects -- passing the wrong one is not
//! a type error, it is a silently misread stack. Every control used here goes
//! through [`Encoder::control`], which takes a `c_int`, because every control
//! used here takes an `int`.
//!
//! `aom_codec_get_cx_data` hands back a pointer into the encoder's own
//! storage that is valid only until the next call into libaom, so packets are
//! copied out before anything else happens.
//!
//! And an encoder that was initialised must be destroyed exactly once, with
//! the image it was given freed alongside it -- [`Drop`]'s job.

use std::{os::raw::c_int, ptr};

use libaom_sys::{
    AOM_CODEC_CX_FRAME_PKT, AOM_CODEC_OK, AOM_CODEC_USE_HIGHBITDEPTH,
    AOM_ENCODER_ABI_VERSION, AOM_FRAME_IS_KEY, AOM_IMG_FMT_HIGHBITDEPTH,
    AOM_IMG_FMT_I420, AOM_IMG_FMT_I422, AOM_IMG_FMT_I444, AOM_USAGE_ALL_INTRA,
    AOM_USAGE_GOOD_QUALITY, AOME_SET_CPUUSED, AOME_SET_CQ_LEVEL,
    AV1E_SET_COLOR_PRIMARIES, AV1E_SET_COLOR_RANGE, AV1E_SET_LOSSLESS,
    AV1E_SET_MATRIX_COEFFICIENTS, AV1E_SET_TILE_COLUMNS, AV1E_SET_TILE_ROWS,
    AV1E_SET_TRANSFER_CHARACTERISTICS, aom_codec_av1_cx, aom_codec_control,
    aom_codec_ctx_t, aom_codec_destroy, aom_codec_enc_cfg_t,
    aom_codec_enc_config_default, aom_codec_enc_init_ver, aom_codec_encode,
    aom_codec_get_cx_data, aom_image_t, aom_img_alloc, aom_img_free,
};

/// The alignment libaom is asked to give each row of the image it allocates.
///
/// Its own examples use sixteen, which suits the block sizes AV1 works in.
const ROW_ALIGNMENT: u32 = 16;

/// The width in bytes of a sample in a high-bit-depth plane.
pub(crate) const DEEP_SAMPLE_BYTES: usize = 2;

/// How chroma is sampled, as libaom's own image formats name it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sampling {
    /// 4:2:0 -- chroma at half width and half height.
    Quarter,
    /// 4:2:2 -- chroma at half width.
    Half,
    /// 4:4:4 -- a chroma sample per pixel.
    Full,
}

impl Sampling {
    /// The narrowest AV1 profile that can carry this sampling.
    ///
    /// The configuration defaults to Main, which cannot hold 4:4:4 or 4:2:2
    /// at all -- libaom answers `AOM_CODEC_INVALID_PARAM` at init rather
    /// than at the frame that would not fit. AV1 specification § 6.4.1.
    pub(crate) fn profile(self, deep_twelve: bool) -> u32 {
        match (self, deep_twelve) {
            // Twelve bits is Professional whatever the sampling.
            (_, true) | (Self::Half, _) => 2,
            (Self::Full, _) => 1,
            (Self::Quarter, _) => 0,
        }
    }

    /// How far this subsamples chroma, as a shift per axis.
    pub(crate) fn shifts(self) -> (usize, usize) {
        match self {
            Self::Quarter => (1, 1),
            Self::Half => (1, 0),
            Self::Full => (0, 0),
        }
    }

    /// The libaom image format for this sampling at this depth.
    fn format(self, deep: bool) -> u32 {
        let base = match self {
            Self::Quarter => AOM_IMG_FMT_I420,
            Self::Half => AOM_IMG_FMT_I422,
            Self::Full => AOM_IMG_FMT_I444,
        };
        match deep {
            true => base | AOM_IMG_FMT_HIGHBITDEPTH,
            false => base,
        }
    }
}

/// What an encoder is built for.
pub(crate) struct Settings {
    pub width: u32,
    pub height: u32,
    /// Bits a channel: eight, ten or twelve.
    pub bits: u8,
    pub sampling: Sampling,
    /// The constant-quality level, from 0 to 63. Ignored when `lossless`.
    pub quantizer: u32,
    /// How hard libaom looks for a smaller file, 0 to 9, slowest first.
    pub speed: u32,
    /// Whether to code the picture with no loss at all.
    ///
    /// True lossless needs more than this flag: the picture has to reach the
    /// encoder without having already been rounded, which means 4:4:4 and
    /// the identity matrix. The flag alone gives a file that is lossless in
    /// what it was handed.
    pub lossless: bool,
    /// Whether the stream carries luma alone.
    ///
    /// Alpha travels as a monochrome image, and saying so matters for more
    /// than size: libaom allocates the chroma planes regardless, and an
    /// encoder that was not told to ignore them reads whatever was in that
    /// memory. It asserts on the way past -- `av1_count_colors_highbd:
    /// this_val < max_bin_val` -- rather than producing a wrong picture.
    pub monochrome: bool,
    /// Whether this codes one still image rather than a sequence.
    pub still: bool,
    /// How many frames a sequence holds, which is also how far apart its key
    /// frames are put: one at the start and none after it. A sequence exists
    /// to code its frames against each other, and a second key frame throws
    /// that away for the frames that follow it.
    pub frames: u32,
    pub threads: u32,
    /// How many times to halve the frame across and down, as the base-two
    /// logarithms libaom's tile controls take.
    ///
    /// Both zero is one tile, which is libaom's own default and what this
    /// coded before the controls were reached at all -- a thread count was
    /// set and there was nothing for the threads to do.
    pub tiling: (u32, u32),
    /// The CICP code points to write into the sequence header, or `None` to
    /// leave the stream saying nothing about its own colour.
    ///
    /// AVIF also states these in the container's `colr` box, and a reader is
    /// entitled to believe either, so the two have to agree.
    pub colour: Option<Colour>,
}

/// The CICP code points a stream declares, as ITU-T H.273 numbers them.
#[derive(Clone, Copy)]
pub(crate) struct Colour {
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    /// Whether the samples use the full coded range rather than the narrower
    /// broadcast one.
    pub full_range: bool,
}

/// One coded frame, and whether a decoder can start from it.
pub(crate) struct Packet {
    pub data: Vec<u8>,
    /// A key frame, which the container records in its sync table so a
    /// seeking reader knows where it may begin.
    pub key: bool,
}

/// libaom's AV1 encoder, configured for one picture or one sequence.
pub(crate) struct Encoder {
    context: aom_codec_ctx_t,
    image: *mut aom_image_t,
    deep: bool,
}

impl Encoder {
    /// Starts an encoder.
    pub(crate) fn new(settings: &Settings) -> Result<Self, String> {
        let deep = settings.bits > 8;
        let usage = match settings.still {
            true => AOM_USAGE_ALL_INTRA,
            false => AOM_USAGE_GOOD_QUALITY,
        };

        // SAFETY: `config` is written by this call before it is read, and the
        // interface pointer is libaom's own. A failure leaves it untouched,
        // which is why the status is checked before it is used.
        let mut config = unsafe { std::mem::zeroed::<aom_codec_enc_cfg_t>() };
        // SAFETY: as above.
        let status = unsafe {
            aom_codec_enc_config_default(aom_codec_av1_cx(), &mut config, usage)
        };
        if status != AOM_CODEC_OK {
            return Err(format!(
                "Could not configure the AV1 encoder: {status}"
            ));
        }

        config.g_profile = settings.sampling.profile(settings.bits >= 12);
        config.g_w = settings.width;
        config.g_h = settings.height;
        config.g_bit_depth = u32::from(settings.bits);
        config.g_input_bit_depth = u32::from(settings.bits);
        config.g_threads = settings.threads;
        config.monochrome = u32::from(settings.monochrome);
        // Constant quality, which is what a canvas export wants: a quality
        // dial rather than a bitrate to hit.
        config.rc_end_usage = libaom_sys::AOM_Q;
        config.rc_min_quantizer = 0;
        config.rc_max_quantizer = match settings.lossless {
            true => 0,
            false => 63,
        };
        // No lag, so a frame comes out for every frame put in and the caller
        // does not have to reason about how many are still inside.
        config.g_lag_in_frames = 0;
        if !settings.still {
            config.kf_mode = libaom_sys::AOM_KF_AUTO;
            config.kf_min_dist = settings.frames;
            config.kf_max_dist = settings.frames;
        }

        // `aom_codec_flags_t` is a C long, not the u32 the constant is
        // declared as.
        let flags = match deep {
            true => AOM_CODEC_USE_HIGHBITDEPTH as std::os::raw::c_long,
            false => 0,
        };

        // SAFETY: `context` is initialised by this call and read afterwards
        // only if it reported success. The ABI version is the constant the
        // header requires, which is what makes the struct layouts agree.
        let mut context = unsafe { std::mem::zeroed::<aom_codec_ctx_t>() };
        // SAFETY: as above; `config` outlives the call.
        let status = unsafe {
            aom_codec_enc_init_ver(
                &mut context,
                aom_codec_av1_cx(),
                &config,
                flags,
                AOM_ENCODER_ABI_VERSION as c_int,
            )
        };
        if status != AOM_CODEC_OK {
            // SAFETY: the context is initialised enough to carry an error
            // even when init failed, which is the only reason to read it
            // here; the string is libaom's static storage.
            let detail = unsafe {
                let text = libaom_sys::aom_codec_error(&context);
                match text.is_null() {
                    true => String::new(),
                    false => std::ffi::CStr::from_ptr(text)
                        .to_string_lossy()
                        .into_owned(),
                }
            };
            return Err(format!(
                "Could not start the AV1 encoder: {status} {detail}"
            ));
        }

        // SAFETY: a null `img` asks libaom to allocate the descriptor as well
        // as the storage, which is what is wanted here; the result is checked
        // for null before use and freed in `drop`.
        let image = unsafe {
            aom_img_alloc(
                ptr::null_mut(),
                settings.sampling.format(deep),
                settings.width,
                settings.height,
                ROW_ALIGNMENT,
            )
        };
        if image.is_null() {
            // SAFETY: the context was initialised above and is destroyed once.
            unsafe { aom_codec_destroy(&mut context) };
            return Err("Could not allocate an AV1 frame".to_string());
        }

        let encoder = Self {
            context,
            image,
            deep,
        };
        encoder.control(AOME_SET_CPUUSED as c_int, settings.speed as c_int)?;
        // Tiles, which is what the thread count above is for. Set even when
        // both are zero: that is libaom's default, and saying so costs
        // nothing and keeps the two controls together where a reader looks
        // for them.
        let (across, down) = settings.tiling;
        encoder.control(AV1E_SET_TILE_COLUMNS as c_int, across as c_int)?;
        encoder.control(AV1E_SET_TILE_ROWS as c_int, down as c_int)?;
        if let Some(colour) = settings.colour {
            encoder.control(
                AV1E_SET_COLOR_PRIMARIES as c_int,
                c_int::from(colour.primaries),
            )?;
            encoder.control(
                AV1E_SET_TRANSFER_CHARACTERISTICS as c_int,
                c_int::from(colour.transfer),
            )?;
            encoder.control(
                AV1E_SET_MATRIX_COEFFICIENTS as c_int,
                c_int::from(colour.matrix),
            )?;
            encoder.control(
                AV1E_SET_COLOR_RANGE as c_int,
                c_int::from(colour.full_range),
            )?;
        }
        match settings.lossless {
            true => encoder.control(AV1E_SET_LOSSLESS as c_int, 1)?,
            false => encoder.control(
                AOME_SET_CQ_LEVEL as c_int,
                settings.quantizer as c_int,
            )?,
        }
        Ok(encoder)
    }

    /// Sets one integer control.
    fn control(&self, id: c_int, value: c_int) -> Result<(), String> {
        // SAFETY: `aom_codec_control` is variadic, so the argument has to
        // match the width the identifier expects. Every control reached here
        // takes an `int`, which is what this signature enforces by taking
        // nothing else.
        let status = unsafe {
            aom_codec_control(&self.context as *const _ as *mut _, id, value)
        };
        match status {
            AOM_CODEC_OK => Ok(()),
            code => {
                Err(format!("The AV1 encoder refused control {id}: {code}"))
            }
        }
    }

    /// The image's three planes, as mutable rows, for a caller to fill.
    ///
    /// Rows are handed out as bytes. A deep picture holds two bytes a sample
    /// in the host's own order, which is the one place byte order matters.
    pub(crate) fn planes(&mut self) -> [Vec<&mut [u8]>; 3] {
        [0, 1, 2].map(|index| {
            // SAFETY: the image was allocated by `aom_img_alloc` for these
            // dimensions, so libaom guarantees `height` rows of `stride`
            // bytes from each plane pointer. Each slice is cut to the
            // plane's own width, so the alignment padding past it is never
            // touched, and the borrow on `self` keeps them from outliving
            // the image.
            unsafe {
                let image = &*self.image;
                let (width, height) = (
                    libaom_sys::aom_img_plane_width(image, index) as usize,
                    libaom_sys::aom_img_plane_height(image, index) as usize,
                );
                let plane = image.planes[index as usize];
                let stride = image.stride[index as usize] as usize;
                let scale = match self.deep {
                    true => DEEP_SAMPLE_BYTES,
                    false => 1,
                };
                (0..height)
                    .map(|row| {
                        std::slice::from_raw_parts_mut(
                            plane.add(row * stride),
                            width * scale,
                        )
                    })
                    .collect()
            }
        })
    }

    /// Codes the frame currently in the image and returns its packets.
    ///
    /// `pts` and `duration` are in the caller's own time base, and matter
    /// only for a sequence.
    pub(crate) fn encode(
        &mut self,
        pts: i64,
        duration: u64,
    ) -> Result<Vec<Packet>, String> {
        self.send(Some(pts), duration)
    }

    /// Tells the encoder no more frames are coming and drains what is left.
    pub(crate) fn finish(&mut self) -> Result<Vec<Packet>, String> {
        self.send(None, 0)
    }

    fn send(
        &mut self,
        pts: Option<i64>,
        duration: u64,
    ) -> Result<Vec<Packet>, String> {
        let image = match pts {
            Some(_) => self.image as *const aom_image_t,
            // A null image is how the C API says "flush".
            None => ptr::null(),
        };
        // SAFETY: the image is either the one allocated in `new` or null, and
        // libaom copies what it needs during the call rather than retaining
        // the pointer.
        let status = unsafe {
            aom_codec_encode(
                &mut self.context,
                image,
                pts.unwrap_or(0),
                duration as std::os::raw::c_ulong,
                0,
            )
        };
        if status != AOM_CODEC_OK {
            return Err(format!("Could not encode as AV1: {status}"));
        }

        let mut packets = Vec::new();
        let mut iterator = ptr::null();
        loop {
            // SAFETY: `iterator` starts null as the header requires, and the
            // returned pointer is libaom's, valid only until the next call
            // into it -- so the bytes are copied out before the next turn of
            // this loop.
            let packet = unsafe {
                aom_codec_get_cx_data(&mut self.context, &mut iterator)
            };
            let Some(packet) = (unsafe { packet.as_ref() }) else {
                break;
            };
            if packet.kind != AOM_CODEC_CX_FRAME_PKT {
                continue;
            }
            // SAFETY: the packet said it is a frame, which is the union arm
            // this reads, and the buffer it names is libaom's for the moment
            // it takes to copy it.
            unsafe {
                let frame = packet.data.frame;
                packets.push(Packet {
                    data: std::slice::from_raw_parts(
                        frame.buf as *const u8,
                        frame.sz,
                    )
                    .to_vec(),
                    key: frame.flags & AOM_FRAME_IS_KEY != 0,
                });
            }
        }
        Ok(packets)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: both were created in `new`, which is the only way to build
        // this type, and this runs once.
        unsafe {
            aom_img_free(self.image);
            aom_codec_destroy(&mut self.context);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::aom::Decoder;

    const SIDE: u32 = 64;

    /// A pattern with something in every plane and no flat regions, so a
    /// plane written to the wrong place or a row read at the wrong stride
    /// shows up rather than averaging out.
    fn pattern(x: usize, y: usize) -> [u8; 3] {
        [
            (x * 4 % 256) as u8,
            (y * 4 % 256) as u8,
            ((x + y) * 2 % 256) as u8,
        ]
    }

    fn settings(lossless: bool) -> Settings {
        Settings {
            width: SIDE,
            height: SIDE,
            bits: 8,
            sampling: Sampling::Full,
            quantizer: 20,
            speed: 6,
            lossless,
            monochrome: false,
            still: true,
            frames: 1,
            threads: 1,
            // One tile, which is what the fixtures here want: they check
            // what the codec does with the pixels, not how it divides them.
            tiling: (0, 0),
            colour: None,
        }
    }

    fn encoded(lossless: bool) -> Vec<u8> {
        let mut encoder = Encoder::new(&settings(lossless)).expect("starts");
        {
            let mut planes = encoder.planes();
            for (index, plane) in planes.iter_mut().enumerate() {
                for (y, row) in plane.iter_mut().enumerate() {
                    for (x, sample) in row.iter_mut().enumerate() {
                        *sample = pattern(x, y)[index];
                    }
                }
            }
        }
        let mut out = encoder.encode(0, 1).expect("encodes");
        out.extend(encoder.finish().expect("flushes"));
        out.into_iter().flat_map(|packet| packet.data).collect()
    }

    #[test]
    fn lossless_survives_the_codec_exactly() {
        // The whole reason this module exists. rav1e cannot do this at all --
        // its lossless block is unimplemented -- so before libaom encoded
        // here, a quantizer of zero still filtered, and a round trip landed
        // within a level rather than on it.
        //
        // No container: the encoder's own bytes go straight to the decoder,
        // so this measures the codec and nothing around it.
        let bytes = encoded(true);
        let mut decoder = Decoder::new(1).expect("starts");
        let picture = decoder.decode(&bytes).expect("decodes");

        let planes = [picture.luma(), picture.cb(), picture.cr()];
        assert_eq!(planes[0].len(), SIDE as usize, "rows");
        assert_eq!(planes[0][0].len(), SIDE as usize, "columns");
        for (index, plane) in planes.iter().enumerate() {
            for (y, row) in plane.iter().enumerate() {
                for (x, sample) in row.iter().enumerate() {
                    assert_eq!(
                        *sample,
                        pattern(x, y)[index],
                        "plane {index} at {x},{y}"
                    );
                }
            }
        }
    }

    #[test]
    fn lossy_is_smaller_than_lossless_and_not_exact() {
        // Both halves matter. Smaller proves the quantizer is reaching the
        // encoder; inexact proves the lossless flag is doing something rather
        // than being the default.
        let lossless = encoded(true);
        let lossy = encoded(false);
        assert!(
            lossy.len() < lossless.len(),
            "lossy should be smaller: {} against {}",
            lossy.len(),
            lossless.len()
        );

        let mut decoder = Decoder::new(1).expect("starts");
        let picture = decoder.decode(&lossy).expect("decodes");
        let luma = picture.luma();
        let differs = luma.iter().enumerate().any(|(y, row)| {
            row.iter()
                .enumerate()
                .any(|(x, sample)| *sample != pattern(x, y)[0])
        });
        assert!(differs, "a quantized picture should not come back exact");
    }
}
