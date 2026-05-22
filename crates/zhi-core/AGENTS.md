# AGENTS.md — zhi-core

> Lee `/AGENTS.md` y `docs/architecture.md` antes de tocar este crate.
> Implementado: tipos de dominio (`Message`/`Role`), `Engine`/`Session` (Fase 1),
> `store` —persistencia SQLite— (Fase 2) y el bucle de agente
> (`Engine::run_turn` → `AgentEvent`) con tools y permisos
> (`PermissionResolver`) (Fase 3, ver
> [ADR-0007](../../docs/decisions/0007-tools-permisos-bucle-agente.md)).
> Pendiente: snapshots/revert (3c), config, multi-proveedor.

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
