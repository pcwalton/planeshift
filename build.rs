#[cfg(any(target_os = "linux", feature = "enable-glx"))]
extern crate gl_generator;

#[cfg(any(target_os = "linux", feature = "enable-glx"))]
use gl_generator::{Registry, Api, Profile, Fallbacks, GlobalGenerator};

use std::env;
use std::fs::File;
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
fn egl() {
    let dest = PathBuf::from(env::var("OUT_DIR").unwrap());

    let mut file = File::create(&dest.join("egl_bindings.rs")).unwrap();
    Registry::new(Api::Egl,
                  (1, 4),
                  Profile::Core,
                  Fallbacks::All,
                  []).write_bindings(gl_generator::StaticGenerator, &mut file).unwrap();

    println!("cargo:rustc-link-lib=EGL");
}

fn main() {
    #[cfg(target_os = "linux")]
    egl();
}
