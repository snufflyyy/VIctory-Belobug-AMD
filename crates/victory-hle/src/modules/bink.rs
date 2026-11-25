//! `VICTORY_SKIP_MOVIES`: an opt-in shortcut through the title's movie
//! playback, so a test run gets past splash and intro movies in seconds.
//!
//! The title plays movies with RAD's Bink library, which it ships as a DLL
//! beside the executable. Playback is paced by the real clock: a loop asks
//! `BinkWait` whether the next frame is due, then advances with
//! `BinkNextFrame` and decodes it. With the switch on, the executable's
//! imports of four of the library's functions are bound to the wrappers in
//! this module instead of the library's own exports:
//!
//! - `BinkSetWillLoop` records whether the movie loops, then forwards;
//! - `BinkWait` reports "due" at once for a movie being skipped;
//! - `BinkNextFrame`, the first time a skipped movie advances, asks the
//!   library to go to the last frame (`BinkGoto` with a frame number past
//!   the end, which the library clamps to the last frame; it decodes from
//!   the nearest key frame, so the frame buffers hold a valid picture) and
//!   leaves the title to decode it. The next advance is the real one, which
//!   wraps past the end exactly as at the end of a movie played in full, so
//!   the title's own end-of-movie handling (pause, close, sound) runs;
//! - `BinkClose` forgets the movie, then forwards.
//!
//! Everything else (open, close, buffer registration, decoding, the sound
//! system the title hands the library) is the library's own, so memory and
//! frame buffers are as in a normal run. A movie is only skipped once it
//! has been through `BinkWait` (a movie the title steps by hand never is)
//! and the title has said whether it loops (`BinkSetWillLoop`, which sets
//! bit 7 of the handle's open flags); a movie that loops is left to the
//! library unless the switch is `all`.
//!
//! Values of `VICTORY_SKIP_MOVIES`: unset, empty or `0` is off, `1` skips
//! movies that do not loop, `all` skips those that loop as well (once each).
//!
//! Not scoped to startup: every non-looping, clock-paced movie the title
//! opens for the whole run ends the same way, whenever it plays.
//!
//! Ported from the `VICTORY_SKIP_MOVIES` commit (3ac02f1) of
//! <https://github.com/ka1qux/BRVIctory-ka1qux>, same license as this tree,
//! adapted to this branch's loader API.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Mutex, MutexGuard, OnceLock};

use victory_loader::{ImportResolver, Provenance, Resolution};
use victory_pe::ImportSymbol;

/// The environment variable that turns the shortcut on.
pub const SWITCH: &str = "VICTORY_SKIP_MOVIES";

/// A Bink handle (`HBINK`).
type Bink = *mut c_void;

/// What `BinkGoto` clamps to the last frame.
const PAST_THE_END: u32 = u32::MAX;

/// Which movies to skip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipMode {
    Off,
    /// Movies the title has not flagged to loop.
    NonLooping,
    /// Every movie.
    All,
}

impl SkipMode {
    /// The mode a value of [`SWITCH`] names; `None` if it names none.
    pub fn parse(value: &str) -> Option<SkipMode> {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "0" | "off" | "false" | "no" => Some(SkipMode::Off),
            "1" | "on" | "true" | "yes" => Some(SkipMode::NonLooping),
            "all" => Some(SkipMode::All),
            _ => None,
        }
    }

    /// From [`SWITCH`]; off when unset or not understood (the latter warns).
    pub fn from_env() -> SkipMode {
        let Some(value) = std::env::var_os(SWITCH) else {
            return SkipMode::Off;
        };
        let value = value.to_string_lossy();
        SkipMode::parse(&value).unwrap_or_else(|| {
            tracing::warn!(
                "{SWITCH}={value} is not understood (use 0, 1 or all); movies play in full"
            );
            SkipMode::Off
        })
    }

    /// Whether a movie is skipped, given what the title said of its looping
    /// (`None`: nothing yet, which `NonLooping` leaves alone).
    pub fn skips(self, looping: Option<bool>) -> bool {
        match self {
            SkipMode::Off => false,
            SkipMode::NonLooping => looping == Some(false),
            SkipMode::All => true,
        }
    }
}

/// Whether `dll` is a Bink library (they are named `bink*.dll`).
pub fn is_bink_dll(dll: &str) -> bool {
    let dll = dll.to_ascii_lowercase();
    dll.starts_with("bink") && dll.ends_with(".dll")
}

/// A movie as the log describes it (read from the handle's first fields).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MovieInfo {
    pub width: u32,
    pub height: u32,
    pub frames: u32,
    /// The frame the movie was at.
    pub frame: u32,
}

/// The library's own functions the wrappers forward to, with the argument
/// lists the executable calls them with.
#[derive(Clone, Copy)]
pub struct Api {
    pub wait: unsafe extern "system" fn(Bink) -> i32,
    pub next_frame: unsafe extern "system" fn(Bink),
    pub goto: unsafe extern "system" fn(Bink, u32, i32),
    pub set_will_loop: unsafe extern "system" fn(Bink, i32),
    pub close: unsafe extern "system" fn(Bink),
    pub describe: unsafe fn(Bink) -> MovieInfo,
}

/// Reads the public fields of a live handle that the log prints: width,
/// height, frame count and current frame number, the first four 32-bit
/// fields of `BINK` (checked against the library's own accessors).
///
/// # Safety
/// `bink` is a live handle from the library.
unsafe fn read_info(bink: Bink) -> MovieInfo {
    let field = |index: usize| {
        // SAFETY: the handle is a live `BINK`, whose first fields are
        // 32-bit integers.
        unsafe { bink.cast::<u32>().add(index).read_unaligned() }
    };
    MovieInfo {
        width: field(0),
        height: field(1),
        frames: field(2),
        frame: field(3),
    }
}

#[derive(Default)]
struct Movie {
    /// What the title last said to `BinkSetWillLoop`.
    looping: Option<bool>,
    /// The title has asked `BinkWait` about it: it is paced by the clock.
    paced: bool,
    /// Already sent to its last frame.
    jumped: bool,
}

/// The state machine behind the wrappers: what to do with each call, per
/// handle, given the library's functions.
pub struct Skipper {
    mode: SkipMode,
    api: Api,
    movies: Mutex<HashMap<usize, Movie>>,
}

impl Skipper {
    pub fn new(mode: SkipMode, api: Api) -> Skipper {
        Skipper {
            mode,
            api,
            movies: Mutex::new(HashMap::new()),
        }
    }

    /// The library's exports by name (`exports`), or `None` if one of the
    /// functions the wrappers forward to is missing.
    pub fn from_exports(
        mode: SkipMode,
        exports: &dyn Fn(&str) -> Option<usize>,
    ) -> Option<Skipper> {
        let find = |name: &str| {
            let found = exports(name);
            if found.is_none() {
                tracing::warn!("{SWITCH}: the movie library has no {name}; movies play in full");
            }
            found
        };
        let (wait, next_frame, goto, set_will_loop, close) = (
            find("BinkWait")?,
            find("BinkNextFrame")?,
            find("BinkGoto")?,
            find("BinkSetWillLoop")?,
            find("BinkClose")?,
        );
        // SAFETY: each address is the library's export of that name, which
        // takes the arguments declared in `Api` (RAD's public header).
        let api = unsafe {
            Api {
                wait: std::mem::transmute::<usize, unsafe extern "system" fn(Bink) -> i32>(wait),
                next_frame: std::mem::transmute::<usize, unsafe extern "system" fn(Bink)>(
                    next_frame,
                ),
                goto: std::mem::transmute::<usize, unsafe extern "system" fn(Bink, u32, i32)>(goto),
                set_will_loop: std::mem::transmute::<usize, unsafe extern "system" fn(Bink, i32)>(
                    set_will_loop,
                ),
                close: std::mem::transmute::<usize, unsafe extern "system" fn(Bink)>(close),
                describe: read_info,
            }
        };
        Some(Skipper::new(mode, api))
    }

    fn movies(&self) -> MutexGuard<'_, HashMap<usize, Movie>> {
        self.movies.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `BinkSetWillLoop`.
    ///
    /// # Safety
    /// As the library's function.
    pub unsafe fn set_will_loop(&self, bink: Bink, will_loop: i32) {
        if !bink.is_null() {
            self.movies().entry(bink as usize).or_default().looping = Some(will_loop != 0);
        }
        // SAFETY: forwarded with the title's arguments.
        unsafe { (self.api.set_will_loop)(bink, will_loop) }
    }

    /// `BinkWait`: nonzero when the next frame is not due yet.
    ///
    /// # Safety
    /// As the library's function.
    pub unsafe fn wait(&self, bink: Bink) -> i32 {
        if !bink.is_null() {
            let mut movies = self.movies();
            let movie = movies.entry(bink as usize).or_default();
            movie.paced = true;
            // A movie that loops, once sent to its last frame, plays on at
            // the library's pace.
            let resumed = movie.jumped && movie.looping == Some(true);
            if self.mode.skips(movie.looping) && !resumed {
                return 0;
            }
        }
        // SAFETY: forwarded with the title's argument.
        unsafe { (self.api.wait)(bink) }
    }

    /// `BinkNextFrame`.
    ///
    /// # Safety
    /// As the library's function.
    pub unsafe fn next_frame(&self, bink: Bink) {
        if self.take_jump(bink) {
            // SAFETY: a live handle, as the title passed it to us.
            let before = unsafe { (self.api.describe)(bink) };
            // SAFETY: the title's handle; the frame is clamped by the library.
            unsafe { (self.api.goto)(bink, PAST_THE_END, 0) };
            // SAFETY: as above.
            let after = unsafe { (self.api.describe)(bink) };
            if before.frames == 0 || after.frame == before.frames {
                tracing::info!(
                    skipped = before.frames - before.frame.min(before.frames),
                    frames = before.frames,
                    width = before.width,
                    height = before.height,
                    "movie skipped to its last frame"
                );
            } else {
                // The library does not go anywhere while a frame decodes on
                // another thread: try again at the next advance.
                tracing::debug!(frame = after.frame, "movie did not move to its last frame");
                if let Some(movie) = self.movies().get_mut(&(bink as usize)) {
                    movie.jumped = false;
                }
            }
        } else {
            // SAFETY: forwarded with the title's argument.
            unsafe { (self.api.next_frame)(bink) }
        }
    }

    /// Whether this advance is the one that goes to the last frame: once
    /// per movie, for one that is skipped and paced by the clock.
    fn take_jump(&self, bink: Bink) -> bool {
        let mut movies = self.movies();
        match movies.get_mut(&(bink as usize)) {
            Some(movie) if movie.paced && !movie.jumped && self.mode.skips(movie.looping) => {
                movie.jumped = true;
                true
            }
            _ => false,
        }
    }

    /// `BinkClose`.
    ///
    /// # Safety
    /// As the library's function.
    pub unsafe fn close(&self, bink: Bink) {
        self.movies().remove(&(bink as usize));
        // SAFETY: forwarded with the title's argument.
        unsafe { (self.api.close)(bink) }
    }
}

static SKIPPER: OnceLock<Option<Skipper>> = OnceLock::new();

fn skipper() -> Option<&'static Skipper> {
    SKIPPER.get().and_then(Option::as_ref)
}

unsafe extern "system" fn set_will_loop_hook(bink: Bink, will_loop: i32) {
    if let Some(skipper) = skipper() {
        // SAFETY: the title's own call.
        unsafe { skipper.set_will_loop(bink, will_loop) }
    }
}

unsafe extern "system" fn wait_hook(bink: Bink) -> i32 {
    // SAFETY: the title's own call.
    skipper().map_or(0, |skipper| unsafe { skipper.wait(bink) })
}

unsafe extern "system" fn next_frame_hook(bink: Bink) {
    if let Some(skipper) = skipper() {
        // SAFETY: the title's own call.
        unsafe { skipper.next_frame(bink) }
    }
}

unsafe extern "system" fn close_hook(bink: Bink) {
    if let Some(skipper) = skipper() {
        // SAFETY: the title's own call.
        unsafe { skipper.close(bink) }
    }
}

/// The address to bind an import of `name` from the DLL `dll` to, given the
/// address `address` of the library's own export: a wrapper if the switch
/// is on and `dll` is a Bink library that has the functions the wrappers
/// need (`exports` looks one up by name), else `address`.
pub fn wrap_import(
    mode: SkipMode,
    dll: &str,
    name: &str,
    address: usize,
    exports: &dyn Fn(&str) -> Option<usize>,
) -> usize {
    if mode == SkipMode::Off || !is_bink_dll(dll) {
        return address;
    }
    let hook = match name {
        "BinkSetWillLoop" => set_will_loop_hook as unsafe extern "system" fn(Bink, i32) as usize,
        "BinkWait" => wait_hook as unsafe extern "system" fn(Bink) -> i32 as usize,
        "BinkNextFrame" => next_frame_hook as unsafe extern "system" fn(Bink) as usize,
        "BinkClose" => close_hook as unsafe extern "system" fn(Bink) as usize,
        _ => return address,
    };
    match SKIPPER.get_or_init(|| Skipper::from_exports(mode, exports)) {
        Some(_) => hook,
        None => address,
    }
}

/// Looks up an export by DLL name and function name.
type Exports<'a> = Box<dyn Fn(&str, &str) -> Option<usize> + 'a>;

/// Binds the title's imports through `inner`, except that those from the
/// movie library go to the wrappers when [`SWITCH`] is on.
pub struct SkipMovies<'a> {
    inner: &'a dyn ImportResolver,
    exports: Exports<'a>,
    mode: SkipMode,
}

impl<'a> SkipMovies<'a> {
    /// `exports` finds the export `name` of the title's DLL `dll`. Logs a
    /// line if the switch is on.
    pub fn new(inner: &'a dyn ImportResolver, exports: Exports<'a>, mode: SkipMode) -> Self {
        match mode {
            SkipMode::Off => {}
            SkipMode::NonLooping => tracing::info!(
                "{SWITCH}=1: movies that do not loop end on their first playback tick"
            ),
            SkipMode::All => tracing::info!(
                "{SWITCH}=all: every movie, looping ones too, is sent to its last frame once"
            ),
        }
        SkipMovies {
            inner,
            exports,
            mode,
        }
    }

    /// The switch from the environment, the exports from the title's DLLs.
    #[cfg(windows)]
    pub fn from_env(
        inner: &'a dyn ImportResolver,
        dlls: &'a [(String, victory_loader::LoadedImage)],
    ) -> Self {
        let exports = move |dll: &str, name: &str| {
            dlls.iter()
                .find(|(file, _)| file.eq_ignore_ascii_case(dll))
                .and_then(|(_, image)| image.export(name))
        };
        SkipMovies::new(inner, Box::new(exports), SkipMode::from_env())
    }
}

impl ImportResolver for SkipMovies<'_> {
    fn resolve(&self, dll: &str, symbol: &ImportSymbol) -> Resolution {
        let found = self.inner.resolve(dll, symbol);
        match (found, symbol) {
            (
                Resolution::Found {
                    address,
                    provenance: Provenance::Title,
                },
                ImportSymbol::Name { name, .. },
            ) if self.mode != SkipMode::Off => Resolution::Found {
                address: wrap_import(self.mode, dll, name, address, &|export| {
                    (self.exports)(dll, export)
                }),
                provenance: Provenance::Title,
            },
            (found, _) => found,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    thread_local! {
        /// What the fake library was asked, in order (each test runs on its
        /// own thread).
        static CALLS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        /// The frame its one movie is at.
        static FRAME: Cell<u32> = const { Cell::new(1) };
        /// Whether its `BinkGoto` refuses to move (a frame decoding elsewhere).
        static STUCK: Cell<bool> = const { Cell::new(false) };
    }

    fn note(call: String) {
        CALLS.with(|c| c.borrow_mut().push(call));
    }

    fn calls() -> Vec<String> {
        CALLS.with(|c| c.borrow().clone())
    }

    unsafe extern "system" fn fake_wait(bink: Bink) -> i32 {
        note(format!("wait {:x}", bink as usize));
        1
    }
    unsafe extern "system" fn fake_next_frame(bink: Bink) {
        note(format!("next {:x}", bink as usize));
    }
    unsafe extern "system" fn fake_goto(bink: Bink, frame: u32, flags: i32) {
        note(format!("goto {:x} {frame:x} {flags}", bink as usize));
        if !STUCK.with(Cell::get) {
            FRAME.with(|f| f.set(FRAMES));
        }
    }
    unsafe extern "system" fn fake_set_will_loop(bink: Bink, will_loop: i32) {
        note(format!("loop {:x} {will_loop}", bink as usize));
    }
    unsafe extern "system" fn fake_close(bink: Bink) {
        note(format!("close {:x}", bink as usize));
    }
    unsafe fn fake_describe(_: Bink) -> MovieInfo {
        MovieInfo {
            width: 1920,
            height: 1080,
            frames: FRAMES,
            frame: FRAME.with(Cell::get),
        }
    }

    const FRAMES: u32 = 100;

    fn fake_api() -> Api {
        Api {
            wait: fake_wait,
            next_frame: fake_next_frame,
            goto: fake_goto,
            set_will_loop: fake_set_will_loop,
            close: fake_close,
            describe: fake_describe,
        }
    }

    const MOVIE: Bink = 0x1000 as Bink;

    #[test]
    fn switch_values() {
        for off in ["", "0", "off", " 0 ", "False"] {
            assert_eq!(SkipMode::parse(off), Some(SkipMode::Off), "{off:?}");
        }
        for on in ["1", "on", "true", "YES"] {
            assert_eq!(SkipMode::parse(on), Some(SkipMode::NonLooping), "{on:?}");
        }
        assert_eq!(SkipMode::parse("all"), Some(SkipMode::All));
        assert_eq!(SkipMode::parse("ALL"), Some(SkipMode::All));
        assert_eq!(SkipMode::parse("2"), None);
        assert_eq!(SkipMode::parse("everything"), None);
    }

    #[test]
    fn skip_or_forward_by_loop_state() {
        assert!(!SkipMode::Off.skips(Some(false)));
        assert!(!SkipMode::Off.skips(Some(true)));
        assert!(!SkipMode::Off.skips(None));
        assert!(SkipMode::NonLooping.skips(Some(false)));
        assert!(!SkipMode::NonLooping.skips(Some(true)));
        // Nothing said yet: not touched.
        assert!(!SkipMode::NonLooping.skips(None));
        assert!(SkipMode::All.skips(Some(false)));
        assert!(SkipMode::All.skips(Some(true)));
        assert!(SkipMode::All.skips(None));
    }

    #[test]
    fn library_names() {
        assert!(is_bink_dll("bink2test.dll"));
        assert!(is_bink_dll("BINK2W64.DLL"));
        assert!(!is_bink_dll("xmem.dll"));
        assert!(!is_bink_dll("bink.lib"));
    }

    #[test]
    fn a_movie_that_does_not_loop_ends_on_its_first_tick() {
        let s = Skipper::new(SkipMode::NonLooping, fake_api());
        // SAFETY: the fakes take any handle.
        unsafe {
            // The title's first ticks: it says it does not loop, asks if the
            // frame is due (it is, at once), advances, then advances again.
            s.set_will_loop(MOVIE, 0);
            assert_eq!(s.wait(MOVIE), 0);
            s.next_frame(MOVIE);
            assert_eq!(s.wait(MOVIE), 0);
            s.next_frame(MOVIE);
            s.next_frame(MOVIE);
            s.close(MOVIE);
        }
        // The jump replaces the first advance only; the clock is never read;
        // the movie is closed by the library.
        assert_eq!(
            calls(),
            [
                "loop 1000 0",
                "goto 1000 ffffffff 0",
                "next 1000",
                "next 1000",
                "close 1000"
            ]
        );
    }

    #[test]
    fn a_jump_the_library_refuses_is_tried_again_at_the_next_advance() {
        let s = Skipper::new(SkipMode::NonLooping, fake_api());
        STUCK.with(|c| c.set(true));
        // SAFETY: the fakes take any handle.
        unsafe {
            s.set_will_loop(MOVIE, 0);
            assert_eq!(s.wait(MOVIE), 0);
            s.next_frame(MOVIE);
            s.next_frame(MOVIE);
            STUCK.with(|c| c.set(false));
            s.next_frame(MOVIE);
            s.next_frame(MOVIE);
        }
        assert_eq!(
            calls(),
            [
                "loop 1000 0",
                "goto 1000 ffffffff 0",
                "goto 1000 ffffffff 0",
                "goto 1000 ffffffff 0",
                "next 1000"
            ]
        );
    }

    #[test]
    fn a_movie_that_loops_is_left_to_the_library() {
        let s = Skipper::new(SkipMode::NonLooping, fake_api());
        // SAFETY: the fakes take any handle.
        unsafe {
            s.set_will_loop(MOVIE, 1);
            assert_eq!(s.wait(MOVIE), 1);
            s.next_frame(MOVIE);
            assert_eq!(s.wait(MOVIE), 1);
        }
        assert_eq!(
            calls(),
            ["loop 1000 1", "wait 1000", "next 1000", "wait 1000"]
        );
    }

    #[test]
    fn the_title_changing_its_mind_about_looping_counts() {
        let s = Skipper::new(SkipMode::NonLooping, fake_api());
        // SAFETY: the fakes take any handle.
        unsafe {
            s.set_will_loop(MOVIE, 1);
            assert_eq!(s.wait(MOVIE), 1);
            s.set_will_loop(MOVIE, 0);
            assert_eq!(s.wait(MOVIE), 0);
            s.next_frame(MOVIE);
        }
        assert_eq!(
            calls(),
            [
                "loop 1000 1",
                "wait 1000",
                "loop 1000 0",
                "goto 1000 ffffffff 0"
            ]
        );
    }

    #[test]
    fn a_movie_whose_looping_is_unknown_is_left_alone() {
        let s = Skipper::new(SkipMode::NonLooping, fake_api());
        // SAFETY: the fakes take any handle.
        unsafe {
            assert_eq!(s.wait(MOVIE), 1);
            s.next_frame(MOVIE);
        }
        assert_eq!(calls(), ["wait 1000", "next 1000"]);
    }

    #[test]
    fn a_movie_the_clock_never_paces_is_left_alone() {
        let s = Skipper::new(SkipMode::NonLooping, fake_api());
        // SAFETY: the fakes take any handle.
        unsafe {
            s.set_will_loop(MOVIE, 0);
            s.next_frame(MOVIE);
            s.next_frame(MOVIE);
        }
        assert_eq!(calls(), ["loop 1000 0", "next 1000", "next 1000"]);
    }

    #[test]
    fn a_closed_movie_leaves_nothing_for_the_next_one_at_its_address() {
        let s = Skipper::new(SkipMode::NonLooping, fake_api());
        // SAFETY: the fakes take any handle.
        unsafe {
            s.set_will_loop(MOVIE, 0);
            assert_eq!(s.wait(MOVIE), 0);
            s.next_frame(MOVIE);
            s.close(MOVIE);
            // Same address, a new movie: it says nothing yet, so the
            // library paces it, and it is skipped only once it is known.
            assert_eq!(s.wait(MOVIE), 1);
            s.next_frame(MOVIE);
            s.set_will_loop(MOVIE, 0);
            assert_eq!(s.wait(MOVIE), 0);
            s.next_frame(MOVIE);
        }
        assert_eq!(
            calls(),
            [
                "loop 1000 0",
                "goto 1000 ffffffff 0",
                "close 1000",
                "wait 1000",
                "next 1000",
                "loop 1000 0",
                "goto 1000 ffffffff 0"
            ]
        );
    }

    #[test]
    fn movies_are_told_apart_by_handle() {
        let s = Skipper::new(SkipMode::NonLooping, fake_api());
        let other = 0x2000 as Bink;
        // SAFETY: the fakes take any handle.
        unsafe {
            s.set_will_loop(MOVIE, 0);
            s.set_will_loop(other, 1);
            assert_eq!(s.wait(MOVIE), 0);
            assert_eq!(s.wait(other), 1);
            s.next_frame(other);
            s.next_frame(MOVIE);
        }
        assert_eq!(
            calls(),
            [
                "loop 1000 0",
                "loop 2000 1",
                "wait 2000",
                "next 2000",
                "goto 1000 ffffffff 0"
            ]
        );
    }

    #[test]
    fn all_sends_a_looping_movie_to_its_end_once_then_leaves_it_paced() {
        let s = Skipper::new(SkipMode::All, fake_api());
        // SAFETY: the fakes take any handle.
        unsafe {
            s.set_will_loop(MOVIE, 1);
            assert_eq!(s.wait(MOVIE), 0);
            s.next_frame(MOVIE);
            // Now at its last frame; it loops on at the library's pace.
            assert_eq!(s.wait(MOVIE), 1);
            s.next_frame(MOVIE);
        }
        assert_eq!(
            calls(),
            [
                "loop 1000 1",
                "goto 1000 ffffffff 0",
                "wait 1000",
                "next 1000"
            ]
        );
    }

    #[test]
    fn off_changes_nothing() {
        let s = Skipper::new(SkipMode::Off, fake_api());
        // SAFETY: the fakes take any handle.
        unsafe {
            s.set_will_loop(MOVIE, 0);
            assert_eq!(s.wait(MOVIE), 1);
            s.next_frame(MOVIE);
            s.close(MOVIE);
        }
        assert_eq!(
            calls(),
            ["loop 1000 0", "wait 1000", "next 1000", "close 1000"]
        );
    }

    #[test]
    fn a_null_handle_is_forwarded_untracked() {
        let s = Skipper::new(SkipMode::All, fake_api());
        // SAFETY: the fakes take any handle.
        unsafe {
            s.set_will_loop(std::ptr::null_mut(), 0);
            assert_eq!(s.wait(std::ptr::null_mut()), 1);
            s.next_frame(std::ptr::null_mut());
            s.close(std::ptr::null_mut());
        }
        assert_eq!(calls(), ["loop 0 0", "wait 0", "next 0", "close 0"]);
        assert!(s.movies().is_empty());
    }

    /// The fake library's exports.
    fn fake_exports() -> HashMap<&'static str, usize> {
        HashMap::from([
            (
                "BinkWait",
                fake_wait as unsafe extern "system" fn(Bink) -> i32 as usize,
            ),
            (
                "BinkNextFrame",
                fake_next_frame as unsafe extern "system" fn(Bink) as usize,
            ),
            (
                "BinkGoto",
                fake_goto as unsafe extern "system" fn(Bink, u32, i32) as usize,
            ),
            (
                "BinkSetWillLoop",
                fake_set_will_loop as unsafe extern "system" fn(Bink, i32) as usize,
            ),
            (
                "BinkClose",
                fake_close as unsafe extern "system" fn(Bink) as usize,
            ),
            (
                "BinkOpenWithOptions",
                fake_close as unsafe extern "system" fn(Bink) as usize,
            ),
        ])
    }

    #[test]
    fn the_library_must_have_every_function_the_wrappers_forward_to() {
        let all = fake_exports();
        let found = |name: &str| all.get(name).copied();
        assert!(Skipper::from_exports(SkipMode::NonLooping, &found).is_some());
        let no_goto = |name: &str| all.get(name).copied().filter(|_| name != "BinkGoto");
        assert!(Skipper::from_exports(SkipMode::NonLooping, &no_goto).is_none());
    }

    /// An inner resolver that answers every import with one resolution.
    struct Fixed(Resolution);

    impl ImportResolver for Fixed {
        fn resolve(&self, _: &str, _: &ImportSymbol) -> Resolution {
            self.0
        }
    }

    fn name(n: &str) -> ImportSymbol {
        ImportSymbol::Name {
            hint: 0,
            name: n.into(),
        }
    }

    #[test]
    fn the_resolver_rewrites_only_the_titles_movie_library_imports() {
        const LIBRARY: &str = "bink2test.dll";
        let exports = || {
            let all = fake_exports();
            Box::new(move |dll: &str, export: &str| {
                assert_eq!(dll, LIBRARY);
                all.get(export).copied()
            })
        };
        let library = Resolution::Found {
            address: 0x1234,
            provenance: Provenance::Title,
        };

        // Off (the default): every answer is the inner resolver's.
        let inner = Fixed(library);
        let off = SkipMovies::new(&inner, exports(), SkipMode::Off);
        for symbol in ["BinkWait", "BinkNextFrame", "BinkSetWillLoop", "BinkClose"] {
            assert_eq!(off.resolve(LIBRARY, &name(symbol)), library, "{symbol}");
        }

        // On: the four wrapped functions change address, the rest and every
        // other kind of answer do not.
        let on = SkipMovies::new(&inner, exports(), SkipMode::NonLooping);
        let hooks = [
            (
                "BinkWait",
                wait_hook as unsafe extern "system" fn(Bink) -> i32 as usize,
            ),
            (
                "BinkNextFrame",
                next_frame_hook as unsafe extern "system" fn(Bink) as usize,
            ),
            (
                "BinkSetWillLoop",
                set_will_loop_hook as unsafe extern "system" fn(Bink, i32) as usize,
            ),
            (
                "BinkClose",
                close_hook as unsafe extern "system" fn(Bink) as usize,
            ),
        ];
        for (symbol, hook) in hooks {
            assert_eq!(
                on.resolve(LIBRARY, &name(symbol)),
                Resolution::Found {
                    address: hook,
                    provenance: Provenance::Title
                },
                "{symbol}"
            );
        }
        assert_eq!(on.resolve(LIBRARY, &name("BinkOpenWithOptions")), library);
        assert_eq!(on.resolve(LIBRARY, &ImportSymbol::Ordinal(7)), library);
        assert_eq!(on.resolve("xmem.dll", &name("BinkWait")), library);
        for other in [
            Resolution::Missing,
            Resolution::Found {
                address: 0x1234,
                provenance: Provenance::Hle,
            },
            Resolution::Found {
                address: 0x1234,
                provenance: Provenance::Host,
            },
        ] {
            let inner = Fixed(other);
            let on = SkipMovies::new(&inner, exports(), SkipMode::All);
            assert_eq!(on.resolve(LIBRARY, &name("BinkWait")), other);
        }
    }

    #[test]
    fn imports_are_not_touched_unless_the_switch_is_on_and_the_dll_is_a_movie_library() {
        let nothing = |_: &str| None;
        assert_eq!(
            wrap_import(SkipMode::Off, "bink2test.dll", "BinkWait", 0x1234, &nothing),
            0x1234
        );
        assert_eq!(
            wrap_import(SkipMode::All, "xmem.dll", "BinkWait", 0x1234, &nothing),
            0x1234
        );
        // Functions the wrappers do not replace keep the library's address.
        assert_eq!(
            wrap_import(
                SkipMode::All,
                "bink2w64.dll",
                "BinkOpenWithOptions",
                0x1234,
                &nothing
            ),
            0x1234
        );
    }
}
