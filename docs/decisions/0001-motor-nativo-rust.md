# ADR-0001: Motor de agente nativo en Rust e in-process

- **Estado:** aceptado
- **Fecha:** 2026-05-21

## Contexto

OpenCode separa un motor headless (server HTTP en Bun, con OpenAPI y SDKs
generados) de sus frontends (TUI, desktop Electron). Su desktop es un cliente que
arranca y consume ese server.

Para la alternativa de escritorio en Rust + GTK4 había que decidir si reutilizar
ese motor o reimplementarlo.

## Decisión

Reimplementar el motor de agente (sesiones, proveedores LLM, tools, MCP, LSP) de
forma **nativa en Rust** y **embeberlo in-process** en la app GTK4. No se usa el
server de OpenCode ni el runtime de Bun. No hay capa de red interna.

## Alternativas consideradas

- **Frontend GTK4 sobre el server de OpenCode** — arrancar `opencode serve` y
  consumirlo vía un SDK Rust.
  - Pros: mínimo esfuerzo; compatibilidad inmediata con proveedores/tools/MCP.
  - Contras: dependencia del binario OpenCode y del runtime Bun; IPC de red;
    arranque más lento; no es un producto verdaderamente nativo.
- **Híbrido/compatible** — motor nativo Rust pero replicando el contrato HTTP y
  el formato de config de OpenCode.
  - Pros: interoperabilidad con el ecosistema OpenCode.
  - Contras: arrastra complejidad de mantener compatibilidad sin beneficio claro
    para una app de escritorio autónoma.
- **Motor nativo in-process (elegida).**
  - Pros: un solo binario nativo, sin Bun, arranque rápido, integración estrecha
    con GTK, control total.
  - Contras: hay que reimplementar la lógica del motor.

## Consecuencias

- Se reimplementa el motor por fases (ver `roadmap.md`); coste inicial alto pero
  amortizado.
- No se garantiza compatibilidad binaria con los archivos de config/estado de
  OpenCode; se sigue su modelo conceptual donde sea razonable.
- La arquitectura de concurrencia (Tokio ↔ GLib) pasa a ser un punto crítico de
  diseño (ver `architecture.md` §6).
