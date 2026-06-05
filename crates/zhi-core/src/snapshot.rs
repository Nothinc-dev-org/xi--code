//! Snapshots del worktree para revertir cambios del agente (Fase 3c).
//!
//! Se apoya en un **repositorio git aislado** (`GIT_DIR` separado del `.git` del
//! usuario, si lo hay) y se invoca al binario `git` como subproceso, igual que
//! la tool `bash` invoca `sh`. Estrategia descrita en
//! [ADR-0007](../../docs/decisions/0007-tools-permisos-bucle-agente.md).
//!
//! - `track`: añade los cambios y devuelve el hash de un `tree` (write-tree).
//! - `patch_files`: lista los archivos que difieren entre ese `tree` y el
//!   estado actual del worktree.
//! - `restore`: reescribe el worktree para que coincida con el `tree` indicado
//!   (`read-tree` + `checkout-index -a -f`). Operación destructiva: el llamador
//!   debe confirmar antes.

use std::path::PathBuf;
use std::process::Stdio;

use tokio::process::Command;

use crate::{Error, Result};

/// Manager de snapshots de un worktree. Clonable y barato: solo guarda rutas.
#[derive(Debug, Clone)]
pub struct Snapshots {
    workdir: PathBuf,
    git_dir: PathBuf,
    available: bool,
}

impl Snapshots {
    /// Abre (o inicializa) el repo shadow en `git_dir` para `workdir`. Si `git`
    /// no está en `PATH`, devuelve un manager con `available() == false` que no
    /// hace nada. No falla por la ausencia de git: los snapshots son una red de
    /// seguridad opcional, no una precondición.
    pub async fn open(workdir: PathBuf, git_dir: PathBuf) -> Result<Self> {
        if !git_available().await {
            tracing::warn!("`git` no está disponible en PATH; los snapshots quedan deshabilitados");
            return Ok(Self {
                workdir,
                git_dir,
                available: false,
            });
        }

        let existed = git_dir.exists();
        std::fs::create_dir_all(&git_dir).map_err(|e| Error::Snapshot(e.to_string()))?;

        let me = Self {
            workdir,
            git_dir,
            available: true,
        };

        if !existed {
            me.run(&["init", "--quiet"]).await?;
            // Configs que cubren los casos borde conocidos en worktrees reales,
            // tomados de la implementación equivalente en OpenCode.
            for (key, value) in [
                ("core.autocrlf", "false"),
                ("core.longpaths", "true"),
                ("core.symlinks", "true"),
                ("core.fsmonitor", "false"),
            ] {
                me.run(&["config", key, value]).await?;
            }
        }
        Ok(me)
    }

    /// `true` si el manager puede tomar y restaurar snapshots.
    pub fn available(&self) -> bool {
        self.available
    }

    /// Captura el estado actual del worktree y devuelve el hash del `tree`.
    /// `Ok(None)` si los snapshots están deshabilitados.
    pub async fn track(&self) -> Result<Option<String>> {
        if !self.available {
            return Ok(None);
        }
        self.stage().await?;
        let hash = self.run(&["write-tree"]).await?.trim().to_string();
        Ok(Some(hash))
    }

    /// Archivos del worktree que difieren respecto al snapshot `hash`. Incluye
    /// archivos nuevos: se hace `stage` antes para que el diff los vea (sin
    /// stagear, `git diff <tree>` omite los untracked).
    pub async fn patch_files(&self, hash: &str) -> Result<Vec<PathBuf>> {
        if !self.available {
            return Ok(Vec::new());
        }
        self.stage().await?;
        let out = self.run(&["diff", "--cached", "--name-only", hash]).await?;
        Ok(out
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect())
    }

    /// Diff unificado del worktree actual respecto al snapshot `hash`.
    pub async fn patch(&self, hash: &str) -> Result<String> {
        if !self.available {
            return Ok(String::new());
        }
        self.stage().await?;
        self.run(&["diff", "--cached", "--unified=3", hash]).await
    }

    /// Diff del repositorio Git real del worktree. Es un fallback de UI para
    /// sesiones sin snapshot asociado todavía.
    pub async fn worktree_patch(&self) -> Result<String> {
        if !self.available {
            return Ok(String::new());
        }
        let output = Command::new("git")
            .args(["diff", "--no-ext-diff", "--unified=3"])
            .current_dir(&self.workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| Error::Snapshot(format!("no se pudo ejecutar git: {e}")))?;
        if !output.status.success() {
            return Ok(String::new());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// `git add --all` **sin pathspec explícito**: recorre el worktree
    /// respetando el `.gitignore` del usuario y se salta los archivos ignorados
    /// silenciosamente (con pathspec explícito como `.`, git falla en lugar de
    /// saltarse los ignorados, que es lo que pasaba hasta la corrección).
    /// Confiamos en el `.gitignore` del usuario: si quiere que `target/`,
    /// `node_modules/`, claves o bases de datos queden fuera del snapshot, ahí
    /// es donde lo expresa.
    async fn stage(&self) -> Result<()> {
        self.run(&["add", "--all"]).await?;
        Ok(())
    }

    /// Restaura el worktree al estado del snapshot `hash`, archivo por archivo.
    /// `files` es la lista que devolvió `patch_files(hash)`: archivos que
    /// difieren entre el snapshot y el worktree actual (incluye los creados
    /// tras el snapshot, gracias a `stage()` en `patch_files`).
    ///
    /// Para cada archivo:
    /// - `git checkout <hash> -- <file>` restaura su contenido desde el snapshot.
    /// - Si el archivo no estaba en el snapshot (checkout falla y `ls-tree` no
    ///   lo encuentra), se elimina del worktree.
    ///
    /// Esto cubre el caso típico del agente: crea archivos nuevos (HolaMundo.rs)
    /// y modifica existentes; revertir debe borrar los nuevos y restaurar los
    /// modificados.
    pub async fn restore(&self, hash: &str, files: &[PathBuf]) -> Result<()> {
        if !self.available {
            return Ok(());
        }
        for file in files {
            let rel = file.to_string_lossy().into_owned();
            let checkout = self.run(&["checkout", hash, "--", &rel]).await;
            if checkout.is_ok() {
                continue;
            }
            // El checkout falló: comprobar si el archivo existía en el snapshot.
            let ls = self.run(&["ls-tree", hash, "--", &rel]).await;
            let existed = ls.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false);
            if existed {
                // Estaba en el snapshot pero no se pudo restaurar (raro): log y seguir.
                tracing::warn!(file = %rel, "checkout falló pero el archivo existía en el snapshot");
                continue;
            }
            // No estaba: eliminar del worktree.
            let abs = self.workdir.join(file);
            if let Err(e) = tokio::fs::remove_file(&abs).await {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(file = %rel, error = %e, "no se pudo eliminar archivo nuevo");
                }
            }
        }
        Ok(())
    }

    /// Ejecuta `git` con `GIT_DIR`/`GIT_WORK_TREE` apuntando al shadow.
    async fn run(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("git")
            .env("GIT_DIR", &self.git_dir)
            .env("GIT_WORK_TREE", &self.workdir)
            .args(args)
            .current_dir(&self.workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| Error::Snapshot(format!("no se pudo ejecutar git: {e}")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(Error::Snapshot(format!(
                "git {} → {}: {}",
                args.first().copied().unwrap_or(""),
                output.status,
                stderr.trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

async fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_paths() -> (PathBuf, PathBuf) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let work = std::env::temp_dir().join(format!("zhi-snap-test-{nanos}"));
        let git = std::env::temp_dir().join(format!("zhi-snap-git-{nanos}"));
        std::fs::create_dir_all(&work).unwrap();
        (work, git)
    }

    /// Skip el test si `git` no está instalado (entorno mínimo); no es una
    /// dependencia hard del proyecto.
    async fn snapshots_or_skip(work: PathBuf, git: PathBuf) -> Option<Snapshots> {
        let snap = Snapshots::open(work, git).await.unwrap();
        if !snap.available() {
            eprintln!("`git` no disponible: test saltado");
            return None;
        }
        Some(snap)
    }

    #[tokio::test]
    async fn track_restore_round_trip() {
        let (work, git) = temp_paths();
        let Some(snap) = snapshots_or_skip(work.clone(), git.clone()).await else {
            return;
        };

        std::fs::write(work.join("a.txt"), "v1").unwrap();
        let hash1 = snap.track().await.unwrap().unwrap();
        std::fs::write(work.join("a.txt"), "v2").unwrap();
        let hash2 = snap.track().await.unwrap().unwrap();
        assert_ne!(hash1, hash2);

        let files = snap.patch_files(&hash1).await.unwrap();
        assert_eq!(files, vec![PathBuf::from("a.txt")]);

        snap.restore(&hash1, &files).await.unwrap();
        let restored = std::fs::read_to_string(work.join("a.txt")).unwrap();
        assert_eq!(restored, "v1");

        let _ = std::fs::remove_dir_all(&work);
        let _ = std::fs::remove_dir_all(&git);
    }

    /// Regresión del bug reportado: revertir debe **eliminar** archivos nuevos
    /// creados después del snapshot (no solo restaurar los modificados).
    #[tokio::test]
    async fn revert_deletes_files_created_after_snapshot() {
        let (work, git) = temp_paths();
        let Some(snap) = snapshots_or_skip(work.clone(), git.clone()).await else {
            return;
        };

        std::fs::write(work.join("existing.txt"), "v1").unwrap();
        let hash = snap.track().await.unwrap().unwrap();

        // El agente "crea" un nuevo archivo y modifica el existente.
        std::fs::write(work.join("HolaMundo.rs"), "fn main() {}").unwrap();
        std::fs::write(work.join("existing.txt"), "v2").unwrap();

        let files = snap.patch_files(&hash).await.unwrap();
        snap.restore(&hash, &files).await.unwrap();

        assert!(
            !work.join("HolaMundo.rs").exists(),
            "el archivo creado tras el snapshot debe eliminarse al revertir"
        );
        assert_eq!(
            std::fs::read_to_string(work.join("existing.txt")).unwrap(),
            "v1",
            "el archivo existente debe volver a su contenido del snapshot"
        );

        let _ = std::fs::remove_dir_all(&work);
        let _ = std::fs::remove_dir_all(&git);
    }

    #[tokio::test]
    async fn patch_files_lists_multiple_changes() {
        let (work, git) = temp_paths();
        let Some(snap) = snapshots_or_skip(work.clone(), git.clone()).await else {
            return;
        };

        std::fs::write(work.join("a.txt"), "1").unwrap();
        let base = snap.track().await.unwrap().unwrap();

        std::fs::write(work.join("a.txt"), "2").unwrap();
        std::fs::write(work.join("b.txt"), "new").unwrap();
        let mut files = snap.patch_files(&base).await.unwrap();
        files.sort();
        assert_eq!(files, vec![PathBuf::from("a.txt"), PathBuf::from("b.txt")]);

        let _ = std::fs::remove_dir_all(&work);
        let _ = std::fs::remove_dir_all(&git);
    }

    /// Regresión: si el worktree tiene un `.gitignore` que ignora archivos
    /// reales, `git add` sin `--force` falla con exit 1 ("paths are ignored by
    /// one of your .gitignore files"). El shadow debe ignorar el `.gitignore`
    /// del usuario; nuestras exclusiones viven en `SKIP_DIRS`.
    #[tokio::test]
    async fn worktree_gitignore_does_not_break_track() {
        let (work, git) = temp_paths();
        let Some(snap) = snapshots_or_skip(work.clone(), git.clone()).await else {
            return;
        };

        std::fs::write(work.join(".gitignore"), "build/\nlog.txt\n").unwrap();
        std::fs::create_dir_all(work.join("build")).unwrap();
        std::fs::write(work.join("build/out.o"), "obj").unwrap();
        std::fs::write(work.join("log.txt"), "logged").unwrap();
        std::fs::write(work.join("src.rs"), "fn main() {}").unwrap();

        let hash = snap.track().await.unwrap().expect("track debe funcionar");
        assert!(!hash.is_empty());

        let _ = std::fs::remove_dir_all(&work);
        let _ = std::fs::remove_dir_all(&git);
    }

    /// El shadow respeta el `.gitignore` del usuario: si `node_modules/` está
    /// en `.gitignore`, no entra al snapshot ni aparece en el diff.
    #[tokio::test]
    async fn gitignore_excludes_from_snapshot() {
        let (work, git) = temp_paths();
        let Some(snap) = snapshots_or_skip(work.clone(), git.clone()).await else {
            return;
        };

        std::fs::write(work.join(".gitignore"), "node_modules/\n").unwrap();
        std::fs::create_dir_all(work.join("node_modules")).unwrap();
        std::fs::write(work.join("node_modules/foo.js"), "v1").unwrap();
        std::fs::write(work.join("a.txt"), "v1").unwrap();
        let hash = snap.track().await.unwrap().unwrap();

        std::fs::write(work.join("node_modules/foo.js"), "v2").unwrap();
        let files = snap.patch_files(&hash).await.unwrap();
        assert!(
            files.is_empty(),
            "node_modules debería estar excluido por .gitignore: {files:?}"
        );

        let _ = std::fs::remove_dir_all(&work);
        let _ = std::fs::remove_dir_all(&git);
    }
}
