// planeshift/src/backends/core-animation.rs

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

#[cfg(feature = "enable-winit")]
use winit::Window;
#[cfg(all(feature = "enable-winit", target_os = "macos"))]
use winit::os::macos::WindowExt;

use crate::{Context, LayerId, LayerMap};

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
        self.native_component.add(new_layer, NativeInfo {
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
