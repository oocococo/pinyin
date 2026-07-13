use std::{
    ffi::CString,
    os::raw::{c_char, c_int, c_uint},
};

use anyhow::{Context, Result};

use crate::CandidateLayout;

pub const PLATFORM_NAME: &str = "macOS";

pub const EVENT_KEYBOARD: i32 = 1;
pub const EVENT_MOUSE: i32 = 2;
pub const EVENT_CONTEXT: i32 = 3;
pub const STATUS_PRESSED: i32 = 1;

pub const MODIFIER_COMMAND: u32 = 1 << 0;
pub const MODIFIER_CONTROL: u32 = 1 << 1;
pub const MODIFIER_OPTION: u32 = 1 << 2;
pub const MODIFIER_BUFFERED_REPLAY: u32 = 1 << 4;
pub const MODIFIER_REWRITE_ACTIVE: u32 = 1 << 5;

pub const KEY_BACKSPACE: u32 = 51;
pub const KEY_ENTER: u32 = 36;
pub const KEY_RETURN: u32 = 76;
pub const KEY_ESCAPE: u32 = 53;
pub const KEY_A: u32 = 0;
pub const KEY_C: u32 = 8;
pub const KEY_H: u32 = 4;
pub const KEY_V: u32 = 9;
pub const KEY_W: u32 = 13;
pub const KEY_X: u32 = 7;
pub const KEY_Z: u32 = 6;
pub const KEY_TAB: u32 = 48;
pub const KEY_SPACE: u32 = 49;
pub const KEY_GRAVE: u32 = 50;
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
    pub modifier_flags: c_uint,
    pub buffer: [c_char; 64],
    pub buffer_len: usize,
    pub source_buffer: [c_char; 256],
    pub source_buffer_len: usize,
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

    pub fn input_source_fingerprint(&self) -> String {
        let len = self.source_buffer_len.min(self.source_buffer.len());
        let bytes = self.source_buffer[..len]
            .iter()
            .map(|ch| *ch as u8)
            .collect::<Vec<_>>();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    pub fn has_command_modifier(&self) -> bool {
        let flags = self.modifier_flags;
        flags & MODIFIER_COMMAND != 0
    }

    pub fn has_control_modifier(&self) -> bool {
        let flags = self.modifier_flags;
        flags & MODIFIER_CONTROL != 0
    }

    pub fn has_text_modifier(&self) -> bool {
        let flags = self.modifier_flags;
        flags & (MODIFIER_COMMAND | MODIFIER_CONTROL | MODIFIER_OPTION) != 0
    }

    pub fn is_buffered_replay(&self) -> bool {
        let flags = self.modifier_flags;
        flags & MODIFIER_BUFFERED_REPLAY != 0
    }

    pub fn is_rewrite_active(&self) -> bool {
        let flags = self.modifier_flags;
        flags & MODIFIER_REWRITE_ACTIVE != 0
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

pub fn update_candidate_panel(
    preedit: &str,
    candidates: &[String],
    layout: CandidateLayout,
) -> Result<()> {
    let preedit = CString::new(preedit)?;
    let candidates = CString::new(candidates.join("\n"))?;
    let layout = match layout {
        CandidateLayout::Horizontal => 0,
        CandidateLayout::Vertical => 1,
    };
    unsafe { pal_pinyin_update_candidate_panel(preedit.as_ptr(), candidates.as_ptr(), layout) };
    Ok(())
}

pub fn hide_candidate_panel() {
    unsafe { pal_pinyin_hide_candidate_panel() };
}

pub fn begin_rewrite_transaction() {
    unsafe { pal_pinyin_begin_rewrite_transaction() };
}

pub fn cancel_rewrite_transaction() {
    unsafe { pal_pinyin_abort_rewrite_transaction() };
}

pub fn commit_rewrite_transaction(
    delete_chars: usize,
    replacement_text: &str,
    delay_ms: i32,
) -> Result<()> {
    let text = CString::new(replacement_text)
        .context("replacement text contains an interior NUL byte and cannot be injected")?;
    unsafe {
        pal_pinyin_commit_rewrite_transaction(delete_chars as c_uint, text.as_ptr(), delay_ms);
    }
    Ok(())
}

pub fn start_event_loop(callback: extern "C" fn(InputEvent) -> c_int) -> ! {
    unsafe { pal_pinyin_start_event_loop(callback) };
    unreachable!("macOS event loop returned unexpectedly")
}

unsafe extern "C" {
    fn pal_pinyin_is_accessibility_trusted(prompt: bool) -> bool;
    fn pal_pinyin_has_input_monitoring_access() -> bool;
    fn pal_pinyin_request_input_monitoring_access() -> bool;
    fn pal_pinyin_start_event_loop(callback: extern "C" fn(InputEvent) -> c_int);
    fn pal_pinyin_update_candidate_panel(
        preedit: *const c_char,
        candidates: *const c_char,
        layout: c_int,
    );
    fn pal_pinyin_hide_candidate_panel();
    fn pal_pinyin_begin_rewrite_transaction();
    fn pal_pinyin_abort_rewrite_transaction();
    fn pal_pinyin_commit_rewrite_transaction(
        delete_chars: c_uint,
        replacement_text: *const c_char,
        delay_ms: c_int,
    );
}
