// planeshift/src/backends/lib.rs

#[cfg(all(target_os = "macos", not(feature = "enable-glx-default")))]
pub use self::core_animation as default;
#[cfg(any(target_os = "linux", feature = "enable-glx-default"))]
pub use self::glx as default;

#[cfg(target_os = "macos")]
#[path = "core-animation.rs"]
pub mod core_animation;
#[cfg(any(target_os = "linux", feature = "enable-glx"))]
pub mod glx;
