#![deny(unsafe_code)]

pub mod autostart;
pub mod controller;
pub mod desktop;
pub mod model;

#[allow(unsafe_code, clippy::all)]
pub mod ui {
    slint::include_modules!();
}
