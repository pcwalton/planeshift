// planeshift/examples/ring.rs

extern crate euclid;
extern crate gleam;
extern crate image;
extern crate offscreen_gl_context as gl_context;
extern crate planeshift;
extern crate winit;

use euclid::{Point2D, Rect, Size2D};
use gleam::gl::{self, GLint, GLsizei, GLsizeiptr, GLuint, Gl, GlType};
use planeshift::Context;
use planeshift::backends::default::Surface;
use self::gl_context::{ColorAttachmentType, GLContext, GLContextAttributes, GLVersion};
use self::gl_context::{NativeGLContext};
use std::f32;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;
use winit::{ControlFlow, Event, EventsLoop, Window, WindowEvent};

const SPRITE_SIZE: u32 = 256;
const SPRITE_COUNT: u32 = 5;
const RING_RADIUS_FACTOR: f32 = 0.25;
const ROTATION_SPEED: f32 = 0.05;
const BACKGROUND_COLOR: [f32; 4] = [0.92, 0.91, 0.92, 1.0];

static SPRITE_IMAGE_PATH: &'static str = "resources/examples/firefox.png";

static VERTEX_SHADER_SOURCE: &'static [u8] = b"
    uniform mat2 uTransform;

    attribute vec2 aPosition;
    attribute vec2 aTexCoord;

    varying vec2 vTexCoord;

    void main() {
        vTexCoord = aTexCoord;
        gl_Position = vec4(uTransform * aPosition, 0.0, 1.0);
    }
";

static FRAGMENT_SHADER_SOURCE: &'static [u8] = b"
    uniform sampler2D uTexture;

    varying vec2 vTexCoord;

    void main() {
        gl_FragColor = texture2D(uTexture, vTexCoord);
    }
";

static VERTEX_DATA: [i8; 16] = [
    -1, -1, 0, 0,
     1, -1, 1, 0,
    -1,  1, 0, 1,
     1,  1, 1, 1,
];

pub fn main() {
    // Load sprite image.
    let sprite_image = image::open(SPRITE_IMAGE_PATH).unwrap().to_rgba();

    // Set up `winit`.
    let mut event_loop = EventsLoop::new();
    let window = Window::new(&event_loop).unwrap();

    // Create a `planeshift` context.
    let mut context = Context::new();
    context.begin_transaction();

    // Get our size.
    let hidpi_factor = window.get_hidpi_factor();
    let window_size = window.get_inner_size().unwrap().to_physical(hidpi_factor);
    let (window_width, window_height): (u32, u32) = window_size.into();

    // Create the root layer.
    let root_layer = context.add_container_layer();
    context.host_layer_in_window(&window, root_layer);
    let root_layer_size = Size2D::new(window_size.width as f32, window_size.height as f32);
    let root_layer_rect = Rect::new(Point2D::zero(), root_layer_size);
    context.set_layer_bounds(root_layer, &root_layer_rect);

    // Create the background layer.
    let background_layer = context.add_surface_layer();
    context.set_layer_bounds(background_layer, &root_layer_rect);
    context.append_child(root_layer, background_layer);

    // Create the surface for the background layer.
    // FIXME(pcwalton): HiDPI.
    let background_surface = Surface::new(&Size2D::new(window_width, window_height));
    context.set_layer_contents(background_layer, background_surface.clone());
    context.set_contents_opaque(background_layer, true);

    // Create the sprite layers.
    let mut sprite_layers = Vec::with_capacity(SPRITE_COUNT as usize);
    let mut sprite_surfaces = Vec::with_capacity(SPRITE_COUNT as usize);
    let sprite_layer_length = ((SPRITE_SIZE as f32) * f32::consts::SQRT_2).ceil() as u32;
    let sprite_layer_size = Size2D::new(sprite_layer_length as f32, sprite_layer_length as f32);
    for _ in 0..SPRITE_COUNT {
        let sprite_layer = context.add_surface_layer();
        let sprite_surface = Surface::new(&sprite_layer_size.ceil().to_u32());
        context.set_layer_bounds(sprite_layer,
                                 &Rect::new(Point2D::new(0.0, 0.0), sprite_layer_size));
        context.set_layer_contents(sprite_layer, sprite_surface.clone());
        context.append_child(root_layer, sprite_layer);
        sprite_surfaces.push(sprite_surface);
        sprite_layers.push(sprite_layer);
    }

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
    let background_framebuffer = Framebuffer::from_surface(gl, &background_surface);
    let sprite_framebuffers: Vec<_> = sprite_surfaces.iter().map(|surface| {
         Framebuffer::from_surface(gl, surface)
    }).collect();

    // Create program.
    let program = gl.create_program();
    let vertex_shader = gl.create_shader(gl::VERTEX_SHADER);
    let fragment_shader = gl.create_shader(gl::FRAGMENT_SHADER);
    gl.shader_source(vertex_shader, &[VERTEX_SHADER_SOURCE]);
    gl.shader_source(fragment_shader, &[FRAGMENT_SHADER_SOURCE]);
    gl.compile_shader(vertex_shader);
    gl.compile_shader(fragment_shader);
    gl.attach_shader(program, vertex_shader);
    gl.attach_shader(program, fragment_shader);
    gl.link_program(program);
    gl.use_program(program);

    // Get program uniform locations.
    let transform_uniform = gl.get_uniform_location(program, "uTransform");
    let texture_uniform = gl.get_uniform_location(program, "uTexture");

    // Create VBO.
    let vbo = gl.gen_buffers(1)[0];
    gl.bind_buffer(gl::ARRAY_BUFFER, vbo);
    gl.buffer_data_untyped(gl::ARRAY_BUFFER,
                           VERTEX_DATA.len() as GLsizeiptr,
                           VERTEX_DATA.as_ptr() as *const _,
                           gl::STATIC_DRAW);

    // Create sprite texture.
    let sprite_texture = gl.gen_textures(1)[0];
    gl.bind_texture(gl::TEXTURE_2D, sprite_texture);
    gl.tex_image_2d(gl::TEXTURE_2D,
                    0,
                    gl::RGBA as GLint,
                    sprite_image.width() as GLsizei,
                    sprite_image.height() as GLsizei,
                    0,
                    gl::RGBA,
                    gl::UNSIGNED_BYTE,
                    Some(&sprite_image));
    gl.tex_parameter_i(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as GLint);
    gl.tex_parameter_i(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as GLint);
    gl.tex_parameter_i(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as GLint);
    gl.tex_parameter_i(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as GLint);

    // Paint background.
    background_framebuffer.bind(gl);
    gl.clear_color(BACKGROUND_COLOR[0],
                   BACKGROUND_COLOR[1],
                   BACKGROUND_COLOR[2],
                   BACKGROUND_COLOR[3]);
    gl.clear(gl::COLOR_BUFFER_BIT);
    gl.flush();

    // Present.
    context.begin_transaction();
    context.refresh_layer_contents(background_layer, &root_layer_rect);
    context.end_transaction();

    // Spawn a thread to deliver animation messages.
    //
    // FIXME(pcwalton): This is a terrible way to do animation timing.
    let event_loop_proxy = event_loop.create_proxy();
    let next_animation_frame = Arc::new(AtomicUsize::new(1));
    let next_animation_frame_copy = next_animation_frame.clone();
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_millis(16));
            next_animation_frame_copy.fetch_add(1, Ordering::SeqCst);
            drop(event_loop_proxy.wakeup());
        }
    });

    let mut animation_frame = 0;
    event_loop.run_forever(move |event| {
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            return ControlFlow::Break
        }

        let next_animation_frame = next_animation_frame.load(Ordering::SeqCst);
        if animation_frame == next_animation_frame {
            return ControlFlow::Continue
        }

        animation_frame = next_animation_frame;

        let center_point = Point2D::new((window_size.width as f32) * 0.5,
                                        (window_size.height as f32) * 0.5);
        let ring_radius = f32::min(window_size.width as f32, window_size.height as f32) *
            RING_RADIUS_FACTOR;
        let time = (animation_frame as f32) * ROTATION_SPEED;

        context.begin_transaction();

        // Paint sprites.
        for (sprite_index, sprite_framebuffer) in sprite_framebuffers.iter().enumerate() {
            let angle = -time + (sprite_index as f32) * f32::consts::PI * 2.0 /
                (SPRITE_COUNT as f32);

            sprite_framebuffer.bind(gl);

            gl.use_program(program);
            gl.bind_buffer(gl::ARRAY_BUFFER, vbo);
            bind_vertex_specification(gl, program);
            gl.active_texture(gl::TEXTURE0);
            gl.bind_texture(gl::TEXTURE_2D, sprite_texture);
            gl.uniform_1i(texture_uniform, 0);
            let sprite_scale_factor = (SPRITE_SIZE as f32) / (sprite_layer_length as f32);
            gl.uniform_matrix_2fv(transform_uniform, false, &[
                sprite_scale_factor * angle.cos(), -sprite_scale_factor * angle.sin(),
                sprite_scale_factor * angle.sin(),  sprite_scale_factor * angle.cos(),
            ]);
            gl.draw_arrays(gl::TRIANGLE_STRIP, 0, 4);

            gl.flush();
        }

        // Update sprite positions, and present.
        for (sprite_index, sprite_layer) in sprite_layers.iter().enumerate() {
            let angle = time + (sprite_index as f32) * f32::consts::PI * 2.0 /
                (SPRITE_COUNT as f32);

            let sprite_position = Point2D::new(
                angle.cos() * ring_radius - sprite_layer_size.width * 0.5 + center_point.x,
                angle.sin() * ring_radius - sprite_layer_size.height * 0.5 + center_point.y);

            context.set_layer_bounds(*sprite_layer,
                                     &Rect::new(sprite_position, sprite_layer_size));
            context.refresh_layer_contents(*sprite_layer,
                                           &Rect::new(Point2D::zero(), sprite_layer_size));
        }

        context.end_transaction();

        ControlFlow::Continue
    });
}

struct Framebuffer {
    #[allow(dead_code)]
    texture: GLuint,
    fbo: GLuint,
    size: Size2D<u32>,
}

impl Framebuffer {
    fn from_surface(gl: &Gl, surface: &Surface) -> Framebuffer {
        let size = surface.size();
        let fbo = gl.gen_framebuffers(1)[0];
        let texture = gl.gen_textures(1)[0];
        surface.bind_to_gl_texture(gl, texture).unwrap();
        gl.bind_framebuffer(gl::FRAMEBUFFER, fbo);
        gl.framebuffer_texture_2d(gl::FRAMEBUFFER,
                                  gl::COLOR_ATTACHMENT0,
                                  gl::TEXTURE_RECTANGLE,
                                  texture,
                                  0);
        Framebuffer {
            size,
            fbo,
            texture,
        }
    }

    fn bind(&self, gl: &Gl) {
        gl.bind_framebuffer(gl::FRAMEBUFFER, self.fbo);
        gl.viewport(0, 0, self.size.width as GLint, self.size.height as GLint);
    }
}

fn bind_vertex_specification(gl: &Gl, program: GLuint) {
    let position_attribute = gl.get_attrib_location(program, "aPosition");
    let tex_coord_attribute = gl.get_attrib_location(program, "aTexCoord");
    gl.vertex_attrib_pointer(position_attribute as GLuint, 2, gl::BYTE, false, 4, 0);
    gl.vertex_attrib_pointer(tex_coord_attribute as GLuint, 2, gl::BYTE, false, 4, 2);
    gl.enable_vertex_attrib_array(position_attribute as GLuint);
    gl.enable_vertex_attrib_array(tex_coord_attribute as GLuint);
}
