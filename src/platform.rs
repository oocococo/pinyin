use std::os::raw::{c_char, c_int, c_uint};

pub const EVENT_KEYBOARD: i32 = 1;
pub const EVENT_MOUSE: i32 = 2;
pub const STATUS_PRESSED: i32 = 1;

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

#[cfg(target_os = "macos")]
#[path = "mac.rs"]
mod imp;

#[cfg(target_os = "windows")]
#[path = "win.rs"]
mod imp;

pub use imp::*;
