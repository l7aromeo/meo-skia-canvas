//! Keeping this library's code mapped for as long as a thread it started can
//! run it.
//!
//! The thread pools here live for the process: rayon's global pool, the GIF
//! encoder's palette pool, and the watcher, completion and owner threads,
//! each parked in this library's code between uses. On Windows a DLL is
//! unmapped when the last `FreeLibrary` drops its count to zero -- Node calls
//! it for an addon when the last environment that loaded it, a worker's
//! included, is torn down -- and a parked thread that wakes afterwards
//! executes freed memory, an access violation in `<Unloaded_...>`. The pools
//! cannot be shut down instead: rayon's global pool has no shutdown, and
//! another environment may still be using it.
//!
//! So [`keep_loaded`] pins the module holding this code before anything here
//! starts a thread, and the module then stays mapped until the process ends.
//! Every site that starts one calls it first, which
//! `tests::every_thread_start_keeps_the_module_loaded` holds the tree to. On
//! macOS and Linux it does nothing: neither crashed this way.

/// Pins the module this library's code is in -- the DLL it is linked into,
/// whether its own Node addon or another library that links this crate -- so
/// that it stays mapped until the process exits. Once per process; nothing
/// off Windows.
///
/// Call it before starting a thread, or before a rayon call that may start
/// the global pool's: any thread running this library's code needs that code
/// mapped for as long as it lives.
pub(crate) fn keep_loaded() {
    #[cfg(windows)]
    {
        static PINNED: std::sync::Once = std::sync::Once::new();
        PINNED.call_once(pin);
    }
}

/// `GetModuleHandleExW` with `GET_MODULE_HANDLE_EX_FLAG_PIN`, given an
/// address inside this module rather than its name, which this code does not
/// know: a library linking the crate names its own DLL.
#[cfg(windows)]
fn pin() {
    use windows_sys::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_PIN,
        GetModuleHandleExW,
    };
    let mut module = std::ptr::null_mut();
    let inside = keep_loaded as *const () as *const u16;
    // SAFETY: with `FROM_ADDRESS` the second argument is read as an address,
    // never dereferenced as a string, and `keep_loaded` lies in the module
    // to pin; `module` is a live place for the handle the call writes.
    let pinned = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                | GET_MODULE_HANDLE_EX_FLAG_PIN,
            inside,
            &mut module,
        )
    };
    // A refusal leaves the module as Windows loaded it, which is how it was
    // before this existed. Nothing a caller can do differs, and the address
    // is this module's own, which the call has no documented reason to
    // refuse.
    let _ = pinned;
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    /// What starts a thread, or may: a spawned or built thread, a rayon
    /// pool built, and any call into rayon's global pool -- which a parallel
    /// iterator, a spawn, a broadcast and `current_num_threads` each start
    /// on first use. `current_thread_index` is not here: it reads the
    /// calling thread and starts nothing.
    const STARTS: &[&str] = &[
        "thread::spawn(",
        "thread::Builder::new(",
        "ThreadPoolBuilder::new(",
        "rayon::spawn",
        "rayon::broadcast(",
        "rayon::join(",
        "rayon::scope(",
        "rayon::in_place_scope(",
        "rayon::current_num_threads(",
        ".par_",
        ".into_par_iter(",
    ];

    /// The call a site has to be preceded by.
    const KEEP: &str = "keep_loaded()";

    /// The sites in `source` that start a thread with no [`KEEP`] before
    /// them in their function, as `line: text`. Comments are skipped, and
    /// so is a module compiled only for tests -- a test binary is an
    /// executable, which is never unloaded -- from its `mod` line to the
    /// brace that closes it at the same indent, as rustfmt writes it.
    fn unkept(source: &str) -> Vec<String> {
        let (mut function, mut kept) = (0, None);
        let mut closing: Option<String> = None;
        let mut found = vec![];
        let lines: Vec<&str> = source.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if let Some(end) = &closing {
                if *line == end.as_str() {
                    closing = None;
                }
                continue;
            }
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            if let Some(next) = lines.get(index + 1)
                && is_test_only(code)
                && opens_module(next)
            {
                let indent = &next[..next.len() - next.trim_start().len()];
                closing = Some(format!("{indent}}}"));
                continue;
            }
            if opens_function(code) {
                function = index;
            }
            if code.contains(KEEP) {
                kept = Some(index);
            }
            if STARTS.iter().any(|start| code.contains(start))
                && kept.is_none_or(|at| at < function)
            {
                found.push(format!("{}: {}", index + 1, code));
            }
        }
        found
    }

    /// Whether `code` is a `cfg` attribute that compiles what follows only
    /// for tests: `test` named in it, and not under `not`.
    fn is_test_only(code: &str) -> bool {
        code.starts_with("#[cfg(")
            && code.contains("test")
            && !code.contains("not(test)")
    }

    /// Whether `line` opens a module with a body.
    fn opens_module(line: &str) -> bool {
        line.trim_start()
            .split_once("mod ")
            .is_some_and(|(before, after)| {
                after.trim_end().ends_with('{')
                    && before
                        .split_whitespace()
                        .all(|word| word.starts_with("pub"))
            })
    }

    /// Whether `code` opens a function: `fn` preceded by nothing but its
    /// qualifiers.
    fn opens_function(code: &str) -> bool {
        code.split_once("fn ").is_some_and(|(before, _)| {
            before.split_whitespace().all(|word| {
                word.starts_with("pub")
                    || matches!(word, "const" | "async" | "unsafe" | "extern")
            })
        })
    }

    /// Every `.rs` file under `dir`, but test files.
    fn sources(dir: &Path, into: &mut Vec<PathBuf>) {
        let entries = fs::read_dir(dir).expect("a source directory");
        for entry in entries {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name != "tests") {
                    sources(&path, into);
                }
            } else if path.extension().is_some_and(|ext| ext == "rs")
                && path.file_name().is_some_and(|name| name != "tests.rs")
            {
                into.push(path);
            }
        }
    }

    /// The checker refuses a site with no [`KEEP`], or with one only in an
    /// earlier function, and passes one with it first -- so its green below
    /// is a finding and not a pattern that matches nothing.
    #[test]
    fn the_checker_refuses_a_thread_started_unkept() {
        let bare = "fn go() {\n    std::thread::spawn(|| {});\n}\n";
        assert_eq!(unkept(bare).len(), 1, "a bare spawn");
        let pool = "fn go(v: &[u8]) {\n    v.par_iter().count();\n}\n";
        assert_eq!(unkept(pool).len(), 1, "a parallel iterator");
        let earlier = "fn one() {\n    keep_loaded();\n}\n\
                       pub(crate) fn two() {\n    rayon::spawn_fifo(|| {});\n}\n";
        assert_eq!(unkept(earlier).len(), 1, "kept in another function");
        let kept = "fn go() {\n    keep_loaded();\n    \
                    let n = rayon::current_num_threads();\n}\n";
        assert!(unkept(kept).is_empty(), "kept first: {:?}", unkept(kept));
        let comment = "/// std::thread::spawn(|| {});\nfn go() {}\n";
        assert!(unkept(comment).is_empty(), "a doc comment");
        let test = "fn go() {}\n#[cfg(all(test, unix))]\n\
                    pub(crate) mod tests {\n    \
                    fn t() { std::thread::spawn(|| {}); }\n}\n";
        assert!(unkept(test).is_empty(), "a test module");
        let after =
            format!("{test}fn later() {{\n    rayon::spawn(|| {{}});\n}}\n");
        assert_eq!(unkept(&after).len(), 1, "code after a test module");
        let not_test = "#[cfg(not(test))]\nmod real {\n    \
                        fn go() { std::thread::spawn(|| {}); }\n}\n";
        assert_eq!(
            unkept(not_test).len(),
            1,
            "a module compiled outside tests"
        );
    }

    /// Every site under `src` that starts a thread, or may start rayon's
    /// global pool, calls `keep_loaded` first in its function. A site that
    /// does not would leave a thread parked in code Windows can unmap.
    #[test]
    fn every_thread_start_keeps_the_module_loaded() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = vec![];
        sources(&root, &mut files);
        let (mut sites, mut unkept_sites) = (0, vec![]);
        for file in &files {
            let source = fs::read_to_string(file).expect("a source file");
            sites += source
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .filter(|line| STARTS.iter().any(|start| line.contains(start)))
                .count();
            let name = file.strip_prefix(&root).unwrap_or(file).display();
            unkept_sites.extend(
                unkept(&source)
                    .into_iter()
                    .map(|site| format!("src/{name}:{site}")),
            );
        }
        // The GIF palette pool and rayon's parallel iterators at the least:
        // a scan that finds a handful has stopped reading the tree.
        assert!(sites > 30, "only {sites} sites found");
        assert!(
            unkept_sites.is_empty(),
            "starts a thread without `keep_loaded` first:\n{}",
            unkept_sites.join("\n")
        );
    }
}
