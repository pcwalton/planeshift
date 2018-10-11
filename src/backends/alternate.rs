// planeshift/src/backends/alternate.rs

//! A backend that tries one backend, and if it fails, tries the other.
//!
//! If backend A fails to initialize, then it tries to initialize backend B. Note that more than
//! two backends can be chained together by making backend A or backend B itself a `Chain`.

use euclid::Rect;

#[cfg(feature = "enable-winit")]
use winit::Window;

use crate::{GLContextLayerBinding, GLContextOptions, LayerId, LayerMap, LayerContainerInfo};
use crate::{LayerGeometryInfo, LayerTreeInfo};

pub enum Backend<A, B> where A: crate::Backend, B: crate::Backend {
    A(A),
    B(B),
}

impl<A, B> crate::Backend for Backend<A, B> where A: crate::Backend, B: crate::Backend {
    type Connection = Connection<A, B>;
    type GLContext = GLContext<A, B>;
    type NativeGLContext = NativeGLContext<A, B>;
    type Host = Host<A, B>;

    // Constructor
    fn new(connection: Self::Connection) -> Self {
        match connection {
            Connection::A(connection) => Backend::A(A::new(connection)),
            Connection::B(connection) => Backend::B(B::new(connection)),
        }
    }

    // OpenGL context creation
    fn create_gl_context(&mut self, options: GLContextOptions) -> Result<Self::GLContext, ()> {
        match *self {
            Backend::A(ref mut this) => Ok(GLContext::A(this.create_gl_context(options)?)),
            Backend::B(ref mut this) => Ok(GLContext::B(this.create_gl_context(options)?)),
        }
    }

    unsafe fn wrap_gl_context(&mut self, native_gl_context: Self::NativeGLContext)
                              -> Result<Self::GLContext, ()> {
        match *self {
            Backend::A(ref mut this) => {
                match native_gl_context {
                    NativeGLContext::A(native_gl_context) => {
                        Ok(GLContext::A(this.wrap_gl_context(native_gl_context)?))
                    }
                    NativeGLContext::B(_) => {
                        panic!("wrap_gl_context(): mismatched backend and native GL context")
                    }
                }
            }
            Backend::B(ref mut this) => {
                match native_gl_context {
                    NativeGLContext::B(native_gl_context) => {
                        Ok(GLContext::B(this.wrap_gl_context(native_gl_context)?))
                    }
                    NativeGLContext::A(_) => {
                        panic!("wrap_gl_context(): mismatched backend and native GL context")
                    }
                }
            }
        }
    }

    // Transactions

    fn begin_transaction(&self) {
        match *self {
            Backend::A(ref this) => this.begin_transaction(),
            Backend::B(ref this) => this.begin_transaction(),
        }
    }

    fn end_transaction(&self) {
        match *self {
            Backend::A(ref this) => this.end_transaction(),
            Backend::B(ref this) => this.end_transaction(),
        }
    }

    // Layer creation and destruction

    fn add_container_layer(&mut self, new_layer: LayerId) {
        match *self {
            Backend::A(ref mut this) => this.add_container_layer(new_layer),
            Backend::B(ref mut this) => this.add_container_layer(new_layer),
        }
    }

    fn add_surface_layer(&mut self, new_layer: LayerId) {
        match *self {
            Backend::A(ref mut this) => this.add_surface_layer(new_layer),
            Backend::B(ref mut this) => this.add_surface_layer(new_layer),
        }
    }

    fn delete_layer(&mut self, layer: LayerId) {
        match *self {
            Backend::A(ref mut this) => this.delete_layer(layer),
            Backend::B(ref mut this) => this.delete_layer(layer),
        }
    }

    // Layer tree management

    fn insert_before(&mut self,
                     parent: LayerId,
                     new_child: LayerId,
                     reference: Option<LayerId>,
                     tree_component: &LayerMap<LayerTreeInfo>,
                     container_component: &LayerMap<LayerContainerInfo>,
                     geometry_component: &LayerMap<LayerGeometryInfo>) {
        match *self {
            Backend::A(ref mut this) => {
                this.insert_before(parent,
                                   new_child,
                                   reference,
                                   tree_component,
                                   container_component,
                                   geometry_component)
            }
            Backend::B(ref mut this) => {
                this.insert_before(parent,
                                   new_child,
                                   reference,
                                   tree_component,
                                   container_component,
                                   geometry_component)
            }
        }
    }

    fn remove_from_superlayer(&mut self, layer: LayerId, parent: LayerId) {
        match *self {
            Backend::A(ref mut this) => this.remove_from_superlayer(layer, parent),
            Backend::B(ref mut this) => this.remove_from_superlayer(layer, parent),
        }
    }

    // Native hosting

    unsafe fn host_layer(&mut self,
                         layer: LayerId,
                         host: Self::Host,
                         tree_component: &LayerMap<LayerTreeInfo>,
                         container_component: &LayerMap<LayerContainerInfo>,
                         geometry_component: &LayerMap<LayerGeometryInfo>) {

        match *self {
            Backend::A(ref mut this) => {
                match host {
                    Host::A(host) => {
                        this.host_layer(layer,
                                        host,
                                        tree_component,
                                        container_component,
                                        geometry_component)
                    }
                    Host::B(_) => panic!("host_layer(): mismatched backend and host"),
                }
            }
            Backend::B(ref mut this) => {
                match host {
                    Host::B(host) => {
                        this.host_layer(layer,
                                        host,
                                        tree_component,
                                        container_component,
                                        geometry_component)
                    }
                    Host::A(_) => panic!("host_layer(): mismatched backend and host"),
                }
            }
        }
    }

    fn unhost_layer(&mut self, layer: LayerId) {
        match *self {
            Backend::A(ref mut this) => this.unhost_layer(layer),
            Backend::B(ref mut this) => this.unhost_layer(layer),
        }
    }

    // Geometry
    fn set_layer_bounds(&mut self,
                        layer: LayerId,
                        tree_component: &LayerMap<LayerTreeInfo>,
                        container_component: &LayerMap<LayerContainerInfo>,
                        geometry_component: &LayerMap<LayerGeometryInfo>) {
        match *self {
            Backend::A(ref mut this) => {
                this.set_layer_bounds(layer,
                                      tree_component,
                                      container_component,
                                      geometry_component)
            }
            Backend::B(ref mut this) => {
                this.set_layer_bounds(layer,
                                      tree_component,
                                      container_component,
                                      geometry_component)
            }
        }
    }

    // Miscellaneous layer flags

    fn set_layer_opaque(&mut self, layer: LayerId, opaque: bool) {
        match *self {
            Backend::A(ref mut this) => this.set_layer_opaque(layer, opaque),
            Backend::B(ref mut this) => this.set_layer_opaque(layer, opaque),
        }
    }

    // OpenGL content binding

    fn bind_layer_to_gl_context(&mut self,
                                layer: LayerId,
                                context: &mut Self::GLContext,
                                geometry_component: &LayerMap<LayerGeometryInfo>)
                                -> Result<GLContextLayerBinding, ()> {
        match (self, context) {
            (&mut Backend::A(ref mut this), &mut GLContext::A(ref mut context)) => {
                this.bind_layer_to_gl_context(layer, context, geometry_component)
            }
            (&mut Backend::B(ref mut this), &mut GLContext::B(ref mut context)) => {
                this.bind_layer_to_gl_context(layer, context, geometry_component)
            }
            _ => panic!("bind_layer_to_gl_context(): mismatched backend and GL context"),
        }
    }

    fn present_gl_context(&mut self, binding: GLContextLayerBinding, changed_rect: &Rect<f32>)
                          -> Result<(), ()> {
        match *self {
            Backend::A(ref mut this) => this.present_gl_context(binding, changed_rect),
            Backend::B(ref mut this) => this.present_gl_context(binding, changed_rect),
        }
    }

    // `winit` integration

    #[cfg(feature = "enable-winit")]
    fn connection_from_window(window: &Window) -> Result<Self::Connection, ()> {
        if let Ok(connection) = A::connection_from_window(window) {
            Ok(Connection::A(connection))
        } else {
            Ok(Connection::B(B::connection_from_window(window)?))
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
        match *self {
            Backend::A(ref mut this) => {
                this.host_layer_in_window(window,
                                          layer,
                                          tree_component,
                                          container_component,
                                          geometry_component)
            }
            Backend::B(ref mut this) => {
                this.host_layer_in_window(window,
                                          layer,
                                          tree_component,
                                          container_component,
                                          geometry_component)
            }
        }
    }
}

pub enum Connection<A, B> where A: crate::Backend, B: crate::Backend {
    A(A::Connection),
    B(B::Connection),
}

pub enum GLContext<A, B> where A: crate::Backend, B: crate::Backend {
    A(A::GLContext),
    B(B::GLContext),
}

pub enum NativeGLContext<A, B> where A: crate::Backend, B: crate::Backend {
    A(A::NativeGLContext),
    B(B::NativeGLContext),
}

pub enum Host<A, B> where A: crate::Backend, B: crate::Backend {
    A(A::Host),
    B(B::Host),
}
