//! Contexto de ejecución de las tools: el worktree y la resolución segura de
//! rutas relativas a él.

use std::path::{Component, Path, PathBuf};

use crate::{Error, Result};

/// Contexto compartido por las tools de una sesión. Porta la **raíz del
/// worktree** (canónica) y confina toda ruta dentro de ella.
#[derive(Debug, Clone)]
pub struct ToolContext {
    workdir: PathBuf,
}

impl ToolContext {
    /// Crea un contexto sobre `workdir`. Se canoniza la raíz (mejor esfuerzo)
    /// para que la comprobación de confinamiento sea robusta frente a symlinks.
    pub fn new(workdir: impl Into<PathBuf>) -> Self {
        let workdir = workdir.into();
        let workdir = workdir.canonicalize().unwrap_or(workdir);
        Self { workdir }
    }

    /// Raíz del worktree.
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// Resuelve `rel` (ruta relativa o absoluta provista por el modelo) a una
    /// ruta absoluta dentro del worktree. Rechaza cualquier intento de escapar
    /// de la raíz mediante `..` o rutas absolutas externas.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf> {
        let candidate = Path::new(rel);
        let joined = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.workdir.join(candidate)
        };
        let normalized = normalize(&joined);
        if !normalized.starts_with(&self.workdir) {
            return Err(Error::PathOutsideWorkdir(rel.to_string()));
        }
        Ok(normalized)
    }

    /// Devuelve la ruta relativa al worktree para mostrarla en resultados.
    pub fn relativize<'a>(&self, path: &'a Path) -> &'a Path {
        path.strip_prefix(&self.workdir).unwrap_or(path)
    }
}

/// Normaliza una ruta léxicamente (sin tocar el sistema de archivos):
/// colapsa `.` y resuelve `..` sin permitir subir por encima de la raíz.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}
