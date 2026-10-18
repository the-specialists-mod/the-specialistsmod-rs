//! Native mouse input for Steam GoldSrc clients (FWGS sends mouse deltas through
//! `IN_ClientLookEvent` instead): SDL2 relative mouse mode, looked up at run time in the SDL2
//! library the engine already loaded, and the Windows cursor re-centring of the SDK's
//! inputw32.cpp.

/// SDL2 entry points from the SDL2 library the engine already loaded (looked up at run time).
pub mod sdl {
    use std::ffi::{CStr, c_int, c_void};
    use std::sync::OnceLock;

    type GetRelativeMouseState = unsafe extern "C" fn(*mut c_int, *mut c_int) -> u32;
    type SetRelativeMouseMode = unsafe extern "C" fn(c_int) -> c_int;

    struct Sdl {
        get_relative: Option<GetRelativeMouseState>,
        set_relative: Option<SetRelativeMouseMode>,
    }

    #[cfg(not(windows))]
    pub(crate) fn lookup(name: &CStr) -> *mut c_void {
        unsafe extern "C" {
            fn dlopen(file: *const std::ffi::c_char, mode: c_int) -> *mut c_void;
            fn dlsym(handle: *mut c_void, name: *const std::ffi::c_char) -> *mut c_void;
        }
        const RTLD_NOW: c_int = 2;
        const RTLD_NOLOAD: c_int = 4;
        // SAFETY: dlopen/dlsym are called with NUL-terminated names; RTLD_NOLOAD never loads a new library.
        unsafe {
            for lib in [c"libSDL2-2.0.so.0", c"libSDL2.so"] {
                let h = dlopen(lib.as_ptr(), RTLD_NOW | RTLD_NOLOAD);
                if !h.is_null() {
                    let p = dlsym(h, name.as_ptr());
                    if !p.is_null() {
                        return p;
                    }
                }
            }
            // RTLD_DEFAULT: anything already in the global namespace.
            dlsym(std::ptr::null_mut(), name.as_ptr())
        }
    }

    #[cfg(windows)]
    pub(crate) fn lookup(name: &CStr) -> *mut c_void {
        // SAFETY: GetModuleHandleA takes a NUL-terminated name, only looks up an already-loaded
        // module (it never loads one) and returns null when SDL2.dll isn't loaded.
        let h = unsafe { super::win::GetModuleHandleA(c"SDL2.dll".as_ptr()) };
        // SAFETY: `h` is a non-null module handle from GetModuleHandleA and `name` is
        // NUL-terminated; GetProcAddress returns null for a missing export.
        if h.is_null() { std::ptr::null_mut() } else { unsafe { super::win::GetProcAddress(h, name.as_ptr()) } }
    }

    fn sdl() -> &'static Sdl {
        static S: OnceLock<Sdl> = OnceLock::new();
        S.get_or_init(|| {
            let g = lookup(c"SDL_GetRelativeMouseState");
            let s = lookup(c"SDL_SetRelativeMouseMode");
            Sdl {
                // SAFETY: a non-null symbol with this name is the SDL2 function with this C signature (SDL2 ABI).
                get_relative: (!g.is_null()).then(|| unsafe { std::mem::transmute::<*mut c_void, GetRelativeMouseState>(g) }),
                // SAFETY: a non-null symbol with this name is the SDL2 function with this C signature (SDL2 ABI).
                set_relative: (!s.is_null()).then(|| unsafe { std::mem::transmute::<*mut c_void, SetRelativeMouseMode>(s) }),
            }
        })
    }

    pub fn available() -> bool {
        sdl().get_relative.is_some()
    }

    pub fn relative_delta() -> [f32; 2] {
        let Some(f) = sdl().get_relative else { return [0.0; 2] };
        let (mut x, mut y) = (0, 0);
        // SAFETY: SDL2 function looked up above, called with its C signature and valid arguments.
        unsafe { f(&mut x, &mut y) };
        [x as f32, y as f32]
    }

    pub fn set_relative(on: bool) {
        if let Some(f) = sdl().set_relative {
            // SAFETY: SDL2 function looked up above, called with its C signature and valid arguments.
            unsafe { f(on as c_int) };
        }
    }

    pub use card::{Event, Key, Window, set_clipboard};

    /// A small window of its own for the port's modal cards (restart and reconnect,
    /// docs/implementation/client-plugins.md §13.5): filled rectangles on the window surface
    /// (SDL's software path, so the engine's GL context is never touched) and the window's
    /// events. What to draw and how to answer events is `ts-client`'s.
    mod card {
        use super::lookup;
        use std::ffi::{CStr, c_char, c_int, c_void};
        use std::sync::OnceLock;

        #[repr(C)]
        struct Rect {
            x: c_int,
            y: c_int,
            w: c_int,
            h: c_int,
        }
        /// The leading fields of `SDL_Surface` (SDL2 ABI): only `format` is read.
        #[repr(C)]
        struct Surface {
            flags: u32,
            format: *mut c_void,
        }
        /// `SDL_Event`: a 56-byte union; the fields are read at their SDL2 offsets.
        #[repr(C, align(8))]
        struct RawEvent([u8; 56]);

        type CreateWindow = unsafe extern "C" fn(*const c_char, c_int, c_int, c_int, c_int, u32) -> *mut c_void;
        type WindowFn = unsafe extern "C" fn(*mut c_void);
        type GetWindowId = unsafe extern "C" fn(*mut c_void) -> u32;
        type GetWindowSurface = unsafe extern "C" fn(*mut c_void) -> *mut Surface;
        type UpdateWindowSurface = unsafe extern "C" fn(*mut c_void) -> c_int;
        type FillRect = unsafe extern "C" fn(*mut Surface, *const Rect, u32) -> c_int;
        type MapRgb = unsafe extern "C" fn(*mut c_void, u8, u8, u8) -> u32;
        type WaitEventTimeout = unsafe extern "C" fn(*mut RawEvent, c_int) -> c_int;
        type Void = unsafe extern "C" fn();
        type SetClipboardText = unsafe extern "C" fn(*const c_char) -> c_int;

        struct Api {
            create: CreateWindow,
            destroy: WindowFn,
            raise: WindowFn,
            id: GetWindowId,
            surface: GetWindowSurface,
            update: UpdateWindowSurface,
            fill: FillRect,
            map_rgb: MapRgb,
            wait: WaitEventTimeout,
            start_text: Void,
            stop_text: Void,
        }

        fn api() -> Option<&'static Api> {
            static A: OnceLock<Option<Api>> = OnceLock::new();
            A.get_or_init(|| {
                macro_rules! sym {
                    ($name:literal, $t:ty) => {{
                        let p = lookup($name);
                        if p.is_null() {
                            return None;
                        }
                        // SAFETY: a non-null symbol with this name is the SDL2 function with this C signature (SDL2 ABI).
                        unsafe { std::mem::transmute::<*mut c_void, $t>(p) }
                    }};
                }
                Some(Api {
                    create: sym!(c"SDL_CreateWindow", CreateWindow),
                    destroy: sym!(c"SDL_DestroyWindow", WindowFn),
                    raise: sym!(c"SDL_RaiseWindow", WindowFn),
                    id: sym!(c"SDL_GetWindowID", GetWindowId),
                    surface: sym!(c"SDL_GetWindowSurface", GetWindowSurface),
                    update: sym!(c"SDL_UpdateWindowSurface", UpdateWindowSurface),
                    fill: sym!(c"SDL_FillRect", FillRect),
                    map_rgb: sym!(c"SDL_MapRGB", MapRgb),
                    wait: sym!(c"SDL_WaitEventTimeout", WaitEventTimeout),
                    start_text: sym!(c"SDL_StartTextInput", Void),
                    stop_text: sym!(c"SDL_StopTextInput", Void),
                })
            })
            .as_ref()
        }

        /// `SDL_SetClipboardText`; false when SDL2 isn't loaded or the call fails.
        pub fn set_clipboard(text: &str) -> bool {
            let p = lookup(c"SDL_SetClipboardText");
            if p.is_null() {
                return false;
            }
            // SAFETY: a non-null symbol with this name is the SDL2 function with this C signature (SDL2 ABI).
            let f = unsafe { std::mem::transmute::<*mut c_void, SetClipboardText>(p) };
            let s = crate::util::to_cstring(text);
            // SAFETY: SDL copies the NUL-terminated string, which outlives the call.
            unsafe { f(s.as_ptr()) == 0 }
        }

        /// The keys a card answers to.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Key {
            Escape,
            Return,
            Backspace,
            Other,
        }

        /// A card window's events (other windows' events are dropped while a card is up).
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub enum Event {
            /// The window was closed, or the application asked to quit.
            Close,
            /// A left click at window coordinates.
            Click(i32, i32),
            /// The pointer moved to window coordinates.
            Motion(i32, i32),
            Key(Key),
            /// Text typed (`SDL_TEXTINPUT`), UTF-8.
            Text(String),
            /// The window needs drawing again (shown, exposed, resized).
            Redraw,
        }

        /// A centred, always-on-top window. Dropped: destroyed (and text input stopped).
        pub struct Window {
            win: *mut c_void,
            id: u32,
            text: bool,
        }

        impl Window {
            /// Opens a `w`×`h` window titled `title`; `None` when SDL2 isn't loaded or SDL refuses.
            /// `text` starts SDL text input for a typed field.
            pub fn open(title: &CStr, w: i32, h: i32, text: bool) -> Option<Window> {
                let a = api()?;
                const CENTERED: c_int = 0x2FFF0000;
                const SHOWN: u32 = 0x4;
                const ALWAYS_ON_TOP: u32 = 0x8000;
                // SAFETY: SDL2 function called with a NUL-terminated title and plain integers.
                let win = unsafe { (a.create)(title.as_ptr(), CENTERED, CENTERED, w, h, SHOWN | ALWAYS_ON_TOP) };
                if win.is_null() {
                    return None;
                }
                // SAFETY: `win` is the live window SDL just created.
                let id = unsafe { (a.id)(win) };
                // SAFETY: as above.
                unsafe { (a.raise)(win) };
                if text {
                    // SAFETY: no arguments; SDL starts sending SDL_TEXTINPUT events.
                    unsafe { (a.start_text)() };
                }
                Some(Window { win, id, text })
            }

            /// Fills each `([x, y, w, h], rgb)` in order on the window surface, then shows it.
            pub fn draw(&self, fills: &[([i32; 4], [u8; 3])]) {
                let Some(a) = api() else { return };
                // SAFETY: `self.win` is a live window; SDL returns its surface or null.
                let s = unsafe { (a.surface)(self.win) };
                // SAFETY: a non-null surface from SDL_GetWindowSurface stays valid until the window
                // is resized or destroyed, neither of which happens during this call.
                let Some(surface) = (unsafe { s.as_ref() }) else { return };
                for ([x, y, w, h], [r, g, b]) in fills {
                    // SAFETY: `surface.format` is the surface's pixel format, owned by SDL.
                    let colour = unsafe { (a.map_rgb)(surface.format, *r, *g, *b) };
                    let rect = Rect { x: *x, y: *y, w: *w, h: *h };
                    // SAFETY: `s` is the window's surface and `rect` a local; SDL clips to the surface.
                    unsafe { (a.fill)(s, &rect, colour) };
                }
                // SAFETY: `self.win` is a live window.
                unsafe { (a.update)(self.win) };
            }

            /// Waits up to `ms` for this window's next event. Every event is taken off SDL's
            /// queue; those for other windows (the engine's) are dropped, as a modal box drops them.
            pub fn wait(&self, ms: i32) -> Option<Event> {
                let a = api()?;
                let mut ev = RawEvent([0; 56]);
                // SAFETY: SDL writes one SDL_Event (56 bytes in SDL2) into `ev`.
                if unsafe { (a.wait)(&mut ev, ms) } == 0 {
                    return None;
                }
                let b = &ev.0;
                let u32_at = |o: usize| u32::from_ne_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
                let i32_at = |o: usize| u32_at(o) as i32;
                let kind = u32_at(0);
                let ours = u32_at(8) == self.id;
                match kind {
                    0x100 => Some(Event::Close), // SDL_QUIT
                    // SDL_WINDOWEVENT: event byte at 12; 14 = CLOSE, 1 SHOWN, 3 EXPOSED, 5 RESIZED, 6 SIZE_CHANGED
                    0x200 if ours => match b[12] {
                        14 => Some(Event::Close),
                        1 | 3 | 5 | 6 => Some(Event::Redraw),
                        _ => None,
                    },
                    // SDL_KEYDOWN: keysym.sym at 20
                    0x300 if ours => Some(Event::Key(match i32_at(20) {
                        27 => Key::Escape,
                        13 | 0x4000_0058 => Key::Return, // SDLK_RETURN, SDLK_KP_ENTER
                        8 => Key::Backspace,
                        _ => Key::Other,
                    })),
                    // SDL_TEXTINPUT: text[32] at 12
                    0x303 if ours => {
                        let t = &b[12..44];
                        let n = t.iter().position(|&c| c == 0).unwrap_or(t.len());
                        Some(Event::Text(String::from_utf8_lossy(&t[..n]).into_owned()))
                    }
                    0x400 if ours => Some(Event::Motion(i32_at(20), i32_at(24))), // SDL_MOUSEMOTION
                    // SDL_MOUSEBUTTONDOWN: button at 16 (1 = left), x at 20, y at 24
                    0x401 if ours && b[16] == 1 => Some(Event::Click(i32_at(20), i32_at(24))),
                    _ => None,
                }
            }
        }

        impl Drop for Window {
            fn drop(&mut self) {
                let Some(a) = api() else { return };
                if self.text {
                    // SAFETY: no arguments.
                    unsafe { (a.stop_text)() };
                }
                // SAFETY: `self.win` is the live window from `open`, destroyed once here.
                unsafe { (a.destroy)(self.win) };
            }
        }
    }

    /// Whether the loaded SDL2 has the function `name`.
    pub fn has(name: &str) -> bool {
        std::ffi::CString::new(name).is_ok_and(|n| !lookup(&n).is_null())
    }

    /// The icon of a [`message_box`] (`SDL_MESSAGEBOX_ERROR` / `WARNING` / `INFORMATION`).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum BoxKind {
        Error,
        Warning,
        Information,
    }

    /// One button of a [`message_box`].
    #[derive(Clone, Copy, Debug)]
    pub struct BoxButton<'a> {
        /// What [`message_box`] returns when it's pressed.
        pub id: i32,
        pub text: &'a str,
        /// Enter presses it (`SDL_MESSAGEBOX_BUTTON_RETURNKEY_DEFAULT`).
        pub enter: bool,
        /// Escape presses it (`SDL_MESSAGEBOX_BUTTON_ESCAPEKEY_DEFAULT`).
        pub escape: bool,
    }

    #[repr(C)]
    struct ButtonData {
        flags: u32,
        buttonid: c_int,
        text: *const std::ffi::c_char,
    }

    #[repr(C)]
    struct MessageBoxData {
        flags: u32,
        window: *mut c_void,
        title: *const std::ffi::c_char,
        message: *const std::ffi::c_char,
        numbuttons: c_int,
        buttons: *const ButtonData,
        color_scheme: *const c_void,
    }

    type ShowMessageBox = unsafe extern "C" fn(*const MessageBoxData, *mut c_int) -> c_int;

    /// `SDL_ShowMessageBox` with no parent window, buttons left to right in `buttons` order
    /// (`SDL_MESSAGEBOX_BUTTONS_LEFT_TO_RIGHT`, which SDL before 2.0.12 ignores). Blocks until
    /// it's answered. The pressed button's id, -1 if the box was closed without one, or `None`
    /// when SDL2 isn't loaded or the box couldn't be shown. Port addition for the client plugin
    /// start-up prompt (client-plugins.md §7.1).
    pub fn message_box(kind: BoxKind, title: &str, text: &str, buttons: &[BoxButton]) -> Option<i32> {
        let p = lookup(c"SDL_ShowMessageBox");
        if p.is_null() {
            return None;
        }
        // SAFETY: a non-null symbol with this name is the SDL2 function with this C signature (SDL2 ABI).
        let show = unsafe { std::mem::transmute::<*mut c_void, ShowMessageBox>(p) };
        let cstr = |s: &str| std::ffi::CString::new(s.replace('\0', " ")).unwrap_or_default();
        let (title, text) = (cstr(title), cstr(text));
        let labels: Vec<std::ffi::CString> = buttons.iter().map(|b| cstr(b.text)).collect();
        let data: Vec<ButtonData> = buttons
            .iter()
            .zip(&labels)
            .map(|(b, l)| ButtonData { flags: b.enter as u32 | (b.escape as u32) << 1, buttonid: b.id, text: l.as_ptr() })
            .collect();
        let flags = match kind {
            BoxKind::Error => 0x10,
            BoxKind::Warning => 0x20,
            BoxKind::Information => 0x40,
        } | 0x80;
        let box_data = MessageBoxData {
            flags,
            window: std::ptr::null_mut(),
            title: title.as_ptr(),
            message: text.as_ptr(),
            numbuttons: data.len() as c_int,
            buttons: data.as_ptr(),
            color_scheme: std::ptr::null(),
        };
        let mut pressed: c_int = -1;
        // SAFETY: `box_data` and everything it points to (the strings and the button array) live
        // until the call returns; SDL only reads them, and writes the pressed id to `pressed`.
        let rc = unsafe { show(&box_data, &mut pressed) };
        (rc == 0).then_some(pressed)
    }
}

/// Windows cursor re-centering (SDK inputw32.cpp, no mouse thread).
#[cfg(windows)]
pub mod win {
    use crate::client::engine;
    use std::ffi::{c_char, c_int, c_void};

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    pub struct Point {
        x: i32,
        y: i32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetCursorPos(p: *mut Point) -> c_int;
        fn SetCursorPos(x: c_int, y: c_int) -> c_int;
        fn SystemParametersInfoA(action: u32, param: u32, pv: *mut c_void, winini: u32) -> c_int;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn GetModuleHandleA(name: *const c_char) -> *mut c_void;
        pub fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    }

    const SPI_GETMOUSE: u32 = 3;
    const SPI_SETMOUSE: u32 = 4;

    #[derive(Default)]
    pub struct State {
        params_valid: bool,
        original: [c_int; 3],
        restore: bool,
    }

    pub fn startup(s: &mut State) {
        // SAFETY: SPI_GETMOUSE writes three ints (threshold1, threshold2, acceleration) to `pvParam`,
        // and `s.original` is a `[c_int; 3]` that outlives the call.
        s.params_valid = unsafe { SystemParametersInfoA(SPI_GETMOUSE, 0, s.original.as_mut_ptr().cast(), 0) } != 0;
    }

    fn raw_input() -> bool {
        engine().cvar_float(c"m_rawinput") != 0.0 && super::sdl::available()
    }

    fn center() -> (i32, i32) {
        let f = &engine().f;
        match (f.GetWindowCenterX, f.GetWindowCenterY) {
            // SAFETY: engine client functions taking no arguments; the table is valid for the process.
            (Some(x), Some(y)) => unsafe { (x(), y()) },
            _ => (0, 0),
        }
    }

    pub fn activate(s: &mut State) {
        if s.params_valid {
            // SDK newmouseparms {0, 0, 1}: no Windows acceleration while playing.
            let mut p: [c_int; 3] = [0, 0, 1];
            // SAFETY: SPI_SETMOUSE reads three ints from `pvParam`; `p` is a local `[c_int; 3]`.
            s.restore = unsafe { SystemParametersInfoA(SPI_SETMOUSE, 0, p.as_mut_ptr().cast(), 0) } != 0;
        }
    }

    pub fn deactivate(s: &mut State) {
        if s.restore {
            // SAFETY: SPI_SETMOUSE reads three ints from `pvParam`; `s.original` is a `[c_int; 3]`
            // filled by SPI_GETMOUSE in `startup`.
            unsafe { SystemParametersInfoA(SPI_SETMOUSE, 0, s.original.as_mut_ptr().cast(), 0) };
            s.restore = false;
        }
    }

    /// Cursor offset from the window centre, then re-centre (or SDL relative motion under
    /// `m_rawinput 1`, as the post-anniversary SDK does).
    pub fn delta() -> [f32; 2] {
        if raw_input() {
            return super::sdl::relative_delta();
        }
        let mut p = Point::default();
        // SAFETY: GetCursorPos writes a POINT (two LONGs, `#[repr(C)] Point`) to a valid pointer.
        if unsafe { GetCursorPos(&mut p) } == 0 {
            return [0.0; 2];
        }
        let (cx, cy) = center();
        let d = [(p.x - cx) as f32, (p.y - cy) as f32];
        if d != [0.0; 2] {
            // SAFETY: SetCursorPos takes plain integer screen coordinates; no pointers.
            unsafe { SetCursorPos(cx, cy) };
        }
        d
    }
}
