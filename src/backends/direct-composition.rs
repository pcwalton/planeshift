// planeshift/src/backends/direct-composition.rs

use euclid::Rect;
use mozangle::egl::ffi::types::{EGLClientBuffer, EGLConfig, EGLContext, EGLDisplay, EGLSurface};
use mozangle::egl::ffi::{D3D11_DEVICE_ANGLE, EGLDeviceEXT};
use mozangle::egl;
use std::ffi::c_void;
use std::ptr;
use winapi::Interface;
use winapi::shared::dxgi1_2::{DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_SCALING_STRETCH};
use winapi::shared::dxgi1_2::{DXGI_SWAP_CHAIN_DESC1, IDXGIFactory2, IDXGISwapChain1};
use winapi::shared::dxgi::{DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, IDXGIAdapter, IDXGIDevice};
use winapi::shared::dxgiformat::DXGI_FORMAT_B8G8R8A8_UNORM;
use winapi::shared::dxgitype::{DXGI_SAMPLE_DESC, DXGI_USAGE_RENDER_TARGET_OUTPUT};
use winapi::shared::minwindef::{FALSE, TRUE};
use winapi::shared::windef::HWND;
use winapi::shared::winerror::{self, S_OK};
use winapi::um::d3d11::{self, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, ID3D11Device};
use winapi::um::d3d11::{ID3D11Texture2D};
use winapi::um::d3dcommon::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use winapi::um::d3dcommon::{D3D_FEATURE_LEVEL_10_1};
use winapi::um::dcomp::{self, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual};
use winapi::um::unknwnbase::IUnknown;

#[cfg(feature = "enable-winit")]
use winit::Window;
#[cfg(all(feature = "enable-winit", target_family = "windows"))]
use winit::os::windows::WindowExt;

use crate::{GLContextLayerBinding, GLContextOptions, LayerContainerInfo, LayerGeometryInfo};
use crate::{LayerId, LayerMap, LayerTreeInfo};
use self::com::ComPtr;

pub struct Backend {
    native_component: LayerMap<NativeInfo>,

    d3d_device: ComPtr<ID3D11Device>,
    dcomp_device: ComPtr<IDCompositionDevice>,
    dxgi_factory: ComPtr<IDXGIFactory2>,

    egl_device: EGLDeviceEXT,
    egl_display: EGLDisplay,
}

impl crate::Backend for Backend {
    type Connection = *mut ID3D11Device;
    type GLContext = GLContext;
    type NativeGLContext = EGLContext;
    type Host = HWND;

    // FIXME(pcwalton): We should make sure this pointer is valid!
    // TODO(pcwalton): Don't panic on error.
    fn new(d3d_device: *mut ID3D11Device) -> Backend {
        unsafe {
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

            Backend {
                native_component: LayerMap::new(),

                d3d_device,
                dcomp_device,
                dxgi_factory,

                egl_device,
                egl_display,
            }
        }
    }

    fn create_gl_context(&mut self, options: GLContextOptions) -> Result<GLContext, ()> {
        unsafe {
            // Enumerate the EGL pixel configurations for ANGLE.
            let (mut configs, mut num_configs) = ([ptr::null(); 64], 0);
            let depth_size = if options.contains(GLContextOptions::DEPTH) { 16 } else { 0 };
            let stencil_size = if options.contains(GLContextOptions::STENCIL) { 8 } else { 0 };
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

    fn begin_transaction(&self) {}

    fn end_transaction(&self) {
        unsafe {
            let result = (**self.dcomp_device).Commit();
            assert_eq!(result, S_OK);
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

    fn remove_from_superlayer(&mut self, layer: LayerId, parent: LayerId) {
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

        native_info.target = Some(target);
    }

    fn unhost_layer(&mut self, layer: LayerId) {
        self.native_component[layer].target = None;
    }

    fn set_layer_bounds(&mut self,
                        layer: LayerId,
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

    fn set_layer_opaque(&mut self, _: LayerId, _: bool) {}

    fn bind_layer_to_gl_context(&mut self,
                                layer: LayerId,
                                context: &mut GLContext,
                                geometry_component: &LayerMap<LayerGeometryInfo>)
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

    fn present_gl_context(&mut self, binding: GLContextLayerBinding, _: &Rect<f32>)
                          -> Result<(), ()> {
        unsafe {
            let surface = self.native_component[binding.layer].surface.as_ref().unwrap();
            if winerror::SUCCEEDED((**surface.dxgi_swap_chain).Present(0, 0)) {
                Ok(())
            } else {
                Err(())
            }
        }
    }

    // `winit` integration

    #[cfg(feature = "enable-winit")]
    fn connection_from_window(_: &winit::Window) -> *mut ID3D11Device {
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
                return d3d_device.copy()
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

            d3d_device.copy()
        }
    }

    #[cfg(feature = "enable-winit")]
    fn host_layer_in_window(&mut self,
                            window: &Window,
                            layer: LayerId,
                            tree_component: &LayerMap<LayerTreeInfo>,
                            container_component: &LayerMap<LayerContainerInfo>,
                            geometry_component: &LayerMap<LayerGeometryInfo>)
                            -> Result<(), ()> {
        unsafe {
            self.host_layer(layer,
                            window.get_hwnd() as HWND,
                            tree_component,
                            container_component,
                            geometry_component);
            Ok(())
        }
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
    target: Option<ComPtr<IDCompositionTarget>>,
    surface: Option<Surface>,
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
