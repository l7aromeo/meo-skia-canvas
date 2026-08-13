use serde::Serialize;
use serde_json::json;
use skia_safe::Matrix;
use winit::{
    dpi::{LogicalPosition, LogicalSize, PhysicalPosition},
    event::{
        ElementState, Ime, KeyEvent, Modifiers, MouseButton, MouseScrollDelta,
        WindowEvent,
    },
    keyboard::{
        Key::{Character, Named},
        KeyCode, KeyLocation, ModifiersState, NamedKey,
        PhysicalKey::Code,
    },
};

use super::{key::Key, window::WindowSpec};
use crate::{
    context::page::Page,
    geometry::{Point, Size},
};

/// A request delivered to the event loop from outside it.
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// Opens a window for this spec, rendering the given page.
    Open(WindowSpec, Page),
    /// Closes the window with this id.
    Close(u32),
    /// Changes the target frame rate, in frames per second.
    FrameRate(u64),
    /// Closes every window and stops the loop.
    Quit,
}

/// A window event, shaped to match its DOM equivalent.
///
/// Serialized to JSON and handed to the JS side, so field names follow the
/// DOM rather than Rust convention.
#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UiEvent {
    /// Scroll wheel or trackpad scroll.
    #[allow(non_snake_case)]
    Wheel {
        /// Horizontal scroll distance.
        deltaX: f32,
        /// Vertical scroll distance.
        deltaY: f32,
    },
    /// The window moved on screen.
    Move {
        /// New left edge, in logical pixels.
        left: f32,
        /// New top edge, in logical pixels.
        top: f32,
    },
    /// A key went down or up.
    Keyboard {
        /// `"keydown"` or `"keyup"`.
        event: String,
        /// The character the key produced, after modifiers.
        key: String,
        /// Physical key position, independent of layout.
        code: Key,
        /// Which of several same-named keys this is, e.g. left or right
        /// shift.
        location: u32,
        /// Modifier keys held at the time.
        modifiers: ModifierKeys,
        /// `true` when produced by key auto-repeat.
        repeat: bool,
    },
    /// An input-method composition changed state.
    Composition {
        /// `"compositionstart"`, `"compositionupdate"`, or
        /// `"compositionend"`.
        event: String,
        /// The composition text so far.
        data: String,
    },
    /// A mouse button or movement.
    Mouse {
        /// `"mousedown"`, `"mouseup"`, `"mousemove"`, and so on.
        event: String,
        /// Button that changed state, if any.
        button: Option<u16>,
        /// Bitmask of every button currently held.
        buttons: u16,
        /// Cursor position in canvas coordinates, with the fitting transform
        /// applied.
        ///
        /// This is the one to hit-test against drawn content.
        point: Point,
        /// Cursor position in untransformed window coordinates.
        ///
        /// Differs from `point` whenever the canvas is scaled to fit the
        /// window.
        page_point: Point,
        /// Modifier keys held at the time.
        modifiers: ModifierKeys,
    },
    /// Text input: the inserted data if any, and the DOM `inputType` that
    /// produced it (`"insertText"`, `"deleteContentBackward"`,
    /// `"insertLineBreak"`, `"insertCompositionText"`).
    ///
    /// Emitted for ordinary keystrokes as well as for input-method commits.
    Input(Option<String>, String),
    /// The window gained (`true`) or lost (`false`) keyboard focus.
    Focus(bool),
    /// The window was resized, in logical pixels.
    Resize(Size),
    /// The window entered (`true`) or left (`false`) fullscreen.
    Fullscreen(bool),
}

/// Which modifier keys were held when an event fired.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModifierKeys {
    shift_key: bool,
    ctrl_key: bool,
    alt_key: bool,
    meta_key: bool,
}

impl ModifierKeys {
    /// Whether shift was held.
    pub fn shift(&self) -> bool {
        self.shift_key
    }

    /// Whether control was held.
    pub fn ctrl(&self) -> bool {
        self.ctrl_key
    }

    /// Whether alt (option) was held.
    pub fn alt(&self) -> bool {
        self.alt_key
    }

    /// Whether the platform's meta key -- command, super, or windows -- was
    /// held.
    pub fn meta(&self) -> bool {
        self.meta_key
    }
}

impl From<ModifiersState> for ModifierKeys {
    fn from(state: ModifiersState) -> Self {
        ModifierKeys {
            shift_key: state.shift_key(),
            ctrl_key: state.control_key(),
            alt_key: state.alt_key(),
            meta_key: state.super_key(),
        }
    }
}

/// Per-window event accumulator.
///
/// winit reports raw events; this turns them into DOM-shaped [`UiEvent`]s,
/// tracking the state a single event does not carry -- which buttons are
/// down, which modifiers are held, whether a composition is in progress --
/// and buffers the result until the loop drains it.
///
/// Internal: this is the plumbing between winit and the loop, not something a
/// caller holds. Its methods take winit and Skia types, and the events it
/// produces reach a consumer through the window's handler rather than through
/// the sieve itself.
#[derive(Debug)]
pub(crate) struct Sieve {
    dpr: f64,
    queue: Vec<UiEvent>,
    key_modifiers: ModifierKeys,
    mouse_point: PhysicalPosition<f64>,
    mouse_button: Option<u16>,
    mouse_buttons: u16,
    mouse_transform: Matrix,
    compose_begun: bool,
    compose_ongoing: bool,
}

impl Sieve {
    /// Creates an empty sieve for a window at this device pixel ratio.
    pub fn new(dpr: f64) -> Self {
        Sieve {
            dpr,
            queue: vec![],
            key_modifiers: Modifiers::default().state().into(),
            mouse_point: PhysicalPosition::default(),
            mouse_button: None,
            mouse_buttons: 0,
            mouse_transform: Matrix::new_identity(),
            compose_begun: false,
            compose_ongoing: false,
        }
    }

    /// Sets the window-to-canvas transform used to derive `point` on mouse
    /// events. `page_point` stays untransformed.
    pub fn use_transform(&mut self, matrix: Matrix) {
        self.mouse_transform = matrix;
    }

    /// Records a fullscreen transition.
    pub fn go_fullscreen(&mut self, is_full: bool) {
        self.queue.push(UiEvent::Fullscreen(is_full));
    }

    fn add_mouse_event(&mut self, event: &str) {
        // helper to attach positions & keyboard modifiers for each type of
        // mouse event
        let raw_position =
            LogicalPosition::<f32>::from_physical(self.mouse_point, self.dpr);
        let canvas_point = self
            .mouse_transform
            .map_point((raw_position.x, raw_position.y));

        self.queue.push(UiEvent::Mouse {
            event: event.to_string(),
            point: Point::new(canvas_point.x, canvas_point.y),
            page_point: Point::new(raw_position.x, raw_position.y),
            button: self.mouse_button,
            buttons: self.mouse_buttons,
            modifiers: self.key_modifiers,
        })
    }

    /// Folds one winit event into the queue, updating tracked input state.
    ///
    /// Events with no DOM equivalent are dropped.
    pub fn capture(&mut self, event: &WindowEvent) {
        match event {
            WindowEvent::Moved(physical_pt) => {
                let LogicalPosition { x, y } = physical_pt.to_logical(self.dpr);
                self.queue.push(UiEvent::Move { left: x, top: y });
            }

            WindowEvent::Resized(physical_size) => {
                let LogicalSize { width, height } =
                    LogicalSize::<f32>::from_physical(*physical_size, self.dpr);
                self.queue.push(UiEvent::Resize(Size::new(width, height)));
            }

            WindowEvent::Focused(in_focus) => {
                self.queue.push(UiEvent::Focus(*in_focus));
            }

            WindowEvent::ModifiersChanged(modifiers) => {
                self.key_modifiers = modifiers.state().into();
            }

            WindowEvent::CursorEntered { .. } => {
                self.add_mouse_event("mouseenter");
            }

            WindowEvent::CursorLeft { .. } => {
                self.add_mouse_event("mouseleave");
            }

            WindowEvent::CursorMoved { position, .. }
                if *position != self.mouse_point =>
            {
                self.mouse_point = *position;
                self.add_mouse_event("mousemove");
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let LogicalPosition { x, y } = match delta {
                    MouseScrollDelta::PixelDelta(physical_pt) => {
                        LogicalPosition::from_physical(*physical_pt, self.dpr)
                    }
                    MouseScrollDelta::LineDelta(h, v) => {
                        LogicalPosition { x: *h, y: *v }
                    }
                };
                self.queue.push(UiEvent::Wheel {
                    deltaX: x,
                    deltaY: y,
                });
            }

            WindowEvent::MouseInput { state, button, .. } => {
                let (button_id, button_bits) = match button {
                    MouseButton::Left => (0, 1),
                    MouseButton::Middle => (1, 4),
                    MouseButton::Right => (2, 2),
                    MouseButton::Back => (3, 8),
                    MouseButton::Forward => (4, 16),
                    MouseButton::Other(num) => (*num, 0),
                };

                self.mouse_button = Some(button_id);
                match state {
                    ElementState::Pressed => {
                        self.mouse_buttons |= button_bits;
                        self.add_mouse_event("mousedown");
                    }
                    ElementState::Released => {
                        self.mouse_buttons &= !button_bits;
                        self.add_mouse_event("mouseup");
                        self.mouse_button = None;
                    }
                }
            }

            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: Code(key_code),
                        logical_key,
                        state,
                        repeat,
                        location,
                        ..
                    },
                ..
            } => {
                //
                // `keyup`/`keydown` events
                //
                let event_type = match state {
                    ElementState::Pressed => "keydown",
                    ElementState::Released => "keyup",
                }
                .to_string();

                let key_text = match logical_key {
                    Named(n) => serde_json::from_value(json!(n))
                        .unwrap_or_else(|_| format!("{:?}", n)),
                    Character(c) => c.to_string(),
                    _ => String::new(),
                };

                let key_location = match location {
                    KeyLocation::Standard => 0,
                    KeyLocation::Left => 1,
                    KeyLocation::Right => 2,
                    KeyLocation::Numpad => 3,
                };

                self.queue.push(UiEvent::Keyboard {
                    event: event_type,
                    key: key_text.clone(),
                    code: (*key_code).into(),
                    location: key_location,
                    modifiers: self.key_modifiers,
                    repeat: *repeat,
                });

                //
                // `input` events
                //
                if self.compose_ongoing {
                    // don't emit the un-composed keystroke if it's part of an
                    // IME composition
                    self.compose_ongoing =
                        !matches!(state, ElementState::Released);
                } else if *state == ElementState::Pressed {
                    // ignore keyups, just report presses & repeats
                    // in addition to printable characters, report
                    // spacing & deletion as input
                    let key_char = match &logical_key {
                        Character(c) => Some(c.to_string()),
                        Named(NamedKey::Tab) => Some("\t".to_string()),
                        Named(NamedKey::Space) => Some(" ".to_string()),
                        Named(
                            NamedKey::Backspace
                            | NamedKey::Delete
                            | NamedKey::Enter,
                        ) => Some("".to_string()),
                        _ => None,
                    };

                    let input_type = match &logical_key {
                        Named(NamedKey::Backspace) => "deleteContentBackward",
                        Named(NamedKey::Delete) => "deleteContentForward",
                        Named(NamedKey::Enter) => "insertLineBreak",
                        _ => "insertText",
                    }
                    .to_string();

                    if let Some(string) = key_char {
                        let data = if !string.is_empty() {
                            Some(string)
                        } else {
                            None
                        };
                        self.queue.push(UiEvent::Input(data, input_type));
                    };
                }
            }

            WindowEvent::Ime(event, ..) => {
                match &event {
                    Ime::Preedit(string, Some(_range)) => {
                        if !self.compose_begun {
                            self.queue.push(UiEvent::Composition {
                                event: "compositionstart".to_string(),
                                data: "".to_string(),
                            });
                            self.compose_begun = true; // flag: don't emit
                            // another `start` until
                            // this commits
                        }
                        self.queue.push(UiEvent::Composition {
                            event: "compositionupdate".to_string(),
                            data: string.clone(),
                        });
                        self.compose_ongoing = true; // flag: don't emit `input`
                        // while composing
                    }
                    Ime::Commit(string) => {
                        self.queue.push(UiEvent::Composition {
                            event: "compositionend".to_string(),
                            data: string.clone(),
                        });
                        self.queue.push(UiEvent::Input(
                            Some(string.clone()),
                            "insertCompositionText".to_string(),
                        )); // emit the composed character
                        self.compose_begun = false;
                    }
                    _ => {}
                };
            }

            _ => {}
        }
    }

    /// Drains the queue and returns it as JSON, leaving the sieve empty.
    pub fn collect(&mut self) -> serde_json::Value {
        let payload = json!(self.queue);
        self.queue.clear();
        payload
    }

    /// Returns `true` when no events are waiting.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    // These pin the wire format, not the Rust types. `lib/classes/gui.js`
    // parses this JSON by destructuring exact field names -- `page_point`,
    // `deltaX`, `shiftKey` -- and switching on the external tag, and nothing
    // else covers that: the window suite needs a display, so it does not run
    // in CI or on most machines.
    //
    // The reason they exist now is that `UiEvent` is about to stop carrying
    // winit's types. A rename or a reshaped variant would compile, pass every
    // other test, and break the JS window API silently. Any diff here that is
    // not deliberate is that bug.

    fn modifiers() -> ModifierKeys {
        ModifierKeys {
            shift_key: true,
            ctrl_key: false,
            alt_key: false,
            meta_key: true,
        }
    }

    #[test]
    fn wheel_carries_both_axes() {
        let event = UiEvent::Wheel {
            deltaX: 1.5,
            deltaY: -2.0,
        };
        assert_eq!(
            to_value(&event).unwrap(),
            json!({ "wheel": { "deltaX": 1.5, "deltaY": -2.0 } })
        );
    }

    #[test]
    fn move_carries_the_new_origin() {
        let event = UiEvent::Move {
            left: 10.0,
            top: 20.0,
        };
        assert_eq!(
            to_value(&event).unwrap(),
            json!({ "move": { "left": 10.0, "top": 20.0 } })
        );
    }

    #[test]
    fn keyboard_names_the_physical_key_as_a_string() {
        let event = UiEvent::Keyboard {
            event: "keydown".to_string(),
            key: "a".to_string(),
            code: Key::KeyA,
            location: 0,
            modifiers: modifiers(),
            repeat: false,
        };
        assert_eq!(
            to_value(&event).unwrap(),
            json!({ "keyboard": {
                "event": "keydown",
                "key": "a",
                "code": "KeyA",
                "location": 0,
                "modifiers": {
                    "shiftKey": true,
                    "ctrlKey": false,
                    "altKey": false,
                    "metaKey": true,
                },
                "repeat": false,
            }})
        );
    }

    #[test]
    fn composition_carries_the_text_so_far() {
        let event = UiEvent::Composition {
            event: "compositionupdate".to_string(),
            data: "".to_string(),
        };
        assert_eq!(
            to_value(&event).unwrap(),
            json!({ "composition": {
                "event": "compositionupdate",
                "data": "",
            }})
        );
    }

    // `point` and `page_point` are the pair the JS side pulls apart into
    // `{x, y}` and `{pageX, pageY}`, so both spellings matter.
    #[test]
    fn mouse_carries_both_coordinate_spaces() {
        let event = UiEvent::Mouse {
            event: "mousedown".to_string(),
            button: Some(0),
            buttons: 1,
            point: Point::new(12.0, 34.0),
            page_point: Point::new(56.0, 78.0),
            modifiers: modifiers(),
        };
        assert_eq!(
            to_value(&event).unwrap(),
            json!({ "mouse": {
                "event": "mousedown",
                "button": 0,
                "buttons": 1,
                "point": { "x": 12.0, "y": 34.0 },
                "page_point": { "x": 56.0, "y": 78.0 },
                "modifiers": {
                    "shiftKey": true,
                    "ctrlKey": false,
                    "altKey": false,
                    "metaKey": true,
                },
            }})
        );
    }

    #[test]
    fn mouse_omits_the_button_on_a_move() {
        let event = UiEvent::Mouse {
            event: "mousemove".to_string(),
            button: None,
            buttons: 0,
            point: Point::new(0.0, 0.0),
            page_point: Point::new(0.0, 0.0),
            modifiers: modifiers(),
        };
        assert_eq!(to_value(&event).unwrap()["mouse"]["button"], json!(null));
    }

    // A two-field tuple variant, so this is an array rather than an object.
    #[test]
    fn input_is_a_pair_of_data_and_input_type() {
        let event =
            UiEvent::Input(Some("x".to_string()), "insertText".to_string());
        assert_eq!(
            to_value(&event).unwrap(),
            json!({ "input": ["x", "insertText"] })
        );
    }

    #[test]
    fn input_carries_null_data_for_a_deletion() {
        let event = UiEvent::Input(None, "deleteContentBackward".to_string());
        assert_eq!(
            to_value(&event).unwrap(),
            json!({ "input": [null, "deleteContentBackward"] })
        );
    }

    #[test]
    fn focus_is_a_bare_boolean() {
        assert_eq!(
            to_value(UiEvent::Focus(true)).unwrap(),
            json!({ "focus": true })
        );
    }

    #[test]
    fn fullscreen_is_a_bare_boolean() {
        assert_eq!(
            to_value(UiEvent::Fullscreen(false)).unwrap(),
            json!({ "fullscreen": false })
        );
    }

    // The one variant whose JSON moved when winit's types were swapped out:
    // `LogicalSize<u32>` serialized `800`, and `Size` is f32, so it now
    // serializes `800.0`. The only such change, and it stops at Rust --
    // JSON has one number type and JavaScript parses both to the same 800,
    // which `win.canvas.prop("width", e.width)` cannot tell apart.
    #[test]
    fn resize_carries_width_and_height() {
        let event = UiEvent::Resize(Size::new(800.0, 600.0));
        assert_eq!(
            to_value(&event).unwrap(),
            json!({ "resize": { "width": 800.0, "height": 600.0 } })
        );
    }

    // The sieve hands the loop an array of these, which is the shape
    // `#eachWindow` iterates. An empty queue has to stay an empty array
    // rather than becoming null.
    #[test]
    fn the_sieve_drains_to_an_array_and_empties() {
        let mut sieve = Sieve::new(1.0);
        assert_eq!(sieve.collect(), json!([]));

        sieve.queue.push(UiEvent::Focus(true));
        assert_eq!(sieve.collect(), json!([{ "focus": true }]));
        assert!(sieve.is_empty());
    }
}
