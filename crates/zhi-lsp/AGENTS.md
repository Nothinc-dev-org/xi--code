# AGENTS.md — zhi-lsp

> Aún sin código. Lee `/AGENTS.md` y `docs/architecture.md` antes de implementar.

## Responsabilidad

Cliente **LSP (Language Server Protocol)**. Arranca servidores de lenguaje para el
worktree y aporta contexto de código (diagnósticos, símbolos, hover) que el motor
puede inyectar en el prompt o usar para enriquecer resultados de tools.

- Detección del lenguaje y arranque del servidor LSP correspondiente.
- Ciclo de vida de la conexión LSP (initialize, sincronización de documentos…).
- Exponer diagnósticos/símbolos de forma consumible por `zhi-core`.

## Depende de

Nada de otros crates del workspace (hoja del grafo).

## Invariantes

- El I/O con servidores LSP es async y no bloquea el motor.
- Un servidor LSP que falla se aísla; su ausencia degrada el contexto pero no
  rompe el flujo de agente.
- Los procesos de servidor LSP se cierran limpiamente al terminar la sesión.
