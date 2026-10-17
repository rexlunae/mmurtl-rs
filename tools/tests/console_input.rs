//! Host tests for the same bounded input queue used by the kernel.
#![allow(dead_code)]
extern crate self as spin;
pub struct Mutex<T>(std::sync::Mutex<T>);
impl<T> Mutex<T> {
    pub const fn new(value: T) -> Self { Self(std::sync::Mutex::new(value)) }
    pub fn lock(&self) -> std::sync::MutexGuard<'_, T> { self.0.lock().unwrap() }
}
mod arch {
    pub fn without_interrupts<R>(f: impl FnOnce() -> R) -> R { f() }
}
#[path = "../../src/keyboard.rs"]
mod keyboard;
