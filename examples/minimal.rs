// planeshift/examples/minimal.rs

extern crate euclid;
extern crate gl;
extern crate planeshift;
extern crate winit;

use euclid::{Point2D, Rect, Size2D};
use gl::types::GLint;
use planeshift::{GLContextOptions, LayerContext};
use winit::{ControlFlow, Event, EventsLoop, Window, WindowEvent};

pub fn main() {
    let mut event_loop = EventsLoop::new();
    let window = Window::new(&event_loop).unwrap();

    let mut context = LayerContext::new(());
    context.begin_transaction();

    let layer = context.add_surface_layer();
    context.host_layer_in_window(&window, layer).unwrap();

    // FIXME(pcwalton): HiDPI.
    let window_size = window.get_inner_size().unwrap();
    let (width, height): (u32, u32) = window_size.into();

    let layer_size = Size2D::new(window_size.width as f32, window_size.height as f32);
    let layer_rect = Rect::new(Point2D::zero(), layer_size);
    context.set_layer_bounds(layer, &layer_rect);
    context.set_layer_opaque(layer, true);

    // Create the GL context.
    let mut gl_context = context.create_gl_context(GLContextOptions::empty()).unwrap();
    let binding = context.bind_layer_to_gl_context(layer, &mut gl_context).unwrap();

    unsafe {
        gl::BindFramebuffer(gl::FRAMEBUFFER, binding.framebuffer);

        // Draw.
        gl::Viewport(0, 0, width as GLint, height as GLint);
        gl::ClearColor(0.0, 0.0, 1.0, 1.0);
        gl::Clear(gl::COLOR_BUFFER_BIT);
        gl::Flush();
    }

    // Present.
    context.present_gl_context(binding, &layer_rect).unwrap();
    context.end_transaction();

    event_loop.run_forever(|event| {
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            ControlFlow::Break
        } else {
            ControlFlow::Continue
        }
    });
}
