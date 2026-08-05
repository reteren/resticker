#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(clippy::approx_constant)]
#![allow(clippy::missing_safety_doc)]
#![allow(clippy::redundant_static_lifetimes)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]
// Сгенерированный bindgen-код (bindings.rs подключается ниже): линтеры,
// всплывшие на bindgen 0.72, не имеют отношения к рукописному коду.
#![allow(unnecessary_transmutes)]
#![allow(unpredictable_function_pointer_comparisons)]

extern crate libc;

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

#[macro_use]
mod avutil;
pub use avutil::*;
