// planeshift/src/backends/lib.rs

#[cfg(all(target_os = "macos", not(feature = "enable-glx-default")))]
pub use self::core_animation as default;
#[cfg(target_family = "windows")]
pub use self::direct_composition as default;
#[cfg(feature = "enable-glx-default")]
pub use self::glx as default;
#[cfg(any(target_os = "linux"))]
pub use self::wayland as default;

#[cfg(target_os = "macos")]
#[path = "core-animation.rs"]
pub mod core_animation;
#[cfg(target_family = "windows")]
#[path = "direct-composition.rs"]
pub mod direct_composition;
#[cfg(feature = "enable-glx")]
pub mod glx;
#[cfg(any(target_os = "linux"))]
pub mod wayland;

// Special backends
pub mod alternate;
