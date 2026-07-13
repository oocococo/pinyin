#[cfg(target_os = "macos")]
#[path = "mac.rs"]
mod imp;

#[cfg(target_os = "windows")]
#[path = "win.rs"]
mod imp;

pub use imp::*;
