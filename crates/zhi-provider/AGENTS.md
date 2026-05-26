# AGENTS.md — zhi-provider

> Implementado: trait `Provider` (un método `stream_chat`), cliente único
> `OpenAiCompatible::new(key, base_url)` que cubre cualquier endpoint con API
> estilo OpenAI (DeepSeek, OpenAI, Groq, Together, DeepInfra, Cerebras,
> Mistral, Perplexity, xAI, Vercel, vLLM, Ollama, …). Streaming SSE (Fase 1),
> *function calling* (Fase 3) y *chain of thought* (`reasoning_content`).
>
> **Catálogo dinámico** en el módulo `catalog`: poblado desde `models.dev`
> (snapshot embebido `assets/models.json` + cache XDG + refresh background).
> Filtro `Catalog::openai_compatible()` a los SDKs en `OPENAI_COMPATIBLE_NPM`
> (los que el cliente `OpenAiCompatible` sabe hablar). Identificador
> `ModelRef { provider_id, model_id }` serializado como `provider/model`.
> Ver [ADR-0009](../../docs/decisions/0009-catalogo-models-dev.md) (sustituye
> a [ADR-0008](../../docs/decisions/0008-multi-proveedor-catalogo-estatico.md)
> en lo relativo al catálogo).
>
> **Auth y OAuth**: módulo `auth` (persistencia en `auth.json` con permisos
> 0600, enum `AuthInfo` = `Api | Oauth`) y módulo `oauth` (PKCE S256,
> servidor HTTP local con `tokio::net::TcpListener`, flujo browser para
> OpenAI calcado del plugin `codex` de OpenCode). El formato de `auth.json`
> es compatible con OpenCode. Ver
> [ADR-0010](../../docs/decisions/0010-auth-oauth-openai.md).
>
> Pendiente: cliente Codex Responses API (consumir el `access_token` de
> OAuth para inferencia); método headless / device flow; otros formatos
> no-OpenAI (Anthropic) si entran — habría que ampliar
> `OPENAI_COMPATIBLE_NPM` y añadir el `Provider` correspondiente.
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
