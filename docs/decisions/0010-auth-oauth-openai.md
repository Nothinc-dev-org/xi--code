# ADR-0010: Auth de proveedores con OAuth (estilo OpenCode)

- **Estado:** aceptado, con un follow-up explícito en *Consecuencias*.
- **Fecha:** 2026-05-25
- **Relacionado con:** [ADR-0008] (Engine infalible), [ADR-0009] (catálogo
  dinámico desde `models.dev`).

## Contexto

El ADR-0008 dejó la falta de clave como un error del **turno**, no del
arranque. Hasta ahora la única forma de aportar la clave era exportar la
variable de entorno (`DEEPSEEK_API_KEY`, `OPENAI_API_KEY`). OpenCode tiene un
flujo "Connect" que (a) **persiste API keys** introducidas desde la UI y (b)
**autentica con OAuth** contra el proveedor (Cuenta ChatGPT en el caso de
OpenAI), replicando lo que hace el Codex CLI.

Para alcanzar paridad: añadimos persistencia de credenciales por proveedor y
un flujo OAuth-browser equivalente.

## Decisión

1. **Almacén de credenciales `auth.json`.** Un único archivo en
   `$XDG_DATA_HOME/xiě-code/auth.json` con permisos `0o600` (Unix). El
   formato es **compatible** con el `auth.json` de OpenCode (mismas
   variantes `oauth` / `api`) por si el usuario quiere intercambiar
   credenciales. Tipos: `AuthInfo::Api { key, metadata }` y
   `AuthInfo::Oauth { refresh, access, expires_at_ms, account_id }`.

2. **Flujo OAuth (browser) calcado del plugin `codex` de OpenCode**
   (`packages/opencode/src/plugin/codex.ts`):

   - PKCE S256, `state` aleatorio.
   - URL: `https://auth.openai.com/oauth/authorize` con `CLIENT_ID =
     app_EMoamEEZ73f0CkXaXp7hrann` (el ID público de Codex CLI),
     `scope = "openid profile email offline_access"`, parámetros
     `id_token_add_organizations=true`, `codex_cli_simplified_flow=true`,
     `originator=xie-code`.
   - Servidor local en `http://127.0.0.1:1455/auth/callback` con un
     parser HTTP mínimo (`tokio::net::TcpListener`) que escucha una sola
     conexión y devuelve HTML de éxito o error.
   - Intercambio de `code` por tokens en `POST /oauth/token`.
   - Refresco vía `grant_type=refresh_token` cuando `expires_at_ms < now`.
   - Extracción de `chatgpt_account_id` del JWT (`id_token` o
     `access_token`) parseando solo el payload (no se verifica firma —
     la verificación real la hace el endpoint Codex al recibir el
     `Bearer`).

3. **Orden de prioridad en `Engine::provider_for`**:

   1. `AuthStore` con `AuthInfo::Api { key }` → se usa esa key.
   2. `AuthStore` con `AuthInfo::Oauth { .. }` → ver follow-up.
   3. Variable de entorno declarada por el proveedor en el catálogo →
      fallback histórico.
   4. Ninguna → `Error::MissingApiKey { env_var, model }`.

4. **UI:**

   - Botón "Settings" (icono `preferences-system-symbolic`) junto al de
     Modelo.
   - Modal **Configuración**: sección "Cuentas" con la lista de
     proveedores conectados (con botón "Desconectar" por fila) y un
     botón "Conectar proveedor".
   - Modal **Conectar proveedor**: select (con búsqueda) sobre los
     proveedores del catálogo OpenAI-compatible. OpenAI aparece primero
     porque es el único con flujo OAuth implementado.
   - Al elegir OpenAI: levanta el servidor local, lanza `xdg-open
     <authorize_url>` y muestra un modal informativo. Cuando el callback
     llega, se persiste `AuthInfo::Oauth` y se notifica con toast.
   - Para cualquier otro proveedor: modal de entrada de API key
     (`PasswordEntry` con peek), persiste como `AuthInfo::Api`.

## Alternativas consideradas

- **Solo entrada manual de API key, sin OAuth.** Trivial pero rompería
  paridad con OpenCode y obligaría al usuario de ChatGPT Plus a generar
  una API key con coste aparte.
- **Reusar el `auth.json` de OpenCode directamente.** Tentador pero
  acopla la app al sistema de archivos de otra app que el usuario puede
  no tener instalada. Optamos por **el mismo formato** pero **archivo
  propio** (`xiě-code/auth.json`); migrar entre apps es copiar y pegar.
- **Implementar el endpoint Codex Responses API ahora mismo.** Significa
  un cliente `Provider` distinto al `OpenAiCompatible` actual (la
  Responses API usa otro formato de body y otro stream). Se decide
  diferirlo para mantener el PR acotado; ver follow-up.

## Consecuencias

- El botón **Connect** queda funcional para OpenAI (OAuth) y para
  cualquier otro proveedor del catálogo (API key manual).
- Las API keys introducidas desde la UI **se aplican inmediatamente**
  (sin reiniciar) y tienen prioridad sobre las variables de entorno.
- Los modelos OpenAI vía API key funcionan end-to-end con la
  credencial persistida.

### Follow-up explícito

- **Cliente Codex Responses API.** Hoy `Engine::provider_for` devuelve
  `Error::OauthInferenceNotImplemented` cuando encuentra `AuthInfo::Oauth`
  para un proveedor. La razón: el endpoint que usa Codex CLI
  (`https://chatgpt.com/backend-api/codex/responses`) habla la **Responses
  API** de OpenAI, no `/chat/completions`. Necesita un `Provider` específico
  (formato de body distinto, parser de stream distinto). Cuando entre, ese
  provider debe:
  - leer el `AuthInfo::Oauth` del `AuthStore`,
  - refrescar si `expires_at_ms < now()` (función ya disponible:
    `oauth::openai::refresh_access_token`),
  - enviar `Authorization: Bearer <access>` y `ChatGPT-Account-Id:
    <account_id>` si está.
- **Método headless (device flow)** y **método "Manually enter API Key"
  dentro del propio modal de OpenAI** quedan para una siguiente iteración.

[ADR-0008]: 0008-multi-proveedor-catalogo-estatico.md
[ADR-0009]: 0009-catalogo-models-dev.md
