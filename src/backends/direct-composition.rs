// planeshift/src/backends/direct-composition.rs

use mozangle::egl::ffi::types::{EGLClientBuffer, EGLConfig, EGLContext, EGLDisplay, EGLSurface};
use mozangle::egl::ffi::{D3D11_DEVICE_ANGLE, EGLDeviceEXT};
use mozangle::egl;
use winapi::shared::dxgi1_2::{DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_SCALING_STRETCH};
use winapi::shared::dxgi1_2::{DXGI_SWAP_CHAIN_DESC1, IDXGIFactory2, IDXGISwapChain1};
use winapi::shared::dxgi::{DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, IDXGIDevice};
use winapi::shared::dxgiformat::DXGI_FORMAT_B8G8R8A8_UNORM;
use winapi::shared::dxgitype::{DXGI_SAMPLE_DESC, DXGI_USAGE_RENDER_TARGET_OUTPUT};
use winapi::shared::minwindef::{BOOL, FALSE, TRUE};
use winapi::shared::windef::HWND;
use winapi::shared::winerror;
use winapi::um::d3d11::{ID3D11Device, ID3D11Texture2D};
use winapi::um::dcomp::{self, IDCompositionDevice, IDCompositionVisual};

#[cfg(feature = "enable-winit")]
use winit::Window;
#[cfg(all(feature = "enable-winit", target_family = "windows"))]
use winit::os::windows::WindowExt;

use crate::LayerMap;
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
    type Connection = ();
    type GLContext = GLContext;
    type NativeGLContext = EGLContext;
    type Host = HWND;

    // FIXME(pcwalton): We should make sure this pointer is valid!
    // TODO(pcwalton): Don't panic on error.
    fn new(d3d_device: *mut ID3D11Device) -> Backend {
        unsafe {
            assert!(!d3d_device.is_null());

            // Create the DirectComposition device.
            let (mut d3d_device, mut dcomp_device) = (ComPtr(device), ComPtr::null());
            let result = dcomp::DCompositionCreateDevice(*d3d_device,
                                                         IDCompositionDevice::uuidof(),
                                                         &mut *dcomp_device);
            assert!(winerror::SUCCEEDED(result));

            // Grab the adapter from the D3D11 device.
            let mut dxgi_device: ComPtr<IDXGIDevice> = ComPtr(d3d_device.query_interface());
            let mut adapter = ptr::null_mut();
            let result = dxgi_device.get_adapter(&mut *adapter);
            assert!(winerror::SUCCEEDED(result));

            // Create the DXGI factory. This will be used for creating swap chains.
            let mut dxgi_factory = ComPtr::null();
            let result = adapter.GetParent(IDXGIFactory2::uuidof(), &mut *dxgi_factory);
            assert!(winerror::SUCCEEDED(result));

            // Create the ANGLE EGL device.
            let egl_device = egl::ffi::eglCreateDeviceANGLE(D3D11_DEVICE_ANGLE,
                                                            &*d3d_device,
                                                            ptr::null());
            assert!(!egl_device.is_null());

            // Open the ANGLE EGL display.
            let attributes = [
                egl::ffi::EXPERIMENTAL_PRESENT_PATH_ANGLE,
                egl::ffi::EXPERIMENTAL_PRESENT_PATH_FAST_ANGLE,
                egl::ffi::NONE,
            ];
            let egl_display = egl::ffi::GetPlatformDisplayEXT(egl::ffi::PLATFORM_DEVICE_EXT,
                                                              egl_device,
                                                              attributes.as_ptr());
            assert!(!egl_display.is_null());

            // Initialize EGL via ANGLE.
            let result = egl::ffi::Initialize(egl_display, ptr::null_mut(), ptr::null_mut());
            assert_eq!(result, egl::ffi::TRUE);

            Backend {
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
            let depth_size = if options.depth { 16 } else { 0 };
            let stencil_size = if options.stencil { 8 } else { 0 };
            let attributes = [
                egl::ffi::SURFACE_TYPE,     egl::ffi::WINDOW_BIT,
                egl::ffi::RENDERABLE_TYPE,  egl::ffi::OPENGL_ES2_BIT,
                egl::ffi::RED_SIZE,         8,
                egl::ffi::GREEN_SIZE,       8,
                egl::ffi::BLUE_SIZE,        8,
                egl::ffi::ALPHA_SIZE,       8,
                egl::ffi::DEPTH_SIZE,       depth_size,
                egl::ffi::STENCIL_SIZE,     stencil_size,
                0,                          0,
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
                egl::ffi::CONTEXT_CLIENT_VERSION,   3,
                0,                                  0,
            ];
            let egl_context = egl::CreateContext(self.egl_display,
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

        let mut egl_config = 0;
        let result = egl::ffi::QueryContext(self.egl_display,
                                            egl_context,
                                            egl::ffi::CONFIG_ID,
                                            &mut egl_config);
        if result != egl::ffi::TRUE {
            return Err(())
        }

        Ok(GLContext {
            egl_context,
            egl_config,
            egl_display: self.egl_display,
        })
    }

    fn begin_transaction(&self) {}

    fn end_transaction(&self) {
        unsafe {
            let result = (*self).dcomp_device.Commit();
            assert!(winerror::SUCCEEDED(result));
        }
    }

    fn add_container_layer(&mut self, new_layer: LayerId) {
        unsafe {
            let mut visual = ComPtr::null();
            let result = dcomp_device.CreateVisual(&mut *visual);
            assert!(winerror::SUCCEEDED(result));

            self.native_component.add(new_layer, NativeInfo {
                visual,
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
            let (reference_visual, insert_above) = match reference {
                None => (ptr::null_mut(), TRUE),
                Some(reference) => (&self.native_component[reference].visual, FALSE);
            };
            let result = (**parent_visual).AddVisual(*new_child_visual,
                                                     insert_above,
                                                     **reference_visual);
            assert!(winerror::SUCCEEDED(result));
        }
    }

    fn remove_from_superlayer(&mut self,
                              layer: LayerId,
                              container_component: &LayerMap<LayerContainerInfo>) {
        unsafe {
            let parent_visual = match self.native_component.get(parent) {
                None => return,
                Some(parent_visual) => parent_visual,
            };
            let layer_visual = &self.native_component[layer].visual;
            let result = (**parent_visual).RemoveVisual(*layer_visual);
            assert!(winerror::SUCCEEDED(result));
        }
    }

    unsafe fn host_layer(&mut self,
                         layer: LayerId,
                         host: HWND,
                         _: &LayerMap<LayerTreeInfo>,
                         _: &LayerMap<LayerContainerInfo>,
                         _: &LayerMap<LayerGeometryInfo>) {
        let mut native_info = &mut self.native_info[layer];
        assert!(native_info.target.is_none());

        let mut target = ComPtr::new();
        let result = (*self.dcomp_device).CreateTargetForHwnd(host, TRUE, &mut *target);
        assert!(winerror::SUCCEEDED(result));

        (*target).SetRoot(&*native_info.visual);

        native_info.target = Some(target);
    }

    fn unhost_layer(&mut self, layer: LayerId) {
        self.native_info[layer].target = None;
    }

    fn set_layer_bounds(&mut self,
                        layer: LayerId,
                        tree_component: &LayerMap<LayerTreeInfo>,
                        _: &LayerMap<LayerContainerInfo>,
                        geometry_component: &LayerMap<LayerGeometryInfo>) {
        unsafe {
            let new_bounds = match geometry_component.get(layer) {
                None => return,
                Some(new_bounds) => geometry_info.bounds,
            };

            let visual = &self.native_component[layer].visual;
            (*visual).SetOffsetX(&new_bounds.origin.x);
            (*visual).SetOffsetY(&new_bounds.origin.y);
        }
    }

    fn set_layer_opaque(&mut self, _: LayerId, _: bool) {}

    fn bind_layer_to_gl_context(&mut self,
                                layer: LayerId,
                                context: &mut GLContext,
                                geometry_component: &LayerMap<LayerGeometryInfo>)
                                -> Result<GLContextLayerBinding, ()> {
        let native_component = &mut self.native_component[layer];
        let bounds = &self.geometry_component[layer].bounds;

        unsafe {
            // Create the surface if necessary.
            if native_component.surface.is_none() {
                // Build the DXGI swap chain.
                let descriptor = DXGI_SWAP_CHAIN_DESC1 {
                    Width: bounds.size.width,
                    Height: bounds.size.height,
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
                let mut dxgi_swap_chain = ComPtr::null();
                let result = self.dxgi_factory
                                 .CreateSwapChainForComposition(*self.d3d_device,
                                                                &descriptor,
                                                                ptr::null_mut(),
                                                                &mut *dxgi_swap_chain);
                if !winerror::SUCCEEDED(result) {
                    return Err(())
                }

                // Create the D3D11 texture.
                let mut d3d_texture = ComPtr::null();
                let result = dxgi_swap_chain.GetBuffer(0,
                                                       ID3D11Texture2D::uuidof(),
                                                       &mut *d3d_texture);
                if !winerror::SUCCEEDED(result) {
                    return Err(())
                }

                // Build the EGL surface.
                let attributes = [
                    egl::ffi::WIDTH,                                            bounds.size.width,
                    egl::ffi::HEIGHT,                                           bounds.size.height,
                    egl::ffi::FLEXIBLE_SURFACE_COMPATIBILITY_SUPPORTED_ANGLE,   egl::ffi::TRUE,
                    0,                                                          0,
                ];
                let egl_surface =
                    egl::ffi::CreatePbufferFromClientBuffer(self.display,
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
            let result = native_component.visual.SetContent(*surface.dxgi_swap_chain);
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
            if self.dxgi_swap_chain.Present(0, 0).is_ok() {
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
            let mut d3d_device = ComPtr::null();
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
            assert!(winerror::SUCCEEDED(result));
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
    d3d_texture: ComPtr<ID3D11Texture2D>,
    egl_surface: EGLSurface,
}

struct GLContext {
    egl_context: EGLContext,
    egl_config: EGLConfig,
    egl_display: EGLDisplay,
}

impl Drop for GLContext {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            let result = egl::ffi::DestroyContext(self.egl_context);
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
        pub fn null() -> *mut T {
            ComPtr(ptr::null_mut())
        }

        #[inline]
        pub unsafe fn attach(&mut self, ptr: *mut T) {
            self.release();
            self.0 = ptr;
        }

        #[inline]
        pub fn copy(&self) -> *mut T {
            unsafe {
                (self.0 as *mut IUnknown).AddRef();
                self.0
            }
        }

        #[inline]
        pub fn detach(&mut self) -> *mut T {
            unsafe {
                self.release();
                mem::replace(&mut self.0, ptr::null_mut())
            }
        }

        #[inline]
        pub unsafe fn is_equal_object(&self, other: *mut IUnknown) -> bool {
            self.0 == other
        }

        #[inline]
        pub fn query_interface<Q>(&self) -> Result<*mut Q, QueryInterfaceError>
                                  where Q: Interface {
            if self.0.is_null() {
                return Err(QueryInterfaceError::PointerNull)
            }

            let mut result = ptr::null_mut();
            unsafe {
                if winerror::SUCCEEDED((self.0 as *mut IUnknown).QueryInterface(Q::uuidof(),
                                                                                &mut result)) {
                    Ok(result)
                } else {
                    Err(QueryInterfaceError::NoInterface)
                }
            }
        }

        #[inline]
        pub unsafe fn release(&mut self) {
            (self.0 as *mut IUnknown).Release();
        }

        #[derive(Clone, Copy, PartialEq, Debug)]
        pub enum QueryInterfaceError {
            NoInterface,
            PointerNull,
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
}
