#[cfg(any(target_os = "linux", feature = "enable-glx"))]
extern crate gl_generator;

#[cfg(any(target_os = "linux", feature = "enable-glx"))]
use gl_generator::{Registry, Api, Profile, Fallbacks, GlobalGenerator};

use std::env;
use std::fs::File;
use std::path::{Path, PathBuf};

#[cfg(any(target_os = "linux", feature = "enable-glx"))]
fn glx() {
    let dest = PathBuf::from(env::var("OUT_DIR").unwrap());

    let mut file = File::create(&dest.join("glx_bindings.rs")).unwrap();
    Registry::new(Api::Glx,
                  (1, 4),
                  Profile::Core,
                  Fallbacks::All,
                  []).write_bindings(gl_generator::StaticGenerator, &mut file).unwrap();

    let mut file = File::create(&dest.join("glx_extra_bindings.rs")).unwrap();
    Registry::new(Api::Glx,
                  (1, 4),
                  Profile::Core,
                  Fallbacks::All,
                  ["GLX_ARB_create_context"]).write_bindings(gl_generator::StructGenerator,
                                                             &mut file).unwrap();

    println!("cargo:rustc-link-lib=GL");
}

fn main() {
    #[cfg(any(target_os = "linux", feature = "enable-glx"))]
    glx();
}
