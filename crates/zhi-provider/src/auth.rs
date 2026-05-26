//! Persistencia de credenciales por proveedor.
//!
//! Equivalente a `auth/index.ts` de OpenCode: un único archivo JSON en
//! `$XDG_DATA_HOME/xiě-code/auth.json` (permisos `0o600` cuando se puede)
//! con un mapa `provider_id → AuthInfo`. El formato es **compatible** con el
//! `auth.json` de OpenCode (mismas variantes `oauth` / `api`) por si el
//! usuario quiere mover credenciales entre apps.
//!
//! Ver [ADR-0010](../../../docs/decisions/0010-auth-oauth-openai.md).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Información de autenticación por proveedor. El tag JSON `"type"` discrimina
/// entre OAuth (cuenta ChatGPT, Anthropic Claude.ai, etc.) y API key clásica.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum AuthInfo {
    /// Tokens OAuth con refresh.
    Oauth {
        refresh: String,
        access: String,
        /// Instante de expiración del access token (milisegundos UNIX).
        #[serde(rename = "expires")]
        expires_at_ms: i64,
        #[serde(default, skip_serializing_if = "Option::is_none", rename = "accountId")]
        account_id: Option<String>,
    },
    /// API key clásica.
    Api {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<HashMap<String, String>>,
    },
}

impl AuthInfo {
    pub fn is_oauth(&self) -> bool {
        matches!(self, AuthInfo::Oauth { .. })
    }
}

/// Almacén compartido y clonable de credenciales. Las mutaciones (`set`,
/// `remove`) son `async` porque escriben al disco; las lecturas (`get`,
/// `all`) son síncronas y baratas (snapshot bajo `RwLock`).
#[derive(Debug, Clone)]
pub struct AuthStore {
    path: PathBuf,
    state: Arc<RwLock<HashMap<String, AuthInfo>>>,
}

impl AuthStore {
    /// Abre el almacén en la ruta por defecto
    /// (`$XDG_DATA_HOME/xiě-code/auth.json`). Si el archivo no existe o es
    /// inválido, arranca con un mapa vacío y un warning.
    pub fn load_default() -> Self {
        let path = auth_path();
        Self::load_at(path)
    }

    pub fn load_at(path: PathBuf) -> Self {
        let state = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<HashMap<String, AuthInfo>>(&text) {
                Ok(map) => map,
                Err(e) => {
                    tracing::warn!(error = %e, path = %path.display(), "auth.json inválido; ignorando");
                    HashMap::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "no se pudo leer auth.json");
                HashMap::new()
            }
        };
        Self {
            path,
            state: Arc::new(RwLock::new(state)),
        }
    }

    pub fn get(&self, provider_id: &str) -> Option<AuthInfo> {
        self.state.read().ok()?.get(provider_id).cloned()
    }

    pub fn all(&self) -> HashMap<String, AuthInfo> {
        self.state
            .read()
            .map(|s| s.clone())
            .unwrap_or_else(|_| HashMap::new())
    }

    /// `true` si el proveedor tiene credencial registrada (OAuth o API key).
    pub fn has(&self, provider_id: &str) -> bool {
        self.get(provider_id).is_some()
    }

    pub async fn set(&self, provider_id: impl Into<String>, info: AuthInfo) -> Result<()> {
        let provider_id = provider_id.into();
        let snapshot = {
            let mut state = self
                .state
                .write()
                .map_err(|_| Error::Auth("mutex de auth envenenado".into()))?;
            state.insert(provider_id, info);
            state.clone()
        };
        self.persist(&snapshot).await
    }

    pub async fn remove(&self, provider_id: &str) -> Result<()> {
        let snapshot = {
            let mut state = self
                .state
                .write()
                .map_err(|_| Error::Auth("mutex de auth envenenado".into()))?;
            state.remove(provider_id);
            state.clone()
        };
        self.persist(&snapshot).await
    }

    async fn persist(&self, snapshot: &HashMap<String, AuthInfo>) -> Result<()> {
        let json = serde_json::to_vec_pretty(snapshot)?;
        let path = self.path.clone();
        // El I/O del archivo se hace en un blocking task: no es async pero
        // tampoco queremos bloquear el runtime con disco.
        tokio::task::spawn_blocking(move || write_with_mode(&path, &json, 0o600))
            .await
            .map_err(|e| Error::Auth(format!("spawn_blocking: {e}")))??;
        Ok(())
    }
}

/// Escribe `bytes` en `path` (creando el directorio si hace falta) y aplica
/// permisos `mode` en plataformas Unix. En Windows el mode se ignora.
fn write_with_mode(path: &std::path::Path, bytes: &[u8], mode: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Auth(format!("mkdir: {e}")))?;
    }
    std::fs::write(path, bytes).map_err(|e| Error::Auth(format!("write: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(path, perms).map_err(|e| Error::Auth(format!("chmod: {e}")))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    Ok(())
}

/// `$XDG_DATA_HOME/xiě-code/auth.json` (fallback: `~/.local/share/...`).
pub fn auth_path() -> PathBuf {
    let base = match std::env::var_os("XDG_DATA_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(".local/share"),
            None => PathBuf::from("."),
        },
    };
    base.join("xiě-code").join("auth.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("xiě-code-auth-{tag}-{nanos}.json"))
    }

    #[tokio::test]
    async fn roundtrip_oauth_and_api() {
        let path = temp_path("roundtrip");
        let store = AuthStore::load_at(path.clone());
        assert!(store.all().is_empty());

        store
            .set(
                "openai",
                AuthInfo::Oauth {
                    refresh: "r-token".into(),
                    access: "a-token".into(),
                    expires_at_ms: 1_700_000_000_000,
                    account_id: Some("acct_123".into()),
                },
            )
            .await
            .unwrap();
        store
            .set(
                "anthropic",
                AuthInfo::Api {
                    key: "sk-ant-xxxx".into(),
                    metadata: None,
                },
            )
            .await
            .unwrap();

        // Releer del disco con una instancia nueva.
        let reloaded = AuthStore::load_at(path.clone());
        let openai = reloaded.get("openai").expect("openai");
        match openai {
            AuthInfo::Oauth {
                refresh,
                access,
                expires_at_ms,
                account_id,
            } => {
                assert_eq!(refresh, "r-token");
                assert_eq!(access, "a-token");
                assert_eq!(expires_at_ms, 1_700_000_000_000);
                assert_eq!(account_id.as_deref(), Some("acct_123"));
            }
            _ => panic!("se esperaba OAuth"),
        }
        let anthropic = reloaded.get("anthropic").expect("anthropic");
        assert!(!anthropic.is_oauth());

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn remove_drops_entry() {
        let path = temp_path("remove");
        let store = AuthStore::load_at(path.clone());
        store
            .set(
                "openai",
                AuthInfo::Api {
                    key: "k".into(),
                    metadata: None,
                },
            )
            .await
            .unwrap();
        assert!(store.has("openai"));
        store.remove("openai").await.unwrap();
        assert!(!store.has("openai"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_loads_empty() {
        let path = temp_path("missing");
        let _ = std::fs::remove_file(&path);
        let store = AuthStore::load_at(path);
        assert!(store.all().is_empty());
    }
}
