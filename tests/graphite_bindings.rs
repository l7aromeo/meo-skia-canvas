//! The Graphite bindings `containers/rust-skia-graphite-bindings.patch` adds to
//! skia-safe, held on Metal: the internal multisample count, the caching image
//! provider, and the read of a Graphite-backed image. Built only where the
//! patched skia-bindings is in the build -- see `graphite_bindings` in
//! `build.rs`.
#![cfg(all(meo_graphite, target_os = "macos", feature = "metal"))]

use std::ffi::c_void;

use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_metal::{MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice};
use skia_safe::{
    AlphaType, Color, ColorType, Image, ImageInfo, Rect, Surface,
    gpu::{
        self,
        graphite::{self, mtl as gmtl},
    },
    surfaces,
};

/// The side of the pages drawn here, in pixels.
const PAGE: i32 = 16;

/// The side of the raster images drawn onto them, in pixels.
const IMAGE: i32 = 8;

/// A Graphite context on this machine's Metal device, and a recorder made
/// with `options`.
struct Graphite {
    context: graphite::Context,
    recorder: graphite::Recorder,
    _queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
}

impl Graphite {
    fn new(
        context_options: Option<&graphite::ContextOptions>,
        recorder_options: Option<&graphite::RecorderOptions>,
    ) -> Self {
        let device: Retained<ProtocolObject<dyn MTLDevice>> =
            MTLCreateSystemDefaultDevice().expect("a Metal device");
        let queue = device.newCommandQueue().expect("a command queue");
        // SAFETY: both pointers are live Metal objects, retained by `device`
        // and by `queue`, which the struct keeps for as long as the context.
        let backend = unsafe {
            gmtl::BackendContext::new(
                Retained::as_ptr(&device) as *mut c_void,
                Retained::as_ptr(&queue) as *mut c_void,
            )
        };
        let mut context =
            gmtl::context_factory::make_metal(&backend, context_options)
                .expect("a Graphite context");
        let recorder =
            context.make_recorder(recorder_options).expect("a recorder");
        Self {
            context,
            recorder,
            _queue: queue,
        }
    }

    fn page(&mut self) -> Surface {
        graphite::surfaces::render_target(
            &mut self.recorder,
            &info(PAGE),
            gpu::Mipmapped::No,
            None,
            None,
        )
        .expect("a Graphite surface")
    }

    /// What the recorder holds, played on the GPU and waited for.
    fn flush(&mut self) {
        if let Some(mut recording) = self.recorder.snap() {
            self.context
                .insert_recording(&graphite::InsertRecordingInfo::new(
                    &mut recording,
                ));
        }
        self.context.submit_and_wait();
    }

    /// The pixel at (1, 1) of `surface`, after the recorder's work is played.
    fn pixel(&mut self, surface: &mut Surface) -> [u8; 4] {
        self.flush();
        let mut px = vec![0u8; (PAGE * PAGE * 4) as usize];
        let info = info(PAGE);
        assert!(self.context.read_pixels(
            surface,
            &info,
            &mut px,
            info.min_row_bytes(),
            (0, 0)
        ));
        let at = ((PAGE + 1) * 4) as usize;
        [px[at], px[at + 1], px[at + 2], px[at + 3]]
    }
}

fn info(side: i32) -> ImageInfo {
    ImageInfo::new((side, side), ColorType::RGBA8888, AlphaType::Premul, None)
}

/// An `IMAGE`-sided raster image filled with `color`.
fn raster(color: Color) -> Image {
    let mut surface =
        surfaces::raster_n32_premul((IMAGE, IMAGE)).expect("a raster surface");
    surface.canvas().clear(color);
    surface.image_snapshot()
}

fn draw(surface: &mut Surface, image: &Image) {
    surface.canvas().draw_image_rect(
        image,
        None,
        Rect::from_wh(IMAGE as f32, IMAGE as f32),
        &skia_safe::Paint::default(),
    );
}

#[test]
fn the_internal_multisample_count_round_trips() {
    let mut options = graphite::ContextOptions::new();
    assert_eq!(
        options.internal_multisample_count(),
        graphite::SampleCount::k4
    );
    options.set_internal_multisample_count(graphite::SampleCount::k1);
    assert_eq!(
        options.internal_multisample_count(),
        graphite::SampleCount::k1
    );
    // The context takes it: a context made with one sample still draws.
    let mut graphite = Graphite::new(Some(&options), None);
    let mut page = graphite.page();
    page.canvas().clear(Color::BLUE);
    assert_eq!(graphite.pixel(&mut page), [0, 0, 255, 255]);
}

#[test]
fn a_raster_image_draws_through_the_provider_and_not_without_it() {
    let image = raster(Color::RED);

    let mut bare = Graphite::new(None, None);
    let mut page = bare.page();
    page.canvas().clear(Color::TRANSPARENT);
    draw(&mut page, &image);
    assert_eq!(
        bare.pixel(&mut page),
        [0, 0, 0, 0],
        "a recorder with no provider drew the image"
    );

    let provider = graphite::ImageProvider::new_caching(1 << 20);
    let mut options = graphite::RecorderOptions::new();
    options.set_image_provider(&provider);
    let mut provided = Graphite::new(None, Some(&options));
    let mut page = provided.page();
    page.canvas().clear(Color::TRANSPARENT);
    draw(&mut page, &image);
    assert_eq!(
        provided.pixel(&mut page),
        [255, 0, 0, 255],
        "the provider's upload was not drawn"
    );
}

#[test]
fn the_provider_keeps_an_image_once_and_drops_the_oldest_past_its_budget() {
    let each = (IMAGE * IMAGE * 4) as usize;
    // Room for two of the images and not three.
    let provider = graphite::ImageProvider::new_caching(2 * each + each / 2);
    let mut options = graphite::RecorderOptions::new();
    options.set_image_provider(&provider);
    let mut graphite = Graphite::new(None, Some(&options));
    let mut page = graphite.page();
    let (a, b, c) = (
        raster(Color::RED),
        raster(Color::GREEN),
        raster(Color::BLUE),
    );

    draw(&mut page, &a);
    graphite.flush();
    assert_eq!(provider.kept_bytes(), each, "the first image was not kept");
    draw(&mut page, &a);
    graphite.flush();
    assert_eq!(
        provider.kept_bytes(),
        each,
        "a second draw of it was kept again"
    );
    draw(&mut page, &b);
    graphite.flush();
    assert_eq!(provider.kept_bytes(), 2 * each);
    draw(&mut page, &c);
    graphite.flush();
    assert_eq!(provider.kept_bytes(), 2 * each, "the budget was passed");

    provider.purge();
    assert_eq!(provider.kept_bytes(), 0);
    // Purged, it uploads again what it is next asked for.
    page.canvas().clear(Color::TRANSPARENT);
    draw(&mut page, &a);
    assert_eq!(graphite.pixel(&mut page), [255, 0, 0, 255]);
    assert_eq!(provider.kept_bytes(), each);
}

#[test]
fn a_graphite_image_reads_back_through_the_context() {
    let mut graphite = Graphite::new(None, None);
    let mut page = graphite.page();
    page.canvas().clear(Color::from_argb(255, 10, 200, 30));
    graphite.flush();
    let image =
        graphite::surfaces::as_image(&page).expect("an image of the page");
    let info = info(PAGE);
    let mut px = vec![0u8; info.compute_min_byte_size()];
    assert!(graphite.context.read_image_pixels(
        &image,
        &info,
        &mut px,
        info.min_row_bytes(),
        (0, 0)
    ));
    assert!(
        px.chunks(4).all(|p| p == [10, 200, 30, 255]),
        "the image read back other pixels"
    );
    // Too short a destination is refused rather than written past.
    let mut short = vec![0u8; info.compute_min_byte_size() - 1];
    assert!(!graphite.context.read_image_pixels(
        &image,
        &info,
        &mut short,
        info.min_row_bytes(),
        (0, 0)
    ));
}
