//! A command queue of ours in front of Metal's, so that every command buffer
//! Skia commits on an offscreen context can be confirmed.
//!
//! Ganesh never reads the status of a buffer it does not wait on -- see the
//! module documentation of [`crate::gpu::completion`] -- and skia-safe exposes
//! neither its buffers nor a finished callback that reports one. But Ganesh
//! makes every buffer by sending the queue it was given `commandBuffer`
//! (`GrMtlCommandBuffer::Make`, in `skia/src/gpu/ganesh/mtl/
//! GrMtlCommandBuffer.mm` under the skia-bindings crate, lines 24 to 36), so a
//! queue of ours that forwards each message to Metal's sees every buffer it
//! hands out. [`RecordingQueue::confirm`] then reads the status of each one.
//!
//! In Skia M156 that is the only message the queue receives outside builds
//! with `GPU_TEST_UTILS` or `SK_ENABLE_DUMP_GPU`, which this crate does not
//! make: `testingOnly_startCapture` and `onDumpJSON` (`GrMtlGpu.mm`, lines
//! 1747 to 1754 and 1770 to 1905) are the only other uses of `fQueue`, and no
//! code reads a buffer's `commandQueue` back. Every method of the protocol is
//! forwarded all the same, so that the object is a queue to anything that
//! sends it one.

// UPSTREAM: skia-safe 0.156.0 -- unfiled -- worked around
// Re-check: cargo test a_discarded_early_buffer_fails_the_read

#[cfg(test)]
use std::cell::Cell;
use std::ptr::NonNull;

use objc2::{
    AllocAnyThread, DefinedClass, define_class, msg_send,
    rc::Retained,
    runtime::{NSObject, NSObjectProtocol, ProtocolObject},
};
use objc2_foundation::{NSString, NSUInteger};
#[allow(deprecated)]
use objc2_metal::{
    MTLCommandBuffer, MTLCommandBufferDescriptor, MTLCommandBufferStatus,
    MTLCommandQueue, MTLDevice, MTLResidencySet,
};
use parking_lot::Mutex;

/// The start of the reason [`RecordingQueue::confirm`] gives for a buffer
/// Metal failed, so that a test can match it whatever Metal's own text says.
pub(crate) const BUFFER_DISCARDED: &str = "Metal discarded a command buffer";

/// What [`RecordingQueue::confirm`] answers when it saw no buffer finish since
/// the read began. Every read commits at least one, the copy's, so this means
/// the queue is no longer told about the buffers Skia makes.
pub(crate) const BLIND: &str = "no command buffer was seen to finish for the read, so \
                     its draws are unconfirmed";

/// Where a read began, in buffers seen to finish, for
/// [`RecordingQueue::confirm`].
#[derive(Clone, Copy)]
pub(crate) struct Mark(usize);

/// A command buffer Skia was handed, kept to read its status.
struct Held {
    buffer: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
    /// Under test, whether it is to look failed when it finishes.
    #[cfg(test)]
    doomed: bool,
}

// SAFETY: a `Held` is only retained, released, asked its `status` and `error`
// and waited on with `waitUntilCompleted`, never encoded into or committed.
// Metal's command buffers take those from any thread: committing on one thread
// and waiting or polling status on another is the documented use of
// `waitUntilCompleted` and `status`. objc2-metal leaves the protocol without
// `Send` and `Sync` because encoding into one is not thread-safe, which a
// `Held` never does.
unsafe impl Send for Held {}
// SAFETY: as for `Send`.
unsafe impl Sync for Held {}

/// The buffers handed out and not yet seen to finish, and what was learnt from
/// those that did.
#[derive(Default)]
struct Issued {
    /// Each buffer made and not yet seen to finish, in the order made, which
    /// is the order Skia commits them in.
    buffers: Vec<Held>,
    /// The first failure not yet reported.
    failed: Option<String>,
    /// How many buffers have been seen to finish, cleanly or not.
    finished: usize,
    /// Under test, how many more buffers to hand out before dooming any, and
    /// how many to doom after that.
    #[cfg(test)]
    failing: (usize, usize),
}

impl Issued {
    /// Drops every buffer that has finished, noting the first failure.
    fn sweep(&mut self) {
        let Self {
            buffers,
            failed,
            finished,
            ..
        } = self;
        buffers.retain(|held| {
            let buffer = &held.buffer;
            let status = buffer.status();
            if status != MTLCommandBufferStatus::Completed
                && status != MTLCommandBufferStatus::Error
            {
                return true;
            }
            *finished += 1;
            #[cfg(test)]
            let status = match held.doomed {
                true => MTLCommandBufferStatus::Error,
                false => status,
            };
            if status == MTLCommandBufferStatus::Error && failed.is_none() {
                *failed = Some(failure(buffer));
            }
            false
        });
    }
}

/// The reason for a buffer whose status is an error, quoting Metal's.
fn failure(buffer: &ProtocolObject<dyn MTLCommandBuffer>) -> String {
    match buffer.error() {
        Some(error) => {
            format!("{BUFFER_DISCARDED}: {}", error.localizedDescription())
        }
        None => format!("{BUFFER_DISCARDED}, giving no reason"),
    }
}

/// The instance variables of [`RecordingQueue`].
pub(crate) struct Ivars {
    /// Metal's queue, which makes the buffers.
    inner: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    /// Behind a lock so that the object is `Sync`, as the protocol requires.
    /// Only the thread that owns the context sends it messages, so the lock is
    /// never contended.
    issued: Mutex<Issued>,
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `RecordingQueue` does not implement `Drop`; its ivars release Metal's
    //   queue and the buffers still held when the object deallocates.
    #[unsafe(super(NSObject))]
    #[ivars = Ivars]
    pub(crate) struct RecordingQueue;

    unsafe impl NSObjectProtocol for RecordingQueue {}

    // SAFETY: each method forwards to the same method of Metal's queue, with
    // the arguments it was given, and returns what that returns; the three
    // that make a buffer also keep a reference to it. The selectors, argument
    // and return types are the protocol's, as objc2-metal declares them, and
    // each returns at +0 as its `none` method family requires.
    #[allow(deprecated)]
    unsafe impl MTLCommandQueue for RecordingQueue {
        #[unsafe(method_id(label))]
        fn __label(&self) -> Option<Retained<NSString>> {
            self.ivars().inner.label()
        }

        #[unsafe(method(setLabel:))]
        fn __set_label(&self, label: Option<&NSString>) {
            self.ivars().inner.setLabel(label);
        }

        #[unsafe(method_id(device))]
        fn __device(&self) -> Retained<ProtocolObject<dyn MTLDevice>> {
            self.ivars().inner.device()
        }

        #[unsafe(method_id(commandBuffer))]
        fn __command_buffer(
            &self,
        ) -> Option<Retained<ProtocolObject<dyn MTLCommandBuffer>>> {
            self.issue(self.ivars().inner.commandBuffer())
        }

        #[unsafe(method_id(commandBufferWithDescriptor:))]
        fn __command_buffer_with_descriptor(
            &self,
            descriptor: &MTLCommandBufferDescriptor,
        ) -> Option<Retained<ProtocolObject<dyn MTLCommandBuffer>>> {
            self.issue(
                self.ivars().inner.commandBufferWithDescriptor(descriptor),
            )
        }

        #[unsafe(method_id(commandBufferWithUnretainedReferences))]
        fn __command_buffer_with_unretained_references(
            &self,
        ) -> Option<Retained<ProtocolObject<dyn MTLCommandBuffer>>> {
            self.issue(
                self.ivars().inner.commandBufferWithUnretainedReferences(),
            )
        }

        #[unsafe(method(insertDebugCaptureBoundary))]
        fn __insert_debug_capture_boundary(&self) {
            self.ivars().inner.insertDebugCaptureBoundary();
        }

        #[unsafe(method(addResidencySet:))]
        fn __add_residency_set(
            &self,
            set: &ProtocolObject<dyn MTLResidencySet>,
        ) {
            self.ivars().inner.addResidencySet(set);
        }

        #[unsafe(method(addResidencySets:count:))]
        fn __add_residency_sets(
            &self,
            sets: NonNull<NonNull<ProtocolObject<dyn MTLResidencySet>>>,
            count: NSUInteger,
        ) {
            // SAFETY: the caller's array and count, handed on unchanged; the
            // protocol requires `count` sets at `sets` of whoever sends it.
            unsafe { self.ivars().inner.addResidencySets_count(sets, count) }
        }

        #[unsafe(method(removeResidencySet:))]
        fn __remove_residency_set(
            &self,
            set: &ProtocolObject<dyn MTLResidencySet>,
        ) {
            self.ivars().inner.removeResidencySet(set);
        }

        #[unsafe(method(removeResidencySets:count:))]
        fn __remove_residency_sets(
            &self,
            sets: NonNull<NonNull<ProtocolObject<dyn MTLResidencySet>>>,
            count: NSUInteger,
        ) {
            // SAFETY: as in `addResidencySets:count:`.
            unsafe { self.ivars().inner.removeResidencySets_count(sets, count) }
        }
    }
);

impl RecordingQueue {
    /// A queue of ours in front of `inner`.
    pub(crate) fn new(
        inner: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    ) -> Retained<Self> {
        #[cfg(test)]
        MADE.set(MADE.get() + 1);
        let this = Self::alloc().set_ivars(Ivars {
            inner,
            issued: Mutex::default(),
        });
        // SAFETY: `NSObject`'s designated initialiser, on an object whose
        // ivars are set.
        unsafe { msg_send![super(this), init] }
    }

    /// `buffer`, kept until it is seen to finish.
    fn issue(
        &self,
        buffer: Option<Retained<ProtocolObject<dyn MTLCommandBuffer>>>,
    ) -> Option<Retained<ProtocolObject<dyn MTLCommandBuffer>>> {
        #[cfg(test)]
        if BLIND_FOR_TEST.get() {
            return buffer;
        }
        let mut issued = self.ivars().issued.lock();
        // Swept here so that the list holds only buffers in flight, however
        // long the context goes between confirmations.
        issued.sweep();
        if let Some(buffer) = &buffer {
            #[cfg(test)]
            ISSUED.set(ISSUED.get() + 1);
            #[cfg(test)]
            let doomed = match &mut issued.failing {
                (0, 0) => false,
                (0, times) => {
                    *times -= 1;
                    true
                }
                (skip, _) => {
                    *skip -= 1;
                    false
                }
            };
            issued.buffers.push(Held {
                buffer: buffer.clone(),
                #[cfg(test)]
                doomed,
            });
        }
        buffer
    }

    /// Where a read begins, for [`Self::confirm`].
    pub(crate) fn mark(&self) -> Mark {
        let mut issued = self.ivars().issued.lock();
        issued.sweep();
        Mark(issued.finished)
    }

    /// Whether every buffer this queue has handed out and seen committed ran,
    /// waiting for any still running; called after Skia's own wait.
    ///
    /// A failure is answered once, at the first confirmation after it. With
    /// `since`, where a read began, no buffer seen to finish since then is a
    /// failure too: every read commits at least one, the copy's, so the queue
    /// is not being told about the buffers Skia makes.
    pub(crate) fn confirm(&self, since: Option<Mark>) -> Result<(), String> {
        // Buffers on one queue run in the order committed, so after Skia's
        // wait these have usually finished. Metal does not say that a later
        // buffer finishing means an earlier one has, so each is waited for.
        // One not yet committed is not, since its wait would never return.
        let running: Vec<_> = {
            let issued = self.ivars().issued.lock();
            issued
                .buffers
                .iter()
                .map(|held| &held.buffer)
                .filter(|buffer| {
                    let status = buffer.status();
                    status == MTLCommandBufferStatus::Committed
                        || status == MTLCommandBufferStatus::Scheduled
                })
                .cloned()
                .collect()
        };
        for buffer in &running {
            buffer.waitUntilCompleted();
        }
        #[cfg(test)]
        WAITED.set(WAITED.get() + running.len());
        let mut issued = self.ivars().issued.lock();
        issued.sweep();
        #[cfg(test)]
        if let Some(Mark(since)) = since {
            FINISHED.set(issued.finished - since);
        }
        if let Some(failed) = issued.failed.take() {
            return Err(failed);
        }
        match since {
            Some(Mark(since)) if issued.finished == since => {
                Err(BLIND.to_string())
            }
            _ => Ok(()),
        }
    }

    /// Makes the next `times` buffers handed out, after `skip` more, look
    /// failed when they finish, as buffers the GPU discarded do.
    #[cfg(test)]
    pub(crate) fn fail_after(&self, skip: usize, times: usize) {
        self.ivars().issued.lock().failing = (skip, times);
    }

    /// How many buffers armed by [`Self::fail_after`] have not been handed
    /// out.
    #[cfg(test)]
    pub(crate) fn unused_failures(&self) -> usize {
        self.ivars().issued.lock().failing.1
    }
}

#[cfg(test)]
thread_local! {
    /// How many queues of ours this thread has made.
    pub(crate) static MADE: Cell<usize> = const { Cell::new(0) };
    /// How many buffers the queues on this thread have handed out.
    pub(crate) static ISSUED: Cell<usize> = const { Cell::new(0) };
    /// How many buffers the last confirmation with a mark saw finish since it.
    pub(crate) static FINISHED: Cell<usize> = const { Cell::new(0) };
    /// How many buffers confirmations on this thread have waited for
    /// themselves, after Skia's wait.
    pub(crate) static WAITED: Cell<usize> = const { Cell::new(0) };
    /// While set, the queues on this thread keep nothing, as a queue Skia no
    /// longer asks for buffers would.
    pub(crate) static BLIND_FOR_TEST: Cell<bool> = const { Cell::new(false) };
}
