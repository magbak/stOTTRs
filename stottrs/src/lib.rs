#[cfg(test)]
#[macro_use]
extern crate unic_char_range;
extern crate core;

extern crate chrono;
extern crate chrono_tz;

use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

mod ast;
mod constants;
mod parsing;
mod resolver;

pub mod document;
pub mod errors;
pub mod mapping;
pub mod templates;
pub mod triplestore;
pub(crate) mod literals;
pub(crate) mod io_funcs;
