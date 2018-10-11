// planeshift/src/backends/wayland.rs

//! Wayland native system implementation.

use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Display, GlobalManager};

pub struct Backend {
    native_component: LayerMap<NativeInfo>,

    global_manager: GlobalManager,
    display: Display,
    compositor: WlCompositor,
}

impl crate::Backend for Backend {
    type Connection = Display;
    type GLContext = GLContext;
    type NativeGLContext = CGLContextObj;
    type Host = id;

    // Constructor

    fn new(display: Display) -> Backend {
        let globals = GlobalManager::new(&display);
        Backend {
            globals,
            display,
            compositor,
        }
    }

    // OpenGL context creation

    fn create_gl_context(&mut self, _: GLContextOptions) -> Result<GLContext, ()> {
    }

    unsafe fn wrap_gl_context(&mut self, cgl_context: CGLContextObj) -> Result<GLContext, ()> {
    }

    fn begin_transaction(&self) {
    }

    fn end_transaction(&self) {
    }

    fn add_container_layer(&mut self, new_layer: LayerId) {
        let surface = self.compositor
                          .create_surface(|surface| surface.implement(|_, _| ()))
                          .unwrap();
        self.native_component.add(new_layer, NativeInfo {
            surface,
        });
    }

    fn add_surface_layer(&mut self, new_layer: LayerId) {
        self.add_container_layer(new_layer)
    }

    fn delete_layer(&mut self, layer: LayerId) {
        self.native_component.remove_if_present(layer);
    }

    fn insert_before(&mut self,
                     parent: LayerId,
                     new_child: LayerId,
                     reference: Option<LayerId>,
                     tree_component: &LayerMap<LayerTreeInfo>,
                     container_component: &LayerMap<LayerContainerInfo>,
                     geometry_component: &LayerMap<LayerGeometryInfo>) {
    }

    fn remove_from_superlayer(&mut self, layer: LayerId, _: LayerId) {
    }

    // Increases the reference count of `hosting_view`.
    unsafe fn host_layer(&mut self,
                         layer: LayerId,
                         host: id,
                         tree_component: &LayerMap<LayerTreeInfo>,
                         container_component: &LayerMap<LayerContainerInfo>,
                         geometry_component: &LayerMap<LayerGeometryInfo>) {
    }

    fn unhost_layer(&mut self, layer: LayerId) {
    }

    fn set_layer_bounds(&mut self,
                        layer: LayerId,
                        tree_component: &LayerMap<LayerTreeInfo>,
                        _: &LayerMap<LayerContainerInfo>,
                        geometry_component: &LayerMap<LayerGeometryInfo>) {
    }

    fn set_layer_opaque(&mut self, layer: LayerId, opaque: bool) {
    }

    fn bind_layer_to_gl_context(&mut self,
                                layer: LayerId,
                                context: &mut Self::GLContext,
                                geometry_component: &LayerMap<LayerGeometryInfo>)
                                -> Result<GLContextLayerBinding, ()> {
    }

    fn present_gl_context(&mut self, binding: GLContextLayerBinding, _: &Rect<f32>)
                          -> Result<(), ()> {
    }

    // `winit` integration

    #[cfg(feature = "enable-winit")]
    fn connection_from_window(_: &winit::Window) -> Result<(), ()> {
    }

    #[cfg(feature = "enable-winit")]
    fn host_layer_in_window(&mut self,
                            window: &Window,
                            layer: LayerId,
                            tree_component: &LayerMap<LayerTreeInfo>,
                            container_component: &LayerMap<LayerContainerInfo>,
                            geometry_component: &LayerMap<LayerGeometryInfo>)
                            -> Result<(), ()> {
    }
}

pub struct Connection {
    pub display: Display,
    pub compositor: WlCompositor,
}

struct NativeInfo {
    surface: WlSurface,
}
