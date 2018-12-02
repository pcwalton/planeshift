// planeshift/examples/screenshot.rs

extern crate euclid;
extern crate gl;
extern crate planeshift;
extern crate winit;

use euclid::{Point2D, Rect, Size2D};
use gl::types::{GLint, GLuint};
use planeshift::{Connection, LayerContext, SurfaceOptions};
use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use winit::{ControlFlow, Event, EventsLoop, WindowBuilder, WindowEvent};

pub fn main() {
    // Get the output filename.
    let output_path = match env::args().skip(1).next() {
        None => {
            println!("usage: screenshot OUTPUT.png");
            return;
        }
        Some(output_path) => output_path,
    };

    let mut event_loop = EventsLoop::new();
    let window = WindowBuilder::new().with_title("planeshift minimal example");

    let mut context = LayerContext::new(Connection::Winit(window, &event_loop)).unwrap();
    context.begin_transaction();

    let layer = context.add_surface_layer();
    context.host_layer_in_window(layer).unwrap();

    // Get our size.
    let hidpi_factor = context.window().unwrap().get_hidpi_factor();
    let window_size = context
        .window()
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

    // Create the GL context, and draw.
    let mut gl_context = context.create_gl_context(surface_options).unwrap();
    let binding = context
        .bind_layer_to_gl_context(layer, &mut gl_context)
        .unwrap();
    draw(binding.framebuffer, &Size2D::new(width, height));

    // Present.
    let proxy = event_loop.create_proxy();
    let quit_event_loop = Arc::new(AtomicBool::new(false));
    let quit = quit_event_loop.clone();
    context.present_gl_context(binding, &layer_rect).unwrap();
    context
        .screenshot_hosted_layer(layer)
        .then(Box::new(move |image| {
            image.save(output_path.clone()).unwrap();
            quit.store(true, Ordering::SeqCst);
            drop(proxy.wakeup());
        }));
    context.end_transaction();

    // Take a screenshot after the transaction finishes.
    event_loop.run_forever(|event| {
        match event {
            _ if quit_event_loop.load(Ordering::SeqCst) => return ControlFlow::Break,
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => return ControlFlow::Break,
            Event::WindowEvent {
                event: WindowEvent::Refresh,
                ..
            } => {
                // Redraw.
                context.begin_transaction();
                let binding = context
                    .bind_layer_to_gl_context(layer, &mut gl_context)
                    .unwrap();
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
