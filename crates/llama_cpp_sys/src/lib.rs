//! Raw llama.cpp bindings. The workspace's `llama_cpp` crate provides a safe API.

#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

#[allow(
    unnecessary_transmutes,
    clippy::missing_safety_doc,
    clippy::ptr_offset_with_cast
)]
mod bindings {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

pub use bindings::*;

pub mod common;
