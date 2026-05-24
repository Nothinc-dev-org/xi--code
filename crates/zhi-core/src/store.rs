//! Persistencia en SQLite (vía `sqlx`) de proyectos, sesiones y mensajes.
//!
//! El esquema se crea con `CREATE TABLE IF NOT EXISTS` al abrir (idempotente),
//! evitando depender de migraciones en disco o de comprobación en compilación
//! (`DATABASE_URL`). Las consultas son verificadas en tiempo de ejecución.
//!
//! Ubicación de la base de datos: directorio de datos XDG del usuario
//! (`$XDG_DATA_HOME/xiě-code` o `~/.local/share/xiě-code`).

use std::collections::HashMap;
use std::path::PathBuf;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use zhi_provider::{Message, Role, ToolCall};

use crate::AgentKind;

/// Metadatos de una sesión para listarla en la UI (sin sus mensajes).
#[derive(Debug, Clone)]
pub struct SessionMeta {
    pub id: i64,
    pub title: String,
    pub updated_at: String,
    pub agent: AgentKind,
    /// Modelo elegido para la sesión. `None` en sesiones antiguas o creadas sin
    /// preferencia: la UI cae al `Engine::default_model` del proveedor activo.
    pub model: Option<String>,
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
        // Hash del snapshot (Fase 3c): se asocia al mensaje del asistente que
        // contiene las `tool_calls` del paso. Ver ADR-0007.
        self.ensure_column("messages", "snapshot").await?;
        // Perfil del agente activo de la sesión (Fase 4): "build" o "plan".
        // NULL en sesiones antiguas → tratadas como `Build` por defecto.
        self.ensure_column("sessions", "agent").await?;
        // Modelo elegido para la sesión (Fase 4): nombre tal cual lo entiende
        // el proveedor. NULL → la UI cae al default del `Engine`.
        self.ensure_column("sessions", "model").await?;
        // Chain of thought del paso (Fase 4) tal como lo emite el proveedor
        // (`reasoning_content` del SSE) y su duración medida en ms. NULL en
        // mensajes sin reasoning o anteriores a este campo.
        self.ensure_column("messages", "reasoning").await?;
        self.ensure_column_typed("messages", "reasoning_ms", "INTEGER")
            .await?;
        Ok(())
    }

    /// Añade una columna `TEXT` a `table` si aún no existe (idempotente).
    async fn ensure_column(&self, table: &str, column: &str) -> crate::Result<()> {
        self.ensure_column_typed(table, column, "TEXT").await
    }

    /// Variante de [`Self::ensure_column`] que permite fijar el tipo declarado
    /// (p. ej. `INTEGER`). SQLite usa afinidad de tipos, así que el tipo es
    /// orientativo, pero lo mantenemos honesto para los lectores del esquema.
    async fn ensure_column_typed(&self, table: &str, column: &str, ty: &str) -> crate::Result<()> {
        let cols: Vec<String> =
            sqlx::query_scalar(&format!("SELECT name FROM pragma_table_info('{table}')"))
                .fetch_all(&self.pool)
                .await?;
        if !cols.iter().any(|c| c == column) {
            sqlx::query(&format!("ALTER TABLE {table} ADD COLUMN {column} {ty}"))
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
        let rows = sqlx::query_as::<_, StoredSession>(
            "SELECT id, title, updated_at, agent, model FROM sessions
             WHERE project_id = ? ORDER BY updated_at DESC, id DESC",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(StoredSession::into_meta).collect())
    }

    /// Crea una sesión nueva con el `agent` y `model` indicados y devuelve sus
    /// metadatos. `model = None` deja la sesión sin preferencia (la UI usará el
    /// default del `Engine`).
    pub async fn create_session(
        &self,
        project_id: i64,
        title: &str,
        agent: AgentKind,
        model: Option<&str>,
    ) -> crate::Result<SessionMeta> {
        let id = sqlx::query(
            "INSERT INTO sessions (project_id, title, agent, model) VALUES (?, ?, ?, ?)",
        )
        .bind(project_id)
        .bind(title)
        .bind(agent.as_str())
        .bind(model)
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
            agent,
            model: model.map(str::to_string),
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

    /// Elimina una sesión y, en cascada, todos sus mensajes (ON DELETE
    /// CASCADE en `messages.session_id`).
    pub async fn delete_session(&self, session_id: i64) -> crate::Result<()> {
        sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Cambia el agente activo de una sesión (sin tocar `updated_at`: cambiar
    /// de perfil no es actividad nueva).
    pub async fn set_session_agent(&self, session_id: i64, agent: AgentKind) -> crate::Result<()> {
        sqlx::query("UPDATE sessions SET agent = ? WHERE id = ?")
            .bind(agent.as_str())
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Cambia el modelo activo de una sesión (sin tocar `updated_at`: igual que
    /// `set_session_agent`).
    pub async fn set_session_model(&self, session_id: i64, model: &str) -> crate::Result<()> {
        sqlx::query("UPDATE sessions SET model = ? WHERE id = ?")
            .bind(model)
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Carga el historial de mensajes de una sesión, en orden cronológico,
    /// emparejado con el id de cada fila (la UI lo usa para asociar snapshots
    /// y otros metadatos al mensaje correspondiente).
    pub async fn load_messages(&self, session_id: i64) -> crate::Result<Vec<(i64, Message)>> {
        let rows = sqlx::query_as::<_, StoredMessage>(
            "SELECT id, role, content, tool_calls, tool_call_id, reasoning
             FROM messages WHERE session_id = ? ORDER BY id ASC",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let id = row.id;
                (id, row.into_message())
            })
            .collect())
    }

    /// Mapa `message_id → duración_ms` del razonamiento del paso, para los
    /// mensajes de una sesión que registraron uno. La UI lo usa al cargar
    /// historial para mostrar la duración en las tarjetas colapsadas.
    pub async fn load_reasoning_durations(
        &self,
        session_id: i64,
    ) -> crate::Result<HashMap<i64, u64>> {
        let rows: Vec<(i64, i64)> = sqlx::query_as(
            "SELECT id, reasoning_ms FROM messages
             WHERE session_id = ? AND reasoning_ms IS NOT NULL",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, ms)| (id, ms.max(0) as u64))
            .collect())
    }

    /// Añade un mensaje (con sus *parts*) a una sesión, refresca `updated_at`
    /// y devuelve el id del mensaje insertado (para asociarle un snapshot).
    pub async fn append_message(&self, session_id: i64, message: &Message) -> crate::Result<i64> {
        let tool_calls = if message.tool_calls.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&message.tool_calls).unwrap_or_default())
        };
        let id = sqlx::query(
            "INSERT INTO messages (session_id, role, content, tool_calls, tool_call_id, reasoning)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(session_id)
        .bind(role_str(message.role))
        .bind(&message.content)
        .bind(tool_calls)
        .bind(&message.tool_call_id)
        .bind(&message.reasoning)
        .execute(&self.pool)
        .await?
        .last_insert_rowid();
        sqlx::query("UPDATE sessions SET updated_at = datetime('now') WHERE id = ?")
            .bind(session_id)
            .execute(&self.pool)
            .await?;
        Ok(id)
    }

    /// Asocia la duración (ms) del bloque de razonamiento al mensaje. Se
    /// guarda aparte de `reasoning` para que la UI pueda mostrarla en la
    /// tarjeta colapsada tras reabrir la sesión.
    pub async fn set_message_reasoning_ms(&self, message_id: i64, ms: u64) -> crate::Result<()> {
        sqlx::query("UPDATE messages SET reasoning_ms = ? WHERE id = ?")
            .bind(ms as i64)
            .bind(message_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Asocia un hash de snapshot al mensaje `message_id` (Fase 3c).
    pub async fn set_message_snapshot(&self, message_id: i64, hash: &str) -> crate::Result<()> {
        sqlx::query("UPDATE messages SET snapshot = ? WHERE id = ?")
            .bind(hash)
            .bind(message_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Mapa `message_id → snapshot_hash` para los mensajes de una sesión.
    /// La UI lo usa para repoblar los botones de revertir al cargar historial.
    pub async fn load_snapshots(&self, session_id: i64) -> crate::Result<HashMap<i64, String>> {
        let rows: Vec<(i64, String)> = sqlx::query_as(
            "SELECT id, snapshot FROM messages
             WHERE session_id = ? AND snapshot IS NOT NULL",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().collect())
    }
}

/// Fila cruda de `sessions` antes de mapearse a [`SessionMeta`].
#[derive(sqlx::FromRow)]
struct StoredSession {
    id: i64,
    title: String,
    updated_at: String,
    agent: Option<String>,
    model: Option<String>,
}

impl StoredSession {
    fn into_meta(self) -> SessionMeta {
        SessionMeta {
            id: self.id,
            title: self.title,
            updated_at: self.updated_at,
            agent: self
                .agent
                .as_deref()
                .map(AgentKind::from_str_or_default)
                .unwrap_or_default(),
            model: self.model,
        }
    }
}

/// Fila cruda de `messages` antes de mapearse al tipo de dominio.
#[derive(sqlx::FromRow)]
struct StoredMessage {
    id: i64,
    role: String,
    content: String,
    tool_calls: Option<String>,
    tool_call_id: Option<String>,
    reasoning: Option<String>,
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
            reasoning: self.reasoning,
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

        let session = store
            .create_session(project, "Nueva sesión", AgentKind::Build, None)
            .await
            .unwrap();
        assert_eq!(session.agent, AgentKind::Build);
        assert_eq!(session.model, None);

        store
            .set_session_agent(session.id, AgentKind::Plan)
            .await
            .unwrap();
        store
            .set_session_model(session.id, "deepseek-reasoner")
            .await
            .unwrap();
        let listed = store.list_sessions(project).await.unwrap();
        assert_eq!(listed.first().map(|s| s.agent), Some(AgentKind::Plan));
        assert_eq!(
            listed.first().and_then(|s| s.model.as_deref()),
            Some("deepseek-reasoner")
        );
        store
            .append_message(session.id, &Message::user("hola"))
            .await
            .unwrap();
        let assistant_id = store
            .append_message(
                session.id,
                &Message::assistant("¡hola!").with_reasoning("pienso, luego saludo"),
            )
            .await
            .unwrap();
        store.rename_session(session.id, "Saludo").await.unwrap();

        store
            .set_message_snapshot(assistant_id, "abc123")
            .await
            .unwrap();
        let snaps = store.load_snapshots(session.id).await.unwrap();
        assert_eq!(snaps.get(&assistant_id).map(String::as_str), Some("abc123"));

        store
            .set_message_reasoning_ms(assistant_id, 1234)
            .await
            .unwrap();
        let durations = store.load_reasoning_durations(session.id).await.unwrap();
        assert_eq!(durations.get(&assistant_id).copied(), Some(1234));

        let sessions = store.list_sessions(project).await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].title, "Saludo");

        let messages = store.load_messages(session.id).await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].1.role, Role::User);
        assert_eq!(messages[0].1.content, "hola");
        assert_eq!(messages[1].1.role, Role::Assistant);
        assert_eq!(messages[1].0, assistant_id);
        assert_eq!(
            messages[1].1.reasoning.as_deref(),
            Some("pienso, luego saludo")
        );

        let _ = std::fs::remove_file(&path);
    }
}
