# ADR-0008: Catálogo estático de proveedores y resolución por modelo

- **Estado:** aceptado
- **Fecha:** 2026-05-25

## Contexto

Hasta la Fase 4 el `Engine` se construía con `from_env`, que elegía un único
proveedor entre `DEEPSEEK_API_KEY` y `OPENAI_API_KEY` (cortocircuito en el
primero encontrado) y guardaba un `Arc<dyn Provider>` único. El catálogo de
modelos vivía dentro de la instancia del proveedor (`OpenAiCompatible.models`),
expuesto por `Provider::available_models`.

De esa entanglement salían dos síntomas reales:

1. **El selector de modelo solo mostraba el catálogo del proveedor activo.** Con
   `DEEPSEEK_API_KEY` exportada, los modelos de OpenAI no aparecían aunque
   ambas claves estuvieran disponibles.
2. **Sin ninguna clave, el botón de modelo se deshabilitaba.** Sin engine no
   hay proveedor, sin proveedor no hay catálogo, y la UI bloqueaba la entrada
   por la única ruta de obtenerlo.

OpenCode resuelve un problema análogo separando el **registro de proveedores y
modelos** (catálogo navegable sin credenciales) de la **instanciación de
clientes** (perezosa, gobernada por auth/env). La selección del usuario es un
par `(providerID, modelID)` y el cliente concreto se materializa cuando hace
falta.

## Decisión

1. **Catálogo estático en `zhi-provider`.** `ProviderSpec` describe id estable,
   nombre visible, `base_url`, `env_var` y `&'static [&'static str]` de modelos
   conocidos. `PROVIDERS` enumera el catálogo. El orden define la prioridad
   (primer modelo del primer proveedor = `default_model()`).

2. **Resolución modelo → proveedor.** `find_provider_for_model(model_id)`
   devuelve el `ProviderSpec` correspondiente. El catálogo no permite
   solapamientos: cada modelo pertenece a un único proveedor.

3. **`Engine` infalible y multi-proveedor perezoso.** `Engine::new()` ya no
   pide credenciales: construye la caché de proveedores vacía. En cada
   `run_turn`, antes del primer paso, se resuelve el proveedor del modelo:

   - Si ya está cacheado, se usa.
   - Si no, se lee `spec.env_var` y se construye `OpenAiCompatible::from_spec`.
   - Si la variable no existe, el stream emite `Error::MissingApiKey { env_var,
     model }` y termina; la UI lo pinta como mensaje normal de error.

4. **El trait `Provider` adelgaza.** Se eliminan `default_model` y
   `available_models`: el catálogo no es responsabilidad del cliente. El trait
   queda con un único método (`stream_chat`).

5. **UI desacoplada de credenciales.** El botón de modelo solo se inhabilita
   durante un turno; el picker se alimenta del catálogo estático y anota los
   modelos cuya clave no está presente (`falta DEEPSEEK_API_KEY`, etc.). Si no
   hay ninguna clave, se muestra un mensaje informativo en el chat al
   arrancar, sin bloquear la app.

## Alternativas consideradas

- **Construir todos los proveedores al arranque con sus claves opcionales.**
  Más simple, pero falla los principios del [`.ai/conventions.md`] (no crear
  por anticipado clientes que pueden no usarse) y requeriría que cada proveedor
  aceptara una clave `Option<String>`.
- **Identificador compuesto `(providerID, modelID)` como en OpenCode.**
  Necesario allí porque su catálogo se compone dinámicamente y permite alias.
  Aquí el catálogo es cerrado y los `modelID` son únicos: la complejidad extra
  del par no aporta nada hoy.
- **Mantener `Engine::from_env`** y solo arreglar el botón. Resolvería el
  síntoma del bloqueo pero no el síntoma del catálogo recortado, y dejaría el
  acoplamiento intacto para la siguiente fase (subagentes, agentes
  personalizados desde config) donde el problema reaparecería.

## Consecuencias

- El selector de modelo es siempre interactivo (salvo durante un turno) y
  muestra el catálogo completo, anotando los modelos sin clave disponible.
- Cambiar de modelo en caliente entre proveedores es transparente: el cliente
  del nuevo proveedor se construye y cachea al vuelo en el siguiente turno.
- La falta de clave ya no es un error de arranque: el usuario puede explorar
  la app, abrir sesiones, cambiar de agente y elegir modelo sin variables; el
  error solo aparece cuando intenta enviar un mensaje con un modelo cuya clave
  no está exportada, con un mensaje que indica **qué variable concreta falta**.
- Añadir un proveedor nuevo equivale a sumar una entrada a `PROVIDERS` (id,
  nombre, base_url, env_var, modelos) — sin tocar el `Engine` ni la UI.
- Queda preparada la transición a la pieza siguiente de la Fase 4 (agentes
  personalizados desde config): un agente custom podrá fijar `model` y este
  resolverá su proveedor por catálogo sin código nuevo.
