# AGENTS.md — zhi-mcp

> Aún sin código. Lee `/AGENTS.md` y `docs/architecture.md` antes de implementar.

## Responsabilidad

Cliente de **MCP (Model Context Protocol)**. Conecta con servidores MCP externos
y expone sus tools al agente como si fueran tools nativas.

- Arranque/conexión de servidores MCP (stdio y/o transporte de red).
- Descubrimiento de las tools que ofrece cada servidor.
- Ejecución de esas tools y mapeo de su resultado al modelo de parts.
- Configuración de qué servidores levantar (vía config en `zhi-core`).

## Depende de

Nada de otros crates del workspace (hoja del grafo). Comparte el concepto de
"tool invocable" con `zhi-tool`/`zhi-core`; alinear el contrato al implementar
para que el motor trate tools nativas y de MCP de forma uniforme.

## Invariantes

- Las tools de MCP pasan por el mismo sistema de permisos que las nativas.
- Fallos de un servidor MCP no deben tumbar el motor: se aíslan y se reportan.
- El ciclo de vida de los procesos de servidor MCP se gestiona limpiamente
  (arranque/cierre, sin procesos huérfanos).
