# AGENTS.md — zhi-provider

> Implementado: trait `Provider` (Fase 4) y un único cliente
> `OpenAiCompatible` que cubre cualquier endpoint con API estilo OpenAI
> (DeepSeek, OpenAI, Groq, vLLM, Ollama, …) — `::deepseek(key)` /
> `::openai(key)` para los dos defaults; `::new(key, base_url, model)` para
> compatibles arbitrarios. Streaming SSE (Fase 1) y *function calling*
> —`tools` en la petición, agregación de `tool_calls` del stream— (Fase 3).
> `zhi-core::Engine` posee `Arc<dyn Provider>` y elige proveedor en
> `from_env` según `DEEPSEEK_API_KEY` u `OPENAI_API_KEY`. Pendiente:
> selector de modelo en la UI, otros formatos no-OpenAI (Anthropic) si
> entran.
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
