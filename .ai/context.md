# Contexto de trabajo para IA

Punto de entrada rápido para un agente que retoma el proyecto. No dupliques aquí
lo que ya está en `docs/`; esto son punteros y estado.

## Lectura obligatoria antes de empezar

1. [`/AGENTS.md`](../AGENTS.md) — reglas globales.
2. [`docs/architecture.md`](../docs/architecture.md) — diseño del sistema.
3. El `AGENTS.md` del crate que vayas a tocar.

## Mapa mental en una frase

> UI GTK4 (`zhi-gtk`) ↔ canales async ↔ motor (`zhi-core`) que orquesta sesiones,
> habla con proveedores LLM (`zhi-provider`), ejecuta tools (`zhi-tool`), e
> integra MCP (`zhi-mcp`) y LSP (`zhi-lsp`). Estado persistido en SQLite.

## Decisiones ya tomadas (no reabrir sin ADR)

- Motor **nativo en Rust** in-process — no se reutiliza el server de OpenCode.
  → [ADR-0001](../docs/decisions/0001-motor-nativo-rust.md)
- UI con **GTK4 + libadwaita + Relm4**. → [ADR-0002](../docs/decisions/0002-gtk4-libadwaita-relm4.md)
- Layout de **workspace Cargo** con crates por dominio. → [ADR-0003](../docs/decisions/0003-workspace-cargo.md)
- Alcance v1 = **paridad funcional**. → [ADR-0004](../docs/decisions/0004-alcance-paridad-funcional.md)
- Proveedor LLM inicial = **DeepSeek** (no Anthropic). → [ADR-0005](../docs/decisions/0005-proveedor-deepseek.md)
- Persistencia SQLite: esquema `IF NOT EXISTS`, consultas en runtime, conexión
  perezosa, DB por usuario (XDG), proyecto = worktree. → [ADR-0006](../docs/decisions/0006-persistencia-sqlite.md)
- Tools/permisos/bucle de agente: contrato `Tool` en crate hoja `zhi-tool`,
  function calling en `zhi-provider`, `Engine::run_turn`, permisos vía
  `PermissionResolver` + `oneshot`, parts en `messages` con columnas idempotentes.
  → [ADR-0007](../docs/decisions/0007-tools-permisos-bucle-agente.md)
- Catálogo dinámico desde `models.dev` con snapshot embebido + cache XDG +
  refresh background; filtrado a proveedores OpenAI-compatible; identificador
  compuesto `provider/model` (`ModelRef`). `Engine` posee `Arc<Catalog>` y
  resuelve el cliente del proveedor en el turno; la falta de clave es un
  error del turno con la `env_var` exacta. → [ADR-0009](../docs/decisions/0009-catalogo-models-dev.md)
  (sustituye a [ADR-0008](../docs/decisions/0008-multi-proveedor-catalogo-estatico.md)
  en lo relativo al catálogo; mantiene la decisión de Engine infalible).

## Estado actual

- **Fase 0 (andamiaje)** completa.
- **Fase 1 (MVP de chat)** completa: `zhi-provider` con DeepSeek (SSE),
  `zhi-core` con `Engine`/`Session`, `zhi-gtk` con vista de chat y render de
  Markdown. Puente Tokio↔GLib vía `relm4::spawn` + `sender.input`.
- **Fase 2 (persistencia y sesiones)** completa: módulo `zhi-core::store`
  (SQLite/`sqlx`) con proyectos/sesiones/mensajes; `zhi-gtk` con sidebar de
  sesiones, sesión nueva y reanudación de existentes. Todo persistido.
- **Fase 3 (tools y permisos)** completa: `zhi-tool`
  (read/write/edit/list/glob/grep/bash, confinadas al worktree), bucle de
  agente `Engine::run_turn`→`AgentEvent`, function calling en `zhi-provider`,
  permisos con `PermissionResolver`+`oneshot` y diálogo en la UI.
  **Snapshots (3c)**: módulo `zhi-core::snapshot` con repo git aislado
  (`GIT_DIR` separado bajo `$XDG_DATA_HOME/xiě-code/snapshots/<project_id>/`,
  subproceso `git`); un snapshot por turno antes del primer paso con efectos,
  asociado al mensaje del asistente vía columna `snapshot` en `messages`;
  botón "Revertir" en la última tarjeta del paso con `adw::MessageDialog` que
  lista los archivos afectados. `fmt`/`clippy -D warnings`/`test`/build del
  workspace en verde.
- Arranque: `DEEPSEEK_API_KEY=... cargo run -p zhi-gtk`.
- Próximo hito: **Fase 4 — agentes y multi-proveedor**. Ver
  [`docs/roadmap.md`](../docs/roadmap.md).

### Notas de implementación

- `libadwaita` está en 0.7.x vía `relm4`. `zhi-gtk` declara `libadwaita` como
  dependencia directa con feature `v1_2` (misma crate que `relm4::adw`; cargo
  unifica features). Para widgets ≥1.4 (p. ej. `adw::ToolbarView`,
  `NavigationSplitView`) subir la feature.
- El binario se llama `xie-code` (`cargo run -p zhi-gtk`).
- **Patrón Tokio↔GLib**: en `update_with_view`, al enviar se hace `relm4::spawn`
  de la tarea async que consume el stream del motor y reenvía cada `Delta` con
  `sender.input(...)` (marshalado al hilo de UI por Relm4). La UI nunca bloquea.
- **Render Markdown**: durante el stream se muestra texto plano (`set_text`); al
  terminar el turno se convierte a marcado Pango (`markdown::to_pango` con
  `pulldown-cmark`) y se aplica con `set_markup`, para evitar markup a medias.
- **Proveedor LLM**: trait `Provider` en `zhi-provider` (un método:
  `stream_chat`). Catálogo en `zhi_provider::catalog::Catalog` poblado de
  `models.dev` (snapshot embebido `assets/models.json` + cache
  `$XDG_CACHE_HOME/xiě-code/models.json` con TTL 5 min + refresh cada 60 min).
  Filtro `Catalog::openai_compatible()` a los proveedores que `OpenAiCompatible`
  sabe hablar. Identificador `ModelRef { provider_id, model_id }`
  serializado como `provider/model`. `Engine::new(Arc<Catalog>)` cachea
  clientes por proveedor; `Error::MissingApiKey { env_var, model }` se emite
  **en el turno** si la `env_var` del proveedor no está definida. Envs:
  `XIE_MODELS_URL`, `XIE_MODELS_PATH`, `XIE_DISABLE_MODELS_FETCH`.
- **Agentes**: `AgentKind` (`Build`/`Plan`) en `zhi-core` determina el system
  prompt y filtra las tools que se exponen al modelo (`Plan` solo lectura).
  `Engine::run_turn(agent, ...)`. Persistido en `sessions.agent`. Selector
  Build/Plan linked a la izquierda del campo de entrada; atajo `Shift+Tab`
  desde el `entry` (ShortcutController local con `Propagation::Stop` para no
  caer en la navegación por foco). Se deshabilita durante un turno y se
  sincroniza al cargar/crear sesión.
- **Bucle de agente/UI**: `Engine::run_turn(history, ctx, resolver)` devuelve un
  stream de `AgentEvent` (Delta / ToolStarted / ToolFinished / Turn). La UI lo
  consume en `relm4::spawn` y reenvía cada evento como `Msg`. Los permisos van por
  un back-channel: el resolver de la UI (`UiPermissions`) emite
  `Msg::PermissionRequested { request, reply: oneshot::Sender }`, el componente
  renderiza controles embebidos en la conversación y responde por el `oneshot`;
  el bloqueo ocurre en la tarea Tokio, no en GLib. La burbuja de texto del
  asistente se crea de forma perezosa al primer `Delta`; las tools se muestran
  como tarjetas (`.card`).
- **Persistencia/UI**: `Store` se construye sin `await` en `init` (conexión
  perezosa); la migración, la carga de sesiones y el guardado se hacen en tareas
  `relm4::spawn` que devuelven el resultado por `sender.input(...)` —mismo patrón
  Tokio↔GLib que el streaming. El sidebar es un `gtk::ListBox`; la fila→sesión se
  mapea por índice (`row.index()`), y la selección programática se neutraliza
  comparando con `current_session` para no recargar en bucle.

## Cómo proponer cambios estructurales

Crea un ADR nuevo en `docs/decisions/` copiando `0000-template.md`, increméntalo,
y enlázalo desde `architecture.md` si altera el diseño.
