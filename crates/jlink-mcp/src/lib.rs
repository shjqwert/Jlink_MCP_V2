//! Configuration and MCP process boundaries owned by the MCP crate.

pub mod config;
pub mod discovery;
pub mod mcp;
pub mod runtime;
/// ELF/DWARF symbol indexing and immutable access-plan caching.
pub mod symbols;
pub mod worker_client;
