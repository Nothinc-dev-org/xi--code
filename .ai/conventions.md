# Convenciones de código (Rust)

Detalle del estilo resumido en `/AGENTS.md`. Aplica a todos los crates.

## Errores

- Librerías (`zhi-core`, `zhi-provider`, …): tipos de error propios con
  `thiserror`. Cada crate expone su `enum Error` y un `pub type Result<T>`.
- Binario/bordes (`zhi-gtk`, `main`): `anyhow` para agregar contexto.
- No usar `unwrap()`/`expect()` en rutas de ejecución normales; permitido en
  tests y en invariantes verdaderamente imposibles (con comentario que lo
  justifique).

## Async y concurrencia

- Un único runtime Tokio multi-thread propiedad del proceso.
- **El hilo de GTK nunca bloquea.** La UI envía comandos al motor y recibe
  actualizaciones por canales (`tokio::sync::mpsc` / `async_channel` puenteado al
  loop de GLib). Ver el patrón en `crates/zhi-gtk/AGENTS.md`.
- El streaming de tokens del LLM se modela como un stream que la UI consume
  incrementalmente.

## Estilo

- `cargo fmt` (rustfmt por defecto) y `cargo clippy -- -D warnings` limpios.
- Prefiere early returns; evita `else` tras un return.
- Inlinea valores de un solo uso; no extraigas helpers de un solo uso salvo que
  nombren un concepto real o aíslen un borde complejo.
- Inferencia de tipos cuando sea legible; anota en firmas públicas y donde
  clarifique.
- snake_case para campos de tablas SQLite, igual que los nombres de columna.

## Documentación de código

- `///` para API pública y para invariantes/decisiones no obvias.
- No comentes lo evidente. Comenta el *por qué*, no el *qué*.

## Tests

- Evita mocks; prueba la implementación real. Para proveedores LLM, usa
  fixtures/grabaciones de respuestas en vez de mockear la red.
- Tests unitarios junto al código (`#[cfg(test)] mod tests`); de integración en
  `crates/<crate>/tests/`.

## Dependencias entre crates

Sentido único, sin ciclos:

```
zhi-gtk → zhi-core → { zhi-provider, zhi-tool, zhi-mcp, zhi-lsp }
```

`zhi-core` no depende de `zhi-gtk`. Tipos compartidos de dominio viven en
`zhi-core` (o en un futuro `zhi-types` si crece).
