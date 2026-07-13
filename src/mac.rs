use std::{
    ffi::CString,
    os::raw::{c_char, c_int, c_uint},
};

use anyhow::Result;

use super::InputEvent;

pub const PLATFORM_NAME: &str = "macOS";
pub const KEY_BACKSPACE: u32 = 51;
pub const KEY_ENTER: u32 = 36;
pub const KEY_RETURN: u32 = 76;
pub const KEY_ESCAPE: u32 = 53;
pub const KEY_ARROW_LEFT: u32 = 123;
pub const KEY_ARROW_RIGHT: u32 = 124;
pub const KEY_ARROW_DOWN: u32 = 125;
pub const KEY_ARROW_UP: u32 = 126;

pub fn is_backspace_key(key_code: u32) -> bool {
    key_code == KEY_BACKSPACE
}

pub fn is_buffer_boundary_key(key_code: u32) -> bool {
    matches!(
        key_code,
        KEY_ENTER
            | KEY_RETURN
            | KEY_ESCAPE
            | KEY_ARROW_LEFT
            | KEY_ARROW_RIGHT
            | KEY_ARROW_DOWN
            | KEY_ARROW_UP
    )
}

pub fn is_accessibility_trusted(prompt: bool) -> bool {
    unsafe { pal_pinyin_is_accessibility_trusted(prompt) }
}

pub fn has_input_monitoring_access() -> bool {
    unsafe { pal_pinyin_has_input_monitoring_access() }
}

pub fn request_input_monitoring_access() -> bool {
    unsafe { pal_pinyin_request_input_monitoring_access() }
}

pub fn inject_backspaces(count: usize, delay_ms: i32) {
    unsafe { pal_pinyin_inject_backspaces(count as c_uint, delay_ms) };
}

pub fn inject_string(text: &str, delay_ms: i32) -> Result<()> {
    let text = CString::new(text)?;
    unsafe { pal_pinyin_inject_string(text.as_ptr(), delay_ms) };
    Ok(())
}

pub fn start_event_loop(callback: extern "C" fn(InputEvent)) -> ! {
    unsafe { pal_pinyin_start_event_loop(callback) };
    unreachable!("macOS event loop returned unexpectedly")
}

unsafe extern "C" {
    fn pal_pinyin_is_accessibility_trusted(prompt: bool) -> bool;
    fn pal_pinyin_has_input_monitoring_access() -> bool;
    fn pal_pinyin_request_input_monitoring_access() -> bool;
    fn pal_pinyin_start_event_loop(callback: extern "C" fn(InputEvent));
    fn pal_pinyin_inject_backspaces(count: c_uint, delay_ms: c_int);
    fn pal_pinyin_inject_string(string: *const c_char, delay_ms: c_int);
}
