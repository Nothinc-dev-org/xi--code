//! Implementaciones de las tools integradas.
//!
//! De solo lectura (sin permiso): `read_file`, `list_dir`, `glob`, `grep`.
//! Con efectos (requieren permiso): `write_file`, `edit_file`, `bash`.

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use walkdir::WalkDir;

use crate::{arg_str, arg_str_opt, Error, Result, Tool, ToolContext};

/// Tiempo máximo de ejecución de un comando de shell.
const BASH_TIMEOUT_SECS: u64 = 120;
/// Límite de coincidencias devueltas por `grep` para no inundar el contexto.
const GREP_MAX_MATCHES: usize = 200;
/// Directorios que `glob`/`grep` ignoran al recorrer el árbol.
const SKIP_DIRS: [&str; 4] = [".git", "target", "node_modules", ".venv"];

// ── read_file ───────────────────────────────────────────────────────────────

pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }
    fn description(&self) -> &str {
        "Lee el contenido completo de un archivo de texto del worktree."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Ruta relativa al worktree." }
            },
            "required": ["path"]
        })
    }
    fn requires_permission(&self) -> bool {
        false
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let path = ctx.resolve(arg_str(&args, "path")?)?;
        Ok(tokio::fs::read_to_string(&path).await?)
    }
}

// ── write_file ──────────────────────────────────────────────────────────────

pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }
    fn description(&self) -> &str {
        "Crea o sobrescribe un archivo del worktree con el contenido dado."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Ruta relativa al worktree." },
                "content": { "type": "string", "description": "Contenido completo a escribir." }
            },
            "required": ["path", "content"]
        })
    }
    fn requires_permission(&self) -> bool {
        true
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let path = ctx.resolve(arg_str(&args, "path")?)?;
        let content = arg_str(&args, "content")?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, content).await?;
        let rel = ctx.relativize(&path).display().to_string();
        Ok(format!("Escrito {rel} ({} bytes).", content.len()))
    }
}

// ── edit_file ───────────────────────────────────────────────────────────────

pub struct EditFile;

#[async_trait]
impl Tool for EditFile {
    fn name(&self) -> &str {
        "edit_file"
    }
    fn description(&self) -> &str {
        "Reemplaza una porción exacta de texto en un archivo. Por defecto exige \
         que `old_string` aparezca una sola vez; usa `replace_all` para todas."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Ruta relativa al worktree." },
                "old_string": { "type": "string", "description": "Texto exacto a reemplazar." },
                "new_string": { "type": "string", "description": "Texto nuevo." },
                "replace_all": { "type": "boolean", "description": "Reemplazar todas las apariciones." }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }
    fn requires_permission(&self) -> bool {
        true
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let path = ctx.resolve(arg_str(&args, "path")?)?;
        let old = arg_str(&args, "old_string")?;
        let new = arg_str(&args, "new_string")?;
        let replace_all = args
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let original = tokio::fs::read_to_string(&path).await?;
        let count = original.matches(old).count();
        let updated = if replace_all {
            if count == 0 {
                return Err(Error::Edit("`old_string` no aparece en el archivo".into()));
            }
            original.replace(old, new)
        } else {
            match count {
                0 => return Err(Error::Edit("`old_string` no aparece en el archivo".into())),
                1 => original.replacen(old, new, 1),
                n => {
                    return Err(Error::Edit(format!(
                        "`old_string` aparece {n} veces; sé más específico o usa replace_all"
                    )))
                }
            }
        };
        tokio::fs::write(&path, &updated).await?;
        let rel = ctx.relativize(&path).display().to_string();
        let n = if replace_all { count } else { 1 };
        Ok(format!("Editado {rel} ({n} reemplazo(s))."))
    }
}

// ── list_dir ────────────────────────────────────────────────────────────────

pub struct ListDir;

#[async_trait]
impl Tool for ListDir {
    fn name(&self) -> &str {
        "list_dir"
    }
    fn description(&self) -> &str {
        "Lista las entradas de un directorio del worktree (`.` por defecto)."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directorio relativo al worktree." }
            }
        })
    }
    fn requires_permission(&self) -> bool {
        false
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let dir = ctx.resolve(arg_str_opt(&args, "path").unwrap_or("."))?;
        let mut entries: Vec<String> = Vec::new();
        let mut read = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = read.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            let suffix = if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                "/"
            } else {
                ""
            };
            entries.push(format!("{name}{suffix}"));
        }
        entries.sort();
        if entries.is_empty() {
            Ok("(directorio vacío)".to_string())
        } else {
            Ok(entries.join("\n"))
        }
    }
}

// ── glob ────────────────────────────────────────────────────────────────────

pub struct Glob;

#[async_trait]
impl Tool for Glob {
    fn name(&self) -> &str {
        "glob"
    }
    fn description(&self) -> &str {
        "Encuentra archivos por patrón glob (p. ej. `src/**/*.rs`) en el worktree."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Patrón glob relativo al worktree." }
            },
            "required": ["pattern"]
        })
    }
    fn requires_permission(&self) -> bool {
        false
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let pattern = arg_str(&args, "pattern")?;
        let abs_pattern = ctx.workdir().join(pattern);
        let abs_pattern = abs_pattern.to_string_lossy();
        let paths = glob::glob(&abs_pattern).map_err(|e| Error::Pattern(e.to_string()))?;

        let mut matches: Vec<String> = Vec::new();
        for entry in paths {
            let path = match entry {
                Ok(p) => p,
                Err(_) => continue,
            };
            // Confinar al worktree y omitir directorios ignorados.
            if ctx.resolve(&path.to_string_lossy()).is_err() {
                continue;
            }
            if path
                .components()
                .any(|c| SKIP_DIRS.contains(&c.as_os_str().to_string_lossy().as_ref()))
            {
                continue;
            }
            matches.push(ctx.relativize(&path).display().to_string());
        }
        matches.sort();
        if matches.is_empty() {
            Ok("(sin coincidencias)".to_string())
        } else {
            Ok(matches.join("\n"))
        }
    }
}

// ── grep ────────────────────────────────────────────────────────────────────

pub struct Grep;

#[async_trait]
impl Tool for Grep {
    fn name(&self) -> &str {
        "grep"
    }
    fn description(&self) -> &str {
        "Busca un patrón (regex) en los archivos de texto del worktree y devuelve \
         las coincidencias como `ruta:línea:texto`."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Expresión regular a buscar." },
                "path": { "type": "string", "description": "Subdirectorio donde buscar (opcional)." }
            },
            "required": ["pattern"]
        })
    }
    fn requires_permission(&self) -> bool {
        false
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let regex = regex::Regex::new(arg_str(&args, "pattern")?)?;
        let root = ctx.resolve(arg_str_opt(&args, "path").unwrap_or("."))?;

        // El recorrido del FS es síncrono (walkdir): se aísla en un hilo bloqueante.
        let ctx = ctx.clone();
        let matches = tokio::task::spawn_blocking(move || grep_walk(&regex, &root, &ctx))
            .await
            .map_err(|e| Error::Edit(format!("la búsqueda falló: {e}")))?;

        if matches.is_empty() {
            Ok("(sin coincidencias)".to_string())
        } else {
            Ok(matches.join("\n"))
        }
    }
}

/// Recorre `root` y acumula coincidencias `ruta:línea:texto` (síncrono).
fn grep_walk(regex: &regex::Regex, root: &std::path::Path, ctx: &ToolContext) -> Vec<String> {
    let mut out = Vec::new();
    let walker = WalkDir::new(root).into_iter().filter_entry(|e| {
        !e.file_type().is_dir() || !SKIP_DIRS.contains(&e.file_name().to_string_lossy().as_ref())
    });
    for entry in walker.flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(entry.path()) else {
            continue; // binario o sin permisos: se omite
        };
        let rel = ctx.relativize(entry.path()).display().to_string();
        for (i, line) in content.lines().enumerate() {
            if regex.is_match(line) {
                out.push(format!("{rel}:{}:{}", i + 1, line.trim_end()));
                if out.len() >= GREP_MAX_MATCHES {
                    out.push(format!("… (truncado en {GREP_MAX_MATCHES} coincidencias)"));
                    return out;
                }
            }
        }
    }
    out
}

// ── bash ────────────────────────────────────────────────────────────────────

pub struct Bash;

#[async_trait]
impl Tool for Bash {
    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> &str {
        "Ejecuta un comando de shell en la raíz del worktree y devuelve su salida \
         combinada (stdout+stderr) y el código de salida."
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Comando a ejecutar con `sh -c`." }
            },
            "required": ["command"]
        })
    }
    fn requires_permission(&self) -> bool {
        true
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> Result<String> {
        let command = arg_str(&args, "command")?;
        let child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(ctx.workdir())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();

        let output = tokio::time::timeout(Duration::from_secs(BASH_TIMEOUT_SECS), child)
            .await
            .map_err(|_| Error::Timeout(BASH_TIMEOUT_SECS))??;

        let mut out = String::new();
        if !output.stdout.is_empty() {
            out.push_str(&String::from_utf8_lossy(&output.stdout));
        }
        if !output.stderr.is_empty() {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&String::from_utf8_lossy(&output.stderr));
        }
        let code = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "señal".to_string());
        if out.is_empty() {
            out.push_str("(sin salida)");
        }
        Ok(format!("{out}\n[código de salida: {code}]"))
    }
}
