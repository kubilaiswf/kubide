//! Putting a frame on a Wayland surface, with its alpha channel intact.
//!
//! softbuffer does this job on X11 and could on Wayland too, except that it
//! creates its `wl_shm` buffers as XRGB8888 — the X is "ignore" — and a
//! window whose alpha is ignored is an opaque window. Translucency is the
//! whole look, so the presenter is written here: a second connection on
//! winit's display, its own event queue, `wl_shm` from the registry, and
//! two ARGB8888 buffers in a memfd swapped front for back. The shape is
//! softbuffer's; the format is the point.

use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use raw_window_handle::{HasDisplayHandle, RawDisplayHandle, WaylandWindowHandle};
use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_buffer, wl_registry, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};

use crate::Error;

struct State;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut State,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        // Globals that appear after the round trip are winit's concern.
    }
}

impl Dispatch<wl_shm::WlShm, ()> for State {
    fn event(
        _: &mut State,
        _: &wl_shm::WlShm,
        _: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for State {
    fn event(
        _: &mut State,
        _: &wl_shm_pool::WlShmPool,
        _: wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, Arc<AtomicBool>> for State {
    fn event(
        _: &mut State,
        _: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        released: &Arc<AtomicBool>,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        // The compositor is done reading it; it may be drawn into again.
        if let wl_buffer::Event::Release = event {
            released.store(true, Ordering::SeqCst);
        }
    }
}

fn wayland_err<E: std::fmt::Display>(what: &str) -> impl FnOnce(E) -> Error + '_ {
    move |e| Error(format!("wayland {what}: {e}"))
}

/// One shared-memory buffer and the pool it lives in.
struct Buffer {
    fd: OwnedFd,
    map: *mut u8,
    map_len: usize,
    pool: wl_shm_pool::WlShmPool,
    pool_size: i32,
    buffer: wl_buffer::WlBuffer,
    width: i32,
    height: i32,
    released: Arc<AtomicBool>,
    qh: QueueHandle<State>,
}

fn pool_size(width: i32, height: i32) -> i32 {
    ((width * height * 4) as u32).next_power_of_two() as i32
}

/// An anonymous file of `len` bytes, for the compositor to map alongside us.
fn memfd(len: i32) -> crate::Result<OwnedFd> {
    let fd = unsafe { libc::memfd_create(c"kubide-frame".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(Error("memfd_create failed".into()));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    grow(&fd, len)?;
    Ok(fd)
}

fn grow(fd: &OwnedFd, len: i32) -> crate::Result<()> {
    use std::os::fd::AsRawFd;
    if unsafe { libc::ftruncate(fd.as_raw_fd(), len as libc::off_t) } != 0 {
        return Err(Error("ftruncate on the frame memfd failed".into()));
    }
    Ok(())
}

fn map(fd: &OwnedFd, len: usize) -> crate::Result<*mut u8> {
    use std::os::fd::AsRawFd;
    let p = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd.as_raw_fd(),
            0,
        )
    };
    if p == libc::MAP_FAILED {
        return Err(Error("mmap of the frame memfd failed".into()));
    }
    Ok(p.cast())
}

impl Buffer {
    fn new(shm: &wl_shm::WlShm, width: i32, height: i32, qh: &QueueHandle<State>) -> crate::Result<Self> {
        let pool_size = pool_size(width, height);
        let fd = memfd(pool_size)?;
        let map = map(&fd, pool_size as usize)?;
        let pool = shm.create_pool(fd.as_fd(), pool_size, qh, ());
        let released = Arc::new(AtomicBool::new(true));
        let buffer = pool.create_buffer(
            0,
            width,
            height,
            width * 4,
            wl_shm::Format::Argb8888,
            qh,
            released.clone(),
        );
        Ok(Self {
            fd,
            map,
            map_len: pool_size as usize,
            pool,
            pool_size,
            buffer,
            width,
            height,
            released,
            qh: qh.clone(),
        })
    }

    fn resize(&mut self, width: i32, height: i32) -> crate::Result<()> {
        if self.width == width && self.height == height {
            return Ok(());
        }
        self.buffer.destroy();
        let size = pool_size(width, height);
        if size > self.pool_size {
            grow(&self.fd, size)?;
            self.pool.resize(size);
            unsafe { libc::munmap(self.map.cast(), self.map_len) };
            self.map = map(&self.fd, size as usize)?;
            self.map_len = size as usize;
            self.pool_size = size;
        }
        self.buffer = self.pool.create_buffer(
            0,
            width,
            height,
            width * 4,
            wl_shm::Format::Argb8888,
            &self.qh,
            self.released.clone(),
        );
        self.width = width;
        self.height = height;
        Ok(())
    }

    fn pixels(&mut self) -> &mut [u32] {
        // The mapping is at least pool_size bytes and the buffer is the
        // first width*height*4 of them.
        unsafe {
            std::slice::from_raw_parts_mut(self.map.cast::<u32>(), (self.width * self.height) as usize)
        }
    }

    fn released(&self) -> bool {
        self.released.load(Ordering::SeqCst)
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        self.buffer.destroy();
        self.pool.destroy();
        unsafe { libc::munmap(self.map.cast(), self.map_len) };
    }
}

pub struct Presenter {
    // Dropped after the surface below, which borrows the connection; the
    // window handle after both, since the display belongs to it.
    conn: Option<Connection>,
    queue: EventQueue<State>,
    qh: QueueHandle<State>,
    shm: wl_shm::WlShm,
    surface: Option<wl_surface::WlSurface>,
    /// Front (attached last) and back (drawn into next).
    buffers: Option<(Buffer, Buffer)>,
    _window: Arc<winit::window::Window>,
}

impl Presenter {
    pub fn new(window: Arc<winit::window::Window>, handle: WaylandWindowHandle) -> crate::Result<Self> {
        let display = match window.display_handle().map_err(wayland_err("display handle"))?.as_raw() {
            RawDisplayHandle::Wayland(d) => d.display,
            _ => return Err(Error("a Wayland window on a display that is not Wayland".into())),
        };
        // Sharing winit's display rather than opening our own: the surface
        // is winit's, and a proxy for it only means something on the
        // connection that made it.
        let backend = unsafe { Backend::from_foreign_display(display.as_ptr().cast()) };
        let conn = Connection::from_backend(backend);
        let (globals, queue) = registry_queue_init::<State>(&conn).map_err(wayland_err("registry"))?;
        let qh = queue.handle();
        let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).map_err(wayland_err("wl_shm"))?;
        let id = unsafe { ObjectId::from_ptr(wl_surface::WlSurface::interface(), handle.surface.as_ptr().cast()) }
            .map_err(wayland_err("surface id"))?;
        let surface = wl_surface::WlSurface::from_id(&conn, id).map_err(wayland_err("surface proxy"))?;
        Ok(Self {
            conn: Some(conn),
            queue,
            qh,
            shm,
            surface: Some(surface),
            buffers: None,
            _window: window,
        })
    }

    /// Fills the back buffer through `write`, then attaches and commits it.
    /// Waits for the compositor to release the buffer first when it has not
    /// yet — the one case where drawing has to block.
    pub fn present(&mut self, width: i32, height: i32, write: impl FnOnce(&mut [u32])) -> crate::Result<()> {
        let _ = self.queue.dispatch_pending(&mut State);
        if self.buffers.is_none() {
            self.buffers = Some((
                Buffer::new(&self.shm, width, height, &self.qh)?,
                Buffer::new(&self.shm, width, height, &self.qh)?,
            ));
        }
        let (front, back) = self.buffers.as_mut().expect("just allocated");
        while !back.released() {
            self.queue.blocking_dispatch(&mut State).map_err(wayland_err("dispatch"))?;
        }
        back.resize(width, height)?;
        write(back.pixels());

        std::mem::swap(front, back);
        let surface = self.surface.as_ref().expect("dropped only in Drop");
        front.released.store(false, Ordering::SeqCst);
        surface.attach(Some(&front.buffer), 0, 0);
        // damage_buffer arrived in version 4; older servers take the whole
        // surface in surface coordinates, which i32::MAX covers.
        if surface.version() >= 4 {
            surface.damage_buffer(0, 0, width, height);
        } else {
            surface.damage(0, 0, i32::MAX, i32::MAX);
        }
        surface.commit();
        let _ = self.queue.flush();
        Ok(())
    }
}

impl Drop for Presenter {
    fn drop(&mut self) {
        self.buffers = None;
        self.surface = None;
        self.conn = None;
    }
}
