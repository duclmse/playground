//! Bytecode-to-Sol decompilation adapters.
//!
//! Sol's tier-0 bytecode currently has no on-disk container, so the Sol input
//! adapter accepts its raw little-endian 32-bit instruction words. Lua binary
//! chunks are deliberately decoded through a matching `luac -l -l`: chunk
//! layouts and opcode tables change between Lua releases, and silently parsing
//! a chunk with the wrong table is worse than an explicit tool-version error.

pub mod lua;
pub mod sol;

use std::fmt;

#[derive(Debug)]
pub enum Error {
    InvalidInput(String),
    Io(std::io::Error),
    Tool(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(message) | Self::Tool(message) => formatter.write_str(message),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
