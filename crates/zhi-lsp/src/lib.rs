//! Cliente LSP: arranca servidores de lenguaje y aporta contexto de código.
//!
//! Ver `crates/zhi-lsp/AGENTS.md` para el contexto del módulo.

/// Error del crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("error de protocolo LSP: {0}")]
    Protocol(String),
}

pub type Result<T> = std::result::Result<T, Error>;
