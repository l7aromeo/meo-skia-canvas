//
// Windows & event handling
//

"use strict";

const { EventEmitter } = require("events"),
  { RustClass, core, inspect, neon, REPR, PROP, CALL } = require("./neon"),
  { Canvas } = require("./canvas"),
  css = require("./css");

// A `RangeError` for a geometry value that is not a finite number: the
// argument is the right kind and its value is not, which is the distinction
// AGENTS.md's fourth rule exists for. Discarding it silently was the
// alternative, and it left a window at a size or position nobody asked for
// with nothing to say why -- `win.width = NaN` is the case, and it arrives
// from arithmetic rather than from a typo.
//
// The Canvas API ignores a value it cannot use, but that is spec-mandated for
// canvas properties and `Window` is in no standard at all.
//
// `unset` is what separates the position from the size. `left` and `top` are
// `Option<f32>` on the Rust side and begin life unset, so a window that has
// not been placed yet reports `null` through the event loop's `geom` payload
// and `undefined` from its own constructor -- both meaning "no position", and
// both arriving at this setter. `width` and `height` are plain `f32` there and
// always have a value, so nothing legitimate reaches them unset.
const finiteOr = (value, prop, unset = false) => {
  if (unset && (value === null || value === undefined)) return value;
  if (!Number.isFinite(value)) {
    throw new RangeError(
      `Expected a finite number for \`${prop}\` (got ${JSON.stringify(value)})`,
    );
  }
  return value;
};

// The two names the declarations used to list and the runtime never took.
// A caller passing either was already getting the default cursor with nothing
// said about it, so this refusal is where they find out -- and it should name
// the replacement rather than leave them to search a thirty-five-name union
// for it. `"hand"` and `"arrow"` are the legacy IE spellings of the CSS
// `pointer` and `default`.
//
// Two entries by hand rather than a matcher. Edit distance would be a surface
// to maintain and would eventually suggest something wrong; this only claims
// to know about the two names this release removed.
const CURSOR_REPLACEMENTS = { hand: "pointer", arrow: "default" };

// How a `Window` tells `App` it has opened or closed. Module scope rather
// than a static on the class: `App` and `Window` are declared in this file
// and nothing outside it has a reason to emit or listen, while a static was
// reachable as `Window.events` and declared nowhere -- an emitter a caller
// could attach to, with no promise attached.
//
// A caller's own events are on the instance, which `Window extends
// EventEmitter` provides and `lib/index.d.ts` declares.
const windowEvents = new EventEmitter();

const checkSupport = () => {
  if (!neon.App)
    throw new Error("Skia Canvas was compiled without window support");
};

class App extends RustClass {
  static #locale =
    process.env.LC_ALL ||
    process.env.LC_MESSAGES ||
    process.env.LANG ||
    process.env.LANGUAGE;
  #events = "native"; // `native` for an OS event loop or `node` to poll for ui-events from node
  #started = false; // whether the `eventLoop` property is permanently set
  #launcher; // timer set by opening windows to ensure app is launched soon after
  #session; // Promise that resolves when the current set of windows are all closed

  #windows = [];
  #frames = {};
  #fps = 60;

  constructor() {
    super(App);

    // set the callback to use for event dispatch & rendering
    if (neon.App) this[CALL]("register", this.#dispatch.bind(this));

    // track new windows and schedule launch if needed
    windowEvents.on("open", (win) => {
      // The native call first, and everything else only once it has
      // returned. It refuses on a machine with no GPU -- `openWindow`
      // validates one -- and that refusal comes back out through the `Window`
      // constructor, so a caller never gets the handle it would have closed.
      // Scheduling the launch before this meant a window that failed to open
      // still left one pending, and nothing could cancel it: it fired,
      // reached `activate`, and threw the same error again with no caller to
      // give it to, which ends the process rather than the call.
      this[CALL](
        "openWindow",
        JSON.stringify(win._state),
        core(win.canvas.pages[win._state.page - 1]),
      );
      this.#windows.push(win);
      // -1, so the pre-increment below lands on 0 for the first tick: that is
      // the frame the `setup` event is defined to precede, and the number the
      // `frame` event is documented to start from. Seeded at 0, the first tick
      // was frame 1 and `frame == 0` was unreachable, so `setup` -- declared
      // in the types, documented on the Window page, and emitted nowhere else
      // in the package -- could never fire.
      this.#frames[win.id] = -1;
      if (!this.#launcher) this.#launcher = setImmediate(() => this.launch());
    });

    // drop closed windows
    windowEvents.on("close", (win) => {
      this.#windows = this.#windows.filter((w) => w !== win);
      this[CALL]("closeWindow", win.id);
      win.emit("close");

      // A window opened and closed before the launch it scheduled has run
      // leaves nothing to launch. Cancelling here is what keeps that from
      // starting an event loop for no windows -- and on a machine with no
      // display, from failing to start one and reporting it as though the
      // caller had asked.
      if (!this.#windows.length && this.#launcher && !this.#started) {
        clearImmediate(this.#launcher);
        this.#launcher = null;
      }
    });
  }

  get windows() {
    return [...this.#windows];
  }
  get running() {
    return this.#started;
  }
  get eventLoop() {
    return this.#events;
  }
  set eventLoop(mode) {
    if (this.#started)
      throw new Error("Cannot alter event loop after it has begun");
    // A `TypeError` because the value is outside an enumeration, AGENTS.md's
    // second rule. Setting the mode it already has stays a no-op.
    if (!["native", "node"].includes(mode)) {
      throw new TypeError(
        `Expected "native" or "node" for \`eventLoop\` (got ${JSON.stringify(mode)})`,
      );
    }
    if (mode != this.#events) {
      this.#events = this[CALL]("setMode", mode);
    }
  }
  get fps() {
    return this.#fps;
  }
  set fps(rate) {
    checkSupport();
    // A `RangeError` because the argument is a number and its value is not
    // one this accepts, AGENTS.md's fourth rule. Below one frame a second
    // there is no rate to set, and discarding the request silently left the
    // loop running at whatever it had.
    if (!Number.isFinite(rate) || rate < 1) {
      throw new RangeError(
        `Expected a number of at least 1 for \`fps\` (got ${JSON.stringify(rate)})`,
      );
    }
    if (rate != this.#fps) {
      this.#fps = this[CALL]("setRate", rate);
    }
  }

  launch() {
    checkSupport();
    clearImmediate(this.#launcher);
    this.#started = true;

    this.#session ??= this[CALL]("activate").finally(() => {
      this.#session = null;
      this.#launcher = null;
      this.emit("idle", { type: "idle", target: this });
    });

    return this.#session;
  }

  #eachWindow(updates, callback) {
    for (const [id, payload] of Object.entries(updates || {})) {
      let win = this.#windows.find((win) => win.id == id);
      if (win) callback(win, payload);
    }
  }

  #dispatch(isFrame, payload) {
    let { geom, state, ui } = JSON.parse(payload);

    // merge autogenerated window locations into newly opened windows
    if (geom)
      this.#eachWindow(geom, (win, { top, left }) => {
        win.left = win.left || left;
        win.top = win.top || top;
      });

    // update state of windows that are still active and mark others as closed
    if (state)
      this.#windows = this.#windows.filter((win) => {
        // keep active windows and new ones still waiting for a `geom` roundtrip to set their initial position
        if (win.id in state || win.top === undefined) {
          Object.assign(win, state[win.id]);
          return true;
        }

        // but otherwise evict all windows that have been closed via title bar widget
        win.close();
      });

    // deliver ui events to corresponding windows
    if (ui)
      this.#eachWindow(ui, (win, events) => {
        for (const [[type, e]] of events.map((o) => Object.entries(o))) {
          switch (type) {
            case "mouse": {
              let {
                button,
                buttons,
                point,
                page_point: { x: pageX, y: pageY },
                modifiers,
              } = e;
              win.emit(e.event, {
                button,
                buttons,
                ...point,
                pageX,
                pageY,
                ...modifiers,
              });
              break;
            }

            case "input": {
              let [data, inputType] = e;
              win.emit(type, { data, inputType });
              break;
            }

            case "composition":
              win.emit(e.event, { data: e.data, locale: App.#locale });
              break;

            case "keyboard": {
              let { event, key, code, location, repeat, modifiers } = e,
                defaults = true;

              win.emit(event, {
                key,
                code,
                location,
                repeat,
                ...modifiers,
                preventDefault: () => (defaults = false),
              });

              // apply default keybindings unless e.preventDefault() was run
              if (defaults && event == "keydown" && !repeat) {
                let { ctrlKey, altKey, metaKey } = modifiers;
                if (
                  (metaKey && key == "w") ||
                  (ctrlKey && key == "c") ||
                  (altKey && key == "F4")
                ) {
                  win.close();
                } else if ((metaKey && key == "f") || (altKey && key == "F8")) {
                  win.fullscreen = !win.fullscreen;
                }
              }
              break;
            }

            case "focus":
              if (e) win.emit("focus");
              else win.emit("blur");
              break;

            case "resize":
              if (win.fit == "resize") {
                win.ctx[PROP]("size", e.width, e.height);
                win.canvas[PROP]("width", e.width);
                win.canvas[PROP]("height", e.height);
              }
              win.emit(type, e);
              break;

            case "move":
            case "wheel":
              win.emit(type, e);
              break;

            case "fullscreen":
              win.emit(type, { enabled: e });
              break;

            default:
              console.log(type, e);
          }
        }
      });

    // provide frame updates to prompt redraws
    if (isFrame)
      for (let win of this.#windows) {
        let frame = ++this.#frames[win.id];

        if (frame == 0) win.emit("setup");
        win.emit("frame", { frame });
        if (win.listenerCount("draw")) {
          win.canvas.getContext("2d").reset();
          win.emit("draw", { frame });
        }
      }

    // if this is a full roundtrip, return window state & content
    return (
      isFrame && [
        JSON.stringify(this.#windows.map((win) => win._state)),
        this.#windows.map((win) => core(win.canvas.pages[win.page - 1])),
      ]
    );
  }

  quit() {
    this[CALL]("quit");
  }

  [REPR](depth, options) {
    let { eventLoop, fps, windows } = this;
    return `App ${inspect(
      { eventLoop, fps, windows },
      Object.assign(options, {
        depth: 1,
        customInspect: false,
      }),
    )}`;
  }
}

// Mix the EventEmitter properties into App
Object.assign(App.prototype, EventEmitter.prototype);

class Window extends EventEmitter {
  static #kwargs =
    "id,left,top,width,height,title,page,background,fullscreen,cursor,fit,visible,resizable,borderless,closed".split(
      /,/,
    );
  static #nextID = 1;
  #canvas;
  #state;

  // accept either ƒ(width, height, {…}) or ƒ({…})
  constructor(width = 512, height = 512, opts = {}) {
    checkSupport();

    if (!Number.isFinite(width) || !Number.isFinite(height)) {
      opts = [...arguments].slice(-1)[0] || {};
      width = opts.width || (opts.canvas || {}).width || 512;
      height = opts.height || (opts.canvas || {}).height || 512;
    }

    // Refused here rather than ignored. `canvas` is absent from `#kwargs`,
    // so the filter below drops it before any setter runs -- which meant a
    // value of the wrong kind was discarded in silence and the window opened
    // showing a canvas of its own making. That is worse than the discards
    // this refusal joins: they kept a value the caller had set, this one
    // substituted something the caller never asked for.
    //
    // The same `TypeError` and the same sentence as `set canvas`, which is
    // the only other way to get one in.
    // Refused here rather than ignored. `canvas` is absent from `#kwargs`,
    // so the filter below drops it before any setter runs -- which meant a
    // value of the wrong kind was discarded in silence and the window opened
    // showing a canvas of its own making. That is worse than the discards
    // this refusal joins: they kept a value the caller had set, this one
    // substituted something the caller never asked for.
    //
    // The same `TypeError` as `set canvas`, which is the only other way to
    // get one in.
    if (opts.canvas !== undefined && !(opts.canvas instanceof Canvas)) {
      throw new TypeError(
        `Expected a Canvas for \`canvas\` (got ${typeof opts.canvas})`,
      );
    }
    let hasCanvas = opts.canvas instanceof Canvas;
    let { textContrast = 0, textGamma = 1.4 } = hasCanvas
      ? opts.canvas.engine
      : opts;
    // colorType/colorSpace only apply when we build the canvas — a supplied one already
    // carries its own. Forwarded as-is so Canvas applies its own defaults for whichever
    // is absent, rather than this constructor duplicating them.
    let canvas = hasCanvas
      ? opts.canvas
      : new Canvas(width, height, {
          textContrast,
          textGamma,
          colorType: opts.colorType,
          colorSpace: opts.colorSpace,
        });

    super(Window);
    this.#state = {
      title: "",
      visible: true,
      resizable: true,
      borderless: false,
      background: "white",
      fullscreen: false,
      closed: false,
      page: canvas.pages.length,
      left: undefined,
      top: undefined,
      width,
      height,
      textContrast,
      textGamma,
      cursor: "default",
      fit: "contain",
      id: Window.#nextID++,
    };

    Object.assign(
      this,
      { canvas },
      Object.fromEntries(
        Object.entries(opts).filter(
          ([k, v]) => Window.#kwargs.includes(k) && v !== undefined,
        ),
      ),
    );

    windowEvents.emit("open", this);
  }

  // Underscored because it is `App`'s read of a window's spec on the way to
  // the native side, not part of the window API -- a copy, so a caller who
  // reaches it anyway cannot write through it. `#state` cannot serve here:
  // `App` is a different class, and a private field is reachable only from
  // inside the class that declares it.
  get _state() {
    return { ...this.#state };
  }
  get ctx() {
    return this.#canvas.pages[this.page - 1];
  }

  get id() {
    return this.#state.id;
  }
  set id(id) {
    if (id != this.id) throw new Error("Window IDs are immutable");
  }

  get canvas() {
    return this.#canvas;
  }
  set canvas(canvas) {
    // A `TypeError` under AGENTS.md's fifth rule: a value of the wrong kind
    // entirely, rather than an enum's spelling or a number's range. WebIDL
    // raises one when interface conversion fails, which is what a browser
    // does for `drawImage(42)`.
    //
    // The kind received is named because that is what the complaint is about
    // -- unlike the cases above, where the kind is right and the value is
    // not, so the message names the values instead.
    if (!(canvas instanceof Canvas)) {
      throw new TypeError(
        `Expected a Canvas for \`canvas\` (got ${typeof canvas})`,
      );
    }
    {
      canvas.getContext("2d"); // ensure it has at least one page
      this.#canvas = canvas;
      this.#state.page = canvas.pages.length;
      this.#state.textContrast = canvas.engine.textContrast;
      this.#state.textGamma = canvas.engine.textGamma;
    }
  }

  get visible() {
    return this.#state.visible;
  }
  set visible(flag) {
    this.#state.visible = !!flag;
  }

  get resizable() {
    return this.#state.resizable;
  }
  set resizable(flag) {
    this.#state.resizable = !!flag;
  }

  get borderless() {
    return this.#state.borderless;
  }
  set borderless(flag) {
    this.#state.borderless = !!flag;
  }

  get fullscreen() {
    return this.#state.fullscreen;
  }
  set fullscreen(flag) {
    this.#state.fullscreen = !!flag;
  }

  get title() {
    return this.#state.title;
  }
  set title(txt) {
    this.#state.title = (txt != null ? txt : "").toString();
  }

  get cursor() {
    return this.#state.cursor;
  }
  set cursor(icon) {
    // A `TypeError` because the value is outside an enumeration -- WebIDL's
    // rule, and the one AGENTS.md picks for an enum whether or not a standard
    // defines it. The alternative was discarding the name silently, which
    // made a misspelling indistinguishable from success: the window kept the
    // shape it already had and nothing said why. `Object.assign` in the
    // constructor runs this setter too, so a name it refuses is refused there
    // as well.
    if (!css.cursor(icon)) {
      const instead = CURSOR_REPLACEMENTS[icon];
      throw new TypeError(
        `Expected a CursorStyle keyword for \`cursor\` (got ${JSON.stringify(icon)}` +
          (instead ? ` -- use ${JSON.stringify(instead)})` : ")"),
      );
    }
    this.#state.cursor = icon;
  }

  get fit() {
    return this.#state.fit;
  }
  set fit(mode) {
    // A `TypeError` because the value is outside an enumeration, as for
    // `cursor` above -- AGENTS.md's second rule, WebIDL's for an enum.
    if (!css.fit(mode)) {
      throw new TypeError(
        `Expected "none", "contain", "contain-x", "contain-y", "cover", ` +
          `"fill", "scale-down", or "resize" for \`fit\` ` +
          `(got ${JSON.stringify(mode)})`,
      );
    }
    this.#state.fit = mode;
  }

  get left() {
    return this.#state.left;
  }
  set left(val) {
    this.#state.left = finiteOr(val, "left", true);
  }

  get top() {
    return this.#state.top;
  }
  set top(val) {
    this.#state.top = finiteOr(val, "top", true);
  }

  get width() {
    return this.#state.width;
  }
  set width(val) {
    this.#state.width = finiteOr(val, "width");
  }

  get height() {
    return this.#state.height;
  }
  set height(val) {
    this.#state.height = finiteOr(val, "height");
  }

  get page() {
    return this.#state.page;
  }
  set page(val) {
    if (val < 0) val += this.#canvas.pages.length + 1;
    let page = this.#canvas.pages[val - 1];
    // A `RangeError` because the argument is a number outside the set of
    // pages this canvas has, AGENTS.md's fourth rule. A page count only ever
    // grows, so a number the event loop echoes back through `Object.assign`
    // names a page that still exists.
    if (!page) {
      throw new RangeError(
        `Expected a page between 1 and ${this.#canvas.pages.length} (got ${JSON.stringify(val)})`,
      );
    }
    if (this.#state.page != val) {
      let [width, height] = page[PROP]("size");
      this.#canvas[PROP]("width", width);
      this.#canvas[PROP]("height", height);
      this.#state.page = val;
    }
  }

  get background() {
    return this.#state.background;
  }
  set background(c) {
    this.#state.background = (c != null ? c : "").toString();
  }

  get closed() {
    return this.#state.closed;
  }
  close() {
    if (!this.#state.closed) {
      this.#state.closed = true;
      windowEvents.emit("close", this);
    }
  }
  open() {
    if (this.#state.closed) {
      this.#state.closed = false;
      windowEvents.emit("open", this);
    }
  }

  emit(type, e) {
    // report errors in event-handlers but don't crash
    try {
      super.emit(type, Object.assign({ target: this, type }, e));
    } catch (err) {
      console.error(err);
    }
  }

  [REPR](depth, options) {
    let info = Object.fromEntries(
      Window.#kwargs.map((k) => [k, this.#state[k]]),
    );
    return `Window ${inspect(info, options)}`;
  }
}

module.exports = { App: new App(), Window };
