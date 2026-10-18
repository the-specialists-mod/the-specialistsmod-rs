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
