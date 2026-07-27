//! Clipboard access.
//!
//! Small but full of traps: the clipboard is a global lock, and
//! `OpenClipboard` fails while another app holds it. So these return errors
//! instead of panicking — a failed copy must not take the editor down.

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::CF_UNICODETEXT;

/// Reads clipboard text. `None` when empty or not text.
pub fn get_text() -> Option<String> {
    unsafe {
        if OpenClipboard(None).is_err() {
            return None;
        }
        // Guard so early returns can not forget to close the clipboard.
        let guard = ClipboardGuard;
        let handle = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
        let ptr = GlobalLock(HGLOBAL(handle.0)) as *const u16;
        if ptr.is_null() {
            return None;
        }
        let mut len = 0usize;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
        let _ = GlobalUnlock(HGLOBAL(handle.0));
        drop(guard);
        Some(text)
    }
}

/// Writes text to the clipboard.
pub fn set_text(text: &str) -> Result<()> {
    unsafe {
        OpenClipboard(None)?;
        let guard = ClipboardGuard;
        EmptyClipboard()?;

        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = std::mem::size_of_val(wide.as_slice());
        let mem = GlobalAlloc(GMEM_MOVEABLE, bytes)?;
        let dst = GlobalLock(mem) as *mut u16;
        if dst.is_null() {
            let _ = GlobalFree(Some(mem));
            return Err(Error::from_thread());
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
        let _ = GlobalUnlock(mem);

        // After a successful SetClipboardData the clipboard owns the memory.
        SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(mem.0)))?;
        drop(guard);
        Ok(())
    }
}

struct ClipboardGuard;

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}
