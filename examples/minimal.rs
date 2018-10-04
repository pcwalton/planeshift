// planeshift/examples/minimal.rs

extern crate euclid;
extern crate gleam;
extern crate offscreen_gl_context as gl_context;
extern crate planeshift;
extern crate winit;

use euclid::{Point2D, Rect, Size2D};
use gleam::gl::{self, GLint, GlType};
use self::gl_context::{ColorAttachmentType, GLContext, GLContextAttributes, GLVersion};
use self::gl_context::{NativeGLContext};
use planeshift::Context;
use planeshift::backends::default::{Backend, Surface};
use winit::{ControlFlow, Event, EventsLoop, Window, WindowEvent};

pub fn main() {
    let mut event_loop = EventsLoop::new();
    let window = Window::new(&event_loop).unwrap();

    let mut context = Context::new_default();
    context.begin_transaction();

    let layer = context.add_surface_layer();
    context.host_layer_in_window(&window, layer);

    // FIXME(pcwalton): HiDPI.
    let window_size = window.get_inner_size().unwrap();
    let (width, height): (u32, u32) = window_size.into();
    let surface = Surface::new(&Size2D::new(width, height));

    let layer_size = Size2D::new(window_size.width as f32, window_size.height as f32);
    let layer_rect = Rect::new(Point2D::zero(), layer_size);
    context.set_layer_bounds(layer, &layer_rect);
    context.set_layer_contents(layer, surface.clone());
    context.set_contents_opaque(layer, true);

    context.end_transaction();

    // Create the GL context.
    let gl_context: GLContext<NativeGLContext> = GLContext::new(Size2D::new(1, 1),
                                                                GLContextAttributes::default(),
                                                                ColorAttachmentType::default(),
                                                                GlType::default(),
                                                                GLVersion::Major(2),
                                                                None).unwrap();
    gl_context.make_current().unwrap();

    let gl = gl_context.gl();
    let fbo = gl.gen_framebuffers(1)[0];
    let surface_texture = gl.gen_textures(1)[0];
    surface.bind_to_gl_texture(gl, surface_texture).unwrap();
    gl.bind_framebuffer(gl::FRAMEBUFFER, fbo);
    gl.framebuffer_texture_2d(gl::FRAMEBUFFER,
                              gl::COLOR_ATTACHMENT0,
                              gl::TEXTURE_RECTANGLE,
                              surface_texture,
                              0);

    // Draw.
    gl.viewport(0, 0, width as GLint, height as GLint);
    gl.clear_color(0.0, 0.0, 1.0, 1.0);
    gl.clear(gl::COLOR_BUFFER_BIT);
    gl.flush();

    // Present.
    context.begin_transaction();
    context.refresh_layer_contents(layer, &layer_rect);
    context.end_transaction();

    event_loop.run_forever(|event| {
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            ControlFlow::Break
        } else {
            ControlFlow::Continue
        }
    });
}
