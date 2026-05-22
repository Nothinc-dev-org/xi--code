# ADR-0004: Alcance v1 = paridad funcional con OpenCode

- **Estado:** aceptado
- **Fecha:** 2026-05-21

## Contexto

Definir el alcance de la primera versión condiciona el diseño de todos los
crates. Las opciones iban desde un MVP mínimo de chat hasta replicar el flujo
completo de OpenCode.

## Decisión

El objetivo de v1 es **paridad funcional** con el flujo principal de OpenCode:
múltiples sesiones, agentes intercambiables (`build`/`plan`) y subagentes, tools
integradas, sistema de permisos, servidores MCP, integración LSP y adjuntos, con
soporte multi-proveedor de LLM.

Se alcanza por **fases incrementales** (ver `roadmap.md`); cada fase deja la app
usable. No es un "big bang".

## Alternativas consideradas

- **MVP de chat** — solo una sesión, un proveedor, streaming y markdown.
  - Pros: entrega rapidísima.
  - Contras: no cumple el propósito de ser una alternativa real a OpenCode.
- **Paridad funcional (elegida)** — cubre el flujo de trabajo completo.
  - Pros: producto realmente competitivo y útil.
  - Contras: alcance mayor; mitigado con fases.

## Consecuencias

- Todos los crates se diseñan pensando en el flujo completo desde el principio
  (las abstracciones de tool, permiso y provider no se "atajan" para el MVP).
- El roadmap prioriza un MVP de chat funcional temprano (Fase 1) para validar el
  puente Tokio↔GLib antes de añadir complejidad.
- Funcionalidad de nube/compartir queda explícitamente fuera de v1.
