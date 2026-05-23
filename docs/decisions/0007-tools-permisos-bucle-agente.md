# ADR-0007: Contrato de tools, bucle de agente y resolución de permisos

- **Estado:** aceptado
- **Fecha:** 2026-05-22

## Contexto

La Fase 3 convierte el chat en un **agente**: el modelo puede pedir la ejecución
de *tools* (leer/escribir/editar archivos, shell, búsqueda) y el motor las
ejecuta y reinyecta el resultado hasta cerrar el turno (ver
[`architecture.md`](../architecture.md) §5). Esto introduce varias decisiones
estructurales que no estaban resueltas:

1. Cómo se define una tool y cómo se confina al worktree (`zhi-tool`).
2. Cómo se expresan las llamadas a tool en el protocolo del proveedor
   (`zhi-provider`, API estilo OpenAI/DeepSeek).
3. Cómo se orquesta el bucle de agente sin bloquear la UI (`zhi-core`).
4. Cómo se resuelve un **permiso** que requiere intervención humana, dado que el
   motor corre en Tokio y el diálogo vive en el hilo de GLib.
5. Cómo persisten los *parts* estructurados (llamadas y resultados de tool), que
   el [ADR-0006](0006-persistencia-sqlite.md) dejó diferidos a esta fase.

## Decisión

### Contrato `Tool` (`zhi-tool`)

- Trait `Tool: Send + Sync` con `async fn execute` (vía `async-trait`):
  `name`, `description`, `parameters_schema` (JSON Schema para el modelo),
  `requires_permission` y `execute(args, ctx) -> Result<String>`.
- `ToolContext` porta el **worktree** (raíz canónica). Toda ruta se resuelve con
  `ToolContext::resolve`, que normaliza léxicamente y **rechaza** rutas que se
  salgan de la raíz (`Error::PathOutsideWorkdir`). Las tools nunca tocan rutas
  absolutas fuera del worktree.
- `ToolRegistry::with_builtins()` registra las tools integradas; expone
  `get(name)` y los metadatos para construir la petición al proveedor.
- `zhi-tool` no depende de ningún otro crate del workspace (hoja del grafo).

### Tool-calling en el proveedor (`zhi-provider`)

- `Message` se enriquece: un mensaje de asistente puede portar `tool_calls`, y se
  añade el rol `Tool` con `tool_call_id` para el resultado reinyectado.
- `stream_chat(messages, tools)` envía el array `tools` (estilo OpenAI). Durante
  el stream se acumulan los fragmentos de `tool_calls` (llegan troceados por
  `index`); al cerrar el bloque se emite `StreamEvent::ToolCalls(..)`.
- El proveedor sigue siendo concreto (DeepSeek); el trait `Provider` se extrae en
  Fase 4 ([ADR-0005](0005-proveedor-deepseek.md)).

### Bucle de agente (`zhi-core`)

- `Engine::run_turn` devuelve un stream de `AgentEvent` (texto incremental,
  inicio/fin de tool, fin de turno) construido con `async-stream`. Internamente
  itera: llama al proveedor → si hay `tool_calls`, resuelve permiso, ejecuta la
  tool, reinyecta el resultado como mensaje y repite; si no, cierra el turno.
- La UI consume ese stream con el **mismo patrón** que en Fase 1/2
  (`relm4::spawn` + `sender.input`), sin cambios en la forma de consumo.

### Resolución de permisos (back-channel)

- Trait `PermissionResolver: Send + Sync` con
  `async fn resolve(&self, req: PermissionRequest) -> PermissionDecision`.
  El motor lo invoca **antes** de ejecutar una tool que `requires_permission`.
- `zhi-gtk` implementa el resolver con un `relm4::Sender` al componente y un
  `tokio::sync::oneshot`: `resolve` envía `Msg::PermissionRequested { req, reply }`
  al hilo de UI y **espera** (`reply.await`) la decisión. El componente muestra un
  `adw::AlertDialog` y, en la respuesta del usuario, hace `reply.send(decision)`.
- Así el bloqueo ocurre en la tarea Tokio (no en GLib) y la resolución viaja por
  un canal de un solo uso. Es la pieza inversa al stream de eventos: eventos
  motor→UI por el stream; la respuesta de permiso UI→motor por el oneshot.

### Persistencia de *parts* (`zhi-core::store`)

- Se extiende `messages` con columnas nuevas (`tool_calls`, `tool_call_id`,
  `tool_name`) mediante `ALTER TABLE ... ADD COLUMN` **idempotente**: se consulta
  `PRAGMA table_info(messages)` y solo se añade la columna ausente. Mantiene la
  estrategia "sin migraciones en disco" del [ADR-0006](0006-persistencia-sqlite.md).

### Snapshots del worktree (`zhi-core`) — Fase 3c

- **Repo git aislado**: cada proyecto tiene un `GIT_DIR` propio en
  `$XDG_DATA_HOME/xiě-code/snapshots/<project_id>/`. Las llamadas usan el
  binario `git` como subproceso (mismo patrón que `bash` en `zhi-tool`),
  pasando `GIT_DIR` y `GIT_WORK_TREE` por entorno. **No se toca el `.git` del
  usuario** ni su índice; el shadow vive en paralelo.
- **API mínima** en `zhi-core::snapshot::Snapshots`:
  - `open(workdir, git_dir)` → inicializa el shadow si no existe (con
    `core.autocrlf=false`, `core.longpaths=true`, `core.symlinks=true`,
    `core.fsmonitor=false`).
  - `track()` → `git add --all` (sin pathspec explícito: respeta el
    `.gitignore` del worktree y salta los archivos ignorados silenciosamente)
    + `write-tree`; devuelve el hash.
  - `patch_files(hash)` → stage + `git diff --cached --name-only <hash>`.
  - `restore(hash)` → `read-tree <hash>` + `checkout-index -a -f`.
- **Política de exclusión = el `.gitignore` del usuario**: nada de pathspecs
  negativos propios, nada de `--force`. Si el usuario quiere que `target/`,
  `node_modules/`, secretos (`.env`, `*.db`) o cualquier otra cosa queden
  fuera del snapshot, lo expresa en su `.gitignore`. El shadow no decide por
  él. Importante: `git add` **con pathspec explícito** (p. ej. `--`) falla
  cuando hay archivos ignorados en el árbol; `git add --all` sin pathspec los
  salta. Por eso usamos la segunda forma.
- **Granularidad**: un único snapshot por **turno**, tomado antes del primer
  paso del bucle que vaya a invocar al menos una tool con `requires_permission`.
  Revertir restaura el estado previo al turno entero (no a un paso intermedio):
  es la unidad mental natural para el usuario tras leer la respuesta completa.
- **Asociación con el dominio**: nueva columna `snapshot TEXT NULL` en
  `messages`, añadida con el mismo `ensure_column` idempotente que el resto
  de columnas de Fase 3 (sin sistema de migraciones, ver
  [ADR-0006](0006-persistencia-sqlite.md)). El hash se guarda contra el `id`
  del mensaje del asistente que contiene las `tool_calls` del paso.
- **Bucle y eventos**: `Engine::run_turn` recibe `Option<Snapshots>`. Cuando
  toma un snapshot emite `AgentEvent::StepSnapshot { hash, message_index }`;
  la UI lo guarda y, tras persistir los mensajes del turno, asocia el hash al
  `message_id` correcto y pinta el botón "Revertir" en la última tarjeta de
  tool del paso.
- **UI**: la confirmación se hace con `adw::MessageDialog` (disponible desde
  libadwaita 1.2, alineado con la feature actual `v1_2`; cuando en Fase 6 se
  suba a `v1_5` para `NavigationSplitView`, migrar a `AlertDialog`). El
  diálogo lista los primeros 20 archivos afectados.
- **Degradación**: si `git` no está en `PATH`, `Snapshots::open` devuelve un
  manager con `available() == false`; el motor sigue funcionando y la UI no
  muestra el botón "Revertir". Un fallo puntual en `track` no aborta el paso:
  queda registrado en `tracing::warn` y se continúa. Los snapshots son red de
  seguridad, no precondición.

## Alternativas consideradas

- **Permiso vía evento clonable sin back-channel** (la UI responde con un nuevo
  comando al motor) — descartado: obliga a un protocolo de correlación por id y a
  mantener estado de "turnos pausados"; el `oneshot` es más simple y local.
- **Política de permisos puramente declarativa** (allow/deny estático sin UI) —
  insuficiente para la paridad: OpenCode pregunta interactivamente. Se mantiene
  como modo futuro (recordar decisión por sesión).
- **`tools` definidas en `zhi-core`** en vez de un crate hoja — rompería la
  separación del workspace ([ADR-0003](0003-workspace-cargo.md)); `zhi-tool` debe
  ser reutilizable y agnóstico del motor.
- **Tabla `parts` separada** en vez de columnas en `messages` — más fiel al
  modelo, pero sobredimensionado hoy; reabrible si los parts se vuelven ricos
  (adjuntos, razonamiento) en Fase 6.

## Consecuencias

- `zhi-tool` queda como hoja reutilizable; el confinamiento al worktree es un
  invariante verificado en `resolve`, no responsabilidad de cada tool.
- El bucle de agente y los permisos no bloquean la UI: encajan en el patrón
  Tokio↔GLib ya existente, con un `oneshot` como única adición de mecanismo.
- La extensión idempotente del esquema evita introducir un sistema de migraciones
  todavía; si los parts crecen, se reabre el [ADR-0006](0006-persistencia-sqlite.md).
- Nuevas dependencias: `async-trait`, `walkdir`, `glob`, `regex` (en `zhi-tool`).
