//! Persistencia en SQLite (vía `sqlx`) de proyectos, sesiones y mensajes.
//!
//! El esquema se crea con `CREATE TABLE IF NOT EXISTS` al abrir (idempotente),
//! evitando depender de migraciones en disco o de comprobación en compilación
//! (`DATABASE_URL`). Las consultas son verificadas en tiempo de ejecución.
//!
//! Ubicación de la base de datos: directorio de datos XDG del usuario
//! (`$XDG_DATA_HOME/xiě-code` o `~/.local/share/xiě-code`).

use std::path::PathBuf;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use zhi_provider::{Message, Role, ToolCall};

/// Metadatos de una sesión para listarla en la UI (sin sus mensajes).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SessionMeta {
    pub id: i64,
    pub title: String,
    pub updated_at: String,
}

/// Almacén persistente. Clonable y barato de compartir entre tareas: envuelve un
/// pool de conexiones de `sqlx` (referencia contada internamente).
#[derive(Debug, Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    /// Abre (de forma perezosa) la base de datos en la ruta de datos por defecto,
    /// creando el directorio si no existe. No establece conexión todavía: la
    /// primera operación lo hará sobre el runtime Tokio activo en ese momento.
    pub fn connect_default() -> crate::Result<Self> {
        let path = data_dir()?;
        std::fs::create_dir_all(&path).map_err(|e| sqlx::Error::Configuration(Box::new(e)))?;
        Self::connect_at(path.join("xiě-code.db"))
    }

    /// Abre (perezosamente) la base de datos en una ruta concreta.
    pub fn connect_at(db_path: PathBuf) -> crate::Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new().connect_lazy_with(options);
        Ok(Self { pool })
    }

    /// Crea el esquema si no existe. Idempotente; llámalo una vez al arrancar.
    pub async fn migrate(&self) -> crate::Result<()> {
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS projects (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT NOT NULL UNIQUE,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS sessions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                title TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            )",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )",
        )
        .execute(&self.pool)
        .await?;
        // Parts estructurados (Fase 3): columnas añadidas de forma idempotente
        // para no depender de un sistema de migraciones. Ver ADR-0007.
        self.ensure_column("messages", "tool_calls").await?;
        self.ensure_column("messages", "tool_call_id").await?;
        Ok(())
    }

    /// Añade una columna `TEXT` a `table` si aún no existe (idempotente).
    async fn ensure_column(&self, table: &str, column: &str) -> crate::Result<()> {
        let cols: Vec<String> =
            sqlx::query_scalar(&format!("SELECT name FROM pragma_table_info('{table}')"))
                .fetch_all(&self.pool)
                .await?;
        if !cols.iter().any(|c| c == column) {
            sqlx::query(&format!("ALTER TABLE {table} ADD COLUMN {column} TEXT"))
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    /// Devuelve el id del proyecto para `path`, creándolo si es nuevo.
    pub async fn get_or_create_project(&self, path: &str) -> crate::Result<i64> {
        if let Some(id) = sqlx::query_scalar::<_, i64>("SELECT id FROM projects WHERE path = ?")
            .bind(path)
            .fetch_optional(&self.pool)
            .await?
        {
            return Ok(id);
        }
        let id = sqlx::query("INSERT INTO projects (path) VALUES (?)")
            .bind(path)
            .execute(&self.pool)
            .await?
            .last_insert_rowid();
        Ok(id)
    }

    /// Lista las sesiones de un proyecto, de la más reciente a la más antigua.
    pub async fn list_sessions(&self, project_id: i64) -> crate::Result<Vec<SessionMeta>> {
        let rows = sqlx::query_as::<_, SessionMeta>(
            "SELECT id, title, updated_at FROM sessions
             WHERE project_id = ? ORDER BY updated_at DESC, id DESC",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Crea una sesión nueva y devuelve sus metadatos.
    pub async fn create_session(&self, project_id: i64, title: &str) -> crate::Result<SessionMeta> {
        let id = sqlx::query("INSERT INTO sessions (project_id, title) VALUES (?, ?)")
            .bind(project_id)
            .bind(title)
            .execute(&self.pool)
            .await?
            .last_insert_rowid();
        let updated_at =
            sqlx::query_scalar::<_, String>("SELECT updated_at FROM sessions WHERE id = ?")
                .bind(id)
                .fetch_one(&self.pool)
                .await?;
        Ok(SessionMeta {
            id,
            title: title.to_string(),
            updated_at,
        })
    }

    /// Renombra una sesión y marca su `updated_at`.
    pub async fn rename_session(&self, session_id: i64, title: &str) -> crate::Result<()> {
        sqlx::query("UPDATE sessions SET title = ?, updated_at = datetime('now') WHERE id = ?")
            .bind(title)
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Carga el historial de mensajes de una sesión, en orden cronológico.
    pub async fn load_messages(&self, session_id: i64) -> crate::Result<Vec<Message>> {
        let rows = sqlx::query_as::<_, StoredMessage>(
            "SELECT role, content, tool_calls, tool_call_id
             FROM messages WHERE session_id = ? ORDER BY id ASC",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(StoredMessage::into_message).collect())
    }

    /// Añade un mensaje (con sus *parts*) a una sesión y refresca `updated_at`.
    pub async fn append_message(&self, session_id: i64, message: &Message) -> crate::Result<()> {
        let tool_calls = if message.tool_calls.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&message.tool_calls).unwrap_or_default())
        };
        sqlx::query(
            "INSERT INTO messages (session_id, role, content, tool_calls, tool_call_id)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(session_id)
        .bind(role_str(message.role))
        .bind(&message.content)
        .bind(tool_calls)
        .bind(&message.tool_call_id)
        .execute(&self.pool)
        .await?;
        sqlx::query("UPDATE sessions SET updated_at = datetime('now') WHERE id = ?")
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// Fila cruda de `messages` antes de mapearse al tipo de dominio.
#[derive(sqlx::FromRow)]
struct StoredMessage {
    role: String,
    content: String,
    tool_calls: Option<String>,
    tool_call_id: Option<String>,
}

impl StoredMessage {
    fn into_message(self) -> Message {
        let role = match self.role.as_str() {
            "system" => Role::System,
            "assistant" => Role::Assistant,
            "tool" => Role::Tool,
            _ => Role::User,
        };
        let tool_calls: Vec<ToolCall> = self
            .tool_calls
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        Message {
            role,
            content: self.content,
            tool_calls,
            tool_call_id: self.tool_call_id,
        }
    }
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

/// Directorio de datos XDG de la app (`$XDG_DATA_HOME/xiě-code` o el fallback).
fn data_dir() -> crate::Result<PathBuf> {
    let base = match std::env::var_os("XDG_DATA_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => {
            let home = std::env::var_os("HOME").ok_or_else(|| {
                sqlx::Error::Configuration("no se pudo determinar HOME ni XDG_DATA_HOME".into())
            })?;
            PathBuf::from(home).join(".local/share")
        }
    };
    Ok(base.join("xiě-code"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ruta de DB temporal y única para aislar cada test.
    fn temp_db() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("xiě-code-test-{nanos}.db"))
    }

    #[tokio::test]
    async fn round_trip_sessions_and_messages() {
        let path = temp_db();
        let store = Store::connect_at(path.clone()).unwrap();
        store.migrate().await.unwrap();

        // Idempotencia del proyecto: misma ruta → mismo id.
        let project = store.get_or_create_project("/tmp/proyecto").await.unwrap();
        assert_eq!(
            project,
            store.get_or_create_project("/tmp/proyecto").await.unwrap()
        );

        assert!(store.list_sessions(project).await.unwrap().is_empty());

        let session = store.create_session(project, "Nueva sesión").await.unwrap();
        store
            .append_message(session.id, &Message::user("hola"))
            .await
            .unwrap();
        store
            .append_message(session.id, &Message::assistant("¡hola!"))
            .await
            .unwrap();
        store.rename_session(session.id, "Saludo").await.unwrap();

        let sessions = store.list_sessions(project).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title, "Saludo");

        let messages = store.load_messages(session.id).await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[0].content, "hola");
        assert_eq!(messages[1].role, Role::Assistant);

        let _ = std::fs::remove_file(&path);
    }
}
