//! Motor de xiě-code: orquesta el bucle de agente y posee los tipos de dominio.
//! Agnóstico de la UI — este crate no conoce GTK.
//!
//! Ver `crates/zhi-core/AGENTS.md` y `docs/architecture.md` para el contexto.
//!
//! Fase 1 (MVP de chat): un único proveedor (DeepSeek) y el streaming de un turno.
//! Fase 2 (persistencia): proyectos, sesiones y mensajes en SQLite (módulo
//! [`store`]); las sesiones son reanudables.
//! Fase 3 (tools y permisos): bucle de agente que invoca tools de `zhi-tool`,
//! reinyecta sus resultados y resuelve permisos vía un [`PermissionResolver`].
//! Ver [ADR-0007](../../docs/decisions/0007-tools-permisos-bucle-agente.md).

pub mod store;

use std::pin::Pin;
use std::sync::Arc;

use async_stream::try_stream;
use async_trait::async_trait;
use futures::{Stream, StreamExt};

pub use store::{SessionMeta, Store};
pub use zhi_provider::{EventStream, Message, Role, StreamEvent, ToolCall};
pub use zhi_tool::{ToolContext, ToolRegistry};

/// Error del crate. Agrega los errores de los subsistemas del motor.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Provider(#[from] zhi_provider::Error),
    #[error(transparent)]
    Tool(#[from] zhi_tool::Error),
    #[error(transparent)]
    Mcp(#[from] zhi_mcp::Error),
    #[error(transparent)]
    Lsp(#[from] zhi_lsp::Error),
    #[error("error de persistencia: {0}")]
    Storage(#[from] sqlx::Error),
    #[error("falta la variable de entorno DEEPSEEK_API_KEY")]
    MissingApiKey,
}

pub type Result<T> = std::result::Result<T, Error>;

/// Prompt de sistema por defecto del agente.
const SYSTEM_PROMPT: &str = "Eres xiě-code, un asistente de programación útil y conciso. \
Operas sobre el directorio de trabajo del usuario y dispones de tools para leer, \
escribir y editar archivos, buscar (glob/grep) y ejecutar comandos de shell. \
Usa las tools cuando necesites inspeccionar o modificar el proyecto, y explica lo \
que haces. Respondes en el idioma del usuario y usas Markdown cuando ayuda.";

/// Límite de iteraciones del bucle de agente por turno (cortafuegos anti-bucle).
const MAX_STEPS: usize = 16;

// ── Permisos ─────────────────────────────────────────────────────────────────

/// Solicitud de autorización para ejecutar una tool con efectos.
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    pub tool_name: String,
    /// Argumentos de la llamada, formateados para mostrarlos al usuario.
    pub arguments: String,
}

/// Decisión del usuario ante una solicitud de permiso.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Deny,
}

/// Resuelve solicitudes de permiso, normalmente preguntando en la UI.
///
/// El motor invoca [`PermissionResolver::resolve`] **antes** de ejecutar una tool
/// que lo requiere y espera la decisión. La implementación de la UI vive en
/// `zhi-gtk` (canal `oneshot` hacia el hilo de GLib).
#[async_trait]
pub trait PermissionResolver: Send + Sync {
    async fn resolve(&self, request: PermissionRequest) -> PermissionDecision;
}

/// Resolver que concede todo sin preguntar (útil para tests y modos no
/// interactivos).
pub struct AllowAll;

#[async_trait]
impl PermissionResolver for AllowAll {
    async fn resolve(&self, _request: PermissionRequest) -> PermissionDecision {
        PermissionDecision::Allow
    }
}

// ── Eventos del bucle de agente ────────────────────────────────────────────────

/// Evento emitido por el bucle de agente durante un turno.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Fragmento incremental de texto del asistente.
    Delta(String),
    /// El modelo va a ejecutar una tool (tras conceder el permiso si lo requería).
    ToolStarted { name: String, arguments: String },
    /// Una tool terminó; `ok` indica si tuvo éxito.
    ToolFinished {
        name: String,
        output: String,
        ok: bool,
    },
    /// Fin del turno: los mensajes producidos (asistente, tool_calls, resultados)
    /// para que la UI los persista y extienda la sesión.
    Turn(Vec<Message>),
}

/// Stream de eventos de un turno del agente.
pub type AgentStream = Pin<Box<dyn Stream<Item = Result<AgentEvent>> + Send>>;

// ── Motor ──────────────────────────────────────────────────────────────────────

/// El motor: posee el proveedor LLM y el registro de tools; orquesta los turnos.
#[derive(Clone)]
pub struct Engine {
    provider: zhi_provider::DeepSeek,
    tools: ToolRegistry,
}

impl Engine {
    /// Construye el motor leyendo la clave de DeepSeek de `DEEPSEEK_API_KEY` y
    /// registrando las tools integradas.
    pub fn from_env() -> Result<Self> {
        let key = std::env::var("DEEPSEEK_API_KEY").map_err(|_| Error::MissingApiKey)?;
        Ok(Self {
            provider: zhi_provider::DeepSeek::new(key),
            tools: ToolRegistry::with_builtins(),
        })
    }

    /// Ejecuta un turno completo del agente: llama al proveedor, ejecuta las tools
    /// que solicite (resolviendo permisos), reinyecta los resultados y repite
    /// hasta que el modelo cierra el turno. Devuelve el stream de eventos.
    pub fn run_turn(
        &self,
        history: Vec<Message>,
        ctx: ToolContext,
        resolver: Arc<dyn PermissionResolver>,
    ) -> AgentStream {
        let provider = self.provider.clone();
        let registry = self.tools.clone();
        let tool_specs: Vec<zhi_provider::ToolSpec> = registry
            .iter()
            .map(|t| {
                zhi_provider::ToolSpec::function(t.name(), t.description(), t.parameters_schema())
            })
            .collect();

        let stream = try_stream! {
            let mut messages = Vec::with_capacity(history.len() + 1);
            messages.push(Message::system(SYSTEM_PROMPT));
            messages.extend(history);
            let mut appended: Vec<Message> = Vec::new();

            for _ in 0..MAX_STEPS {
                let mut inner = provider.stream_chat(messages.clone(), tool_specs.clone()).await?;
                let mut text = String::new();
                let mut calls: Vec<ToolCall> = Vec::new();

                while let Some(event) = inner.next().await {
                    match event? {
                        StreamEvent::Delta(d) => {
                            text.push_str(&d);
                            yield AgentEvent::Delta(d);
                        }
                        StreamEvent::ToolCalls(c) => calls = c,
                    }
                }

                if calls.is_empty() {
                    if !text.is_empty() {
                        let msg = Message::assistant(text);
                        messages.push(msg.clone());
                        appended.push(msg);
                    }
                    break;
                }

                // El asistente solicita tools: se registra el mensaje con las calls.
                let assistant_msg = Message::assistant_tool_calls(text, calls.clone());
                messages.push(assistant_msg.clone());
                appended.push(assistant_msg);

                for call in calls {
                    let name = call.function.name.clone();
                    let pretty = pretty_args(&call.function.arguments);
                    yield AgentEvent::ToolStarted {
                        name: name.clone(),
                        arguments: pretty.clone(),
                    };

                    let needs_permission = registry
                        .get(&name)
                        .map(|t| t.requires_permission())
                        .unwrap_or(false);
                    let decision = if needs_permission {
                        resolver
                            .resolve(PermissionRequest {
                                tool_name: name.clone(),
                                arguments: pretty,
                            })
                            .await
                    } else {
                        PermissionDecision::Allow
                    };

                    let (output, ok) = if decision == PermissionDecision::Deny {
                        ("El usuario denegó el permiso para ejecutar esta tool.".to_string(), false)
                    } else {
                        let result = match registry.get(&name) {
                            Some(tool) => {
                                let args = serde_json::from_str(&call.function.arguments)
                                    .unwrap_or(serde_json::Value::Null);
                                tool.execute(args, &ctx).await
                            }
                            None => Err(zhi_tool::Error::InvalidArguments(format!(
                                "tool desconocida: {name}"
                            ))),
                        };
                        match result {
                            Ok(out) => (out, true),
                            Err(e) => (format!("Error: {e}"), false),
                        }
                    };

                    yield AgentEvent::ToolFinished {
                        name,
                        output: output.clone(),
                        ok,
                    };
                    let result_msg = Message::tool_result(call.id, output);
                    messages.push(result_msg.clone());
                    appended.push(result_msg);
                }
            }

            yield AgentEvent::Turn(appended);
        };

        Box::pin(stream)
    }
}

/// Formatea los argumentos JSON de una tool para mostrarlos legibles; si no
/// parsean, devuelve el texto crudo.
fn pretty_args(raw: &str) -> String {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| raw.to_string())
}

/// Una conversación en memoria. Mantiene el historial de mensajes de la sesión.
#[derive(Debug, Default, Clone)]
pub struct Session {
    messages: Vec<Message>,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reconstruye una sesión a partir de un historial persistido.
    pub fn from_messages(messages: Vec<Message>) -> Self {
        Self { messages }
    }

    /// `true` si la sesión aún no tiene mensajes de usuario/asistente.
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Historial actual, listo para enviarse al motor.
    pub fn history(&self) -> Vec<Message> {
        self.messages.clone()
    }

    pub fn push_user(&mut self, content: impl Into<String>) {
        self.messages.push(Message::user(content));
    }

    /// Añade los mensajes producidos por un turno del agente.
    pub fn extend(&mut self, messages: impl IntoIterator<Item = Message>) {
        self.messages.extend(messages);
    }
}
