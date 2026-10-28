//! Releasing a JavaScript thread's font caches when the idle watcher asks.
//!
//! Every font cache belongs to the thread that filled it -- the font library
//! is thread-local, and so are the collections it hands out, since skia-safe's
//! `FontCollection` is neither `Send` nor `Sync`. The watcher in
//! `crate::memory` runs on a thread of its own and cannot reach them. So each
//! thread that loads the addon, the main one and every worker, leaves a
//! channel here when it does, and the watcher posts the release through each.
//!
//! A posted release runs on its thread's event loop between two tasks, never
//! inside one. A layout is synchronous from the call that asks for it to the
//! return, so a release cannot land in the middle of one, nor between the
//! layout of a frame and the paint that follows it in the same task: the
//! worst it can do is make the next task lay its text out from cold.
//!
//! The channels are unreferenced: a thread with nothing else to do exits as
//! though they were not there, whether or not a release is waiting on one.

use neon::{
    context::{Context, ModuleContext},
    event::Channel,
};
use parking_lot::Mutex;

use crate::{memory::trim, node::font_library::FontLibrary};

/// A channel to each JavaScript thread that has loaded the addon and not yet
/// exited.
static THREADS: Mutex<Vec<Channel>> = Mutex::new(Vec::new());

/// Leaves a channel to the thread `cx` runs on, for the watcher to post its
/// releases through. Called once per thread, as the addon loads on it.
pub(crate) fn register(cx: &mut ModuleContext) {
    let mut channel = cx.channel();
    channel.unref(cx);
    THREADS.lock().push(channel);
}

/// Asks every JavaScript thread to empty its font caches, and to trim once it
/// has, since the watcher's own trim has run by then.
///
/// A thread that has exited refuses the post, and its channel is dropped
/// here.
pub(crate) fn release_font_caches() {
    THREADS.lock().retain(|channel| {
        channel
            .try_send(|_| {
                FontLibrary::release_shared_caches();
                trim();
                Ok(())
            })
            .is_ok()
    });
}
