# Roadmap

Alcance objetivo de v1: **paridad funcional** con el flujo principal de OpenCode
(ver [ADR-0004](decisions/0004-alcance-paridad-funcional.md)). Se alcanza por
fases incrementales; cada fase deja la app utilizable.

## Fase 0 — Andamiaje ✅

- [x] Workspace Cargo con los 6 crates y sus `Cargo.toml`.
- [x] `AGENTS.md` por crate.
- [x] CI: `fmt`, `clippy -D warnings`, `test`.
- [x] Ventana GTK4 + libadwaita vacía que arranca (`cargo run -p zhi-gtk`).

> Verificado localmente: `cargo fmt --check`, `cargo clippy --workspace
> --all-targets -- -D warnings` y `cargo test --workspace` en verde.

## Fase 1 — MVP de chat ✅

- [x] `zhi-provider`: proveedor **DeepSeek** con streaming SSE
      (compatible con OpenAI). Ver [ADR-0005](decisions/0005-proveedor-deepseek.md).
- [x] `zhi-core`: `Session` (historial en memoria) y `Engine::stream_turn`.
- [x] `zhi-gtk`: vista de chat, envío de mensaje, render de Markdown del turno.
- [x] Puente Tokio ↔ GLib (`relm4::spawn` + `sender.input`); la UI no bloquea.

> Verificado: `fmt`, `clippy -D warnings`, `test` y build del binario en verde.
> Arranque: `DEEPSEEK_API_KEY=... cargo run -p zhi-gtk`. Sin la clave, la app
> abre y muestra un aviso (no crashea).

## Fase 2 — Persistencia y sesiones ✅

- [x] SQLite (`sqlx`): proyectos, sesiones, mensajes. *Parts* estructurados
      diferidos a la Fase 3 (con las tools). Ver [ADR-0006](decisions/0006-persistencia-sqlite.md).
- [x] Múltiples sesiones; reanudar sesión existente (sidebar + carga de historial).
- [x] Proyecto resuelto por directorio de trabajo (worktree); las sesiones se
      agrupan por proyecto. Selector de carpeta explícito: diferido.

> Verificado: `fmt`, `clippy -D warnings`, `test` (incluye round-trip del almacén)
> y build del binario en verde. DB en `$XDG_DATA_HOME/xiě-code/xiě-code.db`.
> Pendiente de validación visual en sesión gráfica del usuario.

## Fase 3 — Tools y permisos 🚧

Ver [ADR-0007](decisions/0007-tools-permisos-bucle-agente.md).

- [x] `zhi-tool`: `read_file`, `write_file`, `edit_file`, `list_dir`, `glob`,
      `grep`, `bash`. Contrato `Tool`, `ToolRegistry` y `ToolContext`
      (confinamiento al worktree). Tests del crate en verde.
- [x] Bucle de agente (`Engine::run_turn` → stream de `AgentEvent`) con
      invocación de tools, reinyección de resultados y `tool_calls` en
      `zhi-provider` (function calling estilo OpenAI). Persistencia de *parts*
      (columnas `tool_calls`/`tool_call_id`, añadidas idempotentemente).
- [x] Sistema de permisos con resolución en la UI: trait `PermissionResolver`
      + back-channel `oneshot`; `zhi-gtk` muestra controles permitir/denegar
      embebidos en la conversación y tarjetas de ejecución de tool.
- [x] **(3c)** Snapshots del worktree + revertir. Repo git aislado (`GIT_DIR`
      separado del `.git` del usuario, subproceso `git`); un snapshot por turno
      capturado antes del primer paso con efectos; hash asociado al mensaje del
      asistente (columna `snapshot` idempotente en `messages`); botón
      "Revertir" en la última tarjeta del paso con diálogo de confirmación que
      lista los archivos afectados. Si `git` no está en PATH, los snapshots
      quedan deshabilitados sin afectar al resto de la app.

> Verificado: `fmt`, `clippy -D warnings` y `test` del workspace en verde
> (zhi-tool: 4 tests; zhi-core: 4 tests —round-trip del almacén y 3 de
> snapshots—) y build del binario. Pendiente de validación visual en sesión
> gráfica del usuario.

## Fase 4 — Agentes y multi-proveedor

- [ ] Agentes `build` y `plan`; cambio de agente.
- [ ] Subagentes para tareas multi-paso.
- [ ] Proveedores adicionales (OpenAI y compatibles); selector de modelo.
- [ ] Agentes personalizados desde config.

## Fase 5 — MCP y LSP

- [ ] `zhi-mcp`: conectar servidores MCP, exponer sus tools al agente.
- [ ] `zhi-lsp`: arrancar servidores de lenguaje, aportar contexto/diagnósticos.

## Fase 6 — Adjuntos y pulido

- [ ] Adjuntos (imágenes, archivos) en mensajes.
- [ ] Atajos de teclado, temas, accesibilidad.
- [ ] Migrar el layout sidebar+conversación a `adw::NavigationSplitView`
      (requiere el feature `v1_4` de libadwaita): una sola tira de controles de
      ventana y colapso responsive del sidebar en ventanas estrechas. Hoy se usan
      dos `gtk::Box` con los botones de título desactivados en el sidebar.
- [ ] Empaquetado (`.deb`, `.rpm`, `.AppImage`, Flatpak).

## Diferido (fuera de v1)

- Sincronización en la nube / compartir sesiones.
- Cuentas y autenticación remota.
- Telemetría.
