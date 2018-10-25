// planeshift/examples/ring.rs

extern crate euclid;
extern crate gl;
extern crate image;
extern crate planeshift;
extern crate winit;

use euclid::{Point2D, Rect, Size2D};
use gl::types::{GLboolean, GLchar, GLint, GLsizei, GLsizeiptr, GLuint};
use planeshift::{Connection, GLAPI, LayerContext, SurfaceOptions};
use std::f32;
use std::os::raw::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;
use winit::{ControlFlow, Event, EventsLoop, WindowBuilder, WindowEvent};

const SPRITE_SIZE: u32 = 256;
const SPRITE_COUNT: u32 = 5;
const RING_RADIUS_FACTOR: f32 = 0.25;
const ROTATION_SPEED: f32 = 0.05;
const BACKGROUND_COLOR: [f32; 4] = [0.92, 0.91, 0.92, 1.0];

static SPRITE_IMAGE_PATH: &'static str = "resources/examples/firefox.png";

static VERTEX_SHADER_SOURCE: &'static [u8] = b"
    precision highp float;

    uniform mat2 uTransform;

    in vec2 aPosition;
    in vec2 aTexCoord;

    out vec2 vTexCoord;

    void main() {
        vTexCoord = aTexCoord;
        gl_Position = vec4(uTransform * aPosition, 0.0, 1.0);
    }
";

static FRAGMENT_SHADER_SOURCE: &'static [u8] = b"
    precision highp float;

    uniform sampler2D uTexture;

    in vec2 vTexCoord;

    out vec4 oFragColor;

    void main() {
        oFragColor = texture(uTexture, vTexCoord);
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
    let window = WindowBuilder::new();

    // Create a `planeshift` context.
    let mut context = LayerContext::new(Connection::Winit(window, &event_loop)).unwrap();
    context.begin_transaction();

    // Get our size.
    let hidpi_factor = context.window().unwrap().get_hidpi_factor();
    let window_size = context.window()
                             .unwrap()
                             .get_inner_size()
                             .unwrap()
                             .to_physical(hidpi_factor);

    // Create the root layer.
    let root_layer = context.add_container_layer();
    context.host_layer_in_window(root_layer).unwrap();
    let root_layer_size = Size2D::new(window_size.width as f32, window_size.height as f32);
    let root_layer_rect = Rect::new(Point2D::zero(), root_layer_size);
    context.set_layer_bounds(root_layer, &root_layer_rect);

    // Create the background layer.
    let background_layer = context.add_surface_layer();
    context.set_layer_bounds(background_layer, &root_layer_rect);
    context.append_child(root_layer, background_layer);
    context.set_layer_surface_options(background_layer, SurfaceOptions::OPAQUE);

    // Create the sprite layers.
    let mut sprite_layers = Vec::with_capacity(SPRITE_COUNT as usize);
    let sprite_layer_length = ((SPRITE_SIZE as f32) * f32::consts::SQRT_2).ceil() as u32;
    let sprite_layer_size = Size2D::new(sprite_layer_length as f32, sprite_layer_length as f32);
    for _ in 0..SPRITE_COUNT {
        let sprite_layer = context.add_surface_layer();
        context.set_layer_bounds(sprite_layer,
                                 &Rect::new(Point2D::new(0.0, 0.0), sprite_layer_size));
        context.append_child(root_layer, sprite_layer);
        sprite_layers.push(sprite_layer);
    }

    // Create the GL context.
    let mut gl_context = context.create_gl_context(SurfaceOptions::OPAQUE).unwrap();
    let binding = context.bind_layer_to_gl_context(background_layer, &mut gl_context).unwrap();
    let gl_api = context.gl_api();

    let (program, transform_uniform, texture_uniform);
    let (mut vao, mut vbo, mut sprite_texture) = (0, 0, 0);
    unsafe {
        // Create program.
        program = gl::CreateProgram();
        let vertex_shader = compile_shader(gl::VERTEX_SHADER, VERTEX_SHADER_SOURCE, gl_api);
        let fragment_shader = compile_shader(gl::FRAGMENT_SHADER, FRAGMENT_SHADER_SOURCE, gl_api);
        gl::AttachShader(program, vertex_shader);
        gl::AttachShader(program, fragment_shader);
        gl::LinkProgram(program);
        gl::UseProgram(program);

        // Get program uniform locations.
        transform_uniform = gl::GetUniformLocation(program,
                                                   b"uTransform\0".as_ptr() as *const GLchar);
        texture_uniform = gl::GetUniformLocation(program, b"uTexture\0".as_ptr() as *const GLchar);

        // Create VAO.
        gl::GenVertexArrays(1, &mut vao);
        gl::BindVertexArray(vao);
        gl::UseProgram(program);

        // Create VBO.
        gl::GenBuffers(1, &mut vbo);
        gl::BindBuffer(gl::ARRAY_BUFFER, vbo);
        gl::BufferData(gl::ARRAY_BUFFER,
                       VERTEX_DATA.len() as GLsizeiptr,
                       VERTEX_DATA.as_ptr() as *const _,
                       gl::STATIC_DRAW);

        // Set up VAO.
        let position_attribute = gl::GetAttribLocation(program,
                                                    b"aPosition\0".as_ptr() as *const GLchar);
        let tex_coord_attribute = gl::GetAttribLocation(program,
                                                        b"aTexCoord\0".as_ptr() as *const GLchar);
        gl::VertexAttribPointer(position_attribute as GLuint,
                                2,
                                gl::BYTE,
                                false as GLboolean,
                                4,
                                0 as *const _);
        gl::VertexAttribPointer(tex_coord_attribute as GLuint,
                                2,
                                gl::BYTE,
                                false as GLboolean,
                                4,
                                2 as *const _);
        gl::EnableVertexAttribArray(position_attribute as GLuint);
        gl::EnableVertexAttribArray(tex_coord_attribute as GLuint);

        // Create sprite texture.
        gl::GenTextures(1, &mut sprite_texture);
        gl::BindTexture(gl::TEXTURE_2D, sprite_texture);
        gl::TexImage2D(gl::TEXTURE_2D,
                       0,
                       gl::RGBA as GLint,
                       sprite_image.width() as GLsizei,
                       sprite_image.height() as GLsizei,
                       0,
                       gl::RGBA,
                       gl::UNSIGNED_BYTE,
                       sprite_image.as_ptr() as *const c_void);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as GLint);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as GLint);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as GLint);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as GLint);

        // Paint background.
        gl::Viewport(0, 0, window_size.width.round() as i32, window_size.height.round() as i32);
        gl::ClearColor(BACKGROUND_COLOR[0],
                       BACKGROUND_COLOR[1],
                       BACKGROUND_COLOR[2],
                       BACKGROUND_COLOR[3]);
        gl::Clear(gl::COLOR_BUFFER_BIT);
        gl::Flush();
    }

    // Present background.
    context.present_gl_context(binding, &root_layer_rect).unwrap();
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
        for (sprite_index, &sprite_layer) in sprite_layers.iter().enumerate() {
            let binding = context.bind_layer_to_gl_context(sprite_layer, &mut gl_context).unwrap();

            let angle = -time + (sprite_index as f32) * f32::consts::PI * 2.0 /
                (SPRITE_COUNT as f32);

            unsafe {
                gl::Viewport(0, 0, sprite_layer_length as GLint, sprite_layer_length as GLint);
                gl::ClearColor(0.0, 0.0, 0.0, 0.0);
                gl::Clear(gl::COLOR_BUFFER_BIT);

                gl::BindVertexArray(vao);
                gl::UseProgram(program);
                gl::BindBuffer(gl::ARRAY_BUFFER, vbo);
                gl::ActiveTexture(gl::TEXTURE0);
                gl::BindTexture(gl::TEXTURE_2D, sprite_texture);
                gl::Uniform1i(texture_uniform, 0);
                let sprite_scale_factor = (SPRITE_SIZE as f32) / (sprite_layer_length as f32);
                let transform_matrix = [
                    sprite_scale_factor * angle.cos(), -sprite_scale_factor * angle.sin(),
                    sprite_scale_factor * angle.sin(),  sprite_scale_factor * angle.cos(),
                ];
                gl::UniformMatrix2fv(transform_uniform,
                                     1,
                                     false as GLboolean,
                                     transform_matrix.as_ptr());
                gl::DrawArrays(gl::TRIANGLE_STRIP, 0, 4);

                gl::Flush();
            }

            let angle = time + (sprite_index as f32) * f32::consts::PI * 2.0 /
                (SPRITE_COUNT as f32);

            let sprite_position = Point2D::new(
                angle.cos() * ring_radius - sprite_layer_size.width * 0.5 + center_point.x,
                angle.sin() * ring_radius - sprite_layer_size.height * 0.5 + center_point.y);

            context.set_layer_bounds(sprite_layer, &Rect::new(sprite_position, sprite_layer_size));
            context.present_gl_context(binding, &Rect::new(Point2D::zero(), sprite_layer_size))
                   .unwrap();
        }

        context.end_transaction();

        ControlFlow::Continue
    });
}

fn compile_shader(kind: GLuint, source: &[u8], api: GLAPI) -> GLuint {
    unsafe {
        let preamble = match api {
            GLAPI::GL => b"#version 330\n" as &[u8],
            GLAPI::GLES => b"#version 300 es\n" as &[u8],
        };

        let shader = gl::CreateShader(kind);
        let source_ptrs = [preamble.as_ptr() as *const GLchar, source.as_ptr() as *const GLchar];
        let source_lens = [preamble.len() as GLint, source.len() as GLint];
        gl::ShaderSource(shader, 2, source_ptrs.as_ptr(), source_lens.as_ptr());
        gl::CompileShader(shader);

        let mut status = 0;
        gl::GetShaderiv(shader, gl::COMPILE_STATUS, &mut status);
        if status != gl::TRUE as i32 {
            let (mut info_log, mut info_log_len) = (vec![0u8; 65536], 0);
            gl::GetShaderInfoLog(shader,
                                 info_log.len() as i32,
                                 &mut info_log_len,
                                 info_log.as_mut_ptr() as *mut _);
            info_log.truncate(info_log_len as usize);
            eprintln!("Shader compilation failed: {}", String::from_utf8(info_log).unwrap());
            panic!();
        }

        shader
    }
}
