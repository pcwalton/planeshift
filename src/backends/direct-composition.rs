// planeshift/src/backends/direct-composition.rs

use euclid::Rect;
use image::{ConvertBuffer, RgbaImage};
use mozangle::egl::ffi::types::{EGLClientBuffer, EGLConfig, EGLContext, EGLDisplay, EGLSurface};
use mozangle::egl::ffi::{D3D11_DEVICE_ANGLE, EGLDeviceEXT};
use mozangle::egl;
use std::cell::RefCell;
use std::ffi::c_void;
use std::mem;
use std::ptr;
use std::slice;
use std::sync::mpsc::{self, Sender};
use std::thread::Builder as ThreadBuilder;
use winapi::Interface;
use winapi::shared::dxgi1_2::{DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_SCALING_STRETCH};
use winapi::shared::dxgi1_2::{DXGI_SWAP_CHAIN_DESC1, IDXGIFactory2, IDXGISwapChain1};
use winapi::shared::dxgi::{DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, IDXGIAdapter, IDXGIDevice};
use winapi::shared::dxgiformat::DXGI_FORMAT_B8G8R8A8_UNORM;
use winapi::shared::dxgitype::{DXGI_SAMPLE_DESC, DXGI_USAGE_RENDER_TARGET_OUTPUT};
use winapi::shared::minwindef::{DWORD, FALSE, LPARAM, LRESULT, TRUE, UINT, WORD, WPARAM};
use winapi::shared::ntdef::LPCSTR;
use winapi::shared::windef::{HBRUSH, HWND, RECT};
use winapi::shared::winerror::{self, S_OK};
use winapi::um::d3d11::{self, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, ID3D11Device};
use winapi::um::d3d11::{ID3D11Texture2D};
use winapi::um::d3dcommon::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use winapi::um::d3dcommon::{D3D_FEATURE_LEVEL_10_1};
use winapi::um::dcomp::{self, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual};
use winapi::um::handleapi;
use winapi::um::libloaderapi;
use winapi::um::unknwnbase::IUnknown;
use winapi::um::winbase;
use winapi::um::wingdi::BITMAPINFOHEADER;
use winapi::um::winuser::{self, INPUT, KEYBDINPUT, MSG, WNDCLASSEXA};

#[cfg(feature = "enable-winit")]
use winit::Window;
#[cfg(all(feature = "enable-winit", target_family = "windows"))]
use winit::os::windows::WindowExt;

use crate::{Connection, ConnectionError, GLAPI, GLContextLayerBinding, LayerContainerInfo};
use crate::{LayerGeometryInfo, LayerId, LayerMap, LayerSurfaceInfo, LayerTreeInfo, Promise};
use crate::{SurfaceOptions};
use self::com::ComPtr;

pub struct Backend {
    native_component: LayerMap<NativeInfo>,

    d3d_device: ComPtr<ID3D11Device>,
    dcomp_device: ComPtr<IDCompositionDevice>,
    dxgi_factory: ComPtr<IDXGIFactory2>,

    egl_device: EGLDeviceEXT,
    egl_display: EGLDisplay,

    screenshot_window: Option<HWND>,

    #[cfg(feature = "enable-winit")]
    window: Option<Window>,
}

impl crate::Backend for Backend {
    type NativeConnection = *mut ID3D11Device;
    type GLContext = GLContext;
    type NativeGLContext = EGLContext;
    type Host = HWND;

    // FIXME(pcwalton): We should make sure the `ID3D11Device` pointer is valid!
    // TODO(pcwalton): Don't panic on error.
    fn new(connection: Connection<Self::NativeConnection>) -> Result<Backend, ConnectionError> {
        unsafe {
            // Unpack the connection.
            let (d3d_device, window) = unpack_connection(connection);
            assert!(!d3d_device.is_null());

            // Create the DirectComposition device.
            let d3d_device = ComPtr(d3d_device);
            let mut dcomp_device: ComPtr<IDCompositionDevice> = ComPtr::null();
            let result = dcomp::DCompositionCreateDevice(
                d3d_device.query_interface().unwrap(),
                &IDCompositionDevice::uuidof(),
                &mut *dcomp_device as *mut *mut _ as *mut *mut c_void);
            assert_eq!(result, S_OK);

            // Grab the adapter from the D3D11 device.
            let dxgi_device: ComPtr<IDXGIDevice> = ComPtr(d3d_device.query_interface().unwrap());
            let mut adapter: ComPtr<IDXGIAdapter> = ComPtr::null();
            let result = (**dxgi_device).GetAdapter(&mut *adapter);
            assert_eq!(result, S_OK);

            // Create the DXGI factory. This will be used for creating swap chains.
            let mut dxgi_factory: ComPtr<IDXGIFactory2> = ComPtr::null();
            let result = (**adapter).GetParent(&IDXGIFactory2::uuidof(),
                                               &mut *dxgi_factory as *mut *mut _ as
                                               *mut *mut c_void);
            assert_eq!(result, S_OK);

            // Create the ANGLE EGL device.
            let egl_device = egl::ffi::eglCreateDeviceANGLE(D3D11_DEVICE_ANGLE,
                                                            *d3d_device as *mut c_void,
                                                            ptr::null());
            assert!(!egl_device.is_null());

            // Open the ANGLE EGL display.
            let attributes = [
                egl::ffi::EXPERIMENTAL_PRESENT_PATH_ANGLE as i32,
                    egl::ffi::EXPERIMENTAL_PRESENT_PATH_FAST_ANGLE as i32,
                egl::ffi::NONE as i32,  egl::ffi::NONE as i32,
            ];
            let egl_display = egl::ffi::GetPlatformDisplayEXT(egl::ffi::PLATFORM_DEVICE_EXT,
                                                              egl_device,
                                                              attributes.as_ptr());
            assert!(!egl_display.is_null());

            // Initialize EGL via ANGLE.
            let result = egl::ffi::Initialize(egl_display, ptr::null_mut(), ptr::null_mut());
            assert_eq!(result, egl::ffi::TRUE);

            // Load GL functions.
            gl::load_with(egl::get_proc_address);

            Ok(Backend {
                native_component: LayerMap::new(),

                d3d_device,
                dcomp_device,
                dxgi_factory,

                egl_device,
                egl_display,

                screenshot_window: None,

                #[cfg(feature = "enable-winit")]
                window,
            })
        }
    }

    fn create_gl_context(&mut self, options: SurfaceOptions) -> Result<GLContext, ()> {
        unsafe {
            // Enumerate the EGL pixel configurations for ANGLE.
            let (mut configs, mut num_configs) = ([ptr::null(); 64], 0);
            let depth_size = if options.contains(SurfaceOptions::DEPTH) { 16 } else { 0 };
            let stencil_size = if options.contains(SurfaceOptions::STENCIL) { 8 } else { 0 };
            let attributes = [
                egl::ffi::SURFACE_TYPE as i32,      egl::ffi::WINDOW_BIT as i32,
                egl::ffi::RENDERABLE_TYPE as i32,   egl::ffi::OPENGL_ES3_BIT as i32,
                egl::ffi::RED_SIZE as i32,          8,
                egl::ffi::GREEN_SIZE as i32,        8,
                egl::ffi::BLUE_SIZE as i32,         8,
                egl::ffi::ALPHA_SIZE as i32,        8,
                egl::ffi::DEPTH_SIZE as i32,        depth_size,
                egl::ffi::STENCIL_SIZE as i32,      stencil_size,
                egl::ffi::NONE as i32,              egl::ffi::NONE as i32,
            ];
            let result = egl::ffi::ChooseConfig(self.egl_display,
                                                attributes.as_ptr(),
                                                configs.as_mut_ptr(),
                                                configs.len() as _,
                                                &mut num_configs);
            if result != egl::ffi::TRUE {
                return Err(())
            }

            // Choose an EGL pixel configuration for ANGLE.
            //
            // FIXME(pcwalton): Do a better job of making sure we get the right context via
            // `eglGetConfigAttrib()`.
            let config = configs[0];

            // Create an EGL context via ANGLE.
            let attributes = [
                egl::ffi::CONTEXT_CLIENT_VERSION as i32,    3,
                egl::ffi::NONE as i32,                      egl::ffi::NONE as i32,
            ];
            let egl_context = egl::ffi::CreateContext(self.egl_display,
                                                      config,
                                                      egl::ffi::NO_CONTEXT,
                                                      attributes.as_ptr());
            self.wrap_gl_context(egl_context)
        }
    }

    unsafe fn wrap_gl_context(&mut self, egl_context: EGLContext) -> Result<GLContext, ()> {
        if egl_context.is_null() {
            return Err(())
        }

        let mut egl_config_index = 0;
        let result = egl::ffi::QueryContext(self.egl_display,
                                            egl_context,
                                            egl::ffi::CONFIG_ID as i32,
                                            &mut egl_config_index);
        if result != egl::ffi::TRUE {
            return Err(())
        }

        let (mut configs, mut num_configs) = ([ptr::null(); 64], 0);
        let result = egl::ffi::GetConfigs(self.egl_display,
                                          configs.as_mut_ptr(),
                                          configs.len() as _,
                                          &mut num_configs);
        if result != egl::ffi::TRUE {
            return Err(())
        }

        assert!(egl_config_index < num_configs);
        let egl_config = configs[egl_config_index as usize];

        Ok(GLContext {
            egl_context,
            egl_config,
            egl_display: self.egl_display,
        })
    }

    fn gl_api(&self) -> GLAPI {
        GLAPI::GLES
    }

    fn begin_transaction(&self) {}

    fn end_transaction(&mut self,
                       promise: &Promise<()>,
                       _: &LayerMap<LayerTreeInfo>,
                       _: &LayerMap<LayerContainerInfo>,
                       _: &LayerMap<LayerGeometryInfo>,
                       _: &LayerMap<LayerSurfaceInfo>) {
        unsafe {
            let result = (**self.dcomp_device).Commit();
            assert_eq!(result, S_OK);

            // FIXME(pcwalton): Is this right?
            promise.resolve(());
        }
    }

    fn add_container_layer(&mut self, new_layer: LayerId) {
        unsafe {
            let mut visual = ComPtr::null();
            let result = (**self.dcomp_device).CreateVisual(&mut *visual);
            assert_eq!(result, S_OK);

            self.native_component.add(new_layer, NativeInfo {
                visual,
                surface: None,
                target: None,
            });
        }
    }

    fn add_surface_layer(&mut self, new_layer: LayerId) {
        self.add_container_layer(new_layer);
    }

    fn delete_layer(&mut self, layer: LayerId) {
        self.native_component.remove_if_present(layer);
    }

    fn insert_before(&mut self,
                     parent: LayerId,
                     new_child: LayerId,
                     reference: Option<LayerId>,
                     _: &LayerMap<LayerTreeInfo>,
                     _: &LayerMap<LayerContainerInfo>,
                     _: &LayerMap<LayerGeometryInfo>) {
        unsafe {
            let parent_visual = &self.native_component[parent].visual;
            let new_child_visual = &self.native_component[new_child].visual;
            let reference_visual = match reference {
                None => ptr::null_mut(),
                Some(reference) => *self.native_component[reference].visual,
            };
            let result = (***parent_visual).AddVisual(**new_child_visual, FALSE, reference_visual);
            assert_eq!(result, S_OK);
        }
    }

    fn remove_from_superlayer(&mut self,
                              layer: LayerId,
                              parent: LayerId,
                              _: &LayerMap<LayerTreeInfo>,
                              _: &LayerMap<LayerGeometryInfo>) {
        unsafe {
            let parent_visual = match self.native_component.get(parent) {
                None => return,
                Some(ref parent_native_component) => &parent_native_component.visual,
            };
            let layer_visual = &self.native_component[layer].visual;
            let result = (***parent_visual).RemoveVisual(**layer_visual);
            assert_eq!(result, S_OK);
        }
    }

    unsafe fn host_layer(&mut self,
                         layer: LayerId,
                         host: HWND,
                         _: &LayerMap<LayerTreeInfo>,
                         _: &LayerMap<LayerContainerInfo>,
                         _: &LayerMap<LayerGeometryInfo>) {
        let native_info = &mut self.native_component[layer];
        assert!(native_info.target.is_none());

        let mut target = ComPtr::null();
        let result = (**self.dcomp_device).CreateTargetForHwnd(host, TRUE, &mut *target);
        assert_eq!(result, S_OK);

        (**target).SetRoot(*native_info.visual);

        native_info.target = Some(Target {
            directcomposition_target: target,
            window: host,
        });
    }

    fn unhost_layer(&mut self, layer: LayerId) {
        self.native_component[layer].target = None;
    }

    fn set_layer_bounds(&mut self,
                        layer: LayerId,
                        _: &Rect<f32>,
                        _: &LayerMap<LayerTreeInfo>,
                        _: &LayerMap<LayerContainerInfo>,
                        geometry_component: &LayerMap<LayerGeometryInfo>) {
        unsafe {
            let new_bounds = match geometry_component.get(layer) {
                None => return,
                Some(geometry_component) => geometry_component.bounds,
            };

            let visual = &self.native_component[layer].visual;
            (***visual).SetOffsetX_1(new_bounds.origin.x);
            (***visual).SetOffsetY_1(new_bounds.origin.y);
        }
    }

    fn set_layer_surface_options(&mut self, _: LayerId, _: &LayerMap<LayerSurfaceInfo>) {}

    fn bind_layer_to_gl_context(&mut self,
                                layer: LayerId,
                                context: &mut GLContext,
                                geometry_component: &LayerMap<LayerGeometryInfo>,
                                _: &LayerMap<LayerSurfaceInfo>)
                                -> Result<GLContextLayerBinding, ()> {
        let native_component = &mut self.native_component[layer];
        let bounds = &geometry_component[layer].bounds;

        unsafe {
            // Create the surface if necessary.
            if native_component.surface.is_none() {
                // Build the DXGI swap chain.
                let size = bounds.size.round().to_u32();
                let descriptor = DXGI_SWAP_CHAIN_DESC1 {
                    Width: size.width,
                    Height: size.height,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    Stereo: FALSE,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                    BufferCount: 2,
                    Scaling: DXGI_SCALING_STRETCH,
                    SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                    AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                    Flags: 0,
                };
                let mut dxgi_swap_chain: ComPtr<IDXGISwapChain1> = ComPtr::null();
                let result = (**self.dxgi_factory).CreateSwapChainForComposition(
                    *self.d3d_device as *mut IUnknown,
                    &descriptor,
                    ptr::null_mut(),
                    &mut *dxgi_swap_chain);
                if !winerror::SUCCEEDED(result) {
                    return Err(())
                }

                // Create the D3D11 texture.
                let mut d3d_texture: ComPtr<ID3D11Texture2D> = ComPtr::null();
                let result = (**dxgi_swap_chain).GetBuffer(0,
                                                           &ID3D11Texture2D::uuidof(),
                                                           &mut *d3d_texture as *mut *mut _ as
                                                           *mut *mut c_void);
                if !winerror::SUCCEEDED(result) {
                    return Err(())
                }

                // Build the EGL surface.
                let attributes = [
                    egl::ffi::WIDTH as i32,     size.width as i32,
                    egl::ffi::HEIGHT as i32,    size.height as i32,
                    egl::ffi::FLEXIBLE_SURFACE_COMPATIBILITY_SUPPORTED_ANGLE as i32,
                        egl::ffi::TRUE as i32,
                    egl::ffi::NONE as i32,      egl::ffi::NONE as i32,
                ];
                let egl_surface =
                    egl::ffi::CreatePbufferFromClientBuffer(self.egl_display,
                                                            egl::ffi::D3D_TEXTURE_ANGLE,
                                                            *d3d_texture as EGLClientBuffer,
                                                            context.egl_config,
                                                            attributes.as_ptr());

                native_component.surface = Some(Surface {
                    dxgi_swap_chain,
                    d3d_texture,
                    egl_surface,
                });
            }

            let surface = native_component.surface.as_ref().unwrap();
            let result = (**native_component.visual).SetContent(*surface.dxgi_swap_chain as
                                                                *mut IUnknown);
            if !winerror::SUCCEEDED(result) {
                return Err(())
            }

            let result = egl::ffi::MakeCurrent(self.egl_display,
                                               surface.egl_surface,
                                               surface.egl_surface,
                                               context.egl_context);
            if result != egl::ffi::TRUE {
                return Err(())
            }

            Ok(GLContextLayerBinding {
                layer,
                framebuffer: 0,
            })
        }
    }

    fn present_gl_context(&mut self,
                          binding: GLContextLayerBinding,
                          _: &Rect<f32>,
                          _: &LayerMap<LayerTreeInfo>,
                          _: &LayerMap<LayerGeometryInfo>)
                          -> Result<(), ()> {
        // TODO(pcwalton): Partial presents?
        unsafe {
            let surface = self.native_component[binding.layer].surface.as_ref().unwrap();
            if winerror::SUCCEEDED((**surface.dxgi_swap_chain).Present(0, 0)) {
                Ok(())
            } else {
                Err(())
            }
        }
    }

    // Screenshots

    fn screenshot_hosted_layer(&mut self,
                               layer: LayerId,
                               transaction_promise: &Promise<()>,
                               _: &LayerMap<LayerTreeInfo>,
                               _: &LayerMap<LayerContainerInfo>,
                               _: &LayerMap<LayerGeometryInfo>,
                               _: &LayerMap<LayerSurfaceInfo>)
                               -> Promise<RgbaImage> {
        self.create_screenshot_window_if_necessary();

        let screenshot_window = self.screenshot_window.unwrap();

        let window: HWND = self.native_component[layer].target.as_ref().unwrap().window;
        let mut window_rect = RECT { left: 0, right: 0, top: 0, bottom: 0, };
        unsafe {
            assert_ne!(winuser::GetWindowRect(window, &mut window_rect), FALSE);

            // The rectangle returned by `GetWindowRect` includes window decorations. Remove them.
            let mut adjusted_rect = RECT { left: 0, right: 0, top: 0, bottom: 0, };
            let style = winuser::GetWindowLongA(window, winuser::GWL_STYLE) as DWORD;
            let ex_style = winuser::GetWindowLongA(window, winuser::GWL_EXSTYLE) as DWORD;
            let has_menu = if winuser::GetMenu(window).is_null() { FALSE } else { TRUE };
            winuser::AdjustWindowRectEx(&mut adjusted_rect, style, has_menu, ex_style);

            window_rect = RECT {
                left: window_rect.left - adjusted_rect.left,
                right: window_rect.right - adjusted_rect.right,
                top: window_rect.top - adjusted_rect.top,
                bottom: window_rect.bottom - adjusted_rect.bottom,
            }
        }

        let result_promise = Promise::new();
        let request = RefCell::new(Some(Box::new(ScreenshotRequest {
            promise: result_promise.clone(),
            window_rect,
        })));

        transaction_promise.then(Box::new(move |()| {
            unsafe {
                // Try to bring the window to the front. This is best-effort.
                winuser::SetForegroundWindow(window);

                // Wake up our screenshot thread.
                let request: Box<ScreenshotRequest> = request.replace(None).unwrap();
                let request_addr = &*request as *const _ as WPARAM;
                mem::forget(request);
                winuser::PostMessageA(screenshot_window, winuser::WM_USER, request_addr, 0);

                // Send a Print Screen key to capture the desktop.
                let mut inputs = [
                    INPUT { type_: winuser::INPUT_KEYBOARD, u: mem::zeroed(), },
                    INPUT { type_: winuser::INPUT_KEYBOARD, u: mem::zeroed(), },
                ];
                *inputs[0].u.ki_mut() = KEYBDINPUT {
                    wVk: winuser::VK_SNAPSHOT as WORD,
                    wScan: 0,
                    dwFlags: 0,
                    time: 0,
                    dwExtraInfo: 0,
                };
                *inputs[1].u.ki_mut() = KEYBDINPUT {
                    wVk: winuser::VK_SNAPSHOT as WORD,
                    wScan: 0,
                    dwFlags: winuser::KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: 0,
                };

                let events_sent = winuser::SendInput(inputs.len() as UINT,
                                                     inputs.as_mut_ptr(),
                                                     mem::size_of::<INPUT>() as _);
                assert_eq!(events_sent, inputs.len() as UINT);
            }
        }));

        result_promise
    }

    // `winit` integration

    #[cfg(feature = "enable-winit")]
    fn window(&self) -> Option<&Window> {
        self.window.as_ref()
    }

    #[cfg(feature = "enable-winit")]
    fn host_layer_in_window(&mut self,
                            layer: LayerId,
                            tree_component: &LayerMap<LayerTreeInfo>,
                            container_component: &LayerMap<LayerContainerInfo>,
                            geometry_component: &LayerMap<LayerGeometryInfo>)
                            -> Result<(), ()> {
        unsafe {
            self.host_layer(layer,
                            self.window.as_ref().unwrap().get_hwnd() as HWND,
                            tree_component,
                            container_component,
                            geometry_component);
            Ok(())
        }
    }
}

impl Backend {
    fn create_screenshot_window_if_necessary(&mut self) {
        if self.screenshot_window.is_some() {
            return
        }

        let (window_sender, window_receiver) = mpsc::channel();
        ThreadBuilder::new().name("PlaneshiftScreenshotThread".to_string()).spawn(move || {
            screenshot_thread(window_sender)
        }).unwrap();
        self.screenshot_window = Some(window_receiver.recv().unwrap().0);
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        unsafe {
            egl::ffi::eglReleaseDeviceANGLE(self.egl_device);
        }
    }
}

struct NativeInfo {
    visual: ComPtr<IDCompositionVisual>,
    target: Option<Target>,
    surface: Option<Surface>,
}

struct Target {
    #[allow(dead_code)]
    directcomposition_target: ComPtr<IDCompositionTarget>,
    window: HWND,
}

struct Surface {
    dxgi_swap_chain: ComPtr<IDXGISwapChain1>,
    #[allow(dead_code)]
    d3d_texture: ComPtr<ID3D11Texture2D>,
    egl_surface: EGLSurface,
}

pub struct GLContext {
    egl_context: EGLContext,
    egl_config: EGLConfig,
    egl_display: EGLDisplay,
}

impl Drop for GLContext {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            let result = egl::ffi::DestroyContext(self.egl_display, self.egl_context);
            assert_eq!(result, egl::ffi::TRUE);
        }
    }
}

#[cfg(not(feature = "enable-winit"))]
type MaybeWindow = ();
#[cfg(feature = "enable-winit")]
type MaybeWindow = Window;

struct NativeWindow(HWND);

unsafe impl Send for NativeWindow {}

struct ScreenshotRequest {
    promise: Promise<RgbaImage>,
    window_rect: RECT,
}

fn unpack_connection(connection: Connection<*mut ID3D11Device>)
                     -> (*mut ID3D11Device, Option<MaybeWindow>) {
    match connection {
        Connection::Native(d3d_device) => (d3d_device, None),
        #[cfg(feature = "enable-winit")]
        Connection::Winit(window_builder, event_loop) => {
            let window = window_builder.build(event_loop).unwrap();
            unsafe {
                let mut d3d_device: ComPtr<ID3D11Device> = ComPtr::null();
                let result = d3d11::D3D11CreateDevice(ptr::null_mut(),
                                                      D3D_DRIVER_TYPE_HARDWARE,
                                                      ptr::null_mut(),
                                                      D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                                                      ptr::null_mut(),
                                                      0,
                                                      D3D11_SDK_VERSION,
                                                      &mut *d3d_device,
                                                      &mut 0,
                                                      ptr::null_mut());
                assert_eq!(result, S_OK);
                assert!(!d3d_device.is_null());

                // Need at least D3D 10.1 for ES 3.
                if (**d3d_device).GetFeatureLevel() >= D3D_FEATURE_LEVEL_10_1 {
                    return (d3d_device.copy(), Some(window))
                }

                // TODO(pcwalton): Allow the user to opt-out of the WARP fallback.
                d3d_device = ComPtr::null();
                let result = d3d11::D3D11CreateDevice(ptr::null_mut(),
                                                      D3D_DRIVER_TYPE_WARP,
                                                      ptr::null_mut(),
                                                      D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                                                      ptr::null_mut(),
                                                      0,
                                                      D3D11_SDK_VERSION,
                                                      &mut *d3d_device,
                                                      &mut 0,
                                                      ptr::null_mut());
                assert_eq!(result, S_OK);
                assert!(!d3d_device.is_null());

                (d3d_device.copy(), Some(window))
            }
        }
    }
}

fn screenshot_thread(window_sender: Sender<NativeWindow>) {
    static WINDOW_CLASS_NAME: &[u8] = b"PlaneshiftScreenshotWindow\0";

    unsafe {
        let hinstance = libloaderapi::GetModuleHandleA(ptr::null_mut());
        let mut class = WNDCLASSEXA {
            cbSize: mem::size_of::<WNDCLASSEXA>() as UINT,
            style: 0,
            lpfnWndProc: Some(screenshot_window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: ptr::null_mut(),
            hCursor: ptr::null_mut(),
            hbrBackground: winuser::COLOR_WINDOW as HBRUSH,
            lpszMenuName: ptr::null_mut(),
            lpszClassName: WINDOW_CLASS_NAME.as_ptr() as LPCSTR,
            hIconSm: ptr::null_mut(),
        };
        let class = winuser::RegisterClassExA(&mut class);
        let window = winuser::CreateWindowExA(
            winuser::WS_EX_OVERLAPPEDWINDOW,
            class as LPCSTR,
            WINDOW_CLASS_NAME.as_ptr() as LPCSTR,
            0,
            0,
            0,
            0,
            0,
            winuser::HWND_MESSAGE,
            ptr::null_mut(),
            hinstance,
            ptr::null_mut());
        assert_ne!(winuser::AddClipboardFormatListener(window), FALSE);
        window_sender.send(NativeWindow(window)).unwrap();

        let mut msg: MSG = mem::zeroed();
        while winuser::GetMessageA(&mut msg, ptr::null_mut(), 0, 0) != 0 {
            winuser::TranslateMessage(&mut msg);
            winuser::DispatchMessageA(&mut msg);
        }
    }
}

unsafe extern "system" fn screenshot_window_proc(window: HWND,
                                                 msg: UINT,
                                                 wparam: WPARAM,
                                                 lparam: LPARAM)
                                                 -> LRESULT {
    match msg {
        winuser::WM_USER => {
            winuser::SetWindowLongPtrA(window, winuser::GWLP_USERDATA, wparam as isize)
        }

        winuser::WM_CLIPBOARDUPDATE => {
            let promise = winuser::GetWindowLongPtrA(window, winuser::GWLP_USERDATA) as
                *mut ScreenshotRequest;
            if promise.is_null() {
                return winuser::DefWindowProcA(window, msg, wparam, lparam);
            }

            let request: Box<ScreenshotRequest> = mem::transmute(promise);
            winuser::SetWindowLongPtrA(window, winuser::GWLP_USERDATA, 0);

            assert_ne!(winuser::OpenClipboard(ptr::null_mut()), FALSE);

            // Screenshot data should have no owner. Verify that.
            //
            // FIXME(pcwalton): This is still fragile, because other apps can also place ownerless
            // data on the clipboard, so we might think we have screenshot data when it's actually
            // some other app placing stuff on the clipboard. But this is better than nothing.
            let owner = winuser::GetClipboardOwner();
            if !owner.is_null() {
                return winuser::DefWindowProcA(window, msg, wparam, lparam);
            }

            let mut clipboard = winuser::GetClipboardData(winuser::CF_DIB);
            if clipboard == handleapi::INVALID_HANDLE_VALUE {
                clipboard = winuser::GetClipboardData(winuser::CF_DIBV5);
            }
            if clipboard == handleapi::INVALID_HANDLE_VALUE {
                return winuser::DefWindowProcA(window, msg, wparam, lparam);
            }

            let dib = winbase::GlobalLock(clipboard) as *mut BITMAPINFOHEADER;
            assert!(!dib.is_null());

            // Bitmap data is bottom-to-top, BGRA. Change to top-to-bottom, RGBA.
            let src_data = slice::from_raw_parts(dib.offset(1) as *const u32,
                                                 ((*dib).biSizeImage / 4) as usize);
            let mut dest_data = Vec::with_capacity(src_data.len() * 4);
            let screen_width = (*dib).biWidth as usize;
            let screen_height = (*dib).biHeight as usize;
            let rect = request.window_rect;
            for y in (rect.top as usize)..(rect.bottom as usize) {
                for x in (rect.left as usize)..(rect.right as usize) {
                    let src_pixel = src_data[(screen_height - y - 1) * screen_width + x];
                    dest_data.extend_from_slice(&[
                        ((src_pixel >> 16) & 0xff) as u8,
                        ((src_pixel >> 8)  & 0xff) as u8,
                        ((src_pixel >> 0)  & 0xff) as u8,
                        ((src_pixel >> 24) & 0xff) as u8,
                    ]);
                }
            }

            winbase::GlobalUnlock(dib as *mut _);

            let image = RgbaImage::from_vec((rect.right - rect.left) as u32,
                                            (rect.bottom - rect.top) as u32,
                                            dest_data).unwrap().convert();
            request.promise.resolve(image);
            0
        }

        _ => winuser::DefWindowProcA(window, msg, wparam, lparam),
    }
}

mod com {
    use std::ops::{Deref, DerefMut};
    use std::ptr;
    use winapi::Interface;
    use winapi::shared::winerror;
    use winapi::um::unknwnbase::IUnknown;

    // Based on Microsoft's `CComPtr`.
    pub struct ComPtr<T>(pub *mut T) where T: Interface;

    impl<T> ComPtr<T> where T: Interface {
        #[inline]
        pub fn null() -> ComPtr<T> {
            ComPtr(ptr::null_mut())
        }

        #[inline]
        pub fn copy(&self) -> *mut T {
            unsafe {
                (*(self.0 as *mut IUnknown)).AddRef();
                self.0
            }
        }

        #[inline]
        pub fn query_interface<Q>(&self) -> Result<*mut Q, QueryInterfaceError>
                                  where Q: Interface {
            if self.0.is_null() {
                return Err(QueryInterfaceError::PointerNull)
            }

            let mut result = ptr::null_mut();
            unsafe {
                if winerror::SUCCEEDED((*(self.0 as *mut IUnknown)).QueryInterface(&Q::uuidof(),
                                                                                   &mut result)) {
                    Ok(result as *mut Q)
                } else {
                    Err(QueryInterfaceError::NoInterface)
                }
            }
        }

        #[inline]
        pub unsafe fn release(&mut self) {
            (*(self.0 as *mut IUnknown)).Release();
        }

        #[inline]
        pub fn is_null(&self) -> bool {
            self.0.is_null()
        }
    }

    impl<T> Drop for ComPtr<T> where T: Interface {
        #[inline]
        fn drop(&mut self) {
            unsafe {
                self.release()
            }
        }
    }

    impl<T> Deref for ComPtr<T> where T: Interface {
        type Target = *mut T;
        #[inline]
        fn deref(&self) -> &*mut T {
            &self.0
        }
    }

    impl<T> DerefMut for ComPtr<T> where T: Interface {
        #[inline]
        fn deref_mut(&mut self) -> &mut *mut T {
            &mut self.0
        }
    }

    #[derive(Clone, Copy, PartialEq, Debug)]
    pub enum QueryInterfaceError {
        NoInterface,
        PointerNull,
    }
}
