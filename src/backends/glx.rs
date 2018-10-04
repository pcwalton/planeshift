// planeshift/src/backends/glx.rs

//! GLX/Xlib/X11-based native system implementation.

use euclid::{Point2D, Rect, Size2D};
use gleam::gl::{GLuint, Gl};
use std::mem;
use std::ptr;
use std::sync::{Arc, Mutex};
use x11::xlib::{self, Display, Visual, Window, XSetWindowAttributes};

use crate::glx::types::GLXContext;
use crate::glx;
use crate::{Context, LayerContainerInfo, LayerGeometryInfo, LayerId, LayerMap, LayerTreeInfo};

#[cfg(all(feature = "enable-winit"))]
use winit;
#[cfg(all(feature = "enable-winit"))]
use winit::os::unix::WindowExt;

pub struct Backend {
    native_component: LayerMap<NativeInfo>,

    display: *mut Display,
    visual: *mut Visual,
    depth: i32,
    root_window: Window,
}

impl crate::Backend for Backend {
    type Surface = Surface;
    type Host = Window;

    fn new() -> Backend {
        unsafe {
            let display = xlib::XOpenDisplay(ptr::null_mut());
            let screen = xlib::XDefaultScreen(display);
            let visual = xlib::XDefaultVisual(display, screen);
            let depth = xlib::XDefaultDepth(display, screen);
            let root_window = xlib::XRootWindow(display, screen);
            Backend {
                native_component: LayerMap::new(),
                display,
                visual,
                depth,
                root_window,
            }
        }
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
            let window = xlib::XCreateWindow(self.display,
                                             self.root_window,
                                             0, 0,
                                             1, 1,
                                             0,
                                             self.depth,
                                             xlib::InputOutput as u32,
                                             self.visual,
                                             0,
                                             &mut attributes);
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

    fn set_layer_contents(&mut self, layer: LayerId, surface: &Self::Surface) {
        let window = self.native_component[layer].window;
        *surface.data.lock().unwrap() = Some(SurfaceData {
            display: self.display,
            window,
        });
    }

    fn refresh_layer_contents(&mut self, layer: LayerId, _: &Rect<f32>) {
        unsafe {
            glx::SwapBuffers(self.display as *mut _, self.native_component[layer].window);
        }
    }

    fn set_contents_opaque(&mut self, _: LayerId, _: bool) {}

    #[cfg(feature = "winit")]
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

// GLX surface implementation

#[derive(Clone)]
pub struct Surface {
    data: Arc<Mutex<Option<SurfaceData>>>,
    size: Size2D<u32>,
}

impl Surface {
    pub fn new(size: &Size2D<u32>) -> Surface {
        Surface {
            data: Arc::new(Mutex::new(None)),
            size: *size,
        }
    }

    #[inline]
    pub fn size(&self) -> Size2D<u32> {
        self.size
    }

    pub fn bind_to_gl_texture(&self, _: &Gl, _: GLuint) -> Result<(), ()> {
        Err(())
    }

    pub fn bind_to_gl_context(&self, context: &GLXContext) -> Result<(), ()> {
        match *self.data.lock().unwrap() {
            None => Err(()),
            Some(ref data) => {
                unsafe {
                    glx::MakeCurrent(data.display as *mut _, data.window, *context);
                    Ok(())
                }
            }
        }
    }
}

pub struct SurfaceData {
    display: *mut Display,
    window: Window,
}
