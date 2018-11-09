// planeshift/src/backends/gl.rs
//
// Copyright © 2018 The Pathfinder Project Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! A fallback backend that renders the layers ourselves using OpenGL.

use euclid::{Point2D, Rect, Size2D};
use gl::types::{GLchar, GLint, GLuint, GLvoid};
use gl;
use image::RgbaImage;
use std::os::raw::c_void;
use std::ptr;

#[cfg(feature = "enable-glutin")]
use glutin::{Api, ContextBuilder, GlContext, GlProfile, GlRequest, GlWindow};
#[cfg(feature = "enable-winit")]
use winit::{EventsLoop, Window, WindowBuilder};

use crate::{Connection, ConnectionError, GLAPI, GLContextLayerBinding, LayerContainerInfo};
use crate::{LayerGeometryInfo, LayerId, LayerMap, LayerParent, LayerSurfaceInfo, LayerTreeInfo};
use crate::{Promise, SurfaceOptions};

// FIXME(pcwalton): Clean up GL resources in destructor.
pub struct Backend {
    native_component: LayerMap<LayerNativeInfo>,

    connection: Box<dyn GLInterface>,
    hosted_layer: Option<LayerId>,
    dirty_rect: Option<Rect<f32>>,

    vertex_shader: GLuint,
    fragment_shader: GLuint,
    program: GLuint,
    uniform_scale: GLint,
    uniform_translation: GLint,
    uniform_depth: GLint,
    uniform_texture: GLint,
    vertex_array: GLuint,
    vertex_buffer: GLuint,
}

impl crate::Backend for Backend {
    type NativeConnection = Box<dyn GLInterface>;
    type GLContext = ();
    type NativeGLContext = ();
    type Host = ();

    // Constructor
    fn new(connection: Connection<Box<dyn GLInterface>>) -> Result<Self, ConnectionError> {
        let connection = match connection {
            #[cfg(feature = "enable-winit")]
            Connection::Winit(window_builder, event_loop) => {
                Box::new(Interface::new(window_builder, event_loop))
            }
            Connection::Native(connection) => connection,
        };

        // Load GL symbols.
        gl::load_with(|name| connection.get_proc_address(name).unwrap_or(ptr::null()));

        connection.make_current();

        let (vertex_shader, fragment_shader, program);
        let (attribute_position, attribute_tex_coord);
        let (uniform_scale, uniform_translation, uniform_depth, uniform_texture);
        let (mut vertex_array, mut vertex_buffer) = (0, 0);
        unsafe {
            gl::GenVertexArrays(1, &mut vertex_array);
            gl::BindVertexArray(vertex_array);

            vertex_shader = create_shader(gl::VERTEX_SHADER, VERTEX_SHADER_SOURCE);
            fragment_shader = create_shader(gl::FRAGMENT_SHADER, FRAGMENT_SHADER_SOURCE);
            program = gl::CreateProgram();
            gl::AttachShader(program, vertex_shader);
            gl::AttachShader(program, fragment_shader);
            gl::LinkProgram(program);
            gl::UseProgram(program);

            attribute_position = gl::GetAttribLocation(program,
                                                       b"aPosition\0".as_ptr() as *const GLchar);
            attribute_tex_coord = gl::GetAttribLocation(program,
                                                        b"aTexCoord\0".as_ptr() as *const GLchar);
            uniform_scale = gl::GetUniformLocation(program, b"uScale\0".as_ptr() as *const GLchar);
            uniform_translation =
                gl::GetUniformLocation(program, b"uTranslation\0".as_ptr() as *const GLchar);
            uniform_depth = gl::GetUniformLocation(program, b"uDepth\0".as_ptr() as *const GLchar);
            uniform_texture = gl::GetUniformLocation(program,
                                                     b"uTexture\0".as_ptr() as *const GLchar);

            gl::GenBuffers(1, &mut vertex_buffer);
            gl::BindBuffer(gl::ARRAY_BUFFER, vertex_buffer);
            gl::BufferData(gl::ARRAY_BUFFER,
                           VERTEX_BUFFER_DATA.len() as isize,
                           VERTEX_BUFFER_DATA.as_ptr() as *const GLvoid,
                           gl::STATIC_DRAW);

            gl::VertexAttribPointer(attribute_tex_coord as GLuint,
                                    2,
                                    gl::BYTE,
                                    gl::FALSE,
                                    4,
                                    2 as *const GLvoid);
            gl::VertexAttribPointer(attribute_position as GLuint,
                                    2,
                                    gl::BYTE,
                                    gl::FALSE,
                                    4,
                                    0 as *const GLvoid);
            gl::EnableVertexAttribArray(attribute_tex_coord as GLuint);
            gl::EnableVertexAttribArray(attribute_position as GLuint);
        }

        Ok(Backend {
            native_component: LayerMap::new(),

            connection,
            hosted_layer: None,
            dirty_rect: None,

            vertex_shader,
            fragment_shader,
            program,
            uniform_scale,
            uniform_translation,
            uniform_depth,
            uniform_texture,
            vertex_array,
            vertex_buffer,
        })
    }

    // OpenGL context creation
    fn create_gl_context(&mut self, _: SurfaceOptions) -> Result<Self::GLContext, ()> {
        Ok(())
    }

    unsafe fn wrap_gl_context(&mut self, _: Self::NativeGLContext) -> Result<Self::GLContext, ()> {
        Ok(())
    }

    fn gl_api(&self) -> GLAPI {
        self.connection.gl_api()
    }

    // Transactions

    fn begin_transaction(&self) {
        self.connection.make_current();
    }

    fn end_transaction(&mut self,
                       promise: &Promise<()>,
                       tree_component: &LayerMap<LayerTreeInfo>,
                       container_component: &LayerMap<LayerContainerInfo>,
                       geometry_component: &LayerMap<LayerGeometryInfo>,
                       surface_component: &LayerMap<LayerSurfaceInfo>) {
        match (self.dirty_rect, self.hosted_layer) {
            (Some(dirty_rect), Some(hosted_layer)) => {
                self.connection.prepare_to_draw();

                // TODO(pcwalton)
                let default_framebuffer = self.connection.default_framebuffer();
                let default_framebuffer_size = self.connection.default_framebuffer_size();

                unsafe {
                    gl::BindVertexArray(self.vertex_array);
                    gl::UseProgram(self.program);
                    gl::BindFramebuffer(gl::FRAMEBUFFER, default_framebuffer);
                    gl::Viewport(0,
                                0,
                                default_framebuffer_size.width as GLint,
                                default_framebuffer_size.height as GLint);

                    gl::ClearDepth(1.0);
                    gl::ClearStencil(0);
                    gl::Clear(gl::DEPTH_BUFFER_BIT | gl::STENCIL_BUFFER_BIT);

                    gl::DepthFunc(gl::LEQUAL);
                    gl::Enable(gl::DEPTH_TEST);
                    gl::Disable(gl::BLEND);

                    let mut depth = 0.0;
                    self.render_opaque_layer_subtree(hosted_layer,
                                                    &Point2D::zero(),
                                                    &mut depth,
                                                    tree_component,
                                                    container_component,
                                                    geometry_component,
                                                    surface_component);

                    gl::Disable(gl::DEPTH_TEST);
                    gl::BlendEquation(gl::FUNC_ADD);
                    gl::BlendFunc(gl::ONE, gl::ONE_MINUS_SRC_ALPHA);
                    gl::Enable(gl::BLEND);

                    self.render_transparent_layer_subtree(hosted_layer,
                                                          &Point2D::zero(),
                                                          &mut depth,
                                                          tree_component,
                                                          container_component,
                                                          geometry_component,
                                                          surface_component);

                    gl::Disable(gl::SCISSOR_TEST);
                    gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
                }

                self.dirty_rect = None;
                promise.resolve(());
                self.connection.present(&dirty_rect);
            }
            (Some(_), None) => {
                self.dirty_rect = None;
                promise.resolve(());
            }
            (None, _) => promise.resolve(()),
        }
    }

    // Layer creation and destruction

    fn add_container_layer(&mut self, _: LayerId) {}

    fn add_surface_layer(&mut self, layer: LayerId) {
        self.native_component.add(layer, LayerNativeInfo {
            framebuffer: None,
        });
    }

    fn delete_layer(&mut self, layer: LayerId) {
        if let Some(native_component) = self.native_component.get_mut(layer) {
            if let Some(ref mut framebuffer) = native_component.framebuffer {
                unsafe {
                    gl::DeleteFramebuffers(1, &mut framebuffer.framebuffer);
                    if let Some(mut renderbuffer) = framebuffer.depth_stencil_renderbuffer {
                        gl::DeleteRenderbuffers(1, &mut renderbuffer);
                    }
                    gl::DeleteTextures(1, &mut framebuffer.color_texture);
                }
            }
        }

        self.native_component.remove_if_present(layer);
    }

    // Layer tree management

    fn insert_before(&mut self,
                     _: LayerId,
                     new_child: LayerId,
                     _: Option<LayerId>,
                     tree_component: &LayerMap<LayerTreeInfo>,
                     _: &LayerMap<LayerContainerInfo>,
                     geometry_component: &LayerMap<LayerGeometryInfo>) {
        let rect = Rect::new(Point2D::zero(), geometry_component[new_child].bounds.size);
        self.invalidate_layer(new_child, &rect, tree_component, geometry_component);
    }

    fn remove_from_superlayer(&mut self,
                              old_child: LayerId,
                              parent: LayerId,
                              tree_component: &LayerMap<LayerTreeInfo>,
                              geometry_component: &LayerMap<LayerGeometryInfo>) {
        let rect = &geometry_component[old_child].bounds;
        self.invalidate_layer(parent, rect, tree_component, geometry_component);
    }

    // Native hosting

    unsafe fn host_layer(&mut self,
                         layer: LayerId,
                         _: Self::Host,
                         _: &LayerMap<LayerTreeInfo>,
                         _: &LayerMap<LayerContainerInfo>,
                         _: &LayerMap<LayerGeometryInfo>) {
        debug_assert!(self.hosted_layer.is_none());
        self.hosted_layer = Some(layer);
    }

    fn unhost_layer(&mut self, layer: LayerId) {
        debug_assert_eq!(self.hosted_layer, Some(layer));
        self.hosted_layer = None;
    }

    // Geometry

    fn set_layer_bounds(&mut self,
                        layer: LayerId,
                        old_bounds: &Rect<f32>,
                        tree_component: &LayerMap<LayerTreeInfo>,
                        _: &LayerMap<LayerContainerInfo>,
                        geometry_component: &LayerMap<LayerGeometryInfo>) {
        if let Some(tree_info) = tree_component.get(layer) {
            match tree_info.parent {
                LayerParent::Layer(parent_layer) => {
                    self.invalidate_layer(parent_layer,
                                          old_bounds,
                                          tree_component,
                                          geometry_component)
                }
                LayerParent::NativeHost => {}
            }
        }

        let new_size = geometry_component[layer].bounds.size;

        if let Some(native_component) = self.native_component.get_mut(layer) {
            if native_component.framebuffer.is_some() {
                let LayerFramebuffer {
                    mut framebuffer,
                    size,
                    ..
                } = native_component.framebuffer.as_ref().unwrap();
                if *size != new_size.round().to_u32() {
                    unsafe {
                        gl::DeleteFramebuffers(1, &mut framebuffer);
                    }
                    native_component.framebuffer = None;
                }
            }
        }

        self.invalidate_layer(layer,
                              &Rect::new(Point2D::zero(), new_size),
                              tree_component,
                              geometry_component);
    }

    // Miscellaneous layer flags

    fn set_layer_surface_options(&mut self, _: LayerId, _: &LayerMap<LayerSurfaceInfo>) {}

    // OpenGL content binding

    fn bind_layer_to_gl_context(&mut self,
                                layer: LayerId,
                                _: &mut Self::GLContext,
                                geometry_component: &LayerMap<LayerGeometryInfo>,
                                surface_component: &LayerMap<LayerSurfaceInfo>)
                                -> Result<GLContextLayerBinding, ()> {
        let native_component = &mut self.native_component[layer];

        if native_component.framebuffer.is_none() {
            let mut framebuffer = LayerFramebuffer {
                color_texture: 0,
                depth_stencil_renderbuffer: None,
                framebuffer: 0,
                size: geometry_component[layer].bounds.round_out().size.to_u32(),
                surface_options: surface_component[layer].options,
            };

            unsafe {
                // Create color texture.
                gl::GenTextures(1, &mut framebuffer.color_texture);
                gl::ActiveTexture(gl::TEXTURE0);
                gl::BindTexture(gl::TEXTURE_2D, framebuffer.color_texture);
                gl::TexImage2D(gl::TEXTURE_2D,
                               0,
                               gl::RGBA as GLint,
                               framebuffer.size.width as GLint,
                               framebuffer.size.height as GLint,
                               0,
                               gl::RGBA,
                               gl::UNSIGNED_BYTE,
                               ptr::null());
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as GLint);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as GLint);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as GLint);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as GLint);

                // Create depth/stencil renderbuffer, if necessary.
                if framebuffer.surface_options
                              .intersects(SurfaceOptions::DEPTH | SurfaceOptions::STENCIL) {
                    let mut renderbuffer = 0;
                    gl::GenRenderbuffers(1, &mut renderbuffer);
                    gl::BindRenderbuffer(gl::RENDERBUFFER, renderbuffer);
                    gl::RenderbufferStorage(gl::RENDERBUFFER,
                                            gl::DEPTH24_STENCIL8,
                                            framebuffer.size.width as GLint,
                                            framebuffer.size.height as GLint);
                    framebuffer.depth_stencil_renderbuffer = Some(renderbuffer);
                }

                // Create FBO.
                gl::GenFramebuffers(1, &mut framebuffer.framebuffer);
                gl::BindFramebuffer(gl::FRAMEBUFFER, framebuffer.framebuffer);
                gl::FramebufferTexture2D(gl::FRAMEBUFFER,
                                         gl::COLOR_ATTACHMENT0,
                                         gl::TEXTURE_2D,
                                         framebuffer.color_texture,
                                         0);
                if let Some(renderbuffer) = framebuffer.depth_stencil_renderbuffer {
                    gl::FramebufferRenderbuffer(gl::FRAMEBUFFER,
                                                gl::DEPTH_STENCIL_ATTACHMENT,
                                                gl::RENDERBUFFER,
                                                renderbuffer);
                }
            }

            native_component.framebuffer = Some(framebuffer);
        }

        let framebuffer = native_component.framebuffer.as_ref().unwrap().framebuffer;

        unsafe {
            gl::BindFramebuffer(gl::FRAMEBUFFER, framebuffer);
        }

        Ok(GLContextLayerBinding {
            layer,
            framebuffer,
        })
    }

    fn present_gl_context(&mut self,
                          binding: GLContextLayerBinding,
                          dirty_rect: &Rect<f32>,
                          tree_component: &LayerMap<LayerTreeInfo>,
                          geometry_component: &LayerMap<LayerGeometryInfo>)
                          -> Result<(), ()> {
        unsafe {
            gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
        }

        self.invalidate_layer(binding.layer, dirty_rect, tree_component, geometry_component);

        Ok(())
    }

    // Screenshots

    fn screenshot_hosted_layer(&mut self,
                               root_layer: LayerId,
                               render_promise: &Promise<()>,
                               tree_component: &LayerMap<LayerTreeInfo>,
                               _: &LayerMap<LayerContainerInfo>,
                               geometry_component: &LayerMap<LayerGeometryInfo>,
                               _: &LayerMap<LayerSurfaceInfo>)
                               -> Promise<RgbaImage> {
        let promise = Promise::new();

        let mut bounds = Rect::new(Point2D::zero(), geometry_component[root_layer].bounds.size);
        let mut layer = root_layer;
        loop {
            bounds.origin += geometry_component[layer].bounds.origin.to_vector();
            match tree_component.get(layer) {
                Some(LayerTreeInfo { parent: LayerParent::Layer(parent), .. }) => layer = *parent,
                Some(_) | None => break,
            }
        }

        let screenshot_info = ScreenshotInfo {
            framebuffer: self.connection.default_framebuffer(),
            bounds: bounds.round().to_u32(),
            promise: promise.clone(),
        };

        render_promise.then(Box::new(move |()| {
            unsafe {
                gl::BindFramebuffer(gl::FRAMEBUFFER, screenshot_info.framebuffer);
                let bounds = screenshot_info.bounds;
                let (width, height) = (bounds.size.width as usize, bounds.size.height as usize);
                let mut pixels = vec![0; width * height * 4];
                gl::ReadPixels(bounds.origin.x as GLint,
                               bounds.origin.y as GLint,
                               bounds.size.width as GLint,
                               bounds.size.height as GLint,
                               gl::RGBA,
                               gl::UNSIGNED_BYTE,
                               pixels.as_mut_ptr() as *mut _);

                // Flip vertically.
                for y0 in 0..(height / 2) {
                    let (start0, start1) = (y0 * width * 4, (height - y0 - 1) * width * 4);
                    for offset in 0..(width * 4) {
                        pixels.swap(start0 + offset, start1 + offset);
                    }
                }

                screenshot_info.promise.resolve(RgbaImage::from_vec(bounds.size.width,
                                                                    bounds.size.height,
                                                                    pixels).unwrap());
            }
        }));

        return promise;

        struct ScreenshotInfo {
            framebuffer: GLuint,
            bounds: Rect<u32>,
            promise: Promise<RgbaImage>,
        }
    }

    // `winit` integration

    #[cfg(feature = "enable-winit")]
    fn window(&self) -> Option<&Window> {
        self.connection.window()
    }

    #[cfg(all(feature = "enable-winit", feature = "enable-glutin"))]
    fn host_layer_in_window(&mut self,
                            layer: LayerId,
                            tree_component: &LayerMap<LayerTreeInfo>,
                            container_component: &LayerMap<LayerContainerInfo>,
                            geometry_component: &LayerMap<LayerGeometryInfo>)
                            -> Result<(), ()> {
        unsafe {
            self.host_layer(layer, (), tree_component, container_component, geometry_component);
            Ok(())
        }
    }

    #[cfg(all(feature = "enable-winit", not(feature = "enable-glutin")))]
    fn connection_from_window(window: &Window) -> Result<Self::Connection, ()> {
        Err(())
    }

    #[cfg(all(feature = "enable-winit", not(feature = "enable-glutin")))]
    fn host_layer_in_window(&mut self,
                            window: &Window,
                            layer: LayerId,
                            tree_component: &LayerMap<LayerTreeInfo>,
                            container_component: &LayerMap<LayerContainerInfo>,
                            geometry_component: &LayerMap<LayerGeometryInfo>)
                            -> Result<(), ()> {
        Err(())
    }
}

impl Backend {
    fn invalidate_layer(&mut self,
                        layer: LayerId,
                        dirty_rect: &Rect<f32>,
                        tree_component: &LayerMap<LayerTreeInfo>,
                        geometry_component: &LayerMap<LayerGeometryInfo>) {
        if let Some(tree_info) = tree_component.get(layer) {
            match tree_info.parent {
                LayerParent::NativeHost => {
                    match self.dirty_rect {
                        None => self.dirty_rect = Some(*dirty_rect),
                        Some(ref mut dirty_rect_ref) => {
                            *dirty_rect_ref = dirty_rect.union(dirty_rect_ref)
                        }
                    }
                }
                LayerParent::Layer(parent) => {
                    let parent_origin = geometry_component[layer].bounds.origin.to_vector();
                    let dirty_rect = dirty_rect.translate(&parent_origin);
                    self.invalidate_layer(parent, &dirty_rect, tree_component, geometry_component)
                }
            }
        }
    }

    fn render_opaque_layer_subtree(&self,
                                   layer: LayerId,
                                   origin: &Point2D<f32>,
                                   next_depth_value: &mut f32,
                                   tree_component: &LayerMap<LayerTreeInfo>,
                                   container_component: &LayerMap<LayerContainerInfo>,
                                   geometry_component: &LayerMap<LayerGeometryInfo>,
                                   surface_component: &LayerMap<LayerSurfaceInfo>) {
        let bounds = geometry_component[layer].bounds;

        // If this is a container layer, don't render anything; just recurse.
        if let Some(container_info) = container_component.get(layer) {
            let new_origin = *origin + bounds.origin.to_vector();
            let mut maybe_kid = container_info.first_child;
            while let Some(kid) = maybe_kid {
                self.render_opaque_layer_subtree(kid,
                                                 &new_origin,
                                                 next_depth_value,
                                                 tree_component,
                                                 container_component,
                                                 geometry_component,
                                                 surface_component);
                maybe_kid = tree_component[kid].next_sibling;
            }
            return
        }

        // Assign a depth value.
        let depth = *next_depth_value;
        *next_depth_value += DEPTH_QUANTUM;

        // Only consider the layers of the appropriate opacity.
        if !surface_component[layer].options.contains(SurfaceOptions::OPAQUE) {
            return
        }

        self.render_layer(layer, origin, depth, geometry_component);
    }

    fn render_transparent_layer_subtree(&self,
                                        layer: LayerId,
                                        origin: &Point2D<f32>,
                                        next_depth_value: &mut f32,
                                        tree_component: &LayerMap<LayerTreeInfo>,
                                        container_component: &LayerMap<LayerContainerInfo>,
                                        geometry_component: &LayerMap<LayerGeometryInfo>,
                                        surface_component: &LayerMap<LayerSurfaceInfo>) {
        let bounds = geometry_component[layer].bounds;

        // If this is a container layer, don't render anything; just recurse.
        if let Some(container_info) = container_component.get(layer) {
            let new_origin = *origin + bounds.origin.to_vector();
            let mut maybe_kid = container_info.last_child;
            while let Some(kid) = maybe_kid {
                self.render_transparent_layer_subtree(kid,
                                                      &new_origin,
                                                      next_depth_value,
                                                      tree_component,
                                                      container_component,
                                                      geometry_component,
                                                      surface_component);
                maybe_kid = tree_component[kid].prev_sibling;
            }
            return
        }

        // Assign a depth value.
        *next_depth_value -= DEPTH_QUANTUM;
        let depth = *next_depth_value;

        // Only consider the layers of the appropriate opacity.
        if surface_component[layer].options.contains(SurfaceOptions::OPAQUE) {
            return
        }

        self.render_layer(layer, origin, depth, geometry_component);
    }

    fn render_layer(&self,
                    layer: LayerId,
                    origin: &Point2D<f32>,
                    depth: f32,
                    geometry_component: &LayerMap<LayerGeometryInfo>) {
        let color_texture = match self.native_component[layer].framebuffer {
            Some(ref framebuffer) => framebuffer.color_texture,
            None => return,
        };

        let bounds = geometry_component[layer].bounds;
        let framebuffer_size = self.connection.default_framebuffer_size().to_f32();

        unsafe {
            // Set uniforms.
            gl::Uniform1f(self.uniform_depth, depth);
            gl::UniformMatrix2fv(self.uniform_scale, 1, gl::FALSE, [
                2.0 * bounds.size.width / framebuffer_size.width, 0.0,
                0.0, 2.0 * bounds.size.height / framebuffer_size.height,
            ].as_ptr());
            gl::Uniform2f(self.uniform_translation,
                          2.0 * (origin.x + bounds.origin.x) / framebuffer_size.width - 1.0,
                          2.0 * (origin.y + bounds.origin.y) / framebuffer_size.height - 1.0);

            // Bind texture.
            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, color_texture);
            gl::Uniform1i(self.uniform_texture, 0);

            // Draw the layer.
            gl::DrawArrays(gl::TRIANGLE_STRIP, 0, 4);
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        unsafe {
            self.connection.make_current();

            gl::DeleteBuffers(1, &mut self.vertex_buffer);
            gl::DeleteVertexArrays(1, &mut self.vertex_array);
            gl::DeleteProgram(self.program);
            gl::DeleteShader(self.fragment_shader);
            gl::DeleteShader(self.vertex_shader);
        }
    }
}

struct LayerNativeInfo {
    framebuffer: Option<LayerFramebuffer>,
}

struct LayerFramebuffer {
    color_texture: GLuint,
    depth_stencil_renderbuffer: Option<GLuint>,
    framebuffer: GLuint,
    size: Size2D<u32>,
    surface_options: SurfaceOptions,
}

pub trait GLInterface {
    fn gl_api(&self) -> GLAPI;

    fn get_proc_address(&self, symbol: &str) -> Option<*const c_void>;
    fn make_current(&self);
    fn prepare_to_draw(&mut self);
    fn present(&mut self, invalid_rect: &Rect<f32>);

    fn default_framebuffer(&self) -> GLuint;
    fn default_framebuffer_size(&self) -> Size2D<u32>;

    #[cfg(feature = "enable-winit")]
    fn window(&self) -> Option<&Window>;
}

struct Interface {
    gl_window: GlWindow,
}

impl Interface {
    fn new(window_builder: WindowBuilder, events_loop: &EventsLoop) -> Interface {
        let context = ContextBuilder::new().with_gl(GlRequest::Specific(Api::OpenGl, (3, 3)))
                                           .with_gl_profile(GlProfile::Core);
        Interface {
            gl_window: GlWindow::new(window_builder, context, events_loop).unwrap(),
        }
    }
}

impl GLInterface for Interface {
    fn gl_api(&self) -> GLAPI {
        GLAPI::GL
    }

    fn get_proc_address(&self, symbol: &str) -> Option<*const c_void> {
        let address = self.gl_window.context().get_proc_address(symbol);
        if address.is_null() {
            None
        } else {
            Some(address as *const c_void)
        }
    }

    fn make_current(&self) {
        unsafe {
            self.gl_window.make_current().unwrap();
        }
    }

    fn prepare_to_draw(&mut self) {}

    fn present(&mut self, _: &Rect<f32>) {
        // TODO(pcwalton): Use the GL extension to swap only a portion of the screen.
        self.gl_window.swap_buffers().unwrap();
    }

    fn default_framebuffer(&self) -> GLuint {
        0
    }

    fn default_framebuffer_size(&self) -> Size2D<u32> {
        let (width, height) = self.gl_window
                                  .get_inner_size()
                                  .unwrap()
                                  .to_physical(self.gl_window.get_hidpi_factor())
                                  .into();
        Size2D::new(width, height)
    }

    #[cfg(feature = "enable-winit")]
    fn window(&self) -> Option<&Window> {
        Some(self.gl_window.window())
    }
}

unsafe fn create_shader(kind: GLuint, source: &[u8]) -> GLuint {
    let shader = gl::CreateShader(kind);
    gl::ShaderSource(shader, 1, &(source.as_ptr() as *const GLchar), &(source.len() as GLint));
    gl::CompileShader(shader);

    let mut compile_status = gl::FALSE as GLint;
    gl::GetShaderiv(shader, gl::COMPILE_STATUS, &mut compile_status);

    if compile_status != gl::TRUE as GLint {
        let (mut log, mut log_len) = (vec![0u8; 65536], 0);
        gl::GetShaderInfoLog(shader,
                             log.len() as GLint,
                             &mut log_len,
                             log.as_mut_ptr() as *mut GLchar);
        log.truncate(log_len as usize);
        eprintln!("Failed to compile shader ({}/{}): {}",
                  log_len,
                  compile_status,
                  String::from_utf8_lossy(&log));
        panic!("Shader compilation failed")
    }

    shader
}

// 4,000 layers should be enough for anybody…
const DEPTH_QUANTUM: f32 = 1.0 / 4096.0;

static VERTEX_BUFFER_DATA: [i8; 16] = [
    0, 0, 0, 0,
    1, 0, 1, 0,
    0, 1, 0, 1,
    1, 1, 1, 1,
];

static VERTEX_SHADER_SOURCE: &'static [u8] = b"\
    #version 330

    uniform mat2 uScale;
    uniform vec2 uTranslation;
    uniform float uDepth;

    in vec2 aPosition;
    in vec2 aTexCoord;

    out vec2 vTexCoord;

    void main() {
        vTexCoord = aTexCoord;
        gl_Position = vec4(uScale * aPosition + uTranslation, uDepth, 1.0);
    }
";

static FRAGMENT_SHADER_SOURCE: &'static [u8] = b"\
    #version 330

    uniform sampler2D uTexture;

    in vec2 vTexCoord;

    out vec4 oFragColor;

    void main() {
        oFragColor = texture(uTexture, vTexCoord);
    }
";
