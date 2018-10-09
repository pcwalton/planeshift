// planeshift/src/backends/glx.rs

//! GLX/Xlib/X11-based native system implementation.

use euclid::{Point2D, Rect, Size2D};
use gl::types::{GLint, GLuint};
use std::ffi::CString;
use std::mem;
use std::ptr;
use std::slice;
use std::sync::{Arc, Mutex};
use x11::xlib::{self, Display, Visual, Window, XSetWindowAttributes};

use crate::glx::types::Display as GLXDisplay;
use crate::glx::types::GLXContext;
use crate::glx;
use crate::glx_extra::Glx as GlxExtra;
use crate::glx_extra::types::Display as GLXExtraDisplay;
use crate::glx_extra;
use crate::{GLContextLayerBinding, GLContextOptions, LayerContainerInfo, LayerContext};
use crate::{LayerGeometryInfo, LayerId, LayerMap, LayerTreeInfo};

#[cfg(all(feature = "enable-winit"))]
use winit;
#[cfg(all(feature = "enable-winit"))]
use winit::os::unix::WindowExt;

pub struct Backend {
    native_component: LayerMap<NativeInfo>,

    display: *mut Display,
    screen: i32,
    visual: *mut Visual,
    depth: i32,
    root_window: Window,

    glx_extra: GlxExtra,
}

impl crate::Backend for Backend {
    type Connection = *mut Display;
    type GLContext = GLContext;
    type NativeGLContext = GLXContext;
    type Host = Window;

    fn new(display: *mut Display) -> Backend {
        unsafe {
            let screen = xlib::XDefaultScreen(display);
            let root_window = xlib::XRootWindow(display, screen);

            let mut visual_info = mem::uninitialized();
            xlib::XMatchVisualInfo(display, screen, 32, xlib::TrueColor, &mut visual_info);
            let (visual, depth) = (visual_info.visual, visual_info.depth);

            gl::load_with(|symbol| {
                let symbol = CString::new(symbol.as_bytes()).unwrap();
                glx::GetProcAddress(symbol.as_ptr() as *const u8) as *const _
            });

            let glx_extra = GlxExtra::load_with(|symbol| {
                let symbol = CString::new(symbol.as_bytes()).unwrap();
                glx::GetProcAddress(symbol.as_ptr() as *const u8) as *const _
            });

            Backend {
                native_component: LayerMap::new(),
                display,
                screen,
                visual,
                depth,
                root_window,
                glx_extra,
            }
        }
    }

    fn create_gl_context(&mut self, options: GLContextOptions) -> Result<GLContext, ()> {
        unsafe {
            let attributes = [
                glx::DRAWABLE_TYPE, glx::WINDOW_BIT,
                glx::DOUBLEBUFFER, xlib::True as GLuint,
                glx::X_RENDERABLE, xlib::True as GLuint,
                glx::RED_SIZE, 8,
                glx::GREEN_SIZE, 8,
                glx::BLUE_SIZE, 8,
                glx::ALPHA_SIZE, 8,
                glx::DEPTH_SIZE, if options.contains(GLContextOptions::DEPTH) { 16 } else { 0 },
                glx::STENCIL_SIZE, if options.contains(GLContextOptions::STENCIL) { 8 } else { 0 },
                0, 0,
            ];
            let mut configs_count = 0;
            let configs = glx::ChooseFBConfig(self.display as *mut GLXDisplay,
                                              self.screen,
                                              attributes.as_ptr() as *const GLint,
                                              &mut configs_count);
            if configs.is_null() {
                return Err(())
            }

            let configs = slice::from_raw_parts(configs, configs_count as usize);
            let (_config_index, &config) = configs.iter().enumerate().filter(|(_, &config)| {
                let visual_info = glx::GetVisualFromFBConfig(self.display as *mut GLXDisplay,
                                                             config);
                (*visual_info).depth == self.depth
            }).next().unwrap();

            let attributes = [
                glx_extra::CONTEXT_MAJOR_VERSION_ARB, 3,
                glx_extra::CONTEXT_MINOR_VERSION_ARB, 2,
                0, 0,
            ];
            let glx_context = self.glx_extra
                                  .CreateContextAttribsARB(self.display as *mut GLXExtraDisplay,
                                                           config,
                                                           ptr::null_mut(),
                                                           xlib::True,
                                                           attributes.as_ptr() as *const GLint);
            if glx_context.is_null() {
                return Err(())
            }

            Ok(GLContext {
                glx_context,
                display: self.display,
            })
        }
    }

    fn wrap_gl_context(&mut self, glx_context: GLXContext) -> Result<GLContext, ()> {
        Ok(GLContext {
            glx_context,
            display: self.display,
        })
    }

    fn begin_transaction() {
        // TODO(pcwalton): Maybe use XCB here?
    }

    fn end_transaction() {
        // TODO(pcwalton): Maybe use XCB here?
    }

    fn add_container_layer(&mut self, new_layer: LayerId) {
        unsafe {
            let mut attributes: XSetWindowAttributes = mem::uninitialized();
            attributes.colormap = xlib::XCreateColormap(self.display,
                                                        self.root_window,
                                                        self.visual,
                                                        xlib::AllocNone);
            attributes.border_pixel = 0;
            attributes.background_pixel = 0;
            let attributes_bits = xlib::CWColormap | xlib::CWBorderPixel | xlib::CWBackPixel;

            let window = xlib::XCreateWindow(self.display,
                                             self.root_window,
                                             0, 0,
                                             1, 1,
                                             0,
                                             self.depth,
                                             xlib::InputOutput as u32,
                                             self.visual,
                                             attributes_bits,
                                             &mut attributes);

            xlib::XCreateGC(self.display, window, 0, ptr::null_mut());

            self.native_component.add(new_layer, NativeInfo {
                window,
            });
        }
    }

    fn add_surface_layer(&mut self, new_layer: LayerId) {
        // There's no distinction between a container layer and a surface layer in the GLX backend.
        self.add_container_layer(new_layer)
    }

    fn delete_layer(&mut self, layer: LayerId) {
        unsafe {
            xlib::XDestroyWindow(self.display, self.native_component[layer].window);
        }

        self.native_component.remove(layer);
    }

    fn insert_before(&mut self,
                     parent: LayerId,
                     new_child: LayerId,
                     mut maybe_reference: Option<LayerId>,
                     tree_component: &LayerMap<LayerTreeInfo>,
                     _: &LayerMap<LayerContainerInfo>,
                     geometry_component: &LayerMap<LayerGeometryInfo>) {
        unsafe {
            let parent_window = self.native_component[parent].window;
            let new_child_window = self.native_component[new_child].window;

            let new_child_origin = match geometry_component.get(new_child) {
                Some(geometry_component) => geometry_component.bounds.origin.round().to_u32(),
                None => Point2D::zero(),
            };

            // This implicitly inserts the child on top.
            xlib::XReparentWindow(self.display,
                                  new_child_window,
                                  parent_window,
                                  new_child_origin.x as i32,
                                  new_child_origin.y as i32);

            // Move to the right position in the hierarchy.
            while let Some(reference) = maybe_reference {
                let reference_window = self.native_component[reference].window;
                xlib::XRaiseWindow(self.display, reference_window);
                maybe_reference = tree_component[reference].next_sibling;
            }

            // Make our window visible.
            xlib::XMapWindow(self.display, new_child_window);
        }
    }

    fn remove_from_superlayer(&mut self, layer: LayerId) {
        unsafe {
            // Unmap the window, and move it to the root.
            let window = self.native_component[layer].window;
            xlib::XReparentWindow(self.display, window, self.root_window, 0, 0);
            xlib::XUnmapWindow(self.display, window);
        }
    }

    fn host_layer(&mut self,
                  child: LayerId,
                  host_window: Window,
                  _: &LayerMap<LayerTreeInfo>,
                  _: &LayerMap<LayerContainerInfo>,
                  geometry_component: &LayerMap<LayerGeometryInfo>) {
        unsafe {
            let child_window = self.native_component[child].window;

            let child_origin = match geometry_component.get(child) {
                Some(geometry_component) => geometry_component.bounds.origin.round().to_u32(),
                None => Point2D::zero(),
            };

            xlib::XReparentWindow(self.display,
                                  child_window,
                                  host_window,
                                  child_origin.x as i32,
                                  child_origin.y as i32);

            // Make the window visible.
            xlib::XMapWindow(self.display, child_window);
        }
    }

    fn unhost_layer(&mut self, layer: LayerId) {
        self.remove_from_superlayer(layer)
    }

    fn set_layer_bounds(&mut self,
                        layer: LayerId,
                        _: &LayerMap<LayerTreeInfo>,
                        _: &LayerMap<LayerContainerInfo>,
                        geometry_component: &LayerMap<LayerGeometryInfo>) {
        unsafe {
            let window = self.native_component[layer].window;
            let bounds = geometry_component[layer].bounds.round().to_u32();
            xlib::XMoveResizeWindow(self.display,
                                    window,
                                    bounds.origin.x as i32, bounds.origin.y as i32,
                                    bounds.size.width, bounds.size.height);
        }
    }

    fn set_layer_opaque(&mut self, _: LayerId, _: bool) {}

    fn bind_layer_to_gl_context(&mut self,
                                layer: LayerId,
                                context: &mut GLContext,
                                _: &LayerMap<LayerGeometryInfo>)
                                -> Result<GLContextLayerBinding, ()> {
        unsafe {
            let window = self.native_component[layer].window;
            glx::MakeCurrent(context.display as *mut _, window, context.glx_context);

            Ok(GLContextLayerBinding {
                layer,
                framebuffer: 0,
            })
        }
    }

    fn present_gl_context(&mut self, binding: GLContextLayerBinding, _: &Rect<f32>)
                          -> Result<(), ()> {
        unsafe {
            gl::Flush();
            glx::SwapBuffers(self.display as *mut _, self.native_component[binding.layer].window);
        }

        Ok(())
    }

    #[cfg(feature = "enable-winit")]
    fn connection_from_window(window: &winit::Window) -> *mut Display {
        window.get_xlib_display().unwrap() as *mut Display
    }

    #[cfg(feature = "enable-winit")]
    fn host_layer_in_window(&mut self,
                            window: &winit::Window,
                            layer: LayerId,
                            tree_component: &LayerMap<LayerTreeInfo>,
                            container_component: &LayerMap<LayerContainerInfo>,
                            geometry_component: &LayerMap<LayerGeometryInfo>)
                            -> Result<(), ()> {
        match window.get_xlib_window() {
            None => Err(()),
            Some(xlib_window) => {
                self.host_layer(layer,
                                xlib_window,
                                tree_component,
                                container_component,
                                geometry_component);
                Ok(())
            }
        }
    }
}

// GLX native component implementation

struct NativeInfo {
    window: Window,
}

// GLX context implementation

pub struct GLContext {
    glx_context: GLXContext,
    display: *mut Display,
}

impl Drop for GLContext {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            glx::DestroyContext(self.display as *mut GLXDisplay, self.glx_context);
        }
    }
}
