// planeshift/src/backends/lib.rs
//
// Copyright © 2018 The Pathfinder Project Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#[cfg(target_os = "macos")]
pub use self::core_animation as default;
#[cfg(target_family = "windows")]
pub use self::direct_composition as default;
#[cfg(target_os = "linux")]
pub use self::wayland as default;

#[cfg(target_os = "macos")]
#[path = "core-animation.rs"]
pub mod core_animation;
#[cfg(target_family = "windows")]
#[path = "direct-composition.rs"]
pub mod direct_composition;
#[cfg(any(target_os = "linux"))]
pub mod wayland;

// Special backends
pub mod alternate;
pub mod gl;
