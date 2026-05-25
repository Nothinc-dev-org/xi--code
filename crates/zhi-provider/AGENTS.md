# AGENTS.md — zhi-provider

> Implementado: trait `Provider` (un método `stream_chat`), cliente único
> `OpenAiCompatible` que cubre cualquier endpoint con API estilo OpenAI
> (DeepSeek, OpenAI, Groq, vLLM, Ollama, …) — `::new(key, base_url)` para
> endpoints arbitrarios y `::from_spec(spec, key)` para los del catálogo.
> Streaming SSE (Fase 1), *function calling* —`tools` en la petición,
> agregación de `tool_calls` del stream— (Fase 3) y *chain of thought*
> (`reasoning_content`) en el stream. **Catálogo estático multi-proveedor**
> (`PROVIDERS: &[ProviderSpec]`, `default_model()`, `find_provider_for_model`)
> que vive aquí: el catálogo es navegable sin instanciar nada ni leer claves;
> la selección del modelo no depende de qué proveedor esté instanciado. Ver
> [ADR-0008](../../docs/decisions/0008-multi-proveedor-catalogo-estatico.md).
> Pendiente: otros formatos no-OpenAI (Anthropic) si entran.
> Lee `/AGENTS.md` y `docs/architecture.md` antes de tocarlo.

## Responsabilidad

Abstracción de **proveedores de LLM** y sus implementaciones concretas.

- Trait `Provider` común: enviar una petición de chat y devolver un **stream** de
  eventos (texto incremental, llamadas a tool, razonamiento, uso de tokens).
- Implementaciones: Anthropic primero; OpenAI y compatibles después.
- **Auth**: gestión de claves de API (sin loguearlas; keyring del SO si existe).
- **Transformación**: traducir el modelo de mensajes/parts de dominio al formato
  de cada API y viceversa.
- Streaming sobre HTTP/SSE con `reqwest`.

## Depende de

Nada de otros crates del workspace (hoja del grafo). Define sus propios tipos de
petición/respuesta o reutiliza tipos de dominio expuestos por `zhi-core` según se
decida al implementar (evitar dependencia inversa: si comparten tipos, viven en
`zhi-core` o en un futuro `zhi-types`).

## Invariantes

- Las claves de API nunca se escriben en logs ni en errores.
- El stream se expone de forma incremental; no se acumula la respuesta completa
  antes de emitir.
- Cada proveedor mapea sus errores al `Error` del crate (`thiserror`); reintentos
  y rate limits se modelan explícitamente.
