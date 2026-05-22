//! Tools integradas que el agente puede invocar, y el contrato `Tool`.
//!
//! Una *tool* declara su nombre, descripción y un esquema de parámetros (JSON
//! Schema, para exponerlo al modelo), si requiere **permiso**, y una ejecución
//! async que devuelve un resultado textual reinyectable como *part*.
//!
//! Toda operación de archivo se confina al worktree del proyecto vía
//! [`ToolContext`]. Ver `crates/zhi-tool/AGENTS.md` y
//! [ADR-0007](../../docs/decisions/0007-tools-permisos-bucle-agente.md).

mod builtins;
mod context;
mod registry;

pub use context::ToolContext;
pub use registry::ToolRegistry;

use async_trait::async_trait;
use serde_json::Value;

/// Error del crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("error de E/S ejecutando la tool: {0}")]
    Io(#[from] std::io::Error),
    #[error("argumentos inválidos: {0}")]
    InvalidArguments(String),
    #[error("la ruta «{0}» queda fuera del worktree")]
    PathOutsideWorkdir(String),
    #[error("no se pudo aplicar la edición: {0}")]
    Edit(String),
    #[error("la tool excedió el tiempo máximo ({0}s)")]
    Timeout(u64),
    #[error("patrón inválido: {0}")]
    Pattern(String),
    #[error(transparent)]
    Regex(#[from] regex::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Contrato común de una tool invocable por el agente.
///
/// Las implementaciones son `Send + Sync` y se comparten tras un `Arc` en el
/// [`ToolRegistry`]. La ejecución es async y no debe asumir nada sobre la UI.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Identificador estable que el modelo usa para invocarla.
    fn name(&self) -> &str;

    /// Descripción en lenguaje natural para el modelo.
    fn description(&self) -> &str;

    /// Esquema JSON de los parámetros aceptados (estilo JSON Schema).
    fn parameters_schema(&self) -> Value;

    /// `true` si ejecutar esta tool requiere autorización del usuario. El motor
    /// (`zhi-core`) consulta esto y resuelve el permiso antes de llamar a
    /// [`Tool::execute`]; la tool nunca decide saltárselo.
    fn requires_permission(&self) -> bool;

    /// Ejecuta la tool con `args` (validados contra su esquema por el modelo,
    /// revalidados aquí) dentro del worktree de `ctx`.
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String>;
}

/// Extrae un campo string obligatorio de los argumentos.
pub(crate) fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidArguments(format!("falta el campo de texto «{key}»")))
}

/// Extrae un campo string opcional de los argumentos.
pub(crate) fn arg_str_opt<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Crea un worktree temporal aislado y su `ToolContext`.
    fn temp_ctx() -> (std::path::PathBuf, ToolContext) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("zhi-tool-test-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = ToolContext::new(&dir);
        (dir, ctx)
    }

    async fn run(name: &str, args: Value, ctx: &ToolContext) -> Result<String> {
        let reg = ToolRegistry::with_builtins();
        reg.get(name)
            .expect("tool registrada")
            .execute(args, ctx)
            .await
    }

    #[tokio::test]
    async fn write_read_edit_round_trip() {
        let (dir, ctx) = temp_ctx();

        run(
            "write_file",
            json!({"path": "a/b.txt", "content": "hola mundo"}),
            &ctx,
        )
        .await
        .unwrap();
        let read = run("read_file", json!({"path": "a/b.txt"}), &ctx)
            .await
            .unwrap();
        assert_eq!(read, "hola mundo");

        run(
            "edit_file",
            json!({"path": "a/b.txt", "old_string": "mundo", "new_string": "Zhi"}),
            &ctx,
        )
        .await
        .unwrap();
        let read = run("read_file", json!({"path": "a/b.txt"}), &ctx)
            .await
            .unwrap();
        assert_eq!(read, "hola Zhi");

        // Edición ambigua sin replace_all → error.
        run(
            "write_file",
            json!({"path": "dup.txt", "content": "x x x"}),
            &ctx,
        )
        .await
        .unwrap();
        let err = run(
            "edit_file",
            json!({"path": "dup.txt", "old_string": "x", "new_string": "y"}),
            &ctx,
        )
        .await;
        assert!(matches!(err, Err(Error::Edit(_))));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn grep_glob_and_list() {
        let (dir, ctx) = temp_ctx();
        run(
            "write_file",
            json!({"path": "src/main.rs", "content": "fn main() {}\nlet x = 1;"}),
            &ctx,
        )
        .await
        .unwrap();
        run(
            "write_file",
            json!({"path": "src/lib.rs", "content": "pub fn foo() {}"}),
            &ctx,
        )
        .await
        .unwrap();

        let grep = run("grep", json!({"pattern": "fn main"}), &ctx)
            .await
            .unwrap();
        assert!(grep.contains("src/main.rs:1:"), "grep: {grep}");

        let glob = run("glob", json!({"pattern": "src/*.rs"}), &ctx)
            .await
            .unwrap();
        assert!(
            glob.contains("src/main.rs") && glob.contains("src/lib.rs"),
            "glob: {glob}"
        );

        let list = run("list_dir", json!({"path": "src"}), &ctx).await.unwrap();
        assert!(
            list.contains("main.rs") && list.contains("lib.rs"),
            "list: {list}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn bash_runs_in_worktree() {
        let (dir, ctx) = temp_ctx();
        let out = run("bash", json!({"command": "echo hola"}), &ctx)
            .await
            .unwrap();
        assert!(out.contains("hola"));
        assert!(out.contains("[código de salida: 0]"), "out: {out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn rejects_paths_outside_worktree() {
        let (dir, ctx) = temp_ctx();
        let err = run("read_file", json!({"path": "../../etc/passwd"}), &ctx).await;
        assert!(matches!(err, Err(Error::PathOutsideWorkdir(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
