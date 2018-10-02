// planeshift/src/backends/core-animation.rs

use cocoa::base::{NO, YES, id, nil};
use cocoa::foundation::{NSPoint, NSRect, NSSize};
use cocoa::quartzcore::{CALayer, transaction};
use core_foundation::base::TCFType;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_graphics::base::CGFloat;
use core_graphics::geometry::{CG_ZERO_POINT, CGPoint, CGRect, CGSize};
use euclid::{Rect, Size2D};
use gleam::gl::{self, GLuint, Gl};
use io_surface::IOSurface;

#[cfg(feature = "enable-winit")]
use winit::Window;
#[cfg(all(feature = "enable-winit", target_os = "macos"))]
use winit::os::macos::WindowExt;

use crate::{Context, LayerId, LayerContainerInfo, LayerGeometryInfo, LayerMap, LayerParent};
use crate::{LayerTreeInfo};

// Core Animation native system implementation

pub struct Backend {
    surface_component: LayerMap<Surface>,
    native_component: LayerMap<NativeInfo>,
}

impl crate::Backend for Backend {
    type Surface = Surface;
    type Host = id;

    fn new() -> Backend {
        Backend {
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
        let layer = CALayer::new();
        layer.set_anchor_point(&CG_ZERO_POINT);
        self.native_component.add(new_layer, NativeInfo {
            host: nil,
            core_animation_layer: layer,
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

    fn insert_before(&mut self,
                     parent: LayerId,
                     new_child: LayerId,
                     reference: Option<LayerId>,
                     tree_component: &LayerMap<LayerTreeInfo>,
                     container_component: &LayerMap<LayerContainerInfo>,
                     geometry_component: &LayerMap<LayerGeometryInfo>) {
        let parent = &self.native_component[parent].core_animation_layer;
        let new_core_animation_child = &self.native_component[new_child].core_animation_layer;
        match reference {
            None => parent.add_sublayer(new_core_animation_child),
            Some(reference) => {
                let reference = &self.native_component[reference].core_animation_layer;
                parent.insert_sublayer_below(new_core_animation_child, reference);
            }
        }

        self.update_layer_subtree_bounds(new_child,
                                         tree_component,
                                         container_component,
                                         geometry_component);
    }

    fn remove_from_superlayer(&mut self, layer: LayerId) {
        self.native_component[layer].core_animation_layer.remove_from_superlayer()
    }

    // Increases the reference count of `hosting_view`.
    fn host_layer(&mut self,
                  layer: LayerId,
                  host: id,
                  tree_component: &LayerMap<LayerTreeInfo>,
                  container_component: &LayerMap<LayerContainerInfo>,
                  geometry_component: &LayerMap<LayerGeometryInfo>) {
        let native_component = &mut self.native_component[layer];
        debug_assert_eq!(native_component.host, nil);

        let core_animation_layer = &native_component.core_animation_layer;
        unsafe {
            msg_send![host, retain];
            msg_send![host, setLayer:core_animation_layer.id()];
            msg_send![host, setWantsLayer:YES];
        }

        native_component.host = host;

        self.update_layer_subtree_bounds(layer,
                                         tree_component,
                                         container_component,
                                         geometry_component);
    }

    fn unhost_layer(&mut self, layer: LayerId) {
        let native_component = &mut self.native_component[layer];
        debug_assert_ne!(native_component.host, nil);

        unsafe {
            msg_send![native_component.host, setWantsLayer:NO];
            msg_send![native_component.host, setLayer:nil];
            msg_send![native_component.host, release];
        }

        native_component.host = nil;
    }

    fn set_layer_bounds(&mut self,
                        layer: LayerId,
                        tree_component: &LayerMap<LayerTreeInfo>,
                        _: &LayerMap<LayerContainerInfo>,
                        geometry_component: &LayerMap<LayerGeometryInfo>) {
        self.update_layer_bounds(layer, tree_component, geometry_component);
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
        let core_animation_layer = &mut self.native_component[layer].core_animation_layer;
        core_animation_layer.set_opaque(opaque);
        core_animation_layer.set_contents_opaque(opaque);
    }
}

impl Backend {
    fn hosting_view(&self, layer: LayerId, tree_component: &LayerMap<LayerTreeInfo>)
                    -> Option<id> {
        match tree_component.get(layer) {
            None => None,
            Some(LayerTreeInfo { parent: LayerParent::Layer(parent_layer), .. }) => {
                self.hosting_view(*parent_layer, tree_component)
            }
            Some(LayerTreeInfo { parent: LayerParent::NativeHost, .. }) => {
                Some(self.native_component[layer].host)
            }
        }
    }

    fn update_layer_bounds_with_hosting_view(&mut self,
                                             layer: LayerId,
                                             hosting_view: id,
                                             geometry_component: &LayerMap<LayerGeometryInfo>) {
        let new_bounds: Rect<CGFloat> = match geometry_component.get(layer) {
            None => return,
            Some(geometry_info) => geometry_info.bounds.to_f64(),
        };

        let new_appkit_bounds =
            NSRect::new(NSPoint::new(new_bounds.origin.x, new_bounds.origin.y),
                        NSSize::new(new_bounds.size.width, new_bounds.size.height));
        let new_appkit_bounds: NSRect = unsafe {
            msg_send![hosting_view, convertRectFromBacking:new_appkit_bounds]
        };

        let new_core_animation_bounds =
            CGRect::new(&CG_ZERO_POINT,
                        &CGSize::new(new_appkit_bounds.size.width, new_appkit_bounds.size.height));

        let core_animation_layer = &self.native_component[layer].core_animation_layer;
        core_animation_layer.set_bounds(&new_core_animation_bounds);
        core_animation_layer.set_position(&CGPoint::new(new_appkit_bounds.origin.x,
                                                        new_appkit_bounds.origin.y));
    }

    fn update_layer_subtree_bounds_with_hosting_view(
            &mut self,
            layer: LayerId,
            hosting_view: id,
            tree_component: &LayerMap<LayerTreeInfo>,
            container_component: &LayerMap<LayerContainerInfo>,
            geometry_component: &LayerMap<LayerGeometryInfo>) {
        self.update_layer_bounds_with_hosting_view(layer, hosting_view, geometry_component);

        if let Some(container_info) = container_component.get(layer) {
            let mut maybe_kid = container_info.first_child;
            while let Some(kid) = maybe_kid {
                self.update_layer_subtree_bounds_with_hosting_view(kid,
                                                                   hosting_view,
                                                                   tree_component,
                                                                   container_component,
                                                                   geometry_component);
                maybe_kid = tree_component[kid].next_sibling;
            }
        }
    }

    fn update_layer_subtree_bounds(&mut self,
                                   layer: LayerId,
                                   tree_component: &LayerMap<LayerTreeInfo>,
                                   container_component: &LayerMap<LayerContainerInfo>,
                                   geometry_component: &LayerMap<LayerGeometryInfo>) {
        if let Some(hosting_view) = self.hosting_view(layer, tree_component) {
            self.update_layer_subtree_bounds_with_hosting_view(layer,
                                                               hosting_view,
                                                               tree_component,
                                                               container_component,
                                                               geometry_component)
        }
    }

    fn update_layer_bounds(&mut self,
                           layer: LayerId,
                           tree_component: &LayerMap<LayerTreeInfo>,
                           geometry_component: &LayerMap<LayerGeometryInfo>) {
        if let Some(hosting_view) = self.hosting_view(layer, tree_component) {
            self.update_layer_bounds_with_hosting_view(layer, hosting_view, geometry_component)
        }
    }
}

struct NativeInfo {
    host: id,
    core_animation_layer: CALayer,
}

pub type LayerNativeHost = id;

impl Default for NativeInfo {
    fn default() -> NativeInfo {
        NativeInfo {
            host: nil,
            core_animation_layer: CALayer::new(),
        }
    }
}

impl Drop for NativeInfo {
    fn drop(&mut self) {
        unsafe {
            if self.host != nil {
                msg_send![self.host, release];
                self.host = nil;
            }
        }
    }
}
// macOS surface implementation

#[derive(Clone)]
pub struct Surface {
    io_surface: IOSurface,
    size: Size2D<u32>,
}

impl Surface {
    // TODO(pcwalton): Pixel formats?
    pub fn new(size: &Size2D<u32>) -> Surface {
        const BGRA: u32 = 0x42475241;   // 'BGRA'

        let io_surface = io_surface::new(&CFDictionary::from_CFType_pairs(&[
            (CFString::from("IOSurfaceWidth"), CFNumber::from(size.width as i32).as_CFType()),
            (CFString::from("IOSurfaceHeight"), CFNumber::from(size.height as i32).as_CFType()),
            (CFString::from("IOSurfaceBytesPerElement"), CFNumber::from(4).as_CFType()),
            (CFString::from("IOSurfacePixelFormat"), CFNumber::from(BGRA as i32).as_CFType()),
        ]));

        Surface {
            io_surface,
            size: *size,
        }
    }

    #[inline]
    pub fn size(&self) -> Size2D<u32> {
        self.size
    }

    pub fn bind_to_gl_texture(&self, gl: &Gl, binding: GLuint) -> Result<(), ()> {
        gl.bind_texture(gl::TEXTURE_RECTANGLE, binding);
        self.io_surface.bind_to_gl_texture(self.size.width as i32, self.size.height as i32);
        Ok(())
    }
}

// macOS `winit` integration

#[cfg(all(feature = "winit", target_os = "macos"))]
impl Context<Backend> {
    pub fn host_layer_in_window(&mut self, window: &Window, layer: LayerId) {
        debug_assert!(self.in_transaction());

        self.host_layer(window.get_nsview() as id, layer)
    }
}
