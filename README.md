# xiě-code

Una aplicación de escritorio nativa escrita en **Rust + GTK4** que funciona como
alternativa de escritorio a [OpenCode](https://opencode.ai): un agente de
codificación asistido por IA.

A diferencia del desktop oficial de OpenCode (Electron + SolidJS sobre un server
headless en Bun), xiě-code reimplementa el motor de agente de forma **100%
nativa en Rust** y lo embebe in-process en una UI GTK4. No depende del runtime
de Bun ni del binario de OpenCode.

> **Estado:** prototipo funcional en desarrollo. Incluye chat con streaming,
> sesiones persistidas, tools integradas y permisos embebidos en la conversación.
> Ver [`docs/architecture.md`](docs/architecture.md).

## Objetivo

Paridad funcional con OpenCode en su flujo de trabajo principal:

- Múltiples sesiones de chat con un proyecto/worktree.
- Agentes intercambiables (`build`, `plan`) y subagentes.
- Herramientas integradas (lectura/edición de archivos, shell, búsqueda…).
- Sistema de permisos para acciones sensibles.
- Servidores **MCP** (Model Context Protocol) externos.
- Integración **LSP** para contexto de código.
- Adjuntos (imágenes, archivos) en los mensajes.
- Soporte multi-proveedor de LLM (Anthropic, OpenAI, etc.).

## Stack

| Capa            | Tecnología                                  |
| --------------- | ------------------------------------------- |
| UI              | GTK4 + libadwaita (vía `gtk-rs`) con Relm4   |
| Runtime async   | Tokio                                       |
| HTTP / LLM      | `reqwest` + SSE                             |
| Persistencia    | SQLite (`sqlx`)                             |
| Serialización   | `serde` / `serde_json`                      |

Las decisiones técnicas se justifican en [`docs/decisions/`](docs/decisions/).

## Estructura del repositorio

```
xiě-code/
├── README.md            Este archivo
├── AGENTS.md            Contexto global para agentes de IA
├── .ai/                 Configuración y contexto de trabajo para IA
├── docs/                Documentación (fuente de verdad)
│   ├── architecture.md  Arquitectura del sistema
│   ├── roadmap.md       Fases hacia la paridad funcional
│   ├── glossary.md      Glosario de dominio (mapeo desde OpenCode)
│   └── decisions/       Registros de decisiones arquitectónicas (ADR)
└── crates/              Código fuente (workspace Cargo) — un AGENTS.md por crate
```

## Desarrollo

```bash
DEEPSEEK_API_KEY=... cargo run -p zhi-gtk
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Licencia

GNU GPL v3.
