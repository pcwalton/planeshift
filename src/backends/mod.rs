// planeshift/src/backends/lib.rs

#[cfg(target_os = "macos")]
pub use self::core_animation as default;

#[cfg(target_os = "macos")]
#[path = "core-animation.rs"]
pub mod core_animation;
