# AGENTS.md — zhi-core

> Lee `/AGENTS.md` y `docs/architecture.md` antes de tocar este crate.
> Implementado: tipos de dominio (`Message`/`Role`), `Engine`/`Session` (Fase 1),
> `store` —persistencia SQLite— (Fase 2), el bucle de agente
> (`Engine::run_turn` → `AgentEvent`) con tools y permisos
> (`PermissionResolver`) (Fase 3a/3b), el módulo `snapshot` —repo git aislado
> con `track`/`patch_files`/`restore`— integrado al bucle como un snapshot por
> turno (Fase 3c), y el perfil `AgentKind` (`Build`/`Plan`) que controla el
> system prompt y filtra las tools ofrecidas al modelo (Fase 4). Persistido por
> sesión en la columna `agent` de `sessions`. Ver
> [ADR-0007](../../docs/decisions/0007-tools-permisos-bucle-agente.md).
> **Engine multi-proveedor**: `new(Arc<Catalog>)` infalible; mapa perezoso
> `HashMap<provider_id, Arc<dyn Provider>>` que se resuelve por `ModelRef`
> (`Catalog::resolve`) en cada turno. La falta de clave es
> `Error::MissingApiKey { env_var, model }` emitida por el stream. El
> catálogo viene de `models.dev` (snapshot embebido + cache + refresh
> background; filtrado a OpenAI-compatible). El modelo se persiste como
> `provider/model`; los ids legacy se resuelven con `resolve_legacy`. Ver
> [ADR-0008](../../docs/decisions/0008-multi-proveedor-catalogo-estatico.md)
> (Engine infalible) y [ADR-0009](../../docs/decisions/0009-catalogo-models-dev.md)
> (catálogo dinámico).
> Pendiente: subagentes, agentes personalizados desde config.

## Responsabilidad

El **motor** de xiě-code. Orquesta el bucle de agente y posee los tipos de
dominio compartidos. Es agnóstico de la UI: **no conoce GTK**.

Incluye:

- **Sesiones**: ciclo de vida, historial de mensajes/parts, estado reanudable.
- **Bucle de agente**: construye contexto, llama al proveedor, procesa el stream,
  ejecuta tools y reinyecta resultados hasta cerrar el turno.
- **Bus de eventos**: emite actualizaciones (parts de stream, solicitudes de
  permiso, cambios de estado) que la UI consume por canales.
- **Config**: carga proveedores, agentes, claves, servidores MCP.
- **Permisos**: modela qué acciones requieren autorización; delega la resolución
  en la UI.
- **Snapshots**: checkpoints del worktree para revertir.
- **Persistencia** (módulo `store`): SQLite vía `sqlx`. Proyectos, sesiones y
  mensajes. Esquema con `CREATE TABLE IF NOT EXISTS`, consultas verificadas en
  runtime y conexión perezosa. Ver [ADR-0006](../../docs/decisions/0006-persistencia-sqlite.md).
- **Perfiles de agente**: `build` (acceso completo) y `plan` (solo lectura).

## Depende de

`zhi-provider`, `zhi-tool`, `zhi-mcp`, `zhi-lsp`. **No** depende de `zhi-gtk`.

## Invariantes

- Nada en este crate referencia GTK/GLib ni bloquea pensando en una UI concreta.
- Toda acción que modifica el sistema pasa por el sistema de permisos antes de
  ejecutarse.
- El estado de una sesión es siempre reanudable desde SQLite.
- Errores con `thiserror`; expone `Error` y `Result<T>` propios del crate.
