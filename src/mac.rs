use std::{
    ffi::CString,
    os::raw::{c_char, c_int, c_uint},
};

use anyhow::Result;

pub const EVENT_KEYBOARD: i32 = 1;
pub const EVENT_MOUSE: i32 = 2;
pub const STATUS_PRESSED: i32 = 1;

pub const KEY_BACKSPACE: u32 = 51;
pub const KEY_ENTER: u32 = 36;
pub const KEY_RETURN: u32 = 76;
pub const KEY_ESCAPE: u32 = 53;
pub const KEY_ARROW_LEFT: u32 = 123;
pub const KEY_ARROW_RIGHT: u32 = 124;
pub const KEY_ARROW_DOWN: u32 = 125;
pub const KEY_ARROW_UP: u32 = 126;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct InputEvent {
    pub event_type: c_int,
    pub status: c_int,
    pub key_code: c_uint,
    pub buffer: [c_char; 64],
    pub buffer_len: usize,
}

impl InputEvent {
    pub fn text(&self) -> String {
        let len = self.buffer_len.min(self.buffer.len());
        let bytes = self.buffer[..len]
            .iter()
            .map(|ch| *ch as u8)
            .collect::<Vec<_>>();
        String::from_utf8_lossy(&bytes).into_owned()
    }
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
