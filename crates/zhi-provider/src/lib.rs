//! Abstracción de proveedores de LLM y la implementación de DeepSeek.
//!
//! El trait [`Provider`] es la abstracción común; [`DeepSeek`] es la primera
//! implementación. DeepSeek expone una API compatible con OpenAI
//! (`POST /chat/completions` con `stream: true`, eventos SSE). Soporta
//! *function calling*: la petición lleva un array `tools` y el stream devuelve
//! `tool_calls` troceados que aquí se agregan. Ver `crates/zhi-provider/AGENTS.md`.

use std::pin::Pin;

use async_stream::try_stream;
use async_trait::async_trait;
use futures::{Stream, StreamExt};

/// Error del crate. Cada proveedor mapea sus fallos a estas variantes.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("error de transporte HTTP: {0}")]
    Http(#[from] reqwest::Error),
    #[error("error decodificando la respuesta del proveedor: {0}")]
    Decode(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Rol de un mensaje en la conversación. Se serializa en minúsculas para la API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    /// Resultado de una tool reinyectado en la conversación.
    Tool,
}

/// Mensaje de la conversación enviado al modelo.
///
/// Un mensaje de asistente puede portar `tool_calls` (peticiones de ejecución de
/// tools); un mensaje con rol [`Role::Tool`] porta el resultado, correlacionado
/// por `tool_call_id`. Para modelos razonadores compatibles con OpenAI (p.ej.
/// `deepseek-reasoner`), `reasoning` guarda la *chain of thought* del paso; se
/// serializa como `reasoning_content`, el campo que DeepSeek exige reenviar en
/// cada mensaje del asistente del histórico.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(rename = "reasoning_content", skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self::text(Role::System, content)
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::text(Role::User, content)
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::text(Role::Assistant, content)
    }

    /// Mensaje de asistente que solicita la ejecución de tools.
    pub fn assistant_tool_calls(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls,
            tool_call_id: None,
            reasoning: None,
        }
    }

    /// Resultado de una tool, correlacionado con la llamada por `tool_call_id`.
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
            reasoning: None,
        }
    }

    /// Asocia el *chain of thought* del paso a un mensaje del asistente. Si el
    /// string es vacío no se almacena (el campo queda `None`).
    pub fn with_reasoning(mut self, reasoning: impl Into<String>) -> Self {
        let s = reasoning.into();
        if !s.is_empty() {
            self.reasoning = Some(s);
        }
        self
    }

    fn text(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            reasoning: None,
        }
    }
}

/// Una llamada a tool solicitada por el modelo (formato OpenAI).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

/// Nombre y argumentos (JSON serializado como texto) de una llamada a tool.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// Declaración de una tool que se ofrece al modelo en la petición.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolSpec {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionSpec,
}

/// Metadatos de una tool: nombre, descripción y esquema JSON de parámetros.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FunctionSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl ToolSpec {
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            kind: "function".to_string(),
            function: FunctionSpec {
                name: name.into(),
                description: description.into(),
                parameters,
            },
        }
    }
}

/// Evento emitido durante el streaming de una respuesta.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Fragmento incremental de texto del asistente.
    Delta(String),
    /// Fragmento incremental del *chain of thought* (campo `reasoning_content`
    /// del SSE estilo OpenAI; lo emiten p.ej. `deepseek-reasoner`).
    Reasoning(String),
    /// El modelo ha terminado de pedir un conjunto de llamadas a tool.
    ToolCalls(Vec<ToolCall>),
}

/// Stream de eventos de una respuesta del modelo.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>;

/// Abstracción común de un proveedor de LLM. Los proveedores concretos
/// (DeepSeek, OpenAI, …) implementan este trait; `zhi-core::Engine` resuelve la
/// instancia adecuada por modelo a partir del catálogo estático [`PROVIDERS`].
#[async_trait]
pub trait Provider: Send + Sync {
    /// Envía la conversación con `model` (con las `tools` disponibles) y devuelve
    /// un stream incremental: texto y, cuando el modelo lo pide, llamadas a tool
    /// agregadas. `model` se pasa por turno para que la UI pueda cambiarlo sin
    /// reconstruir el proveedor.
    async fn stream_chat(
        &self,
        model: &str,
        messages: Vec<Message>,
        tools: Vec<ToolSpec>,
    ) -> Result<EventStream>;
}

/// Descripción estática de un proveedor LLM conocido por la app: su id estable,
/// el nombre visible, la URL base de su API estilo OpenAI, la variable de
/// entorno donde se busca la clave y el catálogo de modelos.
///
/// El catálogo de modelos se expone a la UI sin instanciar ningún cliente y sin
/// requerir credenciales: el botón de modelo siempre es navegable.
#[derive(Debug, Clone, Copy)]
pub struct ProviderSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub base_url: &'static str,
    pub env_var: &'static str,
    pub models: &'static [&'static str],
}

/// Modelos conocidos de DeepSeek expuestos en el selector.
pub const DEEPSEEK_MODELS: &[&str] = &["deepseek-chat", "deepseek-reasoner"];

/// Modelos conocidos de OpenAI expuestos en el selector.
pub const OPENAI_MODELS: &[&str] = &["gpt-4o-mini", "gpt-4o"];

/// Catálogo de proveedores conocidos. El orden define la prioridad para
/// resolver el modelo por defecto y para presentarlos en la UI.
pub const PROVIDERS: &[ProviderSpec] = &[
    ProviderSpec {
        id: "deepseek",
        name: "DeepSeek",
        base_url: "https://api.deepseek.com",
        env_var: "DEEPSEEK_API_KEY",
        models: DEEPSEEK_MODELS,
    },
    ProviderSpec {
        id: "openai",
        name: "OpenAI",
        base_url: "https://api.openai.com/v1",
        env_var: "OPENAI_API_KEY",
        models: OPENAI_MODELS,
    },
];

/// Modelo por defecto del catálogo (primer modelo del primer proveedor).
pub fn default_model() -> &'static str {
    PROVIDERS[0].models[0]
}

/// Busca el proveedor cuyo catálogo contiene `model_id`. Devuelve el primero
/// que coincida; el catálogo está cerrado y no hay solapamientos.
pub fn find_provider_for_model(model_id: &str) -> Option<&'static ProviderSpec> {
    PROVIDERS.iter().find(|p| p.models.contains(&model_id))
}

/// Modelos que exponen *chain of thought* vía `reasoning_content` en el SSE.
/// La UI consulta este catálogo para mostrar/ocultar el botón de visibilidad
/// de pensamientos: si el modelo activo no está aquí, el control no aparece.
pub const REASONING_MODELS: &[&str] = &["deepseek-reasoner"];

/// `true` si `model` emite *chain of thought* (`reasoning_content`).
pub fn is_reasoning_model(model: &str) -> bool {
    REASONING_MODELS.contains(&model)
}

/// Cliente para cualquier endpoint con API **compatible con OpenAI** (DeepSeek,
/// OpenAI, Groq, vLLM, Ollama…). El protocolo (`POST /chat/completions`, SSE,
/// `tool_calls` troceados) es idéntico; lo que cambia es la `base_url`.
///
/// El modelo no se fija aquí: viaja por turno en [`Provider::stream_chat`]
/// (el catálogo vive en [`PROVIDERS`]).
#[derive(Debug, Clone)]
pub struct OpenAiCompatible {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl OpenAiCompatible {
    /// Cliente genérico para cualquier endpoint compatible con OpenAI.
    pub fn new(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.into(),
            base_url: base_url.into(),
        }
    }

    /// Construye el cliente a partir de una entrada del catálogo y su clave.
    pub fn from_spec(spec: &ProviderSpec, api_key: impl Into<String>) -> Self {
        Self::new(api_key, spec.base_url)
    }
}

#[async_trait]
impl Provider for OpenAiCompatible {
    async fn stream_chat(
        &self,
        model: &str,
        mut messages: Vec<Message>,
        tools: Vec<ToolSpec>,
    ) -> Result<EventStream> {
        // DeepSeek exige que todo mensaje de asistente del histórico lleve
        // `reasoning_content`, aunque sea cadena vacía. Sin esto la API rompe
        // al reanudar conversaciones con `deepseek-reasoner`. Se aplica solo
        // cuando el endpoint es DeepSeek para no contaminar otros proveedores.
        if self.base_url.contains("deepseek") {
            for msg in &mut messages {
                if msg.role == Role::Assistant && msg.reasoning.is_none() {
                    msg.reasoning = Some(String::new());
                }
            }
        }

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&ChatRequest {
                model,
                messages: &messages,
                stream: true,
                tools: if tools.is_empty() { None } else { Some(&tools) },
            })
            .send()
            .await?
            .error_for_status()?;

        let stream = try_stream! {
            let mut bytes = response.bytes_stream();
            let mut buffer = String::new();
            let mut pending: Vec<PartialToolCall> = Vec::new();

            while let Some(chunk) = bytes.next().await {
                buffer.push_str(&String::from_utf8_lossy(&chunk?));

                // El stream SSE separa eventos por líneas `data: {...}`.
                while let Some(newline) = buffer.find('\n') {
                    let line = buffer[..newline].trim().to_string();
                    buffer.drain(..=newline);

                    let Some(data) = line.strip_prefix("data:") else { continue };
                    let data = data.trim();
                    if data.is_empty() || data == "[DONE]" {
                        continue;
                    }

                    let chunk: ChatChunk = serde_json::from_str(data)?;
                    let Some(choice) = chunk.choices.into_iter().next() else { continue };

                    if let Some(content) = choice.delta.content {
                        if !content.is_empty() {
                            yield StreamEvent::Delta(content);
                        }
                    }

                    if let Some(reasoning) = choice.delta.reasoning_content {
                        if !reasoning.is_empty() {
                            yield StreamEvent::Reasoning(reasoning);
                        }
                    }

                    // Los `tool_calls` llegan troceados; se acumulan por `index`.
                    for tc in choice.delta.tool_calls {
                        let idx = tc.index as usize;
                        if pending.len() <= idx {
                            pending.resize_with(idx + 1, PartialToolCall::default);
                        }
                        let slot = &mut pending[idx];
                        if let Some(id) = tc.id {
                            slot.id = id;
                        }
                        if let Some(f) = tc.function {
                            if let Some(name) = f.name {
                                slot.name = name;
                            }
                            if let Some(args) = f.arguments {
                                slot.arguments.push_str(&args);
                            }
                        }
                    }

                    if choice.finish_reason.as_deref() == Some("tool_calls") && !pending.is_empty() {
                        let calls = pending
                            .drain(..)
                            .map(PartialToolCall::into_tool_call)
                            .collect();
                        yield StreamEvent::ToolCalls(calls);
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

/// Acumulador de una llamada a tool mientras llega troceada por el stream.
#[derive(Default)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl PartialToolCall {
    fn into_tool_call(self) -> ToolCall {
        ToolCall {
            id: self.id,
            kind: "function".to_string(),
            function: FunctionCall {
                name: self.name,
                arguments: self.arguments,
            },
        }
    }
}

#[derive(serde::Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [ToolSpec]>,
}

#[derive(serde::Deserialize)]
struct ChatChunk {
    #[serde(default)]
    choices: Vec<ChunkChoice>,
}

#[derive(serde::Deserialize)]
struct ChunkChoice {
    delta: ChunkDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(serde::Deserialize)]
struct ChunkDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<DeltaToolCall>,
}

#[derive(serde::Deserialize)]
struct DeltaToolCall {
    index: u32,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<DeltaFunction>,
}

#[derive(serde::Deserialize)]
struct DeltaFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_coherent() {
        // El default debe pertenecer a algún proveedor del catálogo.
        assert!(find_provider_for_model(default_model()).is_some());
        // Cada modelo del catálogo debe ser resoluble a su `ProviderSpec`.
        for spec in PROVIDERS {
            assert!(!spec.models.is_empty(), "{} sin modelos", spec.id);
            for model in spec.models {
                let resolved = find_provider_for_model(model).expect("modelo del catálogo");
                assert_eq!(resolved.id, spec.id);
            }
        }
    }

    #[test]
    fn unknown_model_is_not_resolvable() {
        assert!(find_provider_for_model("modelo-inexistente").is_none());
    }
}
