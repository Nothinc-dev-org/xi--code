# ADR-0005: DeepSeek como proveedor LLM inicial

- **Estado:** aceptado
- **Fecha:** 2026-05-22

## Contexto

El roadmap planteaba arrancar el MVP de chat (Fase 1) con un proveedor LLM. La
propuesta inicial era Anthropic, pero se decidió usar **DeepSeek**.

## Decisión

El primer (y de momento único) proveedor implementado en `zhi-provider` es
**DeepSeek**, modelo `deepseek-chat`. La clave se lee de la variable de entorno
`DEEPSEEK_API_KEY`.

DeepSeek expone una API **compatible con OpenAI** (`POST /chat/completions` con
`stream: true` y eventos SSE), lo que facilita añadir después OpenAI y otros
compatibles reutilizando el mismo formato de petición/respuesta.

Para el MVP el cliente `DeepSeek` es **concreto** (no hay trait `Provider`
todavía): introducir la abstracción con un solo proveedor sería over-engineering.
El trait común se extraerá en la Fase 4, cuando entre el segundo proveedor.

## Alternativas consideradas

- **Anthropic** — propuesta inicial; descartada por preferencia del proyecto.
- **Trait `Provider` desde el día uno** — descartado: abstracción prematura con
  un único implementador (ver `.ai/conventions.md`).

## Consecuencias

- `zhi-core::Engine` depende hoy del cliente concreto de DeepSeek; al extraer el
  trait `Provider` (Fase 4) el `Engine` pasará a depender del trait, no del tipo.
- El formato compatible con OpenAI reduce el coste de añadir más proveedores.
- La `base_url` y el `model` tienen valores por defecto en el cliente; se harán
  configurables vía config de usuario en fases posteriores.
