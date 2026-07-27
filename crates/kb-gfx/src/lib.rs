//! Compositing layer — the only reason kubide can be translucent.
//!
//! A flip-model swapchain bound straight to an HWND cannot be transparent:
//! even with DWM's Mica/Acrylic material set, the opaque swapchain paints over
//! it. So the chain is built like this:
//!
//! ```text
//! D3D11 (BGRA_SUPPORT)
//!   └─ CreateSwapChainForComposition   (PREMULTIPLIED alpha, STRETCH, FLIP_SEQUENTIAL)
//!        └─ IDCompositionVisual
//!             └─ IDCompositionTarget → HWND
//! D2D device context ──draws──> swapchain surface
//! ```
//!
//! Zed, Firefox and Chromium take the same route on Windows.
//!
//! Every color drawn must be premultiplied. Otherwise edges darken or turn
//! milky, which looks like a blur bug and sends you hunting through DWM.
//! Check with [`Renderer::debug_alpha_ramp`] when in doubt.

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

pub use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F as Color;
pub use windows::Win32::Graphics::Direct2D::ID2D1DeviceContext as DrawContext;

/// Builds a premultiplied color. Use this instead of `D2D1_COLOR_F` directly:
/// D2D takes straight alpha but the target surface is premultiplied, and
/// mixing the two is the easiest mistake to make.
pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Color {
    Color { r, g, b, a }
}

pub struct Renderer {
    swap: IDXGISwapChain1,
    dc: ID2D1DeviceContext,
    // Dropping any of these tears down the visual tree; kept only to live.
    _comp_device: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
}

impl Renderer {
    pub fn new(hwnd: HWND, width: u32, height: u32) -> Result<Self> {
        unsafe {
            // Without BGRA_SUPPORT, D2D can't bind to this device.
            let mut d3d: Option<ID3D11Device> = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d),
                None,
                None,
            )?;
            let d3d = d3d.ok_or_else(Error::from_thread)?;

            let dxgi_device: IDXGIDevice = d3d.cast()?;
            let adapter = dxgi_device.GetAdapter()?;
            let factory: IDXGIFactory2 = adapter.GetParent()?;

            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width.max(1),
                Height: height.max(1),
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                // Composition swapchains only support STRETCH.
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                // All of the transparency lives in this field.
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                ..Default::default()
            };
            let swap = factory.CreateSwapChainForComposition(&d3d, &desc, None)?;

            let comp_device: IDCompositionDevice = DCompositionCreateDevice(&dxgi_device)?;
            let target = comp_device.CreateTargetForHwnd(hwnd, true)?;
            let visual = comp_device.CreateVisual()?;
            visual.SetContent(&swap)?;
            target.SetRoot(&visual)?;
            comp_device.Commit()?;

            let d2d: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let d2d_device = d2d.CreateDevice(&dxgi_device)?;
            let dc = d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;

            let me = Self {
                swap,
                dc,
                _comp_device: comp_device,
                _target: target,
                _visual: visual,
            };
            me.bind_backbuffer()?;
            Ok(me)
        }
    }

    fn bind_backbuffer(&self) -> Result<()> {
        unsafe {
            let surface: IDXGISurface = self.swap.GetBuffer(0)?;
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
                colorContext: std::mem::ManuallyDrop::new(None),
            };
            let bitmap = self.dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
            self.dc.SetTarget(&bitmap);
            Ok(())
        }
    }

    pub fn resize(&self, width: u32, height: u32) -> Result<()> {
        unsafe {
            // ResizeBuffers fails while the backbuffer is still referenced.
            self.dc.SetTarget(None);
            self.swap.ResizeBuffers(
                0,
                width.max(1),
                height.max(1),
                DXGI_FORMAT_UNKNOWN,
                DXGI_SWAP_CHAIN_FLAG(0),
            )?;
            self.bind_backbuffer()
        }
    }

    /// Starts a frame and clears to fully transparent — DWM's material shows
    /// through here.
    pub fn begin(&self) -> &DrawContext {
        unsafe {
            self.dc.BeginDraw();
            self.dc.Clear(Some(&rgba(0.0, 0.0, 0.0, 0.0)));
            &self.dc
        }
    }

    pub fn end(&self) -> Result<()> {
        unsafe {
            self.dc.EndDraw(None, None)?;
            // 1 = vsync. A translucent window is locked to the compositor
            // anyway, so asking for tearing buys nothing.
            self.swap.Present(1, DXGI_PRESENT(0)).ok()
        }
    }

    pub fn size(&self) -> (f32, f32) {
        let s = unsafe { self.dc.GetSize() };
        (s.width, s.height)
    }

    pub fn context(&self) -> &DrawContext {
        &self.dc
    }

    /// Visual check for premultiplied alpha: draws a ramp top-left. If it
    /// doesn't fade evenly, or darkens, the blending is wrong.
    pub fn debug_alpha_ramp(&self) -> Result<()> {
        unsafe {
            for i in 0..10 {
                let a = i as f32 / 9.0;
                let brush = self.dc.CreateSolidColorBrush(&rgba(1.0, 1.0, 1.0, a), None)?;
                let x = 8.0 + i as f32 * 22.0;
                self.dc.FillRectangle(
                    &D2D_RECT_F { left: x, top: 8.0, right: x + 20.0, bottom: 28.0 },
                    &brush,
                );
            }
            Ok(())
        }
    }
}
