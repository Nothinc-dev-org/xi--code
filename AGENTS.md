# AGENTS.md — xiě-code (contexto global)

Contexto raíz para agentes de IA que trabajen en este repositorio. Cada crate
bajo `crates/` tiene además su propio `AGENTS.md` con el contexto local del
módulo: **léelo antes de tocar ese módulo**.

## Qué es este proyecto

App de escritorio nativa en **Rust + GTK4**, alternativa a OpenCode. El motor de
agente (sesiones, proveedores LLM, tools, MCP, LSP) se reimplementa en Rust y se
embebe in-process en la UI. No hay server HTTP intermedio ni dependencia de Bun.

La **fuente de verdad** de la arquitectura es [`docs/architecture.md`](docs/architecture.md).
Antes de cualquier cambio estructural, consúltala y respétala.

## Reglas de organización (innegociables)

1. **Separación estricta:**
   - Código → `crates/<crate>/src/`
   - Documentación → `docs/`
   - Configuración/contexto de IA → `.ai/`
2. **Un `AGENTS.md` por crate.** Al crear un nuevo crate (o un submódulo
   relevante dentro de `src/`), genera su `AGENTS.md` describiendo su
   responsabilidad, dependencias y los invariantes que debe mantener.
3. **Decisiones arquitectónicas → `docs/decisions/`.** Toda decisión estructural
   no trivial se registra como ADR numerado. No registres trivialidades.
4. No mezcles responsabilidades entre crates. La UI (`zhi-gtk`) no contiene
   lógica de negocio; el motor (`zhi-core`) no conoce GTK.

## Estilo Rust

- Edición 2021+. `cargo fmt` y `cargo clippy` deben pasar sin warnings.
- Errores con `thiserror` en librerías; `anyhow` solo en los binarios/bordes.
- Async con Tokio. No bloquear el hilo de UI de GTK: el trabajo del motor corre
  en el runtime async y se comunica con la UI por canales/eventos.
- Prefiere tipos e inferencia sobre anotaciones redundantes; documenta con `///`
  los invariantes no obvios, no lo evidente.
- Detalles ampliados en [`.ai/conventions.md`](.ai/conventions.md).

## Commits

Estilo conventional commits: `tipo(scope): resumen`. Tipos: `feat`, `fix`,
`docs`, `chore`, `refactor`, `test`. Scope = crate afectado (`core`, `gtk`,
`provider`, `tool`, `mcp`, `lsp`) cuando aplique.

## Referencia

El repositorio de OpenCode está disponible localmente en `../opencode/` como
referencia conceptual. Mapeo de conceptos en [`docs/glossary.md`](docs/glossary.md).
