//! The bytes of a font made from data, kept where CoreText cannot confuse
//! them with another font's.
//!
//! On macOS a face made from bytes reaches CoreText as those bytes, uncopied:
//! Skia wraps the memory in a `CFData` and hands it over. CoreText keeps a
//! cache of the fonts it has parsed, keyed by the address and length of that
//! memory, and the key outlives the memory -- so a later font whose bytes land
//! at a freed font's address with its exact length is read through the freed
//! font's table directory, and every table it is asked for comes back from
//! the wrong offsets. Each font's bytes therefore get a mapping of their own,
//! and the mapping is given up only after enough further fonts have been
//! loaded for CoreText to have forgotten it; until then its pages are
//! released to the system and its address stays taken.
//!
//! Elsewhere the bytes are an ordinary copy.

use skia_safe::Data;

/// `bytes` as data to make a face from, copied into memory that cannot be
/// mistaken for another font's while CoreText may still remember it.
pub(crate) fn font_data(bytes: &[u8]) -> Data {
    #[cfg(target_os = "macos")]
    return data_or_copy(
        bytes,
        mac::FontBytes::new(bytes, mac::shared(), None),
    );
    #[cfg(not(target_os = "macos"))]
    Data::new_copy(bytes)
}

/// `bytes` as `mapped` holds them, or copied where the system would not map
/// them -- as many faces as before, only without the quarantine.
#[cfg(target_os = "macos")]
fn data_or_copy(bytes: &[u8], mapped: Option<mac::FontBytes>) -> Data {
    mapped.map_or_else(|| Data::new_copy(bytes), Data::new_with_owner)
}

#[cfg(target_os = "macos")]
mod mac {
    use std::{collections::VecDeque, ptr::NonNull, sync::LazyLock};

    use parking_lot::Mutex;

    /// How many fonts made from data CoreText remembers by address and
    /// length: an entry is forgotten once this many more have been parsed
    /// after it. Measured on macOS 27.2 by calling CoreText directly, one
    /// font a load: bytes reused at the same address after 15 further loads
    /// are read through the old entry, after 16 they are not. Apple does not
    /// state it. A face made through Skia here is forgotten after 8 of
    /// [`FontBytes::new`]'s loads -- as if each took two entries, which is
    /// inferred rather than checked; the re-check below reports that figure.
    const CORE_TEXT_FONT_CACHE: u64 = 16;

    /// How many fonts must be loaded after a font's bytes are given up before
    /// their address may be reused: four times [`CORE_TEXT_FONT_CACHE`], and
    /// so eight times the depth measured through Skia -- the margin for a
    /// macOS whose cache is deeper than the one measured.
    // UPSTREAM: macOS CoreText 27.2 -- unfiled -- worked around
    // Re-check: cargo test a_reused_address_reads_the_old_font
    //
    // CoreText keys the fonts it parses from memory by the memory's address
    // and length, and keeps the key after releasing the memory, so a font
    // whose bytes land at a freed font's address with its length is read
    // through the freed font's table directory. The re-check fails once a
    // reused address reads its own tables, and reports the cache's depth.
    // Only faces made through `font_data` are kept apart: a caller making
    // faces from bytes with skia-safe directly is outside this. Linux and
    // Windows build their faces without CoreText and were not measured.
    pub(crate) const QUARANTINE_LOADS: u64 = 4 * CORE_TEXT_FONT_CACHE;

    /// The mappings whose fonts are gone, each held until
    /// [`QUARANTINE_LOADS`] more fonts have been loaded.
    #[derive(Debug, Default)]
    pub(crate) struct Quarantine {
        /// Fonts loaded through this quarantine so far.
        loads: u64,
        /// Mappings given up, with the load count at which each was.
        held: VecDeque<(Mapping, u64)>,
        /// Loads a mapping is held for: [`QUARANTINE_LOADS`], except in tests
        /// that show what happens without it.
        hold_for: u64,
    }

    impl Quarantine {
        /// A quarantine holding each mapping for `hold_for` loads.
        pub(crate) fn holding_for(hold_for: u64) -> Self {
            Self {
                hold_for,
                ..Self::default()
            }
        }

        /// Counts a load, and unmaps whatever has been held long enough.
        fn load(&mut self) {
            self.loads += 1;
            while self
                .held
                .front()
                .is_some_and(|(_, at)| self.loads - at >= self.hold_for)
            {
                self.held.pop_front();
            }
        }

        /// Takes a mapping whose font is gone: its pages go back to the
        /// system now, its address when [`Quarantine::load`] says so.
        fn hold(&mut self, mapping: Mapping) {
            mapping.release_pages();
            if self.hold_for == 0 {
                return;
            }
            self.held.push_back((mapping, self.loads));
        }

        /// How many mappings are held.
        #[cfg(test)]
        pub(crate) fn held(&self) -> usize {
            self.held.len()
        }

        /// How many loads have been counted.
        #[cfg(test)]
        pub(crate) fn loads(&self) -> u64 {
            self.loads
        }
    }

    /// The quarantine every font made from data goes through.
    pub(super) fn shared() -> &'static Mutex<Quarantine> {
        static SHARED: LazyLock<Mutex<Quarantine>> = LazyLock::new(|| {
            Mutex::new(Quarantine::holding_for(QUARANTINE_LOADS))
        });
        &SHARED
    }

    /// An anonymous private mapping, unmapped when dropped.
    #[derive(Debug)]
    pub(crate) struct Mapping {
        at: NonNull<u8>,
        length: usize,
    }

    // SAFETY: the mapping is private to this process and owned by this value
    // alone; nothing about it is tied to the thread that made it.
    unsafe impl Send for Mapping {}

    impl Mapping {
        /// `length` bytes, readable and writable, at `hint` if that address
        /// is free and the system's choice otherwise.
        fn new(length: usize, hint: Option<NonNull<u8>>) -> Option<Self> {
            let hint =
                hint.map_or(std::ptr::null_mut(), |at| at.as_ptr().cast());
            // SAFETY: an anonymous private mapping with no fixed address
            // touches no existing memory; the result is checked below.
            let at = unsafe {
                libc::mmap(
                    hint,
                    length,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANON,
                    -1,
                    0,
                )
            };
            if at == libc::MAP_FAILED {
                return None;
            }
            NonNull::new(at.cast()).map(|at| Self { at, length })
        }

        /// Hands the pages back to the system while keeping the range
        /// mapped and readable: `MADV_FREE_REUSABLE` takes them out of the
        /// process's footprint at once, and on macOS 27.2 the old contents
        /// stay readable until the system reclaims them. `MADV_DONTNEED`
        /// changes neither there.
        fn release_pages(&self) {
            // SAFETY: the range is this mapping's own. The advice only lets
            // the system discard the pages; a failure leaves them as they
            // were, which costs memory and nothing else.
            unsafe {
                libc::madvise(
                    self.at.as_ptr().cast(),
                    self.length,
                    libc::MADV_FREE_REUSABLE,
                );
            }
        }

        /// The mapping's first byte.
        #[cfg(test)]
        pub(crate) fn address(&self) -> NonNull<u8> {
            self.at
        }
    }

    impl Drop for Mapping {
        fn drop(&mut self) {
            // SAFETY: the range was mapped by `Mapping::new` and nothing
            // refers to it once its owner is gone.
            unsafe {
                libc::munmap(self.at.as_ptr().cast(), self.length);
            }
        }
    }

    /// A font's bytes in a mapping of their own, handed to the quarantine
    /// when the last reference to them goes.
    #[derive(Debug)]
    pub(crate) struct FontBytes {
        mapping: Option<Mapping>,
        length: usize,
        quarantine: &'static Mutex<Quarantine>,
    }

    impl FontBytes {
        /// `bytes`, copied into a mapping of their own, at `hint` where that
        /// address is free. Counts as a load of `quarantine`. `None` where
        /// there is nothing to map or the system will not map it.
        pub(crate) fn new(
            bytes: &[u8],
            quarantine: &'static Mutex<Quarantine>,
            hint: Option<NonNull<u8>>,
        ) -> Option<Self> {
            if bytes.is_empty() {
                return None;
            }
            quarantine.lock().load();
            let mapping = Mapping::new(bytes.len(), hint)?;
            // SAFETY: the mapping is `bytes.len()` bytes, writable, and new,
            // so it overlaps nothing `bytes` could be.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    mapping.at.as_ptr(),
                    bytes.len(),
                );
            }
            Some(Self {
                mapping: Some(mapping),
                length: bytes.len(),
                quarantine,
            })
        }

        /// Where the bytes are.
        #[cfg(test)]
        pub(crate) fn address(&self) -> Option<NonNull<u8>> {
            self.mapping.as_ref().map(Mapping::address)
        }
    }

    impl AsRef<[u8]> for FontBytes {
        fn as_ref(&self) -> &[u8] {
            match &self.mapping {
                // SAFETY: the mapping holds `length` initialised bytes,
                // written in `FontBytes::new`, and lives as long as `self`.
                Some(mapping) => unsafe {
                    std::slice::from_raw_parts(mapping.at.as_ptr(), self.length)
                },
                None => &[],
            }
        }
    }

    impl Drop for FontBytes {
        fn drop(&mut self) {
            if let Some(mapping) = self.mapping.take() {
                self.quarantine.lock().hold(mapping);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use std::{fs, process::Command, ptr::NonNull};

        use parking_lot::Mutex;
        use skia_safe::{Data, FontMgr, FourByteTag, Typeface};

        use super::{FontBytes, Mapping, QUARANTINE_LOADS, Quarantine};
        use crate::font::quarantine::data_or_copy;

        const AMSTELVAR: &str = "tests/assets/fonts/AmstelvarAlpha-VF.ttf";
        const OSWALD: &str =
            "tests/assets/fonts/Oswald/Oswald-VariableFont_wght.ttf";

        /// Amstelvar's `sTypoAscender` and `sTypoDescender`, 1500 and -500,
        /// as the file stores them.
        const AMSTELVAR_TYPO: [u8; 4] = [0x05, 0xdc, 0xfe, 0x0c];

        /// `font` with its tables' data laid out in the opposite order: the
        /// same tables, the same length, every table at another offset.
        fn reversed(font: &[u8]) -> Vec<u8> {
            let count = u16::from_be_bytes([font[4], font[5]]) as usize;
            let entry = |i: usize| &font[12 + 16 * i..28 + 16 * i];
            let word = |e: &[u8], at: usize| {
                u32::from_be_bytes(e[at..at + 4].try_into().expect("a word"))
                    as usize
            };
            let mut out = font[..12 + 16 * count].to_vec();
            let mut at = out.len();
            for i in (0..count).rev() {
                let padded = word(entry(i), 12).next_multiple_of(4);
                out[12 + 16 * i + 8..12 + 16 * i + 12]
                    .copy_from_slice(&(at as u32).to_be_bytes());
                at += padded;
            }
            out.resize(at, 0);
            for i in 0..count {
                let (from, length) = (word(entry(i), 8), word(entry(i), 12));
                let to = word(&out[12 + 16 * i..28 + 16 * i], 8);
                out[to..to + length]
                    .copy_from_slice(&font[from..from + length]);
            }
            out
        }

        /// Runs `body` as the test named `name`, alone in a child process of
        /// this test binary. CoreText's cache and the address space belong to
        /// the process, so fonts other tests load at the same time would evict
        /// the entries these tests watch and take the addresses they free.
        fn alone(name: &str, body: impl FnOnce()) {
            /// Set for the child, which runs the body the parent watches.
            const CHILD: &str = "MEO_SKIA_CANVAS_QUARANTINE_CHILD";
            if std::env::var_os(CHILD).is_some() {
                body();
                return;
            }
            let (_, module) =
                module_path!().split_once("::").expect("a crate path");
            let output =
                Command::new(std::env::current_exe().expect("the test binary"))
                    .args([
                        "--exact",
                        &format!("{module}::{name}"),
                        "--test-threads=1",
                        "--nocapture",
                    ])
                    .env(CHILD, "1")
                    .output()
                    .expect("the test binary runs");
            let stderr = String::from_utf8_lossy(&output.stderr);
            stderr
                .lines()
                .filter(|line| line.starts_with("CoreText "))
                .for_each(|line| eprintln!("{line}"));
            assert!(
                output.status.success(),
                "{name}, run alone, failed:\n{stderr}"
            );
        }

        /// A quarantine of its own, holding each mapping for `hold_for` loads.
        fn quarantine(hold_for: u64) -> &'static Mutex<Quarantine> {
            Box::leak(Box::new(Mutex::new(Quarantine::holding_for(hold_for))))
        }

        /// A face made from `bytes` through `quarantine`, at `hint` where
        /// that address is free, and where its bytes are.
        fn face(
            bytes: &[u8],
            quarantine: &'static Mutex<Quarantine>,
            hint: Option<NonNull<u8>>,
        ) -> (Typeface, NonNull<u8>) {
            let bytes =
                FontBytes::new(bytes, quarantine, hint).expect("mapped");
            let at = bytes.address().expect("an address");
            let face = FontMgr::new()
                .new_from_data(Data::new_with_owner(bytes), None)
                .expect("a face");
            (face, at)
        }

        /// The `sTypoAscender` and `sTypoDescender` bytes `face` reports.
        fn typo(face: &Typeface) -> [u8; 4] {
            let mut os2 = [0u8; 72];
            face.get_table_data(
                *FourByteTag::from_chars('O', 'S', '/', '2'),
                &mut os2,
            );
            os2[68..72].try_into().expect("four bytes")
        }

        /// Oswald, its length made distinct by `k` words of padding, so no
        /// two of these share CoreText's key with each other or anything
        /// else here.
        fn filler(k: usize) -> Vec<u8> {
            let mut bytes = fs::read(OSWALD).expect(OSWALD);
            bytes.resize(bytes.len() + 4 * (k + 1), 0);
            bytes
        }

        /// Amstelvar laid out one way, made into a face and let go; then
        /// `between` other fonts; then Amstelvar as it ships, mapped where
        /// the first was. Whether the second reads its own tables, or `None`
        /// where it could not be mapped there.
        fn reuse(between: usize, hold_for: u64) -> Option<bool> {
            let shipped = fs::read(AMSTELVAR).expect(AMSTELVAR);
            let first = reversed(&shipped);
            assert_eq!(first.len(), shipped.len(), "the key is the length too");
            // Copied before the first font is let go, so none of them can
            // take its address.
            let fillers: Vec<Data> =
                (0..between).map(|k| Data::new_copy(&filler(k))).collect();
            let quarantine = quarantine(hold_for);
            let (face_a, at) = face(&first, quarantine, None);
            assert_eq!(
                typo(&face_a),
                AMSTELVAR_TYPO,
                "the first reads its own"
            );
            drop(face_a);
            let kept: Vec<Typeface> = fillers
                .into_iter()
                .map(|data| {
                    FontMgr::new().new_from_data(data, None).expect("a face")
                })
                .collect();
            let (face_b, at_b) = face(&shipped, quarantine, Some(at));
            drop(kept);
            (at_b == at).then(|| typo(&face_b) == AMSTELVAR_TYPO)
        }

        /// A mapping the system refuses is `None`, and the bytes are then
        /// copied as they always were; mapped, the data is the mapping.
        #[test]
        fn a_font_that_cannot_be_mapped_is_copied() {
            assert!(Mapping::new(usize::MAX, None).is_none(), "mmap refused");
            let bytes = fs::read(OSWALD).expect(OSWALD);
            let copied = data_or_copy(&bytes, None);
            assert_eq!(copied.as_bytes(), &bytes[..]);
            let mapped =
                FontBytes::new(&bytes, quarantine(QUARANTINE_LOADS), None)
                    .expect("mapped");
            let at = mapped.address().expect("an address");
            let data = data_or_copy(&bytes, Some(mapped));
            assert_eq!(data.as_bytes(), &bytes[..]);
            assert_eq!(data.as_bytes().as_ptr(), at.as_ptr().cast_const());
        }

        /// Loads and releases on two threads at once are each counted once,
        /// and what is held stays within the quarantine's span.
        #[test]
        fn loads_on_two_threads_are_each_counted() {
            /// Loads each thread makes.
            const EACH: u64 = 200;
            /// Loads a mapping is held for in the second half: few enough
            /// that most are given up while the threads still run.
            const SHORT: u64 = 8;
            let bytes: &'static [u8] = &[0, 1, 0, 0, 0, 0, 0, 0];
            for (hold_for, check) in [(4 * EACH, "kept"), (SHORT, "given up")] {
                let quarantine = quarantine(hold_for);
                let threads: Vec<_> = (0..2)
                    .map(|_| {
                        std::thread::spawn(move || {
                            for _ in 0..EACH {
                                drop(
                                    FontBytes::new(bytes, quarantine, None)
                                        .expect("mapped"),
                                );
                            }
                        })
                    })
                    .collect();
                threads
                    .into_iter()
                    .for_each(|thread| thread.join().expect("joined"));
                let quarantine = quarantine.lock();
                assert_eq!(
                    quarantine.loads(),
                    2 * EACH,
                    "{check}: every load counted"
                );
                if hold_for > 2 * EACH {
                    assert_eq!(
                        quarantine.held() as u64,
                        2 * EACH,
                        "{check}: every mapping held"
                    );
                } else {
                    // Held past the last load are at most the mappings live
                    // then, one a thread, and those dropped in the span since.
                    assert!(
                        quarantine.held() as u64 <= SHORT + 2,
                        "{check}: {} held",
                        quarantine.held()
                    );
                }
            }
        }

        /// What the quarantine is for, and its re-check. With nothing held,
        /// a font mapped at the address of one CoreText has just let go, with
        /// its length, is read through the old font's table directory; and
        /// after enough further loads it is not. The depth found is reported,
        /// and must be inside the quarantine's.
        #[test]
        fn a_reused_address_reads_the_old_font() {
            alone("a_reused_address_reads_the_old_font", || {
                assert_eq!(
                    reuse(1, 0),
                    Some(false),
                    "a reused address read its own tables: CoreText no longer keeps the old key"
                );
                let depth = (2..=QUARANTINE_LOADS as usize)
                    .find(|&between| reuse(between, 0) == Some(true))
                    .expect("the old key outlived the quarantine");
                eprintln!("CoreText forgot the old font after {depth} loads");
            });
        }

        /// Held, the address cannot be reused: the font that would have taken
        /// it lands elsewhere and reads its own tables.
        #[test]
        fn a_held_address_is_not_reused() {
            alone("a_held_address_is_not_reused", || {
                assert_eq!(reuse(1, QUARANTINE_LOADS), None);
                let shipped = fs::read(AMSTELVAR).expect(AMSTELVAR);
                let filler = Data::new_copy(&filler(0));
                let quarantine = quarantine(QUARANTINE_LOADS);
                let (face_a, at) = face(&reversed(&shipped), quarantine, None);
                drop(face_a);
                let _kept =
                    FontMgr::new().new_from_data(filler, None).expect("a face");
                assert_eq!(
                    quarantine.lock().held(),
                    1,
                    "CoreText let the first go"
                );
                let (face_b, at_b) = face(&shipped, quarantine, Some(at));
                assert_ne!(at_b, at);
                assert_eq!(typo(&face_b), AMSTELVAR_TYPO);
            });
        }

        /// An address is given up after exactly [`QUARANTINE_LOADS`] loads,
        /// and a font mapped there then reads its own tables.
        #[test]
        fn a_held_address_is_given_up_after_its_loads() {
            alone("a_held_address_is_given_up_after_its_loads", || {
                let shipped = fs::read(AMSTELVAR).expect(AMSTELVAR);
                let quarantine = quarantine(QUARANTINE_LOADS);
                let (face_a, at) = face(&reversed(&shipped), quarantine, None);
                drop(face_a);
                let mut kept = vec![face(&filler(0), quarantine, None).0];
                assert_eq!(
                    quarantine.lock().held(),
                    1,
                    "CoreText let the first go"
                );
                for k in 1..QUARANTINE_LOADS as usize {
                    kept.push(face(&filler(k), quarantine, None).0);
                    assert_eq!(
                        quarantine.lock().held(),
                        1,
                        "still held after {k} loads"
                    );
                }
                let (face_c, at_c) = face(&shipped, quarantine, Some(at));
                assert_eq!(
                    quarantine.lock().held(),
                    0,
                    "given up on the last load"
                );
                assert_eq!(at_c, at, "the address was free again");
                assert_eq!(typo(&face_c), AMSTELVAR_TYPO);
            });
        }
    }
}
