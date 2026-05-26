//! Flujo OAuth 2.0 con PKCE para "conectar" cuentas de proveedores.
//!
//! La implementación concreta de **OpenAI/ChatGPT** vive en [`openai`], que
//! replica el plugin `codex` de OpenCode. Los helpers genéricos (PKCE,
//! servidor local de callback) están en [`pkce`] y [`server`]. Ver
//! [ADR-0010].

pub mod openai;
pub mod pkce;
pub mod server;

pub use openai::{OpenAiBrowserFlow, OpenAiOauth};
pub use pkce::Pkce;
pub use server::{BoundServer, CallbackResult, LocalCallbackServer};
