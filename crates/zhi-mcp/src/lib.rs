//! Cliente de MCP (Model Context Protocol): conecta servidores externos y expone
//! sus tools al agente.
//!
//! Ver `crates/zhi-mcp/AGENTS.md` para el contexto del módulo.

/// Error del crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("error de protocolo MCP: {0}")]
    Protocol(String),
}

pub type Result<T> = std::result::Result<T, Error>;
