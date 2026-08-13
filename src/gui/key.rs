//! An owned stand-in for winit's physical key codes.
//!
//! A keyboard [`UiEvent`](crate::gui::event::UiEvent) has to name the key
//! that was pressed, and winit's own `KeyCode` was doing that job. That put an
//! optional dependency in a public signature: `winit` is only compiled under
//! the `window` feature, so the type vanishes from the API depending on how
//! the crate is built, and bumping winit becomes a breaking change for anyone
//! matching on it. [`Key`] is the insulating layer -- one variant per
//! `winit::keyboard::KeyCode`, converted at the boundary where the event is
//! captured.
//!
//! What remains of winit in this module's public surface is the conversion
//! itself: the `From` impls here and on
//! [`ModifierKeys`](crate::gui::event::ModifierKeys) name a winit type
//! because naming it is their purpose. Everything that used to carry one --
//! the live window, the window manager, and the dozen methods addressed by
//! `WindowId` or built from an `ActiveEventLoop` -- is crate-private, so a
//! consumer never sees those types at all.
//!
//! `scripts/check-public-api.mjs` does not report on this either way:
//! `FORBIDDEN` lists `skia_safe` and `neon`. Adding `winit` to it now finds
//! only those two `From` impls, which is the check working rather than
//! failing -- but it would need a way to exempt a conversion before it could
//! be turned on.
//!
//! The JS side reads these as DOM `KeyboardEvent.code` strings. That makes
//! the serialized form a wire format, not an implementation detail: the
//! variant names and the bare `Serialize` derive below are load-bearing, and
//! renaming a variant or adding a `serde` attribute would break parsing on
//! the other side of the bridge.

use serde::Serialize;

/// The physical position of a key, independent of the active layout.
///
/// Names the place on the keyboard rather than the character it produces:
/// [`Key::KeyA`] is the key labelled `A` on a US layout, wherever the user's
/// own layout puts it and whatever it types. Mirrors
/// `winit::keyboard::KeyCode`, which in turn follows the UI Events
/// `KeyboardEvent.code` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Key {
    /// The backtick key, left of `1` on a US layout.
    Backquote,
    /// The `\` key, above `Enter` on a US layout.
    Backslash,
    /// The `[` key.
    BracketLeft,
    /// The `]` key.
    BracketRight,
    /// The `,` key.
    Comma,
    /// The `0` key on the number row.
    Digit0,
    /// The `1` key on the number row.
    Digit1,
    /// The `2` key on the number row.
    Digit2,
    /// The `3` key on the number row.
    Digit3,
    /// The `4` key on the number row.
    Digit4,
    /// The `5` key on the number row.
    Digit5,
    /// The `6` key on the number row.
    Digit6,
    /// The `7` key on the number row.
    Digit7,
    /// The `8` key on the number row.
    Digit8,
    /// The `9` key on the number row.
    Digit9,
    /// The `=` key.
    Equal,
    /// The extra key between `Shift` and `Z` on ISO layouts.
    IntlBackslash,
    /// The `ro` key, between `/` and the right-hand `Shift` on Japanese
    /// layouts.
    IntlRo,
    /// The yen key, left of `Backspace` on Japanese layouts.
    IntlYen,
    /// The `A` key.
    KeyA,
    /// The `B` key.
    KeyB,
    /// The `C` key.
    KeyC,
    /// The `D` key.
    KeyD,
    /// The `E` key.
    KeyE,
    /// The `F` key.
    KeyF,
    /// The `G` key.
    KeyG,
    /// The `H` key.
    KeyH,
    /// The `I` key.
    KeyI,
    /// The `J` key.
    KeyJ,
    /// The `K` key.
    KeyK,
    /// The `L` key.
    KeyL,
    /// The `M` key.
    KeyM,
    /// The `N` key.
    KeyN,
    /// The `O` key.
    KeyO,
    /// The `P` key.
    KeyP,
    /// The `Q` key.
    KeyQ,
    /// The `R` key.
    KeyR,
    /// The `S` key.
    KeyS,
    /// The `T` key.
    KeyT,
    /// The `U` key.
    KeyU,
    /// The `V` key.
    KeyV,
    /// The `W` key.
    KeyW,
    /// The `X` key.
    KeyX,
    /// The `Y` key.
    KeyY,
    /// The `Z` key.
    KeyZ,
    /// The `-` key on the number row.
    Minus,
    /// The `.` key.
    Period,
    /// The `'` key.
    Quote,
    /// The `;` key.
    Semicolon,
    /// The `/` key.
    Slash,

    /// The left `Alt` key, `Option` on Apple keyboards.
    AltLeft,
    /// The right `Alt` key, `AltGr` on many non-US layouts.
    AltRight,
    /// The `Backspace` key, labelled `Delete` on Apple keyboards.
    Backspace,
    /// The `Caps Lock` key.
    CapsLock,
    /// The context menu key, right of the right-hand `Super`.
    ContextMenu,
    /// The left `Control` key.
    ControlLeft,
    /// The right `Control` key.
    ControlRight,
    /// The `Enter` or `Return` key.
    Enter,
    /// The left `Super` key: `Windows`, `Command`, or `Meta`.
    SuperLeft,
    /// The right `Super` key: `Windows`, `Command`, or `Meta`.
    SuperRight,
    /// The left `Shift` key.
    ShiftLeft,
    /// The right `Shift` key.
    ShiftRight,
    /// The space bar.
    Space,
    /// The `Tab` key.
    Tab,
    /// The henkan key on Japanese layouts.
    Convert,
    /// The katakana/hiragana/romaji key on Japanese layouts.
    KanaMode,
    /// Hangul mode on Korean layouts, kana on Apple Japanese ones.
    Lang1,
    /// Hanja on Korean layouts, eisu on Apple Japanese ones.
    Lang2,
    /// Katakana on Japanese word-processing keyboards.
    Lang3,
    /// Hiragana on Japanese word-processing keyboards.
    Lang4,
    /// Zenkaku/hankaku on Japanese word-processing keyboards.
    Lang5,
    /// The muhenkan key on Japanese layouts.
    NonConvert,

    /// The forward delete key. The key Apple labels `Delete` on the
    /// main block reports as [`Key::Backspace`].
    Delete,
    /// The `End` key.
    End,
    /// The `Help` key, absent from standard PC keyboards.
    Help,
    /// The `Home` key.
    Home,
    /// The `Insert` key, absent from Apple keyboards.
    Insert,
    /// The `Page Down` key.
    PageDown,
    /// The `Page Up` key.
    PageUp,

    /// The down arrow key.
    ArrowDown,
    /// The left arrow key.
    ArrowLeft,
    /// The right arrow key.
    ArrowRight,
    /// The up arrow key.
    ArrowUp,

    /// The `Num Lock` key. The numpad `Clear` key on Apple
    /// keyboards reports as this.
    NumLock,
    /// The numpad `0` key.
    Numpad0,
    /// The numpad `1` key.
    Numpad1,
    /// The numpad `2` key.
    Numpad2,
    /// The numpad `3` key.
    Numpad3,
    /// The numpad `4` key.
    Numpad4,
    /// The numpad `5` key.
    Numpad5,
    /// The numpad `6` key.
    Numpad6,
    /// The numpad `7` key.
    Numpad7,
    /// The numpad `8` key.
    Numpad8,
    /// The numpad `9` key.
    Numpad9,
    /// The numpad `+` key.
    NumpadAdd,
    /// A numpad backspace key, on the Microsoft Natural
    /// Keyboard.
    NumpadBackspace,
    /// The numpad `Clear` or `All Clear` key, where it is
    /// separate from `Num Lock`.
    NumpadClear,
    /// The numpad `Clear Entry` key.
    NumpadClearEntry,
    /// The numpad thousands separator, which prints `.` in
    /// locales that group that way.
    NumpadComma,
    /// The numpad decimal separator, which prints `,` in
    /// locales that write it that way.
    NumpadDecimal,
    /// The numpad `/` key.
    NumpadDivide,
    /// The numpad `Enter` key.
    NumpadEnter,
    /// The numpad `=` key.
    NumpadEqual,
    /// The `#` key on phones and remote controls.
    NumpadHash,
    /// Adds the current entry to the value in memory.
    NumpadMemoryAdd,
    /// Clears the value stored in memory.
    NumpadMemoryClear,
    /// Replaces the current entry with the value in
    /// memory.
    NumpadMemoryRecall,
    /// Stores the current entry in memory.
    NumpadMemoryStore,
    /// Subtracts the current entry from the value in
    /// memory.
    NumpadMemorySubtract,
    /// The numpad `*` key. Phones and remote controls report
    /// [`Key::NumpadStar`] instead.
    NumpadMultiply,
    /// The numpad `(` key, on the Microsoft Natural
    /// Keyboard.
    NumpadParenLeft,
    /// The numpad `)` key, on the Microsoft Natural
    /// Keyboard.
    NumpadParenRight,
    /// The `*` key on phones and remote controls.
    NumpadStar,
    /// The numpad `-` key.
    NumpadSubtract,

    /// The `Esc` key.
    Escape,
    /// The `Fn` key, usually handled in hardware and never seen here.
    Fn,
    /// The `Fn Lock` key, on the Microsoft Natural Keyboard.
    FnLock,
    /// The `Print Screen` or `SysRq` key.
    PrintScreen,
    /// The `Scroll Lock` key.
    ScrollLock,
    /// The `Pause` or `Break` key.
    Pause,

    /// The browser back key, and the Android back button.
    BrowserBack,
    /// The browser bookmarks key.
    BrowserFavorites,
    /// The browser forward key.
    BrowserForward,
    /// The browser home key, and the Android home button.
    BrowserHome,
    /// The browser reload key.
    BrowserRefresh,
    /// The browser search key.
    BrowserSearch,
    /// The browser stop key.
    BrowserStop,
    /// The `Eject` key, in the function row on some Apple keyboards.
    Eject,
    /// The launch key labelled `My Computer` on some keyboards.
    LaunchApp1,
    /// The launch key labelled `Calculator` on some keyboards.
    LaunchApp2,
    /// The mail launch key.
    LaunchMail,
    /// The play/pause toggle.
    MediaPlayPause,
    /// The media player launch key.
    MediaSelect,
    /// The playback stop key.
    MediaStop,
    /// The next track key.
    MediaTrackNext,
    /// The previous track key.
    MediaTrackPrevious,
    /// The power key, replacing `Eject` on some Apple keyboards.
    Power,
    /// The sleep key.
    Sleep,
    /// The volume down key.
    AudioVolumeDown,
    /// The mute toggle.
    AudioVolumeMute,
    /// The volume up key.
    AudioVolumeUp,
    /// The wake key.
    WakeUp,

    /// A legacy modifier, called `Super` in some places.
    Meta,
    /// A legacy modifier.
    Hyper,
    /// The `Turbo` key on older PCs.
    Turbo,
    /// The `Abort` key.
    Abort,
    /// The `Resume` key.
    Resume,
    /// The `Suspend` key.
    Suspend,
    /// The `Again` key, found on Sun's USB keyboard.
    Again,
    /// The `Copy` key, found on Sun's USB keyboard.
    Copy,
    /// The `Cut` key, found on Sun's USB keyboard.
    Cut,
    /// The `Find` key, found on Sun's USB keyboard.
    Find,
    /// The `Open` key, found on Sun's USB keyboard.
    Open,
    /// The `Paste` key, found on Sun's USB keyboard.
    Paste,
    /// The `Props` key, found on Sun's USB keyboard.
    Props,
    /// The `Select` key, found on Sun's USB keyboard.
    Select,
    /// The `Undo` key, found on Sun's USB keyboard.
    Undo,
    /// The hiragana key on Japanese keyboards.
    Hiragana,
    /// The katakana key on Japanese keyboards.
    Katakana,

    /// The `F1` key.
    F1,
    /// The `F2` key.
    F2,
    /// The `F3` key.
    F3,
    /// The `F4` key.
    F4,
    /// The `F5` key.
    F5,
    /// The `F6` key.
    F6,
    /// The `F7` key.
    F7,
    /// The `F8` key.
    F8,
    /// The `F9` key.
    F9,
    /// The `F10` key.
    F10,
    /// The `F11` key.
    F11,
    /// The `F12` key.
    F12,
    /// The `F13` key.
    F13,
    /// The `F14` key.
    F14,
    /// The `F15` key.
    F15,
    /// The `F16` key.
    F16,
    /// The `F17` key.
    F17,
    /// The `F18` key.
    F18,
    /// The `F19` key.
    F19,
    /// The `F20` key.
    F20,
    /// The `F21` key.
    F21,
    /// The `F22` key.
    F22,
    /// The `F23` key.
    F23,
    /// The `F24` key.
    F24,
    /// The `F25` key.
    F25,
    /// The `F26` key.
    F26,
    /// The `F27` key.
    F27,
    /// The `F28` key.
    F28,
    /// The `F29` key.
    F29,
    /// The `F30` key.
    F30,
    /// The `F31` key.
    F31,
    /// The `F32` key.
    F32,
    /// The `F33` key.
    F33,
    /// The `F34` key.
    F34,
    /// The `F35` key.
    F35,
    /// A key this build cannot name.
    ///
    /// Not one of winit's codes. It is where a code added by a future winit
    /// lands, since `KeyCode` is `#[non_exhaustive]` and the conversion below
    /// cannot be made total. The DOM spells the same situation the same way,
    /// so a handler matching on `code` sees a value it already has to expect.
    Unidentified,
}

/// Converts winit's code for a physical key into the owned equivalent.
///
/// The match spells out all 194 variants rather than leaning on a catch-all.
/// That is deliberate: an arm mapping unknown codes onto some stand-in would
/// report a plausible-looking but wrong `code` to JS, and a wrong key is
/// harder to notice than a missing one.
///
/// The final arm is not a shortcut, it is a tax. `KeyCode` is
/// `#[non_exhaustive]`, so rustc demands a wildcard from outside winit even
/// when every variant it currently has is already listed. That costs the
/// guarantee this match was shaped to buy: a key added by a future winit
/// cannot be caught here at compile time. Re-run this list against
/// `winit::keyboard::KeyCode` when bumping winit; nothing else will.
///
/// It resolves to [`Key::Unidentified`] rather than panicking. This is the
/// key handler of a windowing library, reached from whatever the OS reports,
/// and a keystroke is not an occasion to take someone's application down. It
/// is also what the DOM does -- `KeyboardEvent.code` is `"Unidentified"` for
/// a key the engine cannot name -- so the value stays honest on the wire
/// instead of naming the wrong key.
impl From<winit::keyboard::KeyCode> for Key {
    fn from(code: winit::keyboard::KeyCode) -> Self {
        use winit::keyboard::KeyCode;

        match code {
            KeyCode::Backquote => Key::Backquote,
            KeyCode::Backslash => Key::Backslash,
            KeyCode::BracketLeft => Key::BracketLeft,
            KeyCode::BracketRight => Key::BracketRight,
            KeyCode::Comma => Key::Comma,
            KeyCode::Digit0 => Key::Digit0,
            KeyCode::Digit1 => Key::Digit1,
            KeyCode::Digit2 => Key::Digit2,
            KeyCode::Digit3 => Key::Digit3,
            KeyCode::Digit4 => Key::Digit4,
            KeyCode::Digit5 => Key::Digit5,
            KeyCode::Digit6 => Key::Digit6,
            KeyCode::Digit7 => Key::Digit7,
            KeyCode::Digit8 => Key::Digit8,
            KeyCode::Digit9 => Key::Digit9,
            KeyCode::Equal => Key::Equal,
            KeyCode::IntlBackslash => Key::IntlBackslash,
            KeyCode::IntlRo => Key::IntlRo,
            KeyCode::IntlYen => Key::IntlYen,
            KeyCode::KeyA => Key::KeyA,
            KeyCode::KeyB => Key::KeyB,
            KeyCode::KeyC => Key::KeyC,
            KeyCode::KeyD => Key::KeyD,
            KeyCode::KeyE => Key::KeyE,
            KeyCode::KeyF => Key::KeyF,
            KeyCode::KeyG => Key::KeyG,
            KeyCode::KeyH => Key::KeyH,
            KeyCode::KeyI => Key::KeyI,
            KeyCode::KeyJ => Key::KeyJ,
            KeyCode::KeyK => Key::KeyK,
            KeyCode::KeyL => Key::KeyL,
            KeyCode::KeyM => Key::KeyM,
            KeyCode::KeyN => Key::KeyN,
            KeyCode::KeyO => Key::KeyO,
            KeyCode::KeyP => Key::KeyP,
            KeyCode::KeyQ => Key::KeyQ,
            KeyCode::KeyR => Key::KeyR,
            KeyCode::KeyS => Key::KeyS,
            KeyCode::KeyT => Key::KeyT,
            KeyCode::KeyU => Key::KeyU,
            KeyCode::KeyV => Key::KeyV,
            KeyCode::KeyW => Key::KeyW,
            KeyCode::KeyX => Key::KeyX,
            KeyCode::KeyY => Key::KeyY,
            KeyCode::KeyZ => Key::KeyZ,
            KeyCode::Minus => Key::Minus,
            KeyCode::Period => Key::Period,
            KeyCode::Quote => Key::Quote,
            KeyCode::Semicolon => Key::Semicolon,
            KeyCode::Slash => Key::Slash,
            KeyCode::AltLeft => Key::AltLeft,
            KeyCode::AltRight => Key::AltRight,
            KeyCode::Backspace => Key::Backspace,
            KeyCode::CapsLock => Key::CapsLock,
            KeyCode::ContextMenu => Key::ContextMenu,
            KeyCode::ControlLeft => Key::ControlLeft,
            KeyCode::ControlRight => Key::ControlRight,
            KeyCode::Enter => Key::Enter,
            KeyCode::SuperLeft => Key::SuperLeft,
            KeyCode::SuperRight => Key::SuperRight,
            KeyCode::ShiftLeft => Key::ShiftLeft,
            KeyCode::ShiftRight => Key::ShiftRight,
            KeyCode::Space => Key::Space,
            KeyCode::Tab => Key::Tab,
            KeyCode::Convert => Key::Convert,
            KeyCode::KanaMode => Key::KanaMode,
            KeyCode::Lang1 => Key::Lang1,
            KeyCode::Lang2 => Key::Lang2,
            KeyCode::Lang3 => Key::Lang3,
            KeyCode::Lang4 => Key::Lang4,
            KeyCode::Lang5 => Key::Lang5,
            KeyCode::NonConvert => Key::NonConvert,
            KeyCode::Delete => Key::Delete,
            KeyCode::End => Key::End,
            KeyCode::Help => Key::Help,
            KeyCode::Home => Key::Home,
            KeyCode::Insert => Key::Insert,
            KeyCode::PageDown => Key::PageDown,
            KeyCode::PageUp => Key::PageUp,
            KeyCode::ArrowDown => Key::ArrowDown,
            KeyCode::ArrowLeft => Key::ArrowLeft,
            KeyCode::ArrowRight => Key::ArrowRight,
            KeyCode::ArrowUp => Key::ArrowUp,
            KeyCode::NumLock => Key::NumLock,
            KeyCode::Numpad0 => Key::Numpad0,
            KeyCode::Numpad1 => Key::Numpad1,
            KeyCode::Numpad2 => Key::Numpad2,
            KeyCode::Numpad3 => Key::Numpad3,
            KeyCode::Numpad4 => Key::Numpad4,
            KeyCode::Numpad5 => Key::Numpad5,
            KeyCode::Numpad6 => Key::Numpad6,
            KeyCode::Numpad7 => Key::Numpad7,
            KeyCode::Numpad8 => Key::Numpad8,
            KeyCode::Numpad9 => Key::Numpad9,
            KeyCode::NumpadAdd => Key::NumpadAdd,
            KeyCode::NumpadBackspace => Key::NumpadBackspace,
            KeyCode::NumpadClear => Key::NumpadClear,
            KeyCode::NumpadClearEntry => Key::NumpadClearEntry,
            KeyCode::NumpadComma => Key::NumpadComma,
            KeyCode::NumpadDecimal => Key::NumpadDecimal,
            KeyCode::NumpadDivide => Key::NumpadDivide,
            KeyCode::NumpadEnter => Key::NumpadEnter,
            KeyCode::NumpadEqual => Key::NumpadEqual,
            KeyCode::NumpadHash => Key::NumpadHash,
            KeyCode::NumpadMemoryAdd => Key::NumpadMemoryAdd,
            KeyCode::NumpadMemoryClear => Key::NumpadMemoryClear,
            KeyCode::NumpadMemoryRecall => Key::NumpadMemoryRecall,
            KeyCode::NumpadMemoryStore => Key::NumpadMemoryStore,
            KeyCode::NumpadMemorySubtract => Key::NumpadMemorySubtract,
            KeyCode::NumpadMultiply => Key::NumpadMultiply,
            KeyCode::NumpadParenLeft => Key::NumpadParenLeft,
            KeyCode::NumpadParenRight => Key::NumpadParenRight,
            KeyCode::NumpadStar => Key::NumpadStar,
            KeyCode::NumpadSubtract => Key::NumpadSubtract,
            KeyCode::Escape => Key::Escape,
            KeyCode::Fn => Key::Fn,
            KeyCode::FnLock => Key::FnLock,
            KeyCode::PrintScreen => Key::PrintScreen,
            KeyCode::ScrollLock => Key::ScrollLock,
            KeyCode::Pause => Key::Pause,
            KeyCode::BrowserBack => Key::BrowserBack,
            KeyCode::BrowserFavorites => Key::BrowserFavorites,
            KeyCode::BrowserForward => Key::BrowserForward,
            KeyCode::BrowserHome => Key::BrowserHome,
            KeyCode::BrowserRefresh => Key::BrowserRefresh,
            KeyCode::BrowserSearch => Key::BrowserSearch,
            KeyCode::BrowserStop => Key::BrowserStop,
            KeyCode::Eject => Key::Eject,
            KeyCode::LaunchApp1 => Key::LaunchApp1,
            KeyCode::LaunchApp2 => Key::LaunchApp2,
            KeyCode::LaunchMail => Key::LaunchMail,
            KeyCode::MediaPlayPause => Key::MediaPlayPause,
            KeyCode::MediaSelect => Key::MediaSelect,
            KeyCode::MediaStop => Key::MediaStop,
            KeyCode::MediaTrackNext => Key::MediaTrackNext,
            KeyCode::MediaTrackPrevious => Key::MediaTrackPrevious,
            KeyCode::Power => Key::Power,
            KeyCode::Sleep => Key::Sleep,
            KeyCode::AudioVolumeDown => Key::AudioVolumeDown,
            KeyCode::AudioVolumeMute => Key::AudioVolumeMute,
            KeyCode::AudioVolumeUp => Key::AudioVolumeUp,
            KeyCode::WakeUp => Key::WakeUp,
            KeyCode::Meta => Key::Meta,
            KeyCode::Hyper => Key::Hyper,
            KeyCode::Turbo => Key::Turbo,
            KeyCode::Abort => Key::Abort,
            KeyCode::Resume => Key::Resume,
            KeyCode::Suspend => Key::Suspend,
            KeyCode::Again => Key::Again,
            KeyCode::Copy => Key::Copy,
            KeyCode::Cut => Key::Cut,
            KeyCode::Find => Key::Find,
            KeyCode::Open => Key::Open,
            KeyCode::Paste => Key::Paste,
            KeyCode::Props => Key::Props,
            KeyCode::Select => Key::Select,
            KeyCode::Undo => Key::Undo,
            KeyCode::Hiragana => Key::Hiragana,
            KeyCode::Katakana => Key::Katakana,
            KeyCode::F1 => Key::F1,
            KeyCode::F2 => Key::F2,
            KeyCode::F3 => Key::F3,
            KeyCode::F4 => Key::F4,
            KeyCode::F5 => Key::F5,
            KeyCode::F6 => Key::F6,
            KeyCode::F7 => Key::F7,
            KeyCode::F8 => Key::F8,
            KeyCode::F9 => Key::F9,
            KeyCode::F10 => Key::F10,
            KeyCode::F11 => Key::F11,
            KeyCode::F12 => Key::F12,
            KeyCode::F13 => Key::F13,
            KeyCode::F14 => Key::F14,
            KeyCode::F15 => Key::F15,
            KeyCode::F16 => Key::F16,
            KeyCode::F17 => Key::F17,
            KeyCode::F18 => Key::F18,
            KeyCode::F19 => Key::F19,
            KeyCode::F20 => Key::F20,
            KeyCode::F21 => Key::F21,
            KeyCode::F22 => Key::F22,
            KeyCode::F23 => Key::F23,
            KeyCode::F24 => Key::F24,
            KeyCode::F25 => Key::F25,
            KeyCode::F26 => Key::F26,
            KeyCode::F27 => Key::F27,
            KeyCode::F28 => Key::F28,
            KeyCode::F29 => Key::F29,
            KeyCode::F30 => Key::F30,
            KeyCode::F31 => Key::F31,
            KeyCode::F32 => Key::F32,
            KeyCode::F33 => Key::F33,
            KeyCode::F34 => Key::F34,
            KeyCode::F35 => Key::F35,
            _ => Key::Unidentified,
        }
    }
}

#[cfg(test)]
mod tests {
    use winit::keyboard::KeyCode;

    use super::*;

    /// The JS side parses this JSON, so the two types have to serialize to
    /// the same bytes. Sampled across the groups whose names diverge most
    /// from each other -- a typo in one variant would not show up in
    /// another.
    #[test]
    fn serializes_like_winit() {
        let sample = [
            KeyCode::KeyA,
            KeyCode::KeyZ,
            KeyCode::Digit0,
            KeyCode::Digit9,
            KeyCode::F1,
            KeyCode::F13,
            KeyCode::F35,
            KeyCode::ShiftLeft,
            KeyCode::ControlRight,
            KeyCode::SuperLeft,
            KeyCode::ArrowUp,
            KeyCode::ArrowDown,
            KeyCode::BracketLeft,
            KeyCode::Semicolon,
            KeyCode::NumpadDecimal,
            KeyCode::AudioVolumeMute,
            KeyCode::IntlBackslash,
            KeyCode::Escape,
        ];

        for code in sample {
            assert_eq!(
                serde_json::to_value(Key::from(code)).unwrap(),
                serde_json::to_value(code).unwrap(),
                "{code:?} serializes differently from its winit original"
            );
        }
    }
}
