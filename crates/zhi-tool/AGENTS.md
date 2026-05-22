# AGENTS.md — zhi-tool

> Implementado (Fase 3): contrato `Tool`, `ToolContext` (confinamiento al
> worktree), `ToolRegistry::with_builtins()` y las tools `read_file`,
> `write_file`, `edit_file`, `list_dir`, `glob`, `grep`, `bash`. Ver
> [ADR-0007](../../docs/decisions/0007-tools-permisos-bucle-agente.md).
> Lee `/AGENTS.md` y `docs/architecture.md` antes de tocar este crate.

## Responsabilidad

Las **tools integradas** que el agente puede invocar, y el contrato común que las
define.

- Trait `Tool`: nombre, descripción, esquema de parámetros (JSON Schema para el
  modelo) y ejecución async que devuelve un resultado.
- Tools previstas: leer archivo, escribir archivo, editar archivo, ejecutar
  shell, glob, grep/búsqueda, listar directorio.
- Cada tool declara si requiere **permiso** (el motor lo consulta antes de
  ejecutar; ver `zhi-core`).

## Depende de

Nada de otros crates del workspace (hoja del grafo). El esquema de parámetros se
expresa de forma que `zhi-core`/`zhi-provider` puedan exponerlo al modelo.

## Invariantes

- Una tool nunca decide por sí misma saltarse un permiso: solo declara que lo
  requiere; la autorización la orquesta `zhi-core`.
- Operaciones de archivo confinadas al worktree del proyecto salvo permiso
  explícito.
- Resultados y errores deterministas y serializables para reinyectarse como part.
