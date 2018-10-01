// planeshift/src/lib.rs

extern crate cocoa;
extern crate core_foundation;
extern crate core_graphics;
extern crate euclid;
extern crate gleam;
extern crate io_surface;

#[macro_use]
extern crate objc;

#[cfg(feature = "enable-winit")]
extern crate winit;

use cocoa::base::{YES, id, nil};
use cocoa::quartzcore::{CALayer, transaction};
use core_foundation::base::TCFType;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_graphics::base::CGFloat;
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use euclid::{Rect, Size2D};
use gleam::gl::{self, GLuint, Gl};
use io_surface::IOSurface;
use std::mem;
use std::ops::{Index, IndexMut};

#[cfg(feature = "enable-winit")]
use winit::Window;
#[cfg(all(feature = "enable-winit", target_os = "macos"))]
use winit::os::macos::WindowExt;

pub use self::CoreAnimationSurface as Surface;

pub struct Context<B: Backend> {
    next_layer_id: LayerId,
    transaction_level: u32,

    tree_component: LayerMap<LayerTreeInfo>,
    container_component: LayerMap<LayerContainerInfo>,
    geometry_component: LayerMap<LayerGeometryInfo>,

    backend: B,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LayerId(pub u32);

#[derive(Debug)]
pub struct LayerMap<T>(pub Vec<Option<T>>);

// Backend definition

pub trait Backend {
    type Surface;
    type Host;

    // Constructor
    fn new() -> Self;

    // Transactions
    fn begin_transaction();
    fn end_transaction();

    // Layer creation and destruction
    fn add_container_layer(&mut self, new_layer: LayerId);
    fn add_surface_layer(&mut self, new_layer: LayerId);
    fn delete_layer(&mut self, layer: LayerId);

    // Layer tree management
    fn insert_before(&mut self, parent: LayerId, new_child: LayerId, reference: Option<LayerId>);
    fn remove_from_superlayer(&mut self, layer: LayerId);

    // Native hosting
    fn host_layer(&mut self, layer: LayerId, host: Self::Host);
    fn unhost_layer(&mut self, layer: LayerId);

    // Geometry
    fn set_layer_bounds(&mut self, layer: LayerId, new_bounds: &Rect<f32>);

    // Surface management
    fn set_layer_contents(&mut self, layer: LayerId, new_surface: &Self::Surface);
    fn refresh_layer_contents(&mut self, layer: LayerId, changed_rect: &Rect<f32>);
    fn set_contents_opaque(&mut self, layer: LayerId, opaque: bool);
}

// Components

struct LayerTreeInfo {
    parent: LayerParent,
    prev_sibling: Option<LayerId>,
    next_sibling: Option<LayerId>,
}

struct LayerContainerInfo {
    first_child: Option<LayerId>,
    last_child: Option<LayerId>,
}

struct LayerGeometryInfo {
    bounds: Rect<f32>,
}

// Other data structures

#[derive(PartialEq, Debug)]
pub enum LayerParent {
    Layer(LayerId),
    NativeHost,
}

// Public API for the context

impl<B> Context<B> where B: Backend {
    // Core functions

    pub fn new() -> Context<B> {
        Context {
            next_layer_id: LayerId(0),
            transaction_level: 0,

            tree_component: LayerMap::new(),
            container_component: LayerMap::new(),
            geometry_component: LayerMap::new(),

            backend: Backend::new(),
        }
    }

    pub fn begin_transaction(&mut self) {
        self.transaction_level += 1;

        if self.transaction_level == 1 {
            B::begin_transaction();
        }
    }

    pub fn end_transaction(&mut self) {
        self.transaction_level -= 1;

        if self.transaction_level == 0 {
            B::end_transaction();
        }
    }

    fn in_transaction(&self) -> bool {
        self.transaction_level > 0
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

        self.tree_component.add(new_child, LayerTreeInfo {
            parent: LayerParent::Layer(parent),
            prev_sibling: reference.and_then(|layer| self.tree_component[layer].prev_sibling),
            next_sibling: reference,
        });

        match reference {
            Some(reference) => self.tree_component[reference].next_sibling = Some(new_child),
            None => self.container_component[parent].last_child = Some(new_child),
        }

        if self.tree_component[new_child].prev_sibling.is_none() {
            self.container_component[parent].first_child = Some(new_child)
        }

        self.backend.insert_before(parent, new_child, reference);
    }

    #[inline]
    pub fn append_child(&mut self, parent: LayerId, new_child: LayerId) {
        self.insert_before(parent, new_child, None)
    }

    #[inline]
    pub fn host_layer(&mut self, host: B::Host, layer: LayerId) {
        debug_assert!(self.in_transaction());

        self.tree_component.add(layer, LayerTreeInfo {
            parent: LayerParent::NativeHost,
            prev_sibling: None,
            next_sibling: None,
        });

        self.backend.host_layer(layer, host);
    }

    pub fn remove_from_parent(&mut self, old_child: LayerId) {
        debug_assert!(self.in_transaction());

        let old_tree = self.tree_component.take(old_child);
        match old_tree.parent {
            LayerParent::NativeHost => self.backend.unhost_layer(old_child),

            LayerParent::Layer(parent_layer) => {
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

                self.backend.remove_from_superlayer(old_child);
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

        self.backend.delete_layer(layer);
    }

    // Geometry system

    pub fn set_layer_bounds(&mut self, layer: LayerId, new_bounds: &Rect<f32>) {
        debug_assert!(self.in_transaction());

        self.geometry_component.get_mut_default(layer).bounds = *new_bounds;

        self.backend.set_layer_bounds(layer, new_bounds);
    }

    // Surface system

    pub fn set_layer_contents(&mut self, layer: LayerId, surface: B::Surface) {
        debug_assert!(self.in_transaction());
        debug_assert!(!self.container_component.has(layer));

        self.backend.set_layer_contents(layer, &surface);
    }

    pub fn refresh_layer_contents(&mut self, layer: LayerId, changed_rect: &Rect<f32>) {
        debug_assert!(self.in_transaction());

        self.backend.refresh_layer_contents(layer, changed_rect);
    }

    pub fn set_contents_opaque(&mut self, layer: LayerId, opaque: bool) {
        debug_assert!(self.in_transaction());

        self.backend.set_contents_opaque(layer, opaque);
    }
}

// Core Animation native system implementation

pub struct CoreAnimationBackend {
    surface_component: LayerMap<CoreAnimationSurface>,
    native_component: LayerMap<CoreAnimationNativeInfo>,
}

impl Backend for CoreAnimationBackend {
    type Surface = CoreAnimationSurface;
    type Host = id;

    fn new() -> CoreAnimationBackend {
        CoreAnimationBackend {
            surface_component: LayerMap::new(),
            native_component: LayerMap::new(),
        }
    }

    fn begin_transaction() {
        transaction::begin();

        // Disable implicit animations.
        transaction::set_disable_actions(true);
    }

    fn end_transaction() {
        transaction::commit();
    }

    fn add_container_layer(&mut self, new_layer: LayerId) {
        self.native_component.add(new_layer, CoreAnimationNativeInfo {
            host: nil,
            core_animation_layer: CALayer::new(),
        });
    }

    fn add_surface_layer(&mut self, new_layer: LayerId) {
        // There's no distinction between a container layer and a surface layer in Core Animation.
        self.add_container_layer(new_layer)
    }

    fn delete_layer(&mut self, layer: LayerId) {
        self.surface_component.remove_if_present(layer);
        self.native_component.remove_if_present(layer);
    }

    fn insert_before(&mut self, parent: LayerId, new_child: LayerId, reference: Option<LayerId>) {
        let parent = &self.native_component[parent].core_animation_layer;
        let new_child = &self.native_component[new_child].core_animation_layer;
        match reference {
            None => parent.add_sublayer(new_child),
            Some(reference) => {
                let reference = &self.native_component[reference].core_animation_layer;
                parent.insert_sublayer_below(new_child, reference);
            }
        }
    }

    fn remove_from_superlayer(&mut self, layer: LayerId) {
        self.native_component[layer].core_animation_layer.remove_from_superlayer()
    }

    // Increases the reference count of `hosting_view`.
    fn host_layer(&mut self, layer: LayerId, host: id) {
        let native_component = &mut self.native_component[layer];
        debug_assert_eq!(native_component.host, nil);

        unsafe {
            msg_send![host, retain];
            msg_send![host, setLayer:(native_component.core_animation_layer.id())];
            msg_send![host, setWantsLayer:YES];
        }

        native_component.host = host;
    }

    fn unhost_layer(&mut self, layer: LayerId) {
        let native_component = &mut self.native_component[layer];
        debug_assert_ne!(native_component.host, nil);

        unsafe {
            msg_send![native_component.host, release];
        }

        native_component.host = nil;
    }

    fn set_layer_bounds(&mut self, layer: LayerId, new_bounds: &Rect<f32>) {
        let new_bounds: Rect<CGFloat> = new_bounds.to_f64();
        let new_bounds = CGRect::new(&CGPoint::new(new_bounds.origin.x, new_bounds.origin.y),
                                     &CGSize::new(new_bounds.size.width, new_bounds.size.height));

        self.native_component[layer].core_animation_layer.set_bounds(&new_bounds)
    }

    fn set_layer_contents(&mut self, layer: LayerId, surface: &Self::Surface) {
        unsafe {
            let contents = surface.io_surface.as_CFTypeRef() as id;
            self.native_component[layer].core_animation_layer.set_contents(contents);
        }
    }

    fn refresh_layer_contents(&mut self, layer: LayerId, _: &Rect<f32>) {
        self.native_component[layer].core_animation_layer.reload_value_for_key_path("contents")
    }

    fn set_contents_opaque(&mut self, layer: LayerId, opaque: bool) {
        self.native_component[layer].core_animation_layer.set_opaque(opaque);
        self.native_component[layer].core_animation_layer.set_contents_opaque(opaque);
    }
}

struct CoreAnimationNativeInfo {
    host: id,
    core_animation_layer: CALayer,
}

pub type LayerNativeHost = id;

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

// Specific component infrastructure

impl Default for LayerGeometryInfo {
    fn default() -> LayerGeometryInfo {
        LayerGeometryInfo {
            bounds: Rect::zero(),
        }
    }
}

impl Default for CoreAnimationNativeInfo {
    fn default() -> CoreAnimationNativeInfo {
        CoreAnimationNativeInfo {
            host: nil,
            core_animation_layer: CALayer::new(),
        }
    }
}

impl Drop for CoreAnimationNativeInfo {
    fn drop(&mut self) {
        unsafe {
            if self.host != nil {
                msg_send![self.host, release];
                self.host = nil;
            }
        }
    }
}

// Native surface implementation

#[derive(Clone)]
pub struct CoreAnimationSurface {
    io_surface: IOSurface,
    size: Size2D<u32>,
}

impl CoreAnimationSurface {
    // TODO(pcwalton): Pixel formats?
    pub fn new(size: &Size2D<u32>) -> CoreAnimationSurface {
        const BGRA: u32 = 0x42475241;   // 'BGRA'

        let io_surface = io_surface::new(&CFDictionary::from_CFType_pairs(&[
            (CFString::from("IOSurfaceWidth"), CFNumber::from(size.width as i32).as_CFType()),
            (CFString::from("IOSurfaceHeight"), CFNumber::from(size.height as i32).as_CFType()),
            (CFString::from("IOSurfaceBytesPerElement"), CFNumber::from(4).as_CFType()),
            (CFString::from("IOSurfacePixelFormat"), CFNumber::from(BGRA as i32).as_CFType()),
        ]));

        CoreAnimationSurface {
            io_surface,
            size: *size,
        }
    }

    pub fn bind_to_gl_texture(&self, gl: &Gl, binding: GLuint) -> Result<(), ()> {
        gl.bind_texture(gl::TEXTURE_RECTANGLE, binding);
        self.io_surface.bind_to_gl_texture(self.size.width as i32, self.size.height as i32);
        Ok(())
    }
}

// macOS `winit` integration

#[cfg(all(feature = "winit", target_os = "macos"))]
impl Context<CoreAnimationBackend> {
    pub fn host_layer_in_window(&mut self, window: &Window, layer: LayerId) {
        debug_assert!(self.in_transaction());

        self.host_layer(window.get_nsview() as id, layer)
    }
}
