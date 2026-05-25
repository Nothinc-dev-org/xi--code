//! Catálogo de proveedores y modelos, equivalente al de OpenCode.
//!
//! La fuente de verdad es `https://models.dev/api.json`. Para no bloquear el
//! arranque y soportar uso offline, `Catalog::load` es síncrono y rápido:
//! intenta leer `XIE_MODELS_PATH` (override explícito) o la cache local
//! (`$XDG_CACHE_HOME/xiě-code/models.json`); si no hay nada, cae al snapshot
//! embebido en el binario. El refresco contra `models.dev` se hace en
//! background con [`Catalog::fetch_and_cache`] (ver `ADR-0009`).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::Result;

const MODELS_DEV_URL: &str = "https://models.dev/api.json";

/// Cuánto tiempo se considera fresca la cache antes de pedir refresco.
pub const CACHE_TTL: Duration = Duration::from_secs(5 * 60);

/// Periodicidad del refresco en background.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Snapshot del catálogo congelado al compilar: garantía de funcionamiento
/// offline y de que la UI nunca arranca vacía.
const SNAPSHOT: &str = include_str!("../assets/models.json");

/// `npm` SDKs cuyos endpoints hablan la API estilo OpenAI (`POST /chat/completions`
/// con SSE) y por tanto encajan en [`crate::OpenAiCompatible`]. Es la
/// whitelist mínima para no ofrecer al usuario modelos que el motor no sabría
/// invocar.
pub const OPENAI_COMPATIBLE_NPM: &[&str] = &[
    "@ai-sdk/openai-compatible",
    "@ai-sdk/openai",
    "@ai-sdk/groq",
    "@ai-sdk/togetherai",
    "@ai-sdk/deepinfra",
    "@ai-sdk/cerebras",
    "@ai-sdk/mistral",
    "@ai-sdk/perplexity",
    "@ai-sdk/xai",
    "@ai-sdk/vercel",
];

/// URLs por defecto de proveedores cuyo SDK las conoce internamente y por eso
/// no aparecen en `models.dev` con el campo `api`. Sin esto, el filtro
/// `openai_compatible` excluiría OpenAI.
const SDK_DEFAULT_BASE_URLS: &[(&str, &str)] = &[
    ("openai", "https://api.openai.com/v1"),
    ("groq", "https://api.groq.com/openai/v1"),
    ("mistral", "https://api.mistral.ai/v1"),
    ("perplexity", "https://api.perplexity.ai"),
    ("xai", "https://api.x.ai/v1"),
    ("cerebras", "https://api.cerebras.ai/v1"),
    ("deepinfra", "https://api.deepinfra.com/v1/openai"),
    ("togetherai", "https://api.together.xyz/v1"),
];

/// Referencia a un modelo concreto: `provider_id` + `model_id`. Los modelIDs
/// se repiten entre proveedores en `models.dev` (p. ej. `deepseek-chat` está
/// en `deepseek` y en agregadores como `302ai`), así que la unicidad la da el
/// par. Se serializa como `"provider_id/model_id"` (formato OpenCode).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelRef {
    pub provider_id: String,
    pub model_id: String,
}

impl ModelRef {
    pub fn new(provider_id: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            model_id: model_id.into(),
        }
    }

    /// Parsea `"provider/model"`. Si la cadena no contiene `/`, devuelve `None`:
    /// el llamador debe decidir cómo resolver una referencia legacy (típicamente
    /// vía [`Catalog::resolve_legacy`]).
    pub fn parse(s: &str) -> Option<Self> {
        let (provider, model) = s.split_once('/')?;
        if provider.is_empty() || model.is_empty() {
            return None;
        }
        Some(Self::new(provider, model))
    }
}

impl std::fmt::Display for ModelRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.provider_id, self.model_id)
    }
}

/// Estado de madurez de un modelo según `models.dev`. Solo los catalogados como
/// [`ModelStatus::Deprecated`] se ocultan en la UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelStatus {
    Alpha,
    Beta,
    Deprecated,
}

/// Información del proveedor en el catálogo. Solo se conservan los campos que
/// la app realmente usa hoy; serde ignora los demás silenciosamente.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub npm: Option<String>,
    /// URL base del endpoint compatible con OpenAI. En `models.dev` se llama
    /// `api`; un proveedor sin `api` es típicamente "necesita configuración"
    /// y no se puede usar tal cual.
    #[serde(default, rename = "api")]
    pub base_url: Option<String>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelInfo>,
}

impl ProviderInfo {
    /// `true` si al menos una de las variables de entorno listadas existe.
    pub fn has_api_key(&self) -> bool {
        self.env.iter().any(|var| std::env::var_os(var).is_some())
    }

    /// Variable de entorno cuya clave usar (primera definida; primera del
    /// listado como fallback informativo).
    pub fn env_var(&self) -> Option<&str> {
        self.env
            .iter()
            .find(|v| std::env::var_os(v).is_some())
            .or_else(|| self.env.first())
            .map(|s| s.as_str())
    }
}

/// Información de un modelo del catálogo.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub status: Option<ModelStatus>,
    /// `true` si el modelo admite *chain of thought* vía `reasoning_content`.
    #[serde(default)]
    pub reasoning: bool,
    /// `true` si el modelo soporta function calling.
    #[serde(default)]
    pub tool_call: bool,
    /// `true` si admite adjuntos (imágenes, archivos).
    #[serde(default)]
    pub attachment: bool,
}

/// El catálogo completo, indexado por `provider_id`.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    providers: BTreeMap<String, ProviderInfo>,
}

impl Catalog {
    /// Catálogo embebido al compilar (`assets/models.json`). Siempre disponible.
    pub fn embedded() -> Self {
        Self::from_json(SNAPSHOT).expect("snapshot de models.dev inválido en el binario")
    }

    fn from_json(text: &str) -> Result<Self> {
        let providers: BTreeMap<String, ProviderInfo> = serde_json::from_str(text)?;
        Ok(Self { providers })
    }

    /// Carga rápida (no bloqueante con red): `XIE_MODELS_PATH` → cache en
    /// disco → snapshot embebido. El fetch HTTP vive en
    /// [`Catalog::fetch_and_cache`], pensado para correr en background sin
    /// retrasar el arranque.
    pub fn load() -> Self {
        if let Some(override_path) = std::env::var_os("XIE_MODELS_PATH") {
            match std::fs::read_to_string(&override_path) {
                Ok(text) => match Self::from_json(&text) {
                    Ok(c) => return c,
                    Err(e) => {
                        tracing::warn!(error = %e, "XIE_MODELS_PATH apunta a JSON inválido; usando cache/snapshot");
                    }
                },
                Err(e) => tracing::warn!(error = %e, "no se pudo leer XIE_MODELS_PATH"),
            }
        }
        if let Some(path) = cache_path() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                match Self::from_json(&text) {
                    Ok(c) => return c,
                    Err(e) => {
                        tracing::warn!(error = %e, "cache de catálogo inválida; usando snapshot")
                    }
                }
            }
        }
        Self::embedded()
    }

    /// Hace fetch del catálogo a `models.dev` (o `XIE_MODELS_URL`) y escribe
    /// la cache. Diseñado para correr en background; no aborta si la escritura
    /// del archivo falla (la decodificación sí propaga error).
    pub async fn fetch_and_cache() -> Result<Self> {
        let url = std::env::var("XIE_MODELS_URL").unwrap_or_else(|_| MODELS_DEV_URL.to_string());
        let text = reqwest::Client::new()
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        if let Some(path) = cache_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = std::fs::write(&path, &text) {
                tracing::warn!(error = %e, path = %path.display(), "no se pudo escribir la cache del catálogo");
            }
        }
        Self::from_json(&text)
    }

    /// `true` si existe cache en disco y su `mtime` está dentro del TTL.
    pub fn cache_is_fresh() -> bool {
        let Some(path) = cache_path() else {
            return false;
        };
        let Ok(meta) = std::fs::metadata(&path) else {
            return false;
        };
        let Ok(modified) = meta.modified() else {
            return false;
        };
        modified.elapsed().map(|e| e < CACHE_TTL).unwrap_or(false)
    }

    /// Itera los proveedores del catálogo en orden por id.
    pub fn providers(&self) -> impl Iterator<Item = &ProviderInfo> {
        self.providers.values()
    }

    pub fn get(&self, provider_id: &str) -> Option<&ProviderInfo> {
        self.providers.get(provider_id)
    }

    /// Vista filtrada a proveedores que el motor sabe hablar: `npm` en
    /// [`OPENAI_COMPATIBLE_NPM`] y `base_url` resoluble (explícita en el JSON
    /// o desde [`SDK_DEFAULT_BASE_URLS`]). Es lo que la UI presenta al usuario.
    pub fn openai_compatible(&self) -> Self {
        let providers = self
            .providers
            .iter()
            .filter(|(_, p)| {
                let npm_ok = p
                    .npm
                    .as_deref()
                    .map(|n| OPENAI_COMPATIBLE_NPM.contains(&n))
                    .unwrap_or(false);
                npm_ok && resolve_base_url(p).is_some()
            })
            .map(|(k, v)| {
                let mut value = v.clone();
                if value.base_url.is_none() {
                    value.base_url = resolve_base_url(v).map(|s| s.to_string());
                }
                (k.clone(), value)
            })
            .collect();
        Self { providers }
    }

    /// Resuelve un par `(provider_id, model_id)` contra el catálogo.
    pub fn resolve(&self, model_ref: &ModelRef) -> Option<(&ProviderInfo, &ModelInfo)> {
        let provider = self.providers.get(&model_ref.provider_id)?;
        let model = provider.models.get(&model_ref.model_id)?;
        Some((provider, model))
    }

    /// Para identificadores legacy sin prefijo de proveedor: busca el primer
    /// `ProviderInfo` cuyo catálogo contenga `model_id`. La iteración va por
    /// orden alfabético de `provider_id`. Si hay varios proveedores con el
    /// mismo modelID, gana el primero — el usuario debería migrar a la forma
    /// `provider/model` para fijar la elección.
    pub fn resolve_legacy(&self, model_id: &str) -> Option<ModelRef> {
        let provider = self
            .providers
            .values()
            .find(|p| p.models.contains_key(model_id))?;
        Some(ModelRef::new(&provider.id, model_id))
    }

    /// Modelo por defecto: primer modelo del primer proveedor con clave en el
    /// entorno; si ninguno tiene clave, primer modelo del primer proveedor del
    /// catálogo. `None` solo si el catálogo está vacío.
    pub fn default_model(&self) -> Option<ModelRef> {
        let pick = self
            .providers
            .values()
            .find(|p| p.has_api_key() && !p.models.is_empty())
            .or_else(|| self.providers.values().find(|p| !p.models.is_empty()))?;
        let model_id = pick.models.keys().next()?;
        Some(ModelRef::new(&pick.id, model_id))
    }

    /// `true` si el modelo referenciado está marcado como razonador.
    pub fn is_reasoning_model(&self, model_ref: &ModelRef) -> bool {
        self.resolve(model_ref)
            .map(|(_, m)| m.reasoning)
            .unwrap_or(false)
    }
}

/// URL base efectiva: la del JSON si existe, si no la conocida por SDK.
fn resolve_base_url(p: &ProviderInfo) -> Option<&str> {
    if let Some(url) = p.base_url.as_deref() {
        return Some(url);
    }
    SDK_DEFAULT_BASE_URLS
        .iter()
        .find(|(id, _)| *id == p.id)
        .map(|(_, url)| *url)
}

/// `$XDG_CACHE_HOME/xiě-code/models.json` (fallback: `~/.cache/...`).
fn cache_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CACHE_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".cache"),
    };
    Some(base.join("xiě-code").join("models.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_parses() {
        let cat = Catalog::embedded();
        assert!(cat.providers().count() > 0);
    }

    #[test]
    fn openai_compatible_filter_includes_deepseek_and_openai() {
        let cat = Catalog::embedded().openai_compatible();
        let ids: Vec<_> = cat.providers().map(|p| p.id.as_str()).collect();
        assert!(ids.contains(&"deepseek"), "deepseek debería estar: {ids:?}");
        assert!(ids.contains(&"openai"), "openai debería estar");
    }

    #[test]
    fn openai_compatible_filter_excludes_anthropic_and_google() {
        let cat = Catalog::embedded().openai_compatible();
        let ids: Vec<_> = cat.providers().map(|p| p.id.as_str()).collect();
        assert!(
            !ids.contains(&"anthropic"),
            "anthropic no es openai-compatible"
        );
        assert!(!ids.contains(&"google"), "google no es openai-compatible");
    }

    #[test]
    fn openai_provider_gets_default_base_url() {
        let cat = Catalog::embedded().openai_compatible();
        let openai = cat.get("openai").expect("openai");
        assert_eq!(
            openai.base_url.as_deref(),
            Some("https://api.openai.com/v1")
        );
    }

    #[test]
    fn resolve_resolves_par_provider_model() {
        let cat = Catalog::embedded().openai_compatible();
        let r = ModelRef::new("deepseek", "deepseek-chat");
        let (provider, _model) = cat.resolve(&r).expect("resuelve");
        assert_eq!(provider.id, "deepseek");
    }

    #[test]
    fn resolve_legacy_picks_a_provider_with_that_model() {
        let cat = Catalog::embedded().openai_compatible();
        let r = cat.resolve_legacy("deepseek-chat").expect("hay match");
        assert_eq!(r.model_id, "deepseek-chat");
        assert!(cat.resolve(&r).is_some());
    }

    #[test]
    fn unknown_model_is_not_resolvable() {
        let cat = Catalog::embedded().openai_compatible();
        let r = ModelRef::new("deepseek", "modelo-inexistente");
        assert!(cat.resolve(&r).is_none());
        assert!(cat.resolve_legacy("modelo-inexistente").is_none());
    }

    #[test]
    fn default_model_resolves() {
        let cat = Catalog::embedded().openai_compatible();
        let default = cat.default_model().expect("hay default");
        assert!(cat.resolve(&default).is_some(), "default: {default}");
    }

    #[test]
    fn model_ref_roundtrip() {
        let r = ModelRef::new("deepseek", "deepseek-chat");
        let s = r.to_string();
        assert_eq!(s, "deepseek/deepseek-chat");
        assert_eq!(ModelRef::parse(&s), Some(r));
        assert_eq!(ModelRef::parse("no-slash"), None);
        assert_eq!(ModelRef::parse("/empty"), None);
        assert_eq!(ModelRef::parse("empty/"), None);
    }
}
