// planeshift/examples/minimal.rs

extern crate euclid;
extern crate gl;
extern crate planeshift;
extern crate winit;

use euclid::{Point2D, Rect, Size2D};
use gl::types::{GLint, GLuint};
use planeshift::{Connection, LayerContext, SurfaceOptions};
use winit::{ControlFlow, Event, EventsLoop, WindowBuilder, WindowEvent};

pub fn main() {
    let mut event_loop = EventsLoop::new();
    let window = WindowBuilder::new().with_title("planeshift minimal example");

    let mut context = LayerContext::new(Connection::Winit(window, &event_loop)).unwrap();
    context.begin_transaction();

    let layer = context.add_surface_layer();
    context.host_layer_in_window(layer).unwrap();

    // Get our size.
    let hidpi_factor = context.window().unwrap().get_hidpi_factor();
    let window_size = context.window()
                             .unwrap()
                             .get_inner_size()
                             .unwrap()
                             .to_physical(hidpi_factor);
    let (width, height): (u32, u32) = window_size.into();

    let layer_size = Size2D::new(window_size.width as f32, window_size.height as f32);
    let layer_rect = Rect::new(Point2D::zero(), layer_size);
    context.set_layer_bounds(layer, &layer_rect);
    let surface_options = SurfaceOptions::OPAQUE;
    context.set_layer_surface_options(layer, surface_options);

    // Create the GL context.
    let mut gl_context = context.create_gl_context(surface_options).unwrap();

    // Draw.
    let binding = context.bind_layer_to_gl_context(layer, &mut gl_context).unwrap();
    draw(binding.framebuffer, &Size2D::new(width, height));
    context.present_gl_context(binding, &layer_rect).unwrap();
    context.end_transaction();

    event_loop.run_forever(|event| {
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                return ControlFlow::Break
            }
            Event::WindowEvent { event: WindowEvent::Refresh, .. } => {
                // Redraw.
                context.begin_transaction();
                let binding = context.bind_layer_to_gl_context(layer, &mut gl_context).unwrap();
                draw(binding.framebuffer, &Size2D::new(width, height));
                context.present_gl_context(binding, &layer_rect).unwrap();
                context.end_transaction();
            }
            _ => {}
        }

        ControlFlow::Continue
    });
}

fn draw(fbo: GLuint, size: &Size2D<u32>) {
    unsafe {
        gl::BindFramebuffer(gl::FRAMEBUFFER, fbo);
        gl::Viewport(0, 0, size.width as GLint, size.height as GLint);
        gl::ClearColor(0.0, 0.0, 1.0, 1.0);
        gl::Clear(gl::COLOR_BUFFER_BIT);
        gl::Flush();
    }
}
