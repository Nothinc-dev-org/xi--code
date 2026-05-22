# Arquitectura de xiě-code

> **Fuente de verdad** del diseño del sistema. Las decisiones que la sustentan
> están en [`decisions/`](decisions/). Si un cambio contradice este documento,
> primero se actualiza aquí (con su ADR), luego se implementa.

## 1. Visión

xiě-code es una app de escritorio nativa (Rust + GTK4) que actúa como agente de
codificación asistido por IA, alternativa a OpenCode.

OpenCode separa un **motor headless** (server HTTP en Bun) de sus **frontends**
(TUI, desktop Electron). xiě-code toma una decisión distinta: **el motor se
reimplementa en Rust y se embebe in-process** en la app GTK4. No hay server HTTP
intermedio ni dependencia del runtime de Bun.

Ventajas: un solo binario nativo, sin IPC de red, arranque rápido, integración
estrecha con GTK. Coste: hay que reimplementar la lógica del motor (mitigado por
fases — ver [`roadmap.md`](roadmap.md)).

## 2. Vista de alto nivel

```
┌─────────────────────────────────────────────────────────────┐
│                     Proceso xiě-code                          │
│                                                               │
│   ┌───────────────┐         comandos          ┌───────────┐  │
│   │   zhi-gtk     │ ───────────────────────▶  │           │  │
│   │  (UI, GTK4)   │                            │ zhi-core  │  │
│   │  hilo GLib    │ ◀─────────────────────────  │ (motor)   │  │
│   └───────────────┘     eventos / streams      │  Tokio    │  │
│                                                 └─────┬─────┘  │
│                                                       │        │
│        ┌──────────────┬───────────────┬───────────────┤        │
│        ▼              ▼               ▼               ▼        │
│  ┌──────────┐  ┌────────────┐  ┌──────────┐   ┌──────────┐    │
│  │zhi-provider│ │ zhi-tool   │  │ zhi-mcp  │   │ zhi-lsp  │    │
│  │ (LLM APIs) │ │ (built-in) │  │ (clients)│   │ (clients)│    │
│  └─────┬──────┘ └─────┬──────┘  └────┬─────┘   └────┬─────┘    │
└────────┼──────────────┼──────────────┼──────────────┼─────────┘
         ▼              ▼               ▼              ▼
   APIs LLM        FS / shell      servidores      servidores
  (HTTP/SSE)      del proyecto       MCP             LSP
                       │
                       ▼
                   SQLite (sesiones, mensajes, estado)
```

## 3. Crates del workspace

El código es un workspace Cargo (ver [ADR-0003](decisions/0003-workspace-cargo.md)).
Cada crate tiene su `AGENTS.md` con detalle local.

| Crate          | Responsabilidad                                                        | Depende de |
| -------------- | ---------------------------------------------------------------------- | ---------- |
| `zhi-core`     | Motor: sesiones, orquestación del bucle de agente, bus de eventos, config, permisos, persistencia, snapshots. Tipos de dominio compartidos. | provider, tool, mcp, lsp |
| `zhi-provider` | Abstracción de proveedores LLM y sus implementaciones (Anthropic, OpenAI, …). Streaming, auth, transformación de mensajes. | — |
| `zhi-tool`     | Tools integradas que el agente invoca: leer/escribir/editar archivos, shell, búsqueda, glob, etc. Contrato `Tool`. | — |
| `zhi-mcp`      | Cliente de Model Context Protocol: descubre y ejecuta tools de servidores MCP externos. | — |
| `zhi-lsp`      | Cliente LSP: arranca servidores de lenguaje y aporta diagnósticos/símbolos como contexto. | — |
| `zhi-gtk`      | UI de escritorio (GTK4 + libadwaita + Relm4). Binario de la app. Solo presentación e interacción. | core |

Regla de dependencias (sin ciclos):
`zhi-gtk → zhi-core → { zhi-provider, zhi-tool, zhi-mcp, zhi-lsp }`.

## 4. Modelo de dominio

Conceptos centrales (mapeo a OpenCode en [`glossary.md`](glossary.md)):

- **Project / Worktree** — el directorio de trabajo sobre el que opera el agente.
- **Session** — una conversación con su historial de mensajes, agente activo y
  estado. Múltiples sesiones simultáneas.
- **Message / Part** — mensajes de usuario/asistente compuestos por *parts*
  (texto, llamada a tool, resultado de tool, adjunto, razonamiento).
- **Agent** — perfil de comportamiento (`build`: acceso completo; `plan`:
  solo lectura). Define modelo, prompt de sistema y permisos por defecto.
- **Tool** — capacidad invocable por el modelo (editar archivo, ejecutar shell…).
- **Permission** — autorización requerida antes de ejecutar acciones sensibles;
  resuelta por la UI (preguntar/permitir/denegar).
- **Provider / Model** — fuente del LLM y el modelo concreto.
- **Snapshot** — checkpoint del estado del worktree para revertir cambios.

## 5. Flujo de una interacción (bucle de agente)

1. El usuario escribe en `zhi-gtk` y envía un mensaje (con adjuntos opcionales).
2. `zhi-gtk` despacha un comando al motor por un canal; el hilo GLib no se bloquea.
3. `zhi-core` construye el contexto (system prompt del agente + historial +
   contexto LSP relevante + tools disponibles de `zhi-tool` y `zhi-mcp`) y llama
   al proveedor vía `zhi-provider`.
4. La respuesta llega en **streaming** (SSE). El motor reemite parts a la UI a
   medida que llegan (texto incremental, llamadas a tool).
5. Si el modelo solicita una tool:
   - Si la tool requiere permiso, el motor emite una solicitud de permiso; la UI
     la resuelve (el usuario aprueba/deniega) y responde por el canal.
   - El motor ejecuta la tool (`zhi-tool`/`zhi-mcp`), captura el resultado y lo
     reinyecta como nuevo *part* en la conversación.
6. El bucle continúa hasta que el modelo termina su turno.
7. Cada paso persiste en SQLite; la sesión es reanudable.

## 6. Concurrencia y UI

El reto central de una app GTK con trabajo async: **GTK no es thread-safe y su
loop (GLib) vive en el hilo principal**.

- Un runtime **Tokio** multi-thread ejecuta el motor y todo el I/O.
- La UI ↔ motor se comunican por canales (`async_channel`), puenteados al loop
  de GLib para que las actualizaciones se apliquen en el hilo de UI.
- El streaming del LLM y la ejecución de tools nunca tocan widgets directamente:
  emiten eventos que el lado UI consume y traduce a cambios de estado.

Detalle del patrón concreto en [`crates/zhi-gtk/AGENTS.md`](../crates/zhi-gtk/AGENTS.md).

## 7. Persistencia

SQLite vía `sqlx` (async). Almacena proyectos, sesiones, mensajes/parts y estado
de ejecución, de forma reanudable. Esquema con `snake_case` alineado a columnas.
Ubicación según convenciones XDG del SO.

## 8. Configuración

Config de usuario (proveedores, claves, agentes personalizados, servidores MCP)
en archivos bajo el directorio de config XDG. Por compatibilidad conceptual se
sigue el modelo de OpenCode donde sea razonable, pero **no** es un objetivo
mantener compatibilidad binaria con sus archivos (ver
[ADR-0001](decisions/0001-motor-nativo-rust.md)).

## 9. Seguridad

- Las claves de API nunca se loguean ni se persisten en texto plano si el SO
  ofrece un keyring; si no, permisos de archivo restrictivos.
- Toda acción que modifique el sistema (escritura de archivos, shell) pasa por el
  sistema de permisos antes de ejecutarse.
- El agente `plan` es solo lectura por defecto.

## 10. Qué queda fuera de v1

Ver [`roadmap.md`](roadmap.md) para el alcance por fases y lo diferido
(p. ej. sincronización en la nube, compartir sesiones, multi-ventana avanzada).
