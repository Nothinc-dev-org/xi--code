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

pub mod snapshot;
pub mod store;

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_stream::try_stream;
use async_trait::async_trait;
use futures::{Stream, StreamExt};

pub use snapshot::Snapshots;
pub use store::{SessionMeta, Store};
pub use zhi_provider::{
    catalog as catalog_internals, oauth, AuthInfo, AuthStore, Catalog, EventStream, Message,
    ModelInfo, ModelRef, ModelStatus, Provider, ProviderInfo, Role, StreamEvent, ToolCall,
};
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
    #[error("error de snapshot: {0}")]
    Snapshot(String),
    #[error("modelo desconocido en el catálogo: {0}")]
    UnknownModel(String),
    #[error("falta credencial para usar el modelo «{model}»: registra una API key en Configuración o exporta {env_var}")]
    MissingApiKey { env_var: String, model: String },
    #[error("la cuenta ChatGPT está conectada pero el cliente Codex Responses API todavía no está implementado; usa una API key clásica para «{model}» mientras tanto")]
    OauthInferenceNotImplemented { model: String },
}

pub type Result<T> = std::result::Result<T, Error>;

/// Límite de iteraciones del bucle de agente por turno (cortafuegos anti-bucle).
const MAX_STEPS: usize = 16;

// ── Perfiles de agente ───────────────────────────────────────────────────────

/// Perfil de comportamiento del agente. `Build` tiene acceso completo a las
/// tools; `Plan` queda en **solo lectura**: no se ofrecen las tools con efectos
/// al modelo y si las pide igualmente, se rechaza la ejecución. Ver
/// [`docs/architecture.md` §4](../../docs/architecture.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentKind {
    /// Acceso completo: lee, escribe, edita, ejecuta shell.
    #[default]
    Build,
    /// Solo lectura: lee y propone, nunca modifica el worktree.
    Plan,
}

impl AgentKind {
    /// `true` si el agente puede ejecutar tools con efectos (las que
    /// `Tool::requires_permission()`).
    pub fn allows_writes(self) -> bool {
        matches!(self, AgentKind::Build)
    }

    /// Prompt de sistema que se inyecta como primer mensaje del turno.
    pub fn system_prompt(self) -> &'static str {
        match self {
            AgentKind::Build => {
                "Eres xiě-code, un asistente de programación útil y conciso. \
                 Operas sobre el directorio de trabajo del usuario y dispones de tools para leer, \
                 escribir y editar archivos, buscar (glob/grep) y ejecutar comandos de shell. \
                 Usa las tools cuando necesites inspeccionar o modificar el proyecto, y explica lo \
                 que haces. Respondes en el idioma del usuario y usas Markdown cuando ayuda."
            }
            AgentKind::Plan => {
                "Eres xiě-code en modo plan: un asistente de **solo lectura**. Puedes leer \
                 archivos, listar directorios y buscar con glob/grep, pero NUNCA modifiques el \
                 proyecto: no escribas ni edites archivos, no ejecutes comandos de shell. Si el \
                 usuario pide cambios, explica qué harías paso a paso y por qué, sin aplicarlos. \
                 Respondes en el idioma del usuario y usas Markdown cuando ayuda."
            }
        }
    }

    /// Etiqueta corta para persistencia y para la UI.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentKind::Build => "build",
            AgentKind::Plan => "plan",
        }
    }

    /// Inversa de [`AgentKind::as_str`]; valores desconocidos caen a `Build`.
    pub fn from_str_or_default(s: &str) -> Self {
        match s {
            "plan" => AgentKind::Plan,
            _ => AgentKind::Build,
        }
    }
}

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
    /// Fragmento incremental del *chain of thought* del paso (`reasoning_content`
    /// del SSE; emitido p. ej. por `deepseek-reasoner`).
    ReasoningDelta(String),
    /// Cierre del bloque de razonamiento del paso, con la duración medida desde
    /// el primer `ReasoningDelta`. La UI lo usa para colapsar la tarjeta y
    /// mostrar la duración. Solo se emite si hubo al menos un delta.
    ReasoningFinished { duration_ms: u64 },
    /// Snapshot del worktree tomado antes de ejecutar las tools con efectos del
    /// paso actual. `message_index` apunta al mensaje del asistente con las
    /// `tool_calls` dentro del `Vec<Message>` que vendrá en `Turn(..)`.
    StepSnapshot { hash: String, message_index: usize },
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

/// El motor: catálogo de modelos, registro de tools y caché perezosa de
/// clientes LLM por proveedor.
///
/// El catálogo (filtrado a proveedores OpenAI-compatible) se inyecta al
/// construir el motor. Cada turno trae un [`ModelRef`] (`provider/model`); el
/// motor busca el `ProviderInfo` en el catálogo, instancia
/// `OpenAiCompatible` con su `base_url` y la clave de su `env`, y cachea el
/// cliente para no reconstruirlo. La falta de clave es un error del turno, no
/// del arranque (ver [ADR-0008] y [ADR-0009]).
#[derive(Clone)]
pub struct Engine {
    catalog: Arc<Catalog>,
    auth: AuthStore,
    providers: Arc<Mutex<HashMap<String, Arc<dyn Provider>>>>,
    tools: ToolRegistry,
}

impl Engine {
    /// Construye el motor sobre el catálogo y el almacén de credenciales. El
    /// catálogo se asume ya filtrado a proveedores compatibles
    /// (`Catalog::openai_compatible`).
    pub fn new(catalog: Arc<Catalog>, auth: AuthStore) -> Self {
        Self {
            catalog,
            auth,
            providers: Arc::new(Mutex::new(HashMap::new())),
            tools: ToolRegistry::with_builtins(),
        }
    }

    /// Catálogo activo (compartido con la UI para presentar el picker).
    pub fn catalog(&self) -> &Arc<Catalog> {
        &self.catalog
    }

    /// Almacén de credenciales activo (compartido con la UI de Settings).
    pub fn auth(&self) -> &AuthStore {
        &self.auth
    }

    /// Resuelve el cliente del proveedor asociado al modelo, construyéndolo y
    /// cacheándolo si es la primera vez.
    ///
    /// Orden de prioridad para obtener la credencial:
    /// 1. `AuthStore` con [`AuthInfo::Api`] para ese proveedor.
    /// 2. `AuthStore` con [`AuthInfo::Oauth`] → por ahora devuelve
    ///    [`Error::OauthInferenceNotImplemented`] (los tokens quedan
    ///    guardados pero el cliente Codex Responses API aún no existe; ver
    ///    ADR-0010).
    /// 3. Variable de entorno declarada por el proveedor en el catálogo.
    /// 4. Ninguna → [`Error::MissingApiKey`].
    fn provider_for(&self, model_ref: &ModelRef) -> Result<Arc<dyn Provider>> {
        let (provider, _) = self
            .catalog
            .resolve(model_ref)
            .ok_or_else(|| Error::UnknownModel(model_ref.to_string()))?;
        let base_url = provider.base_url.clone().ok_or_else(|| {
            Error::UnknownModel(format!("el proveedor «{}» no expone base_url", provider.id))
        })?;
        let mut providers = self
            .providers
            .lock()
            .expect("mutex de proveedores envenenado");
        if let Some(p) = providers.get(&provider.id) {
            return Ok(p.clone());
        }

        let key = match self.auth.get(&provider.id) {
            Some(AuthInfo::Api { key, .. }) => key,
            Some(AuthInfo::Oauth { .. }) => {
                return Err(Error::OauthInferenceNotImplemented {
                    model: model_ref.to_string(),
                });
            }
            None => {
                let env_var = provider.env_var().ok_or_else(|| Error::MissingApiKey {
                    env_var: String::new(),
                    model: model_ref.to_string(),
                })?;
                std::env::var(env_var).map_err(|_| Error::MissingApiKey {
                    env_var: env_var.to_string(),
                    model: model_ref.to_string(),
                })?
            }
        };

        let client: Arc<dyn Provider> =
            Arc::new(zhi_provider::OpenAiCompatible::new(key, base_url));
        providers.insert(provider.id.clone(), client.clone());
        Ok(client)
    }

    /// Ejecuta un turno completo del agente: llama al proveedor, ejecuta las tools
    /// que solicite (resolviendo permisos), reinyecta los resultados y repite
    /// hasta que el modelo cierra el turno. Devuelve el stream de eventos.
    ///
    /// `agent` controla el system prompt y el conjunto de tools ofrecidas: en
    /// modo [`AgentKind::Plan`] solo se exponen las de solo lectura, y si el
    /// modelo pide una tool con efectos igualmente, la ejecución se rechaza.
    /// `model` se pasa al proveedor por turno y selecciona qué cliente usar.
    /// Si la clave del proveedor del modelo no está en el entorno, el stream
    /// emite [`Error::MissingApiKey`] y termina.
    ///
    /// Si se provee `snapshots`, antes de ejecutar las tools de un paso que
    /// requiera permiso, se captura el estado del worktree y se emite un
    /// `AgentEvent::StepSnapshot` para que la UI lo asocie al mensaje del paso.
    pub fn run_turn(
        &self,
        agent: AgentKind,
        model: String,
        history: Vec<Message>,
        ctx: ToolContext,
        snapshots: Option<Snapshots>,
        resolver: Arc<dyn PermissionResolver>,
    ) -> AgentStream {
        let engine = self.clone();
        let registry = self.tools.clone();
        let tool_specs: Vec<zhi_provider::ToolSpec> = registry
            .iter()
            .filter(|t| agent.allows_writes() || !t.requires_permission())
            .map(|t| {
                zhi_provider::ToolSpec::function(t.name(), t.description(), t.parameters_schema())
            })
            .collect();

        let stream = try_stream! {
            // El identificador puede llegar como `provider/model` o, por
            // compatibilidad con sesiones legacy, como `model` suelto que se
            // resuelve contra el catálogo.
            let model_ref = ModelRef::parse(&model)
                .or_else(|| engine.catalog.resolve_legacy(&model))
                .ok_or_else(|| Error::UnknownModel(model.clone()))?;
            // Se resuelve aquí (no en `init`) para que la falta de clave salga
            // como error del stream que la UI ya sabe pintar, en vez de bloquear
            // el arranque o el botón de modelo.
            let provider = engine.provider_for(&model_ref)?;
            let mut messages = Vec::with_capacity(history.len() + 1);
            messages.push(Message::system(agent.system_prompt()));
            messages.extend(history);
            let mut appended: Vec<Message> = Vec::new();
            // Un único snapshot por turno, capturado antes del PRIMER paso con
            // efectos. Revertir restaura el estado previo al turno entero
            // (no a un paso intermedio): es la unidad mental natural para el
            // usuario que pulsa "Revertir" tras ver la respuesta completa.
            let mut snapshot_taken = false;

            for _ in 0..MAX_STEPS {
                let mut inner = provider
                    .stream_chat(&model_ref.model_id, messages.clone(), tool_specs.clone())
                    .await?;
                let mut text = String::new();
                let mut reasoning = String::new();
                // Mide la duración del bloque de razonamiento del paso. El reloj
                // arranca con el primer `Reasoning` y se cierra al observar el
                // primer `Delta`/`ToolCalls` o al final del step. Solo se emite
                // `ReasoningFinished` si hubo al menos un delta de reasoning.
                let mut reasoning_started_at: Option<std::time::Instant> = None;
                let mut reasoning_closed = false;
                let mut calls: Vec<ToolCall> = Vec::new();

                while let Some(event) = inner.next().await {
                    match event? {
                        StreamEvent::Delta(d) => {
                            if let (Some(start), false) = (reasoning_started_at, reasoning_closed) {
                                let ms = start.elapsed().as_millis() as u64;
                                yield AgentEvent::ReasoningFinished { duration_ms: ms };
                                reasoning_closed = true;
                            }
                            text.push_str(&d);
                            yield AgentEvent::Delta(d);
                        }
                        StreamEvent::Reasoning(r) => {
                            if reasoning_started_at.is_none() {
                                reasoning_started_at = Some(std::time::Instant::now());
                            }
                            reasoning.push_str(&r);
                            yield AgentEvent::ReasoningDelta(r);
                        }
                        StreamEvent::ToolCalls(c) => {
                            if let (Some(start), false) =
                                (reasoning_started_at, reasoning_closed)
                            {
                                let ms = start.elapsed().as_millis() as u64;
                                yield AgentEvent::ReasoningFinished { duration_ms: ms };
                                reasoning_closed = true;
                            }
                            calls = c;
                        }
                    }
                }

                // Cierre por fin de stream sin haber visto content/tool_calls
                // (raro, pero defensivo: garantiza que la UI cierra el spinner).
                if let (Some(start), false) = (reasoning_started_at, reasoning_closed) {
                    let ms = start.elapsed().as_millis() as u64;
                    yield AgentEvent::ReasoningFinished { duration_ms: ms };
                }

                if calls.is_empty() {
                    if !text.is_empty() {
                        let msg = Message::assistant(text).with_reasoning(reasoning);
                        messages.push(msg.clone());
                        appended.push(msg);
                    }
                    break;
                }

                // El asistente solicita tools: se registra el mensaje con las calls.
                let assistant_msg =
                    Message::assistant_tool_calls(text, calls.clone()).with_reasoning(reasoning);
                messages.push(assistant_msg.clone());
                appended.push(assistant_msg);
                let assistant_index = appended.len() - 1;

                // Si alguna de las tools del paso tiene efectos, captura un
                // snapshot del worktree antes de ejecutarlas. La UI lo asocia
                // al mensaje del asistente para ofrecer "Revertir". Un fallo
                // del snapshot no aborta el paso: queda registrado y seguimos.
                let any_writes = calls.iter().any(|c| {
                    registry
                        .get(&c.function.name)
                        .map(|t| t.requires_permission())
                        .unwrap_or(false)
                });
                if any_writes && !snapshot_taken {
                    if let Some(snap) = snapshots.as_ref() {
                        match snap.track().await {
                            Ok(Some(hash)) => {
                                snapshot_taken = true;
                                yield AgentEvent::StepSnapshot {
                                    hash,
                                    message_index: assistant_index,
                                };
                            }
                            Ok(None) => snapshot_taken = true,
                            Err(e) => {
                                tracing::warn!(error = %e, "no se pudo tomar snapshot");
                            }
                        }
                    }
                }

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
                    let blocked_by_agent = needs_permission && !agent.allows_writes();
                    let decision = if blocked_by_agent {
                        PermissionDecision::Deny
                    } else if needs_permission {
                        resolver
                            .resolve(PermissionRequest {
                                tool_name: name.clone(),
                                arguments: pretty,
                            })
                            .await
                    } else {
                        PermissionDecision::Allow
                    };

                    let (output, ok) = if blocked_by_agent {
                        (
                            "Tool con efectos rechazada: el agente está en modo plan \
                             (solo lectura)."
                                .to_string(),
                            false,
                        )
                    } else if decision == PermissionDecision::Deny {
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
