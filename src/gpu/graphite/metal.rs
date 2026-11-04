//! The Graphite engine on Metal: one context per thread, made when the thread
//! first asks and dropped once it has gone unused.

use std::{cell::RefCell, sync::OnceLock, time::Instant};

use objc2::{
    rc::{Retained, autoreleasepool},
    runtime::ProtocolObject,
};
use objc2_metal::{MTLCreateSystemDefaultDevice, MTLDevice};
use serde_json::{Value, json};
use skia_safe::{
    Canvas, IPoint, ImageInfo, Surface,
    gpu::{
        Mipmapped,
        graphite::{
            Context, ContextOptions, ImageProvider, InsertRecordingInfo,
            InsertStatus, Recorder, RecorderOptions, SampleCount,
            mtl::{BackendContext, Handle, context_factory},
            surfaces,
        },
    },
};

use super::GraphiteRead;
use crate::{
    context::page::ExportOptions,
    gpu::{
        ThreadBound,
        metal::{self, MTL_CONTEXT_LIFESPAN, RecordingQueue, device_name},
    },
};

/// The bytes of uploaded images a context's provider keeps for the raster
/// images its pages draw, least recently used out first.
///
/// Chosen, not measured: it holds a few full-page images at the sizes the
/// tests and benchmarks draw, and no workload has been timed against another
/// figure. Graphite caches nothing for a client's provider, so this is the
/// only bound on what the cache holds.
const IMAGE_BUDGET: usize = 16 << 20;

thread_local!(
    static GRAPHITE: RefCell<Option<GraphiteContext>> =
        const { RefCell::new(None) };
);

static STATUS: OnceLock<Value> = OnceLock::new();

/// An Objective-C object as the opaque handle Graphite's Metal backend takes.
///
/// Handed over unretained: `BackendContext::new` retains what it is given and
/// releases it when it drops, so the caller is free to let its own `Retained`
/// go. Graphite only ever sends the object messages, so the `*mut` it asks for
/// writes nothing through the pointer.
fn handle_of<T>(object: &T) -> Handle {
    std::ptr::from_ref(object).cast_mut().cast()
}

pub(super) fn supported() -> bool {
    probed()["renderer"] == "GPU"
}

pub(super) fn status() -> Value {
    probed().clone()
}

/// Whether a context can be made, asked the first time anything wants to
/// know and held for the process.
fn probed() -> &'static Value {
    STATUS.get_or_init(|| match GraphiteContext::new() {
        Some(made) => json!({
            "renderer": "GPU",
            "api": "Metal",
            // `api` stays "Metal", one of the values `canvas.engine` is
            // typed to give; the device text is free, and says Graphite.
            "device": format!("{}, Graphite", device_name(&made.device)),
            "threads": rayon::current_num_threads(),
        }),
        None => json!({
            "renderer": "CPU",
            "api": "Metal",
            "device": "CPU-based renderer (Fallback)",
            "threads": rayon::current_num_threads(),
            "error": "Graphite initialization failed",
        }),
    })
}

pub(super) fn make_surface(
    info: &ImageInfo,
    opts: &ExportOptions,
) -> Result<Surface, String> {
    with_context(|graphite| graphite.surface(info, opts))
}

/// Where a confirmation begins, in the buffers this thread's queue has seen
/// finish -- see [`RecordingQueue::mark`].
#[derive(Clone, Copy)]
pub(crate) struct Mark(metal::Mark);

pub(super) fn owns(surface: &mut Surface) -> bool {
    surface.canvas().recorder().is_some()
}

pub(super) fn commit(wait: bool) -> Result<(), GraphiteRead> {
    on_context(|graphite| graphite.commit(wait))
        .unwrap_or(Err(GraphiteRead::NoContext))
}

pub(super) fn mark() -> Option<Mark> {
    on_context(|graphite| graphite.mark())
}

pub(super) fn confirm(since: Option<Mark>) -> Result<(), String> {
    on_context(|graphite| graphite.confirm(since)).unwrap_or(Ok(()))
}

pub(super) fn read_raw(
    surface: &mut Surface,
    info: &ImageInfo,
    dst: &mut [u8],
    row_bytes: usize,
    at: IPoint,
) -> bool {
    on_context(|graphite| graphite.read_raw(surface, info, dst, row_bytes, at))
        .unwrap_or(false)
}

/// This thread's queue of ours, for a test to arm failures on.
#[cfg(test)]
pub(super) fn queue() -> Option<Retained<RecordingQueue>> {
    on_context(|graphite| graphite.queue.clone())
}

pub(super) fn probe(
    info: &ImageInfo,
    draw: impl FnOnce(&Canvas),
    read: &ImageInfo,
    dst: &mut [u8],
) -> Result<(), GraphiteRead> {
    autoreleasepool(|_| {
        let mut graphite =
            GraphiteContext::new().ok_or(GraphiteRead::NoContext)?;
        let mut surface = graphite
            .surface(info, &ExportOptions::default())
            .map_err(GraphiteRead::Refused)?;
        draw(surface.canvas());
        graphite.read(
            &mut surface,
            read,
            dst,
            read.min_row_bytes(),
            IPoint::new(0, 0),
        )
    })
}

pub(super) fn release_idle_context() {
    GRAPHITE.with_borrow_mut(|held| {
        held.take_if(|graphite| {
            graphite.last_use.elapsed() > MTL_CONTEXT_LIFESPAN
        });
    });
}

/// Runs `f` on this thread's context, `None` where the thread has none. Makes
/// no context: one made here would hold none of the draws it was asked about.
fn on_context<T>(f: impl FnOnce(&mut GraphiteContext) -> T) -> Option<T> {
    autoreleasepool(|_| GRAPHITE.with_borrow_mut(|held| held.as_mut().map(f)))
}

/// Runs `f` on this thread's context, making it first if the thread has none.
fn with_context<T>(
    f: impl FnOnce(&mut GraphiteContext) -> Result<T, String>,
) -> Result<T, String> {
    if !supported() {
        return Err("Graphite is not supported on this device".to_string());
    }
    autoreleasepool(|_| {
        GRAPHITE.with_borrow_mut(|held| {
            let graphite = match held {
                Some(graphite) => graphite,
                None => held.insert(
                    GraphiteContext::new()
                        .ok_or("Graphite initialization failed".to_string())?,
                ),
            };
            f(graphite)
        })
    })
}

/// A Graphite context on the system's Metal device, with the recorder its
/// pages are drawn through.
struct GraphiteContext {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    // Declared before `context`, so that it drops first: a recorder belongs to
    // the context that made it.
    recorder: Recorder,
    context: Context,
    /// Kept beside the recorder holding a reference of its own, so that what
    /// it caches is visible here.
    _images: ImageProvider,
    /// The queue Graphite makes its command buffers on, so that a read can
    /// confirm each one -- see [`RecordingQueue`].
    queue: Retained<RecordingQueue>,
    last_use: Instant,
    /// This context is active on the thread that built it. See
    /// [`ThreadBound`].
    _thread: ThreadBound,
}

impl GraphiteContext {
    fn new() -> Option<Self> {
        let device = MTLCreateSystemDefaultDevice()?;
        let queue = RecordingQueue::new(device.newCommandQueue()?);
        // SAFETY: `handle_of` explains the contract -- `BackendContext::new`
        // retains both handles and releases them when it drops. Device and
        // queue are live `Retained` values here, so neither pointer dangles
        // for the call.
        let backend = unsafe {
            BackendContext::new(handle_of(&*device), handle_of(&*queue))
        };
        // One sample a pixel selects Graphite's raster atlas: paths and text
        // are covered on the CPU, with the analytic coverage a raster page
        // draws with, rather than multisampled on the GPU.
        let mut options = ContextOptions::default();
        options.set_internal_multisample_count(SampleCount::k1);
        let mut context = context_factory::make_metal(&backend, &options)?;
        // Without a provider a recorder drops every raster image it is asked
        // to draw.
        let images = ImageProvider::new_caching(IMAGE_BUDGET);
        let mut recorder_options = RecorderOptions::default();
        recorder_options.set_image_provider(&images);
        let recorder = context.make_recorder(Some(&recorder_options))?;
        Some(Self {
            device,
            recorder,
            context,
            _images: images,
            queue,
            last_use: Instant::now() + MTL_CONTEXT_LIFESPAN,
            _thread: ThreadBound::new(),
        })
    }

    fn surface(
        &mut self,
        info: &ImageInfo,
        opts: &ExportOptions,
    ) -> Result<Surface, String> {
        self.last_use = self.last_use.max(Instant::now());
        surfaces::render_target(
            &mut self.recorder,
            info,
            Mipmapped::No,
            Some(&opts.surface_props()),
            None,
        )
        .ok_or(format!(
            "Could not allocate new {}×{} Graphite page (color type: {:?})",
            info.width(),
            info.height(),
            info.color_type()
        ))
    }

    fn mark(&self) -> Mark {
        Mark(self.queue.mark())
    }

    fn confirm(&self, since: Option<Mark>) -> Result<(), String> {
        self.queue.confirm(since.map(|Mark(since)| since))
    }

    /// Snaps the recorder, inserts the recording and submits it, waiting for
    /// the GPU to finish it where `wait` says so.
    fn commit(&mut self, wait: bool) -> Result<(), GraphiteRead> {
        self.last_use = self.last_use.max(Instant::now());
        let mut recording = self.recorder.snap().ok_or_else(|| {
            GraphiteRead::Refused("the recorder could not snap".to_string())
        })?;
        let inserted = self
            .context
            .insert_recording(&InsertRecordingInfo::new(&mut recording));
        if inserted != InsertStatus::Success {
            return Err(GraphiteRead::Refused(format!(
                "the recording was not inserted ({inserted:?})"
            )));
        }
        let submitted = match wait {
            true => self.context.submit_and_wait(),
            false => self.context.submit(None),
        };
        match submitted {
            true => Ok(()),
            false => Err(GraphiteRead::Refused(
                "the recording was not submitted".to_string(),
            )),
        }
    }

    /// Reads `surface` through the context. Graphite's read submits and
    /// waits for the copy itself, and with it any work submitted before.
    fn read_raw(
        &mut self,
        surface: &mut Surface,
        info: &ImageInfo,
        dst: &mut [u8],
        row_bytes: usize,
        at: IPoint,
    ) -> bool {
        self.last_use = self.last_use.max(Instant::now());
        self.context.read_pixels(surface, info, dst, row_bytes, at)
    }

    /// A read of `surface` with its draws committed first and every buffer
    /// since the mark confirmed after, for the probe: mark, commit without
    /// waiting, read, confirm. The read's own wait covers the draws.
    fn read(
        &mut self,
        surface: &mut Surface,
        info: &ImageInfo,
        dst: &mut [u8],
        row_bytes: usize,
        at: IPoint,
    ) -> Result<(), GraphiteRead> {
        let since = self.mark();
        self.commit(false)?;
        if !self.read_raw(surface, info, dst, row_bytes, at) {
            return Err(GraphiteRead::Declined);
        }
        self.confirm(Some(since)).map_err(GraphiteRead::Unconfirmed)
    }
}

#[cfg(test)]
mod tests {
    use skia_safe::{
        AlphaType, BlendMode, Color4f, ColorSpace, ColorType, Paint, Pixmap,
        Rect,
    };

    use super::*;
    use crate::gpu::{RenderingEngine, metal::BUFFER_DISCARDED};

    const SIDE: i32 = 16;

    fn f32_info() -> ImageInfo {
        ImageInfo::new(
            (SIDE, SIDE),
            ColorType::RGBAF32,
            AlphaType::Premul,
            Some(ColorSpace::new_srgb()),
        )
    }

    /// Premultiplied colours over the page, translucent ones included, as
    /// the bytes of an `F32` page.
    fn source() -> Vec<u8> {
        (0..SIDE * SIDE)
            .flat_map(|i| {
                let (x, y) =
                    ((i % SIDE) as f32 / 15.0, (i / SIDE) as f32 / 15.0);
                let a = [1.0, 0.75, 0.5, 0.123][i as usize % 4];
                [x * a, y * a, (1.0 - 0.5 * x - 0.5 * y) * a, a]
            })
            .flat_map(f32::to_le_bytes)
            .collect()
    }

    /// The colour [`written`] draws over the left half of the page, which
    /// every format holds exactly.
    const DRAWN: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

    /// A page written with `source`, then drawn on: its left half replaced
    /// with [`DRAWN`].
    fn written() -> Surface {
        let info = f32_info();
        let mut source = source();
        let pixmap = Pixmap::new(&info, &mut source, info.min_row_bytes())
            .expect("a pixmap of the source");
        let mut surface =
            make_surface(&info, &ExportOptions::default()).expect("a page");
        surface.write_pixels_from_pixmap(&pixmap, (0, 0));
        let mut paint = Paint::default();
        paint.set_anti_alias(false);
        paint.set_blend_mode(BlendMode::Src);
        paint.set_color4f(
            Color4f::new(DRAWN[0], DRAWN[1], DRAWN[2], DRAWN[3]),
            &ColorSpace::new_srgb(),
        );
        let half = Rect::from_xywh(0.0, 0.0, (SIDE / 2) as f32, SIDE as f32);
        surface.canvas().draw_rect(half, &paint);
        surface
    }

    /// The bytes a page from [`written`] holds.
    fn expected() -> Vec<u8> {
        let drawn: Vec<u8> =
            DRAWN.into_iter().flat_map(f32::to_le_bytes).collect();
        source()
            .chunks(drawn.len())
            .enumerate()
            .flat_map(|(i, pixel)| match i as i32 % SIDE < SIDE / 2 {
                true => drawn.clone(),
                false => pixel.to_vec(),
            })
            .collect()
    }

    /// `surface` read through this thread's primitives, in the order
    /// [`GraphiteContext::read`] uses: mark, commit the draws without waiting,
    /// read, confirm everything since the mark.
    fn read_page(
        surface: &mut Surface,
        info: &ImageInfo,
        dst: &mut [u8],
        row_bytes: usize,
    ) -> Result<(), GraphiteRead> {
        let since = mark();
        assert!(since.is_some(), "no mark on a thread holding a context");
        commit(false)?;
        if !read_raw(surface, info, dst, row_bytes, IPoint::new(0, 0)) {
            return Err(GraphiteRead::Declined);
        }
        confirm(since).map_err(GraphiteRead::Unconfirmed)
    }

    fn queue() -> Retained<RecordingQueue> {
        GRAPHITE.with_borrow(|held| {
            held.as_ref().expect("this thread's context").queue.clone()
        })
    }

    /// An `F32` page read back in its own format and space holds exactly
    /// what was written and drawn on it.
    #[test]
    fn a_page_reads_back_in_its_own_format_to_the_byte() {
        let info = f32_info();
        let mut surface = written();
        let mut read = vec![0u8; info.compute_min_byte_size()];
        read_page(&mut surface, &info, &mut read, info.min_row_bytes())
            .expect("the read");
        assert!(
            read == expected(),
            "the page read back differs from what was written and drawn"
        );
    }

    /// A command buffer that fails under the read fails the read, though
    /// Graphite's own read reports success.
    #[test]
    fn a_failed_command_buffer_fails_the_read() {
        let info = f32_info();
        let mut surface = written();
        queue().fail_after(0, 1);
        let mut read = vec![0u8; info.compute_min_byte_size()];
        let answer =
            read_page(&mut surface, &info, &mut read, info.min_row_bytes());
        assert_eq!(
            queue().unused_failures(),
            0,
            "the armed buffer was not handed out"
        );
        match answer {
            Err(GraphiteRead::Unconfirmed(why)) => {
                assert!(why.starts_with(BUFFER_DISCARDED), "{why}")
            }
            other => panic!("the read answered {other:?}"),
        }
    }

    /// A queue that sees none of the read's buffers fails the confirmation
    /// from the mark: every read commits at least the copy's, so seeing
    /// none means the draws went unwatched.
    #[test]
    fn a_read_whose_buffers_go_unseen_is_unconfirmed() {
        // Settled first: a context still finishing buffers it issued before
        // the seam was armed would be seen to finish them after the mark.
        drop(written());
        commit(true).expect("the settling commit");
        confirm(None).expect("the settling commit ran");
        let info = f32_info();
        let mut surface = written();
        let mut read = vec![0u8; info.compute_min_byte_size()];
        metal::BLIND_FOR_TEST.set(true);
        let answer =
            read_page(&mut surface, &info, &mut read, info.min_row_bytes());
        metal::BLIND_FOR_TEST.set(false);
        assert_eq!(
            answer,
            Err(GraphiteRead::Unconfirmed(metal::BLIND.to_string()))
        );
    }

    /// A read Graphite cannot make answers false, not whatever `dst` held.
    #[test]
    fn a_read_graphite_declines_answers_false() {
        let info = f32_info();
        let mut surface = written();
        let mut read = vec![0u8; info.compute_min_byte_size()];
        let short = info.min_row_bytes() - 1;
        commit(false).expect("the draws committed");
        assert!(!read_raw(
            &mut surface,
            &info,
            &mut read,
            short,
            IPoint::new(0, 0)
        ));
    }

    /// A waited commit leaves nothing running for the confirmation to wait
    /// on, and a buffer that failed under it fails the confirmation.
    #[test]
    fn a_waited_commit_is_confirmed_with_its_failures() {
        drop(written());
        let waited = metal::WAITED.get();
        commit(true).expect("the commit");
        confirm(None).expect("the commit's buffers ran");
        assert_eq!(
            metal::WAITED.get(),
            waited,
            "a waited commit left work running"
        );

        drop(written());
        queue().fail_after(0, 1);
        commit(true).expect("the commit");
        assert_eq!(
            queue().unused_failures(),
            0,
            "the armed buffer was not handed out"
        );
        let why = confirm(None).expect_err("a failed buffer was confirmed");
        assert!(why.starts_with(BUFFER_DISCARDED), "{why}");
    }

    /// A Graphite page says it is one, and pages of the other engines do not.
    #[test]
    fn a_graphite_page_is_told_from_the_others() {
        assert!(owns(&mut written()));
        let info = f32_info();
        for engine in [RenderingEngine::CPU, RenderingEngine::GPU] {
            let mut page = engine
                .make_surface(
                    &info.with_color_type(ColorType::N32),
                    &ExportOptions::default(),
                )
                .expect("a page");
            assert!(!owns(&mut page), "{engine:?}");
        }
    }

    /// The context is kept while it is in use, dropped once it has gone
    /// unused for the span, and made again when a page next asks for it.
    #[test]
    fn an_idle_context_is_dropped_and_made_again() {
        drop(written());
        let held = || GRAPHITE.with_borrow(Option::is_some);
        release_idle_context();
        assert!(held(), "a context just used was dropped");

        GRAPHITE.with_borrow_mut(|held| {
            let graphite = held.as_mut().expect("this thread's context");
            graphite.last_use = Instant::now() - 2 * MTL_CONTEXT_LIFESPAN;
        });
        release_idle_context();
        assert!(!held(), "an idle context was kept");

        let info = f32_info();
        let mut surface = written();
        assert!(held(), "no context was made for the next page");
        let mut read = vec![0u8; info.compute_min_byte_size()];
        read_page(&mut surface, &info, &mut read, info.min_row_bytes())
            .expect("a read on the new context");
        assert!(read == expected());
    }

    /// A probe leaves no context behind on the thread that asked.
    #[test]
    fn a_probe_leaves_no_context_on_its_thread() {
        let info = f32_info();
        let mut read = vec![0u8; info.compute_min_byte_size()];
        std::thread::spawn(move || {
            probe(&info, |_| {}, &info, &mut read).expect("the probe");
            assert!(GRAPHITE.with_borrow(Option::is_none));
        })
        .join()
        .expect("the probing thread");
    }
}
