# ADR-0003: Layout de workspace Cargo por dominio

- **Estado:** aceptado
- **Fecha:** 2026-05-21

## Contexto

El proyecto separa motor y UI, y dentro del motor hay subsistemas con fronteras
claras (proveedores, tools, MCP, LSP). El lineamiento del ProjectManager pide
separación estricta de responsabilidades y un `AGENTS.md` por módulo.

En Rust, lo idiomático para separar dominios con dependencias controladas es un
**workspace Cargo** con varios crates, no un único crate con muchos `src/`.

## Decisión

Usar un workspace Cargo. El código vive en `crates/<crate>/src/`. Crates:

- `zhi-core` — motor y tipos de dominio.
- `zhi-provider` — proveedores LLM.
- `zhi-tool` — tools integradas.
- `zhi-mcp` — cliente MCP.
- `zhi-lsp` — cliente LSP.
- `zhi-gtk` — binario de la app (UI).

Dependencias en un solo sentido, sin ciclos:
`zhi-gtk → zhi-core → { zhi-provider, zhi-tool, zhi-mcp, zhi-lsp }`.

Cada crate lleva su `AGENTS.md`. La regla del lineamiento "código en `src/`" se
satisface vía `crates/<crate>/src/`; docs en `docs/`, config de IA en `.ai/`.

## Alternativas consideradas

- **Crate único con módulos** — más simple al inicio, pero las fronteras se
  difuminan, el grafo de dependencias deja de ser explícito y los tiempos de
  compilación incremental empeoran al crecer.
- **Workspace por dominio (elegida)** — fronteras forzadas por el compilador,
  compilación incremental por crate, encaja con un `AGENTS.md` por módulo.

## Consecuencias

- El compilador impide ciclos y fugas de responsabilidad (la UI no puede colarse
  en el motor sin una dependencia explícita, que no existe).
- Posible crate futuro `zhi-types` si los tipos compartidos justifican separarse
  de `zhi-core`.
- Algo más de ceremonia inicial (varios `Cargo.toml`).
