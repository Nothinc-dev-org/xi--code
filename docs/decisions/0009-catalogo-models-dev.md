# ADR-0009: Catálogo dinámico desde `models.dev` (estilo OpenCode)

- **Estado:** aceptado
- **Fecha:** 2026-05-25
- **Reemplaza:** [ADR-0008](0008-multi-proveedor-catalogo-estatico.md) en el
  punto del catálogo (la decisión sobre el `Engine` infalible y la resolución
  perezosa del proveedor por modelo sigue vigente).

## Contexto

El [ADR-0008] introdujo un catálogo estático con dos entradas hardcoded
(DeepSeek y OpenAI). Para alcanzar paridad funcional con OpenCode hay que
ofrecer al usuario el mismo universo de modelos: en OpenCode el catálogo se
obtiene de **`https://models.dev/api.json`**, un servicio público que reúne
~135 proveedores y miles de modelos con sus metadatos (estado alpha/beta/
deprecated, reasoning, tool_call, attachment, env vars, base_url, etc.).

## Decisión

1. **Origen único: `models.dev`.** El catálogo se modela según su JSON. La URL
   es configurable con `XIE_MODELS_URL` (igual que `OPENCODE_MODELS_URL`).

2. **Tres rutas de carga, en este orden** (`Catalog::load`, síncrono y rápido):

   1. `XIE_MODELS_PATH` (override explícito a un JSON local).
   2. Cache en disco en `$XDG_CACHE_HOME/xiě-code/models.json`.
   3. **Snapshot embebido en el binario** (`include_str!("../assets/models.json")`)
      como garantía: la app nunca arranca sin catálogo, incluso sin red en el
      primer uso.

3. **Refresco en background** (`spawn_catalog_refresh`): si la cache lleva más
   de `CACHE_TTL` (5 min), refetch + escribir; reintenta cada
   `REFRESH_INTERVAL` (60 min). El catálogo cargado en memoria **no** se
   reemplaza durante la sesión (igual que OpenCode): el JSON fresco se aplica
   al siguiente arranque. `XIE_DISABLE_MODELS_FETCH` desactiva la red.

4. **Filtrado a proveedores que el motor sabe hablar.** El `OpenAiCompatible`
   habla `POST /chat/completions` con SSE; solo se exponen proveedores cuyo
   `npm` está en [`OPENAI_COMPATIBLE_NPM`] (10 SDKs: `openai-compatible`,
   `openai`, `groq`, `togetherai`, `deepinfra`, `cerebras`, `mistral`,
   `perplexity`, `xai`, `vercel`). Los que no encajan (Anthropic, Vertex,
   Bedrock, Azure) quedan **fuera** del picker para no ofrecer al usuario
   modelos que el motor no podría invocar.

5. **Base URL implícita.** Algunos proveedores (OpenAI, Groq…) no exponen `api`
   en `models.dev` porque su SDK la conoce internamente. Una tabla pequeña
   `SDK_DEFAULT_BASE_URLS` la suple para que esos proveedores aparezcan en el
   catálogo filtrado.

6. **Identificador compuesto `provider/model`.** Los modelIDs se repiten entre
   proveedores en `models.dev` (p. ej. `deepseek-chat` está en `deepseek` y en
   agregadores como `302ai`). La selección del usuario y la columna
   `sessions.model` pasan a serializarse como `"provider_id/model_id"`
   (formato OpenCode). Se acepta el identificador legacy suelto y se resuelve
   contra el catálogo (`Catalog::resolve_legacy`); las nuevas selecciones se
   guardan con el par.

7. **Picker no filtra por API key.** Se muestra el universo completo del
   catálogo filtrado, salvo los `status: "deprecated"`. La falta de clave se
   reporta solo cuando el usuario envía con ese modelo (`Error::MissingApiKey
   { env_var, model }`). Esto es **paridad explícita** con OpenCode (su
   `dialog-model.tsx` no filtra por auth).

8. **`is_reasoning_model` se lee del catálogo.** Antes era una whitelist
   hardcoded (`["deepseek-reasoner"]`); ahora cada modelo trae su flag
   `reasoning` desde `models.dev`. El botón "Mostrar pensamientos" aparece
   automáticamente para cualquier modelo razonador del catálogo.

## Alternativas consideradas

- **Mantener catálogo hardcoded en `PROVIDERS`** (ADR-0008). Simple, sin red.
  Descartado: no escala — añadir un proveedor exige editar Rust, y diverge
  del patrón OpenCode que el proyecto eligió como referencia.
- **Fetch síncrono al arrancar.** Más sencillo conceptualmente; descartado
  porque alarga el arranque visiblemente y rompe el modo offline.
- **No incluir snapshot embebido y caer al fetch al primer uso.** Descartado:
  la primera sesión necesitaría red, y la app dejaría de arrancar sin
  conexión incluso para tareas locales. El snapshot embebido cuesta ~2 MB y
  resuelve el problema.
- **Reproducir el `Flock` cross-proceso de OpenCode.** Innecesario en una app
  GTK típica con una instancia abierta; se omite por simplicidad. Si en el
  futuro se vuelve relevante (varias ventanas / múltiples procesos), se
  añade entonces.
- **Aceptar todos los proveedores del catálogo, incluido Anthropic/Google.**
  Descartado: ofrecer modelos que el motor no sabe invocar es mentirle al
  usuario. Se introducirán cuando exista un `Provider` concreto que hable su
  protocolo.

## Consecuencias

- El selector de modelo lista cientos de modelos reales de decenas de
  proveedores, no dos hardcoded.
- El catálogo se mantiene **fuera del código**: actualizar a nuevos modelos
  no requiere recompilar (se refresca al cache cada hora). El snapshot
  embebido marca el "piso" mínimo y se refresca con cada release.
- Persistencia: `sessions.model` ahora guarda `"provider/model"`. Sesiones
  antiguas con el id suelto siguen funcionando vía `Catalog::resolve_legacy`,
  con la advertencia de que el primer proveedor con ese modelID gana.
- Soporte de reasoning ya no es una whitelist en el código: lo dicta el
  catálogo. Modelos nuevos lo heredan automáticamente.
- `OPENCODE`-flavored envs en xiě-code: `XIE_MODELS_URL`,
  `XIE_MODELS_PATH`, `XIE_DISABLE_MODELS_FETCH`.

[`OPENAI_COMPATIBLE_NPM`]: ../../crates/zhi-provider/src/catalog.rs
[ADR-0008]: 0008-multi-proveedor-catalogo-estatico.md
