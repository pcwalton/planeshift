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

use euclid::Rect;
use std::mem;
use std::ops::{Index, IndexMut};

pub mod backends;

pub struct Context<B = backends::default::Backend> where B: Backend {
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
