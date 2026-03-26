//! Auto-generated FFI operation handlers.
//!
//! The build.rs script parses `divinity_ffi.h` and generates Phone handler
//! registrations for every dispatchable `divi_*` function. This module
//! includes that generated code.
//!
//! Call `register_all()` BEFORE registering hand-written override modules,
//! so that Rust-native handlers can replace FFI handlers for complex ops.

#[allow(unused_unsafe, clippy::all)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/ffi_ops_generated.rs"));
}

pub use generated::register_all;
