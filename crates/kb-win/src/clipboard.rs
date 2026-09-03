//! Clipboard access.
//!
//! Small but full of traps: the clipboard is a global lock, and
//! `OpenClipboard` fails while another app holds it. So these return errors
//! instead of panicking — a failed copy must not take the editor down.

#[cfg(windows)]
mod imp {
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
    pub fn set_text(text: &str) -> crate::Result<()> {
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
                return Err(Error::from_thread().into());
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
}

/// On X11 the clipboard is not a place but a promise: whoever copied serves
/// the text until someone else copies. arboard keeps that promise as long as
/// its `Clipboard` lives, so there is exactly one, for the life of the
/// process. On Wayland the data-control protocol takes over and the object
/// costs nothing to keep.
#[cfg(not(windows))]
mod imp {
    use std::cell::RefCell;

    thread_local! {
        static CLIP: RefCell<Option<arboard::Clipboard>> = const { RefCell::new(None) };
    }

    fn with<R>(f: impl FnOnce(&mut arboard::Clipboard) -> Option<R>) -> Option<R> {
        CLIP.with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot.is_none() {
                *slot = arboard::Clipboard::new().ok();
            }
            slot.as_mut().and_then(f)
        })
    }

    /// Reads clipboard text. `None` when empty or not text.
    pub fn get_text() -> Option<String> {
        with(|c| c.get_text().ok())
    }

    /// Writes text to the clipboard.
    pub fn set_text(text: &str) -> crate::Result<()> {
        with(|c| c.set_text(text.to_string()).ok())
            .ok_or_else(|| crate::Error("the clipboard is not available".into()))
    }
}

pub use imp::{get_text, set_text};
