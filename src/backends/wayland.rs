// planeshift/src/backends/wayland.rs

//! Wayland native system implementation.

use euclid::{Rect, Size2D};
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs::File;
use std::io::Write;
use std::mem;
use std::os::raw::c_void;
use std::os::unix::io::AsRawFd;
use std::ptr;
use std::sync::{Arc, Mutex};
use tempfile;
use wayland_client::commons::Interface;
use wayland_client::egl::WlEglSurface;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_compositor::RequestsTrait as WlCompositorRequestsTrait;
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_display::RequestsTrait as WlDisplayRequestsTrait;
use wayland_client::protocol::wl_output::Event as WlOutputEvent;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_registry::RequestsTrait as WlRegistryRequestsTrait;
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_shm::RequestsTrait as WlShmRequestsTrait;
use wayland_client::protocol::wl_shm::{Format, WlShm};
use wayland_client::protocol::wl_shm_pool::RequestsTrait as WlShmPoolRequestsTrait;
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_subcompositor::RequestsTrait as WlSubcompositorRequestsTrait;
use wayland_client::protocol::wl_subcompositor::WlSubcompositor;
use wayland_client::protocol::wl_subsurface::RequestsTrait as WlSubsurfaceRequestsTrait;
use wayland_client::protocol::wl_subsurface::WlSubsurface;
use wayland_client::protocol::wl_surface::Event as WlSurfaceEvent;
use wayland_client::protocol::wl_surface::RequestsTrait as WlSurfaceRequestsTrait;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Display, EventQueue, GlobalEvent, GlobalManager, Proxy};
use wayland_sys::client::{WAYLAND_CLIENT_HANDLE, wl_display, wl_proxy};

#[cfg(feature = "enable-winit")]
use winit::Window;
#[cfg(feature = "enable-winit")]
use winit::os::unix::WindowExt;

use crate::egl::types::{EGLContext, EGLDisplay, EGLSurface, EGLint};
use crate::egl;
use crate::{Connection, ConnectionError, GLAPI, GLContextLayerBinding, LayerContainerInfo};
use crate::{LayerGeometryInfo, LayerId, LayerParent, LayerSurfaceInfo, LayerTreeInfo, LayerMap};
use crate::{SurfaceOptions};

pub struct Backend {
    native_component: LayerMap<NativeInfo>,

    dirty_layers: HashSet<LayerId>,
    #[allow(dead_code)]
    zero_file: File,
    output_scales: Arc<Mutex<HashMap<u32, i32>>>,

    #[allow(dead_code)]
    globals: GlobalManager,
    display: Display,
    event_queue: EventQueue,
    #[allow(dead_code)]
    compositor: Proxy<WlCompositor>,
    #[allow(dead_code)]
    subcompositor: Proxy<WlSubcompositor>,
    #[allow(dead_code)]
    shm: Proxy<WlShm>,

    #[allow(dead_code)]
    zero_pool: Proxy<WlShmPool>,
    zero_buffer: Proxy<WlBuffer>,

    egl_display: EGLDisplay,

    window: Option<Window>,
}

impl crate::Backend for Backend {
    type NativeConnection = WaylandConnection;
    type GLContext = GLContext;
    type NativeGLContext = EGLContext;
    type Host = Proxy<WlSurface>;

    // Constructor

    fn new(connection: Connection<WaylandConnection>) -> Result<Backend, ConnectionError> {
        // Unpack the connection if necessary.
        let (mut connection, window) = match connection {
            Connection::Native(wayland_connection) => (wayland_connection, None),
            #[cfg(feature = "enable-winit")]
            Connection::Winit(window_builder, event_queue) => {
                let window = match window_builder.build(event_queue) {
                    Err(_) => return Err(ConnectionError::new()),
                    Ok(window) => window,
                };
                match window.get_wayland_display() {
                    Some(display) => {
                        unsafe {
                            let (display, event_queue) =
                                Display::from_external_display(display as *mut wl_display);
                            (WaylandConnection {
                                display,
                                event_queue,
                            }, Some(window))
                        }
                    }
                    None => return Err(ConnectionError::new())
                }
            }
        };

        // Initialize the output scale map.
        let output_scales = Arc::new(Mutex::new(HashMap::new()));

        // Set up our globals manager.
        let registry = connection.display.get_registry().unwrap();
        let output_scales_c = output_scales.clone();
        let globals = GlobalManager::new_with_cb(registry,
                                                 move |global_event, registry: Proxy<WlRegistry>| {
            if let GlobalEvent::New { id, interface, version } = global_event {
                if interface == "wl_output" {
                    let output_scales = output_scales_c.clone();
                    registry.bind(version, id)
                            .unwrap()
                            .implement(move |output_event, output: Proxy<WlOutput>| {
                        if let WlOutputEvent::Scale { factor } = output_event {
                            let mut output_scales = output_scales.lock().unwrap();
                            output_scales.insert(output.id(), factor);
                        }
                    });
                }
            }
        });

        // Sync to make sure we have all the globals.
        connection.event_queue.sync_roundtrip().unwrap();

        // Grab some references to singletons.
        let compositor: Proxy<WlCompositor> =
            globals.instantiate_auto().unwrap().implement(|_, _| ());
        let subcompositor: Proxy<WlSubcompositor> =
            globals.instantiate_auto().unwrap().implement(|_, _| ());
        let shm: Proxy<WlShm> = globals.instantiate_auto().unwrap().implement(|_, _| ());

        // Open a temporary file so we can supply layer contents for transparent layers.
        let mut zero_file = tempfile::tempfile().unwrap();
        zero_file.write_all(&[0; 4]).unwrap();
        drop(zero_file.flush());
        let zero_pool = shm.create_pool(zero_file.as_raw_fd(), 4).unwrap().implement(|_, _| ());
        let zero_buffer = zero_pool.create_buffer(0, 1, 1, 4, Format::Argb8888)
                                   .unwrap()
                                   .implement(|_, _| ());

        let egl_display;
        unsafe {
            egl::BindAPI(egl::OPENGL_API);

            egl_display = egl::GetDisplay(connection.display.get_display_ptr());

            assert_eq!(egl::Initialize(egl_display, ptr::null_mut(), ptr::null_mut()), egl::TRUE);

            // Load GL functions.
            gl::load_with(|symbol| {
                let symbol = CString::new(symbol.as_bytes()).unwrap();
                egl::GetProcAddress(symbol.as_ptr()) as *const _ as *const c_void
            });
        }

        Ok(Backend {
            native_component: LayerMap::new(),

            dirty_layers: HashSet::new(),
            zero_file,
            output_scales,

            globals,
            display: connection.display,
            event_queue: connection.event_queue,
            compositor,
            subcompositor,
            shm,

            zero_pool,
            zero_buffer,

            egl_display,

            window,
        })
    }

    // OpenGL context creation

    fn create_gl_context(&mut self, options: SurfaceOptions) -> Result<GLContext, ()> {
        unsafe {
            // Enumerate the EGL pixel configurations.
            let (mut configs, mut num_configs) = ([ptr::null(); 64], 0);
            let depth_size = if options.contains(SurfaceOptions::DEPTH) { 16 } else { 0 };
            let stencil_size = if options.contains(SurfaceOptions::STENCIL) { 8 } else { 0 };
            let attributes = [
                egl::SURFACE_TYPE as i32,       egl::WINDOW_BIT as i32,
                egl::RENDERABLE_TYPE as i32,    egl::OPENGL_BIT as i32,
                egl::RED_SIZE as i32,           8,
                egl::GREEN_SIZE as i32,         8,
                egl::BLUE_SIZE as i32,          8,
                egl::ALPHA_SIZE as i32,         8,
                egl::DEPTH_SIZE as i32,         depth_size,
                egl::STENCIL_SIZE as i32,       stencil_size,
                egl::NONE as i32,               egl::NONE as i32,
            ];
            let result = egl::ChooseConfig(self.egl_display,
                                           attributes.as_ptr(),
                                           configs.as_mut_ptr(),
                                           configs.len() as _,
                                           &mut num_configs);
            if result != egl::TRUE {
                return Err(())
            }

            // Choose an EGL pixel configuration.
            //
            // FIXME(pcwalton): Do a better job of making sure we get the right context via
            // `eglGetConfigAttrib()`.
            let config = configs[0];

            // Create an EGL context.
            let attributes = [
                egl::CONTEXT_CLIENT_VERSION as i32, 3,
                egl::NONE as i32,                   egl::NONE as i32,
            ];
            let egl_context = egl::CreateContext(self.egl_display,
                                                 config,
                                                 egl::NO_CONTEXT,
                                                 attributes.as_ptr());
            if egl_context == egl::NO_CONTEXT {
                return Err(())
            }

            self.wrap_gl_context(egl_context)
        }
    }

    unsafe fn wrap_gl_context(&mut self, egl_context: EGLContext) -> Result<GLContext, ()> { 
        Ok(GLContext {
            egl_context,
        })
    }

    fn gl_api(&self) -> GLAPI {
        GLAPI::GLES
    }

    fn begin_transaction(&self) {}

    fn end_transaction(&mut self,
                       tree_component: &LayerMap<LayerTreeInfo>,
                       _: &LayerMap<LayerContainerInfo>,
                       _: &LayerMap<LayerGeometryInfo>,
                       _: &LayerMap<LayerSurfaceInfo>) {
        // Reverse topological sort.
        let (mut commit_order, mut visited) = (vec![], HashSet::new());
        for layer in self.dirty_layers.drain() {
            add_ancestors_to_commit_order(layer,
                                          &mut commit_order,
                                          &mut visited,
                                          tree_component,
                                          &self.native_component);
        }

        // Commit layers in order, children before parents.
        for surface in commit_order.iter() {
            surface.commit();
        }

        self.display.flush().unwrap();
        self.event_queue.dispatch().unwrap();

        fn add_ancestors_to_commit_order<'a>(layer: LayerId,
                                             commit_order: &mut Vec<&'a Proxy<WlSurface>>,
                                             visited: &mut HashSet<LayerId>,
                                             tree_component: &'a LayerMap<LayerTreeInfo>,
                                             native_component: &'a LayerMap<NativeInfo>) {
            if visited.contains(&layer) {
                return
            }
            visited.insert(layer);

            if let Some(ref tree) = tree_component.get(layer) {
                if let LayerParent::Layer(parent) = tree.parent {
                    add_ancestors_to_commit_order(parent,
                                                  commit_order,
                                                  visited,
                                                  tree_component,
                                                  native_component)
                }
            }

            let native_component = &native_component[layer];
            commit_order.push(&native_component.surface);
            if let Some(ref host_surface) = native_component.host_surface {
                commit_order.push(&host_surface.surface);
            }
        }
    }

    fn add_container_layer(&mut self, new_layer: LayerId) {
        self.add_layer(new_layer);
        self.native_component[new_layer].surface.attach(Some(&self.zero_buffer), 0, 0);
    }

    fn add_surface_layer(&mut self, new_layer: LayerId) {
        self.add_layer(new_layer);
    }

    fn delete_layer(&mut self, layer: LayerId) {
        self.native_component.remove_if_present(layer);
        self.dirty_layers.insert(layer);
    }

    fn insert_before(&mut self,
                     parent: LayerId,
                     new_child: LayerId,
                     reference: Option<LayerId>,
                     _: &LayerMap<LayerTreeInfo>,
                     _: &LayerMap<LayerContainerInfo>,
                     _: &LayerMap<LayerGeometryInfo>) {
        let subsurface = self.subcompositor
                             .get_subsurface(&self.native_component[new_child].surface,
                                             &self.native_component[parent].surface)
                             .unwrap()
                             .implement(|_, _| ());

        if let Some(reference) = reference {
            subsurface.place_below(&self.native_component[reference].surface);
            self.dirty_layers.insert(reference);
        }

        self.native_component[new_child].subsurface = Some(subsurface);

        self.dirty_layers.insert(parent);
        self.dirty_layers.insert(new_child);
    }

    fn remove_from_superlayer(&mut self,
                              layer: LayerId,
                              _: LayerId,
                              _: &LayerMap<LayerTreeInfo>,
                              _: &LayerMap<LayerGeometryInfo>) {
        if let Some(subsurface) = mem::replace(&mut self.native_component[layer].subsurface,
                                               None) {
            subsurface.destroy();
        }

        self.dirty_layers.insert(layer);
    }

    unsafe fn host_layer(&mut self,
                         layer: LayerId,
                         host_surface: Proxy<WlSurface>,
                         _: &LayerMap<LayerTreeInfo>,
                         _: &LayerMap<LayerContainerInfo>,
                         _: &LayerMap<LayerGeometryInfo>) {
        let subsurface = self.subcompositor
                             .get_subsurface(&self.native_component[layer].surface, &host_surface)
                             .unwrap()
                             .implement(|_, _| ());

        subsurface.set_position(0, 0);

        host_surface.attach(Some(&self.zero_buffer), 0, 0);

        let native_component = &mut self.native_component[layer];
        native_component.subsurface = Some(subsurface);
        native_component.host_surface = Some(HostSurface {
            surface: host_surface,
        });

        self.dirty_layers.insert(layer);
    }

    fn unhost_layer(&mut self, layer: LayerId) {
        let native_component = &mut self.native_component[layer];
        if let Some(subsurface) = native_component.subsurface.take() {
            subsurface.destroy();
            native_component.host_surface = None;

            self.dirty_layers.insert(layer);
        }
    }

    fn set_layer_bounds(&mut self,
                        layer: LayerId,
                        _: &Rect<f32>,
                        _: &LayerMap<LayerTreeInfo>,
                        _: &LayerMap<LayerContainerInfo>,
                        geometry_component: &LayerMap<LayerGeometryInfo>) {
        let bounds = geometry_component[layer].bounds.round().to_i32();

        if let Some(ref subsurface) = self.native_component[layer].subsurface {
            subsurface.set_position(bounds.origin.x, bounds.origin.y);
        }

        let native_component = &mut self.native_component[layer];
        if native_component.egl_window_size.to_i32() != bounds.size {
            native_component.egl_window.resize(bounds.size.width, bounds.size.height, 0, 0);
            native_component.egl_window_size = bounds.size.to_u32();
            native_component.cached_egl_surface = None;
        }

        self.dirty_layers.insert(layer);
    }

    fn set_layer_surface_options(&mut self, layer: LayerId, _: &LayerMap<LayerSurfaceInfo>) {
        self.dirty_layers.insert(layer);
    }

    fn bind_layer_to_gl_context(&mut self,
                                layer: LayerId,
                                context: &mut Self::GLContext,
                                _: &LayerMap<LayerGeometryInfo>,
                                _: &LayerMap<LayerSurfaceInfo>)
                                -> Result<GLContextLayerBinding, ()> {
        unsafe {
            let native_component = &mut self.native_component[layer];

            let egl_window = &native_component.egl_window;

            let mut config_id = 0;
            assert_eq!(egl::QueryContext(self.egl_display,
                                         context.egl_context,
                                         egl::CONFIG_ID as i32,
                                         &mut config_id),
                       egl::TRUE);

            match native_component.cached_egl_surface {
                Some(ref cached_surface) if cached_surface.config_id == config_id => {}
                _ => {
                    let attributes = [
                        egl::CONFIG_ID as i32,  config_id,
                        egl::NONE as i32,       egl::NONE as i32,
                    ];
                    let (mut config, mut num_configs) = (ptr::null(), 0);
                    assert_eq!(egl::ChooseConfig(self.egl_display,
                                                 attributes.as_ptr(),
                                                 &mut config,
                                                 1,
                                                 &mut num_configs),
                               egl::TRUE);

                    let egl_surface = egl::CreateWindowSurface(self.egl_display,
                                                               config,
                                                               egl_window.ptr() as *mut _,
                                                               ptr::null());
                    assert!(egl_surface != egl::NO_SURFACE);
                    native_component.cached_egl_surface = Some(CachedEGLSurface {
                        egl_surface,
                        config_id,
                    })
                }
            }

            let egl_surface = native_component.cached_egl_surface.as_ref().unwrap().egl_surface;
            debug_assert!(egl_surface != egl::NO_SURFACE);

            if egl::MakeCurrent(self.egl_display, egl_surface, egl_surface, context.egl_context) !=
                    egl::TRUE {
                return Err(())
            }

            self.dirty_layers.insert(layer);

            Ok(GLContextLayerBinding {
                layer,
                framebuffer: 0,
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
            let egl_surface = self.native_component[binding.layer]
                                  .cached_egl_surface
                                  .as_ref()
                                  .unwrap()
                                  .egl_surface;
            debug_assert!(egl_surface != egl::NO_SURFACE);

            if egl::SwapBuffers(self.egl_display, egl_surface) != egl::TRUE {
                return Err(())
            }

            self.dirty_layers.insert(binding.layer);
            Ok(())
        }
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
        match self.window().unwrap().get_wayland_surface() {
            Some(surface) => {
                unsafe {
                    self.host_layer(layer,
                                    Proxy::from_c_ptr(surface as *mut wl_proxy),
                                    tree_component,
                                    container_component,
                                    geometry_component);
                }
                Ok(())
            }
            None => Err(()),
        }
    }
}

impl Backend {
    fn add_layer(&mut self, new_layer: LayerId) {
        let output_scales = self.output_scales.clone();
        let surface = self.compositor
                          .create_surface()
                          .unwrap()
                          .implement(move |event, surface: Proxy<WlSurface>| {
            match event {
                WlSurfaceEvent::Enter {
                    output,
                    ..
                } => {
                    let output_scales = output_scales.lock().unwrap();
                    if let Some(&scale) = output_scales.get(&output.id()) {
                        surface.set_buffer_scale(scale);
                    }
                }
                _ => {}
            }
        });

        surface.attach(Some(&self.zero_buffer), 0, 0);
        let egl_window = WlEglSurface::new(&surface, 1, 1);

        self.native_component.add(new_layer, NativeInfo {
            surface,
            subsurface: None,
            host_surface: None,
            egl_window,
            egl_window_size: Size2D::new(1, 1),
            cached_egl_surface: None,
        });

        self.dirty_layers.insert(new_layer);
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        unsafe {
            egl::Terminate(self.egl_display);
        }
    }
}

pub struct WaylandConnection {
    pub display: Display,
    pub event_queue: EventQueue,
}

pub struct GLContext {
    pub egl_context: EGLContext,
}

struct NativeInfo {
    surface: Proxy<WlSurface>,
    subsurface: Option<Proxy<WlSubsurface>>,
    host_surface: Option<HostSurface>,
    egl_window: WlEglSurface,
    egl_window_size: Size2D<u32>,
    cached_egl_surface: Option<CachedEGLSurface>,
}

struct HostSurface {
    surface: Proxy<WlSurface>,
}

struct CachedEGLSurface {
    egl_surface: EGLSurface,
    config_id: EGLint,
}

trait ProxyExt {
    fn id(&self) -> u32;
}

impl<T> ProxyExt for Proxy<T> where T: Interface {
    fn id(&self) -> u32 {
        unsafe {
            ffi_dispatch!(WAYLAND_CLIENT_HANDLE, wl_proxy_get_id, self.c_ptr())
        }
    }
}
