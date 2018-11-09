// planeshift/src/lib.rs
//
// Copyright © 2018 The Pathfinder Project Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

extern crate euclid;
extern crate gl;
extern crate image;
extern crate tempfile;

#[macro_use]
extern crate bitflags;
#[macro_use]
extern crate lazy_static;

#[cfg(feature = "enable-winit")]
extern crate winit;

#[cfg(target_os = "linux")]
extern crate dbus;
#[cfg(target_os = "linux")]
extern crate wayland_client;
#[cfg(target_os = "linux")]
#[macro_use]
extern crate wayland_sys;

#[cfg(target_os = "macos")]
extern crate block;
#[cfg(target_os = "macos")]
extern crate cgl;
#[cfg(target_os = "macos")]
extern crate cocoa;
#[cfg(target_os = "macos")]
extern crate core_foundation;
#[cfg(target_os = "macos")]
extern crate core_graphics;
#[cfg(target_os = "macos")]
extern crate io_surface;
#[cfg(target_os = "macos")]
#[macro_use]
extern crate objc;

#[cfg(target_family = "windows")]
extern crate mozangle;
#[cfg(target_family = "windows")]
extern crate winapi;

use euclid::Rect;
use gl::types::GLuint;
use image::RgbaImage;
use std::fmt::{self, Debug, Formatter};
use std::mem;
use std::ops::{Index, IndexMut};
use std::sync::{Arc, Mutex};

#[cfg(feature = "enable-winit")]
use winit::{EventsLoop, Window, WindowBuilder};

use crate::backend::Backend;

pub mod backend;
pub mod backends;

#[cfg(target_os = "linux")]
#[allow(non_camel_case_types)]
mod egl {
    use std::os::raw::{c_long, c_void};
    use wayland_sys::client::wl_display;
    use wayland_sys::egl::wl_egl_window;

    pub type EGLNativeDisplayType = *mut wl_display;
    pub type EGLNativePixmapType = *mut c_void;
    pub type EGLNativeWindowType = *mut wl_egl_window;
    pub type EGLint = khronos_int32_t;
    pub type NativeDisplayType = EGLNativeDisplayType;
    pub type NativePixmapType = EGLNativePixmapType;
    pub type NativeWindowType = EGLNativeWindowType;
    pub type khronos_int32_t = i32;
    pub type khronos_ssize_t = c_long;
    pub type khronos_uint64_t = u64;
    pub type khronos_utime_nanoseconds_t = khronos_uint64_t;

    include!(concat!(env!("OUT_DIR"), "/egl_bindings.rs"));
}

pub struct LayerContext<B = backends::default::Backend> where B: Backend {
    next_layer_id: LayerId,
    transaction: Option<TransactionInfo>,

    tree_component: LayerMap<LayerTreeInfo>,
    container_component: LayerMap<LayerContainerInfo>,
    geometry_component: LayerMap<LayerGeometryInfo>,
    surface_component: LayerMap<LayerSurfaceInfo>,

    backend: B,
}

#[derive(Clone, Copy, PartialEq, PartialOrd, Eq, Ord, Hash, Debug)]
pub struct LayerId(pub u32);

#[derive(Debug)]
pub struct LayerMap<T>(pub Vec<Option<T>>);


// Public structures

pub enum Connection<'a, N> {
    Native(N),
    #[cfg(feature = "enable-winit")]
    Winit(WindowBuilder, &'a EventsLoop),
}

bitflags! {
    pub struct SurfaceOptions: u8 {
        const OPAQUE = 0x01;
        const DEPTH = 0x02;
        const STENCIL = 0x04;
    }
}

pub struct GLContextLayerBinding {
    pub layer: LayerId,
    pub framebuffer: GLuint,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GLAPI {
    GL,
    GLES,
}

#[derive(Clone)]
pub struct Promise<T>(Arc<Mutex<PromiseData<T>>>) where T: 'static + Clone + Send;

// Components

#[doc(hidden)]
pub struct LayerTreeInfo {
    parent: LayerParent,
    prev_sibling: Option<LayerId>,
    next_sibling: Option<LayerId>,
}

#[doc(hidden)]
pub struct LayerContainerInfo {
    first_child: Option<LayerId>,
    last_child: Option<LayerId>,
}

#[doc(hidden)]
pub struct LayerGeometryInfo {
    bounds: Rect<f32>,
}

#[doc(hidden)]
pub struct LayerSurfaceInfo {
    options: SurfaceOptions,
}

// Other data structures

#[derive(PartialEq, Debug)]
pub enum LayerParent {
    Layer(LayerId),
    NativeHost,
}

struct PromiseData<T> where T: Clone + Send {
    on_fulfilled: Vec<Box<dyn FnMut(T) + Send>>,
    on_rejected: Vec<Box<dyn FnMut() + Send>>,
    result: PromiseResult<T>,
}

enum PromiseResult<T> where T: Clone + Send {
    Pending,
    Resolved(T),
    Rejected,
}

// Public API for the context

impl<B> LayerContext<B> where B: Backend {
    // Core functions

    pub fn with_backend_connection(connection: Connection<B::NativeConnection>)
                                   -> Result<LayerContext<B>, ConnectionError> {
        Ok(LayerContext {
            backend: Backend::new(connection)?,

            next_layer_id: LayerId(0),
            transaction: None,

            tree_component: LayerMap::new(),
            container_component: LayerMap::new(),
            geometry_component: LayerMap::new(),
            surface_component: LayerMap::new(),
        })
    }

    // OpenGL context creation

    pub fn create_gl_context(&mut self, options: SurfaceOptions) -> Result<B::GLContext, ()> {
        self.backend.create_gl_context(options)
    }

    pub unsafe fn wrap_gl_context(&mut self, native_gl_context: B::NativeGLContext)
                                  -> Result<B::GLContext, ()> {
        self.backend.wrap_gl_context(native_gl_context)
    }

    pub fn gl_api(&self) -> GLAPI {
        self.backend.gl_api()
    }

    // Transactions

    pub fn begin_transaction(&mut self) {
        match self.transaction {
            None => {
                self.transaction = Some(TransactionInfo {
                    level: 1,
                    promise: Promise::new(),
                });
                self.backend.begin_transaction();
            }
            Some(ref mut transaction) => {
                transaction.level += 1;
            }
        }
    }

    pub fn end_transaction(&mut self) {
        {
            let transaction = self.transaction
                                  .as_mut()
                                  .expect("end_transaction(): Not in a transaction!");
            transaction.level -= 1;
            if transaction.level > 0 {
                return
            }
        }

        // If we got here, we're done with the transaction.
        let transaction = self.transaction.take().unwrap();
        self.backend.end_transaction(&transaction.promise,
                                     &self.tree_component,
                                     &self.container_component,
                                     &self.geometry_component,
                                     &self.surface_component);
    }

    #[inline]
    fn in_transaction(&self) -> bool {
        self.transaction.is_some()
    }

    // Layer tree management system

    pub fn add_container_layer(&mut self) -> LayerId {
        debug_assert!(self.in_transaction());

        let layer = self.next_layer_id;
        self.next_layer_id.0 += 1;

        self.container_component.add(layer, LayerContainerInfo {
            first_child: None,
            last_child: None,
        });
        self.backend.add_container_layer(layer);
        layer
    }

    pub fn add_surface_layer(&mut self) -> LayerId {
        debug_assert!(self.in_transaction());

        let layer = self.next_layer_id;
        self.next_layer_id.0 += 1;

        self.surface_component.add(layer, LayerSurfaceInfo {
            options: SurfaceOptions::empty(),
        });

        self.backend.add_surface_layer(layer);
        layer
    }

    pub fn parent_of(&self, layer: LayerId) -> Option<&LayerParent> {
        self.tree_component.get(layer).map(|info| &info.parent)
    }

    pub fn insert_before(&mut self,
                         parent: LayerId,
                         new_child: LayerId,
                         reference: Option<LayerId>) {
        debug_assert!(self.in_transaction());

        if let Some(reference) = reference {
            debug_assert_eq!(self.parent_of(reference), Some(&LayerParent::Layer(parent)));
        }

        let new_prev_sibling = match reference {
            Some(reference) => self.tree_component[reference].prev_sibling,
            None => self.container_component[parent].last_child,
        };

        self.tree_component.add(new_child, LayerTreeInfo {
            parent: LayerParent::Layer(parent),
            prev_sibling: new_prev_sibling,
            next_sibling: reference,
        });

        match reference {
            Some(reference) => self.tree_component[reference].next_sibling = Some(new_child),
            None => self.container_component[parent].last_child = Some(new_child),
        }

        if self.tree_component[new_child].prev_sibling.is_none() {
            self.container_component[parent].first_child = Some(new_child)
        }

        self.backend.insert_before(parent,
                                   new_child,
                                   reference,
                                   &self.tree_component,
                                   &self.container_component,
                                   &self.geometry_component);
    }

    #[inline]
    pub fn append_child(&mut self, parent: LayerId, new_child: LayerId) {
        self.insert_before(parent, new_child, None)
    }

    #[inline]
    pub unsafe fn host_layer(&mut self, host: B::Host, layer: LayerId) {
        debug_assert!(self.in_transaction());

        self.tree_component.add(layer, LayerTreeInfo {
            parent: LayerParent::NativeHost,
            prev_sibling: None,
            next_sibling: None,
        });

        self.backend.host_layer(layer,
                                host,
                                &self.tree_component,
                                &self.container_component,
                                &self.geometry_component);
    }

    pub fn remove_from_parent(&mut self, old_child: LayerId) {
        debug_assert!(self.in_transaction());

        let old_tree = self.tree_component.take(old_child);
        match old_tree.parent {
            LayerParent::NativeHost => self.backend.unhost_layer(old_child),

            LayerParent::Layer(parent_layer) => {
                self.backend.remove_from_superlayer(old_child,
                                                    parent_layer,
                                                    &self.tree_component,
                                                    &self.geometry_component);

                match old_tree.prev_sibling {
                    None => {
                        self.container_component[parent_layer].first_child = old_tree.next_sibling
                    }
                    Some(prev_sibling) => {
                        self.tree_component[prev_sibling].next_sibling = old_tree.next_sibling
                    }
                }
                match old_tree.next_sibling {
                    None => {
                        self.container_component[parent_layer].last_child = old_tree.prev_sibling
                    }
                    Some(next_sibling) => {
                        self.tree_component[next_sibling].prev_sibling = old_tree.prev_sibling
                    }
                }
            }
        }
    }

    /// The layer must be removed from the tree first.
    pub fn delete_layer(&mut self, layer: LayerId) {
        debug_assert!(self.in_transaction());

        // TODO(pcwalton): Use a free list to recycle IDs.
        debug_assert!(self.parent_of(layer).is_none());

        self.tree_component.remove_if_present(layer);
        self.container_component.remove_if_present(layer);
        self.geometry_component.remove_if_present(layer);
        self.surface_component.remove_if_present(layer);

        self.backend.delete_layer(layer);
    }

    // Geometry system

    pub fn layer_bounds(&self, layer: LayerId) -> Rect<f32> {
        debug_assert!(self.in_transaction());

        match self.geometry_component.get(layer) {
            None => Rect::zero(),
            Some(geometry) => geometry.bounds,
        }
    }

    pub fn set_layer_bounds(&mut self, layer: LayerId, new_bounds: &Rect<f32>) {
        debug_assert!(self.in_transaction());

        let old_bounds = mem::replace(&mut self.geometry_component.get_mut_default(layer).bounds,
                                      *new_bounds);

        self.backend.set_layer_bounds(layer,
                                      &old_bounds,
                                      &self.tree_component,
                                      &self.container_component,
                                      &self.geometry_component);
    }

    // Miscellaneous layer flags

    pub fn set_layer_surface_options(&mut self, layer: LayerId, surface_options: SurfaceOptions) {
        debug_assert!(self.in_transaction());

        self.surface_component[layer].options = surface_options;
        self.backend.set_layer_surface_options(layer, &self.surface_component);
    }

    // Surface system

    pub fn bind_layer_to_gl_context(&mut self, layer: LayerId, context: &mut B::GLContext)
                                    -> Result<GLContextLayerBinding, ()> {
        debug_assert!(self.in_transaction());
        debug_assert!(!self.container_component.has(layer));

        self.backend.bind_layer_to_gl_context(layer,
                                              context,
                                              &self.geometry_component,
                                              &self.surface_component)
    }

    pub fn present_gl_context(&mut self, binding: GLContextLayerBinding, changed_rect: &Rect<f32>)
                              -> Result<(), ()> {
        debug_assert!(self.in_transaction());

        self.backend.present_gl_context(binding,
                                        changed_rect,
                                        &self.tree_component,
                                        &self.geometry_component)
    }

    // Screenshots

    pub fn screenshot_hosted_layer(&mut self, layer: LayerId) -> Promise<RgbaImage> {
        debug_assert!(self.in_transaction());
        assert_eq!(self.tree_component[layer].parent, LayerParent::NativeHost);

        let transaction_promise = self.transaction.as_ref().unwrap().promise.clone();
        self.backend.screenshot_hosted_layer(layer,
                                             &transaction_promise,
                                             &self.tree_component,
                                             &self.container_component,
                                             &self.geometry_component,
                                             &self.surface_component)
    }

    // `winit` integration

    #[cfg(feature = "enable-winit")]
    pub fn window(&self) -> Option<&Window> {
        self.backend.window()
    }

    #[cfg(feature = "enable-winit")]
    pub fn host_layer_in_window(&mut self, layer: LayerId) -> Result<(), ()> {
        debug_assert!(self.in_transaction());

        self.tree_component.add(layer, LayerTreeInfo {
            parent: LayerParent::NativeHost,
            prev_sibling: None,
            next_sibling: None,
        });

        self.backend.host_layer_in_window(layer,
                                          &self.tree_component,
                                          &self.container_component,
                                          &self.geometry_component)
    }
}

impl LayerContext<backends::default::Backend> {
    #[inline]
    pub fn new(connection: Connection<<backends::default::Backend as Backend>::NativeConnection>)
               -> Result<LayerContext<backends::default::Backend>, ConnectionError> {
        LayerContext::with_backend_connection(connection)
    }
}

// Errors

pub struct ConnectionError {
    #[cfg(feature = "enable-winit")]
    window_builder: Option<WindowBuilder>,
}

impl Debug for ConnectionError {
    fn fmt(&self, formatter: &mut Formatter) -> Result<(), fmt::Error> {
        "ConnectionError".fmt(formatter)
    }
}

impl ConnectionError {
    #[inline]
    pub fn new() -> ConnectionError {
        ConnectionError {
            #[cfg(feature = "enable-winit")]
            window_builder: None,
        }
    }
}

// Promise infrastructure

impl<T> Promise<T> where T: 'static + Clone + Send {
    fn new() -> Promise<T> {
        Promise(Arc::new(Mutex::new(PromiseData {
            on_fulfilled: vec![],
            on_rejected: vec![],
            result: PromiseResult::Pending,
        })))
    }

    fn all(promises: Vec<Promise<T>>) -> Promise<Vec<T>> {
        let result_promise = Promise::new();
        let all = Arc::new(Mutex::new(All {
            result_promise: result_promise.clone(),
            promises: promises,
            results: vec![],
        }));
        wait(all);
        return result_promise;

        fn wait<T>(all: Arc<Mutex<All<T>>>) where T: 'static + Clone + Send {
            let next_promise;
            {
                let mut all = all.lock().unwrap();
                if all.results.len() == all.promises.len() {
                    let results = mem::replace(&mut all.results, vec![]);
                    all.result_promise.resolve(results);
                    return;
                }
                next_promise = all.promises[all.results.len()].clone();
            }

            next_promise.then(Box::new(move |result| {
                all.lock().unwrap().results.push(result);
                wait(all.clone());
            }));
        }

        struct All<T> where T: 'static + Clone + Send {
            result_promise: Promise<Vec<T>>,
            promises: Vec<Promise<T>>,
            results: Vec<T>,
        }
    }

    pub fn then(&self, mut on_fulfilled: Box<FnMut(T) + Send>) {
        let mut this = self.0.lock().unwrap();
        match this.result {
            PromiseResult::Rejected => {}
            PromiseResult::Resolved(ref result) => on_fulfilled((*result).clone()),
            PromiseResult::Pending => this.on_fulfilled.push(on_fulfilled),
        }
    }

    pub fn or_else(&self, mut on_rejected: Box<FnMut() + Send>) {
        let mut this = self.0.lock().unwrap();
        match this.result {
            PromiseResult::Rejected => on_rejected(),
            PromiseResult::Resolved(_) => {}
            PromiseResult::Pending => this.on_rejected.push(on_rejected),
        }
    }

    fn resolve(&self, result: T) {
        let mut this = self.0.lock().unwrap();
        this.result = PromiseResult::Resolved(result.clone());
        for mut on_fulfilled in this.on_fulfilled.drain(..) {
            on_fulfilled(result.clone())
        }
    }

    fn reject(&self) {
        let mut this = self.0.lock().unwrap();
        this.result = PromiseResult::Rejected;
        for mut on_rejected in this.on_rejected.drain(..) {
            on_rejected()
        }
    }
}

struct TransactionInfo {
    level: u32,
    promise: Promise<()>,
}

// Entity-component system infrastructure

impl<T> LayerMap<T> {
    #[inline]
    fn new() -> LayerMap<T> {
        LayerMap(vec![])
    }

    fn add(&mut self, layer_id: LayerId, element: T) {
        while self.0.len() <= (layer_id.0 as usize) {
            self.0.push(None)
        }
        debug_assert!(self.0[layer_id.0 as usize].is_none());
        self.0[layer_id.0 as usize] = Some(element);
    }

    fn has(&self, layer_id: LayerId) -> bool {
        (layer_id.0 as usize) < self.0.len() && self.0[layer_id.0 as usize].is_some()
    }

    fn take(&mut self, layer_id: LayerId) -> T {
        debug_assert!(self.has(layer_id));
        mem::replace(&mut self.0[layer_id.0 as usize], None).unwrap()
    }

    fn remove(&mut self, layer_id: LayerId) {
        drop(self.take(layer_id))
    }

    fn remove_if_present(&mut self, layer_id: LayerId) {
        if self.has(layer_id) {
            self.remove(layer_id)
        }
    }

    fn get(&self, layer_id: LayerId) -> Option<&T> {
        if (layer_id.0 as usize) >= self.0.len() {
            None
        } else {
            self.0[layer_id.0 as usize].as_ref()
        }
    }

    fn get_mut(&mut self, layer_id: LayerId) -> Option<&mut T> {
        if (layer_id.0 as usize) >= self.0.len() {
            None
        } else {
            self.0[layer_id.0 as usize].as_mut()
        }
    }
}

impl<T> LayerMap<T> where T: Default {
    fn get_mut_default(&mut self, layer_id: LayerId) -> &mut T {
        while self.0.len() <= (layer_id.0 as usize) {
            self.0.push(None)
        }
        if self.0[layer_id.0 as usize].is_none() {
            self.0[layer_id.0 as usize] = Some(T::default());
        }
        self.0[layer_id.0 as usize].as_mut().unwrap()
    }
}

impl<T> Index<LayerId> for LayerMap<T> {
    type Output = T;

    #[inline]
    fn index(&self, layer_id: LayerId) -> &T {
        self.0[layer_id.0 as usize].as_ref().unwrap()
    }
}

impl<T> IndexMut<LayerId> for LayerMap<T> {
    #[inline]
    fn index_mut(&mut self, layer_id: LayerId) -> &mut T {
        self.0[layer_id.0 as usize].as_mut().unwrap()
    }
}

// Specific type infrastructure

impl<'a, N> Connection<'a, N> {
    pub fn into_window(self) -> Option<Window> {
        match self {
            Connection::Native(_) => None,
            #[cfg(feature = "enable-winit")]
            Connection::Winit(window_builder, event_loop) => window_builder.build(event_loop).ok(),
        }
    }
}

// Specific component infrastructure

impl Default for LayerGeometryInfo {
    fn default() -> LayerGeometryInfo {
        LayerGeometryInfo {
            bounds: Rect::zero(),
        }
    }
}
