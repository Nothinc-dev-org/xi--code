# Glosario de dominio y mapeo desde OpenCode

Conceptos del dominio de xiě-code y cómo se relacionan con los módulos de
OpenCode (referencia local en `../opencode/packages/opencode/src/`). El mapeo es
**conceptual**: no se reutiliza código ni se garantiza compatibilidad de formato.

| Concepto xiě-code | Dónde vive          | Equivalente en OpenCode            |
| ------------------ | ------------------- | ---------------------------------- |
| Session            | `zhi-core`          | `session/` (`session.ts`, `processor.ts`) |
| Message / Part     | `zhi-core`          | `session/message-v2.ts`            |
| Bus de eventos     | `zhi-core`          | `bus/`                             |
| Config             | `zhi-core`          | `config/`                          |
| Permission         | `zhi-core` + `zhi-gtk` | `permission/`                   |
| Snapshot           | `zhi-core`          | `snapshot/`                        |
| Project / Worktree | `zhi-core`          | `project/`, `worktree/`            |
| Provider / Model   | `zhi-provider`      | `provider/`                        |
| Transformación de mensajes | `zhi-provider` | `provider/transform.ts`         |
| Tool               | `zhi-tool`          | `tool/`                            |
| Agent (build/plan) | `zhi-core` (perfil) | `agent/`                           |
| MCP client         | `zhi-mcp`           | `mcp/`                             |
| LSP client         | `zhi-lsp`           | `lsp/`                             |
| Compaction / overflow | `zhi-core`       | `session/compaction.ts`, `overflow.ts` |

## Diferencias deliberadas

- **Sin server HTTP.** OpenCode expone un server (`server/`) con OpenAPI y SDKs
  generados, consumido por sus frontends. xiě-code embebe el motor in-process;
  no hay capa de red interna ni SDK.
- **Sin runtime de Bun.** Todo es nativo Rust + Tokio.
- **UI distinta.** OpenCode desktop usa Electron + SolidJS; xiě-code usa GTK4 +
  libadwaita + Relm4.

## Términos

- **Turno**: una ronda completa del bucle de agente (mensaje del usuario →
  respuesta del modelo, incluyendo cualquier ciclo de tools intermedio).
- **Part**: unidad atómica de un mensaje (texto, llamada a tool, resultado de
  tool, adjunto, razonamiento).
- **Worktree**: directorio del proyecto sobre el que el agente lee y escribe.
