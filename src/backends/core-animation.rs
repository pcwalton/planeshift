// planeshift/src/backends/core-animation.rs
//
// Copyright © 2018 The Pathfinder Project Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Core Animation native system implementation.

use block::ConcreteBlock;
use cgl::{CGLChoosePixelFormat, CGLContextObj, CGLCreateContext, CGLPixelFormatAttribute};
use cgl::{CGLSetCurrentContext, kCGLNoError, kCGLPFAOpenGLProfile};
use cocoa::base::{NO, YES, id, nil};
use cocoa::foundation::{NSPoint, NSRect, NSSize};
use cocoa::quartzcore::{CALayer, transaction};
use core_foundation::base::TCFType;
use core_foundation::bundle::CFBundle;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_graphics::base::CGFloat;
use core_graphics::geometry::{CG_ZERO_POINT, CGPoint, CGRect, CGSize};
use core_graphics::window::{self, CGWindowID, kCGWindowImageBestResolution};
use core_graphics::window::{kCGWindowImageBoundsIgnoreFraming, kCGWindowListOptionAll};
use euclid::{Rect, Size2D};
use gl::types::{GLint, GLuint};
use gl;
use image::RgbaImage;
use io_surface::IOSurface;
use std::ptr;
use std::sync::Mutex;

#[cfg(feature = "enable-winit")]
use winit::Window;
#[cfg(feature = "enable-winit")]
use winit::os::macos::WindowExt;

use crate::{Connection, ConnectionError, GLAPI, GLContextLayerBinding, LayerContainerInfo};
use crate::{LayerGeometryInfo, LayerId, LayerMap, LayerParent, LayerSurfaceInfo, LayerTreeInfo};
use crate::{Promise, SurfaceOptions};

#[allow(non_upper_case_globals)]
const kCGLOGLPVersion_3_2_Core: CGLPixelFormatAttribute = 0x3200;

static OPENGL_FRAMEWORK_IDENTIFIER: &'static str = "com.apple.opengl";

lazy_static! {
    static ref CREATE_CONTEXT_MUTEX: Mutex<()> = Mutex::new(());
}

pub struct Backend {
    native_component: LayerMap<NativeInfo>,

    #[cfg(feature = "winit")]
    window: Option<Window>,
}

impl crate::Backend for Backend {
    type NativeConnection = ();
    type GLContext = GLContext;
    type NativeGLContext = CGLContextObj;
    type Host = id;

    fn new(connection: Connection<Self::NativeConnection>) -> Result<Backend, ConnectionError> {
        let identifier = CFString::from(OPENGL_FRAMEWORK_IDENTIFIER);
        let bundle = CFBundle::bundle_with_identifier(identifier).unwrap();
        gl::load_with(move |name| bundle.function_pointer_for_name(CFString::from(name)));

        Ok(Backend {
            native_component: LayerMap::new(),

            window: connection.into_window(),
        })
    }

    // TODO(pcwalton): Options.
    fn create_gl_context(&mut self, _: SurfaceOptions) -> Result<GLContext, ()> {
        // Multiple threads can't open a display connection simultaneously, so take a lock here.
        let _lock = CREATE_CONTEXT_MUTEX.lock().unwrap();
        let mut attributes = [kCGLPFAOpenGLProfile, kCGLOGLPVersion_3_2_Core, 0, 0];
        let mut cgl_context = ptr::null_mut();
        unsafe {
            let (mut pixel_format, mut pixel_format_count) = (ptr::null_mut(), 0);
            if CGLChoosePixelFormat(attributes.as_mut_ptr(),
                                    &mut pixel_format,
                                    &mut pixel_format_count) != kCGLNoError {
                return Err(())
            }

            if CGLCreateContext(pixel_format, ptr::null_mut(), &mut cgl_context) != kCGLNoError {
                return Err(())
            }
        }

        unsafe {
            self.wrap_gl_context(cgl_context)
        }
    }

    unsafe fn wrap_gl_context(&mut self, cgl_context: CGLContextObj) -> Result<GLContext, ()> {
        Ok(GLContext {
            cgl_context,
        })
    }

    #[inline]
    fn gl_api(&self) -> GLAPI {
        GLAPI::GL
    }

    fn begin_transaction(&self) {
        transaction::begin();

        // Disable implicit animations.
        transaction::set_disable_actions(true);
    }

    fn end_transaction(&mut self,
                       promise: &Promise<()>,
                       _: &LayerMap<LayerTreeInfo>,
                       _: &LayerMap<LayerContainerInfo>,
                       _: &LayerMap<LayerGeometryInfo>,
                       _: &LayerMap<LayerSurfaceInfo>) {
        let promise = Mutex::new(Some((*promise).clone()));
        transaction::set_completion_block(ConcreteBlock::new(move || {
            (*promise.lock().unwrap()).take().unwrap().resolve(())
        }));

        transaction::commit();
    }

    fn add_container_layer(&mut self, new_layer: LayerId) {
        let layer = CALayer::new();
        layer.set_anchor_point(&CG_ZERO_POINT);

        self.native_component.add(new_layer, NativeInfo {
            host: nil,
            core_animation_layer: layer,
            surface: None,
        });
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

    fn remove_from_superlayer(&mut self,
                              layer: LayerId,
                              _: LayerId,
                              _: &LayerMap<LayerTreeInfo>,
                              _: &LayerMap<LayerGeometryInfo>) {
        self.native_component[layer].core_animation_layer.remove_from_superlayer()
    }

    // Increases the reference count of `hosting_view`.
    unsafe fn host_layer(&mut self,
                         layer: LayerId,
                         host: id,
                         tree_component: &LayerMap<LayerTreeInfo>,
                         container_component: &LayerMap<LayerContainerInfo>,
                         geometry_component: &LayerMap<LayerGeometryInfo>) {
        let native_component = &mut self.native_component[layer];
        debug_assert_eq!(native_component.host, nil);

        let core_animation_layer = &native_component.core_animation_layer;
        msg_send![host, retain];
        msg_send![host, setLayer:core_animation_layer.id()];
        msg_send![host, setWantsLayer:YES];

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
                        _: &Rect<f32>,
                        tree_component: &LayerMap<LayerTreeInfo>,
                        _: &LayerMap<LayerContainerInfo>,
                        geometry_component: &LayerMap<LayerGeometryInfo>) {
        self.update_layer_bounds(layer, tree_component, geometry_component);
    }

    fn set_layer_surface_options(&mut self,
                                 layer: LayerId,
                                 surface_component: &LayerMap<LayerSurfaceInfo>) {
        let surface_options = surface_component[layer].options;

        let core_animation_layer = &mut self.native_component[layer].core_animation_layer;
        let opaque = surface_options.contains(SurfaceOptions::OPAQUE);
        core_animation_layer.set_opaque(opaque);
        core_animation_layer.set_contents_opaque(opaque);
    }

    // TODO(pcwalton): Support depth and stencil!
    fn bind_layer_to_gl_context(&mut self,
                                layer: LayerId,
                                context: &mut Self::GLContext,
                                geometry_component: &LayerMap<LayerGeometryInfo>,
                                _: &LayerMap<LayerSurfaceInfo>)
                                -> Result<GLContextLayerBinding, ()> {
        let native_component = &mut self.native_component[layer];
        let layer_size = geometry_component[layer].bounds.size.round().to_u32();
        unsafe {
            if CGLSetCurrentContext(context.cgl_context) != kCGLNoError {
                return Err(())
            }

            // FIXME(pcwalton): Verify that GL objects belong to the right context!
            if native_component.surface.is_none() ||
                    native_component.surface.as_ref().unwrap().size != layer_size {
                native_component.surface = Some(Surface::new(&layer_size));
            }

            let surface = native_component.surface.as_mut().unwrap();
            let contents = surface.io_surface.as_CFTypeRef() as id;
            native_component.core_animation_layer.set_contents(contents);

            gl::BindTexture(gl::TEXTURE_RECTANGLE, surface.texture);
            surface.io_surface.bind_to_gl_texture(layer_size.width as i32,
                                                  layer_size.height as i32);
            gl::BindFramebuffer(gl::FRAMEBUFFER, surface.framebuffer);
            gl::FramebufferTexture2D(gl::FRAMEBUFFER,
                                     gl::COLOR_ATTACHMENT0,
                                     gl::TEXTURE_RECTANGLE,
                                     surface.texture,
                                     0);

            Ok(GLContextLayerBinding {
                layer,
                framebuffer: surface.framebuffer,
            })
        }
    }

    fn present_gl_context(&mut self,
                          binding: GLContextLayerBinding,
                          _: &Rect<f32>,
                          _: &LayerMap<LayerTreeInfo>,
                          _: &LayerMap<LayerGeometryInfo>)
                          -> Result<(), ()> {
        unsafe {
            gl::Flush();

            if CGLSetCurrentContext(ptr::null_mut()) != kCGLNoError {
                return Err(())
            }
        }

        self.native_component[binding.layer]
            .core_animation_layer
            .reload_value_for_key_path("contents");

        Ok(())
    }

    // Screenshots

    fn screenshot_hosted_layer(&mut self,
                               layer: LayerId,
                               transaction_promise: &Promise<()>,
                               _: &LayerMap<LayerTreeInfo>,
                               _: &LayerMap<LayerContainerInfo>,
                               _: &LayerMap<LayerGeometryInfo>,
                               _: &LayerMap<LayerSurfaceInfo>)
                               -> Promise<RgbaImage> {
        let result_promise = Promise::new();
        let result_promise_to_return = result_promise.clone();

        let hosting_view = self.native_component[layer].host as usize;
        transaction_promise.then(Box::new(move |()| {
            let hosting_view: id = hosting_view as id;
            let image;
            unsafe {
                let view_bounds: NSRect = msg_send![hosting_view, bounds];
                let mut view_frame: NSRect =
                    msg_send![hosting_view, convertRect:view_bounds toView:nil];

                let window: id = msg_send![hosting_view, window];
                let window_id: CGWindowID = msg_send![window, windowNumber];

                let window_frame: NSRect = msg_send![window, frame];
                view_frame.origin.x += window_frame.origin.x;
                view_frame.origin.y += window_frame.origin.y;

                let screen: id = msg_send![window, screen];
                let screen_frame: NSRect = msg_send![screen, frame];
                let screen_rect = CGRect::new(&CGPoint::new(view_frame.origin.x,
                                                            screen_frame.size.height -
                                                            view_frame.origin.y -
                                                            view_frame.size.height),
                                              &CGSize::new(view_frame.size.width,
                                                           view_frame.size.height));

                image = window::create_image(screen_rect,
                                             kCGWindowListOptionAll,
                                             window_id,
                                             kCGWindowImageBoundsIgnoreFraming |
                                             kCGWindowImageBestResolution).unwrap();
            }

            let (width, height) = (image.width() as u32, image.height() as u32);
            let mut data = image.data().bytes().to_vec();
            data.chunks_mut(4).for_each(|pixel| pixel.swap(0, 2));
            result_promise.resolve(RgbaImage::from_vec(width, height, data).unwrap());
        }));

        result_promise_to_return
    }

    // `winit` integration

    #[cfg(feature = "enable-winit")]
    fn window(&self) -> Option<&Window> {
        self.window.as_ref()
    }

    #[cfg(feature = "enable-winit")]
    fn host_layer_in_window(&mut self,
                            layer: LayerId,
                            tree_component: &LayerMap<LayerTreeInfo>,
                            container_component: &LayerMap<LayerContainerInfo>,
                            geometry_component: &LayerMap<LayerGeometryInfo>)
                            -> Result<(), ()> {
        unsafe {
            self.host_layer(layer,
                            self.window().ok_or(())?.get_nsview() as id,
                            tree_component,
                            container_component,
                            geometry_component);
            Ok(())
        }
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

pub struct GLContext {
    cgl_context: CGLContextObj,
}

impl Drop for GLContext {
    fn drop(&mut self) {
        unsafe {
            assert_eq!(cgl::CGLDestroyContext(self.cgl_context), kCGLNoError);
        }
    }
}

struct NativeInfo {
    host: id,
    core_animation_layer: CALayer,
    surface: Option<Surface>,
}

pub type LayerNativeHost = id;

impl Default for NativeInfo {
    fn default() -> NativeInfo {
        NativeInfo {
            host: nil,
            core_animation_layer: CALayer::new(),
            surface: None,
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

struct Surface {
    io_surface: IOSurface,
    framebuffer: GLuint,
    texture: GLuint,
    size: Size2D<u32>,
}

impl Surface {
    // NB: There must be a current context before calling this.
    fn new(size: &Size2D<u32>) -> Surface {
        let io_surface = Surface::create_io_surface(size);

        let (mut framebuffer, mut texture) = (0, 0);
        unsafe {
            gl::GenFramebuffers(1, &mut framebuffer);
            gl::GenTextures(1, &mut texture);
            gl::BindTexture(gl::TEXTURE_RECTANGLE, texture);
            gl::TexParameteri(gl::TEXTURE_RECTANGLE, gl::TEXTURE_MIN_FILTER, gl::LINEAR as GLint);
            gl::TexParameteri(gl::TEXTURE_RECTANGLE, gl::TEXTURE_MAG_FILTER, gl::LINEAR as GLint);
            gl::TexParameteri(gl::TEXTURE_RECTANGLE,
                              gl::TEXTURE_WRAP_S,
                              gl::CLAMP_TO_EDGE as GLint);
            gl::TexParameteri(gl::TEXTURE_RECTANGLE,
                              gl::TEXTURE_WRAP_T,
                              gl::CLAMP_TO_EDGE as GLint);
        }

        Surface {
            io_surface,
            framebuffer,
            texture,
            size: *size,
        }
    }

    fn create_io_surface(size: &Size2D<u32>) -> IOSurface {
        const BGRA: u32 = 0x42475241;   // 'BGRA'

        io_surface::new(&CFDictionary::from_CFType_pairs(&[
            (CFString::from("IOSurfaceWidth"), CFNumber::from(size.width as i32).as_CFType()),
            (CFString::from("IOSurfaceHeight"), CFNumber::from(size.height as i32).as_CFType()),
            (CFString::from("IOSurfaceBytesPerElement"), CFNumber::from(4).as_CFType()),
            (CFString::from("IOSurfacePixelFormat"), CFNumber::from(BGRA as i32).as_CFType()),
        ]))
    }
}
