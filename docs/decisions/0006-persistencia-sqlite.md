# ADR-0006: Esquema de persistencia y estrategia de consultas con SQLite

- **Estado:** aceptado
- **Fecha:** 2026-05-22

## Contexto

La Fase 2 introduce la persistencia (proyectos, sesiones, mensajes) en SQLite vía
`sqlx`, dentro de `zhi-core` (la ubicación del módulo ya está fijada en
[`architecture.md`](../architecture.md) §3 y §7, no requiere ADR). Quedaban por
decidir tres cuestiones operativas: cómo crear/evolucionar el esquema, qué tipo
de consultas usar y dónde y cómo abrir la base de datos.

## Decisión

- **Esquema con `CREATE TABLE IF NOT EXISTS`** ejecutado en `Store::migrate()` al
  arrancar, en lugar de `sqlx::migrate!` con un directorio de migraciones.
- **Consultas verificadas en tiempo de ejecución** (`sqlx::query`,
  `query_as`, `query_scalar`), **no** las macros `query!` comprobadas en
  compilación. Estas exigen un `DATABASE_URL` accesible al compilar (o datos
  offline versionados), lo que complica el build y la CI sin aportar valor con un
  esquema tan pequeño.
- **Conexión perezosa** (`connect_lazy_with`): el pool se crea sin abrir conexión;
  la primera operación conecta sobre el runtime Tokio activo en ese momento. Así
  el `Store` se puede construir en el hilo de UI (en `init`) sin `await`, y las
  conexiones reales nacen en las tareas `relm4::spawn`.
- **Ubicación XDG**: la DB vive en `$XDG_DATA_HOME/xiě-code/xiě-code.db` (o
  `~/.local/share/...`). Un **proyecto** se identifica por la ruta de su
  directorio de trabajo (`UNIQUE`), de modo que las sesiones se agrupan por
  worktree.
- **Esquema** (`snake_case`): `projects(id, path UNIQUE, created_at)`,
  `sessions(id, project_id→projects, title, created_at, updated_at)`,
  `messages(id, session_id→sessions, role, content, created_at)`, con
  `ON DELETE CASCADE`. Los mensajes guardan `role`+`content` de texto; los
  *parts* estructurados (tool calls, adjuntos) se modelarán al llegar las tools
  (Fase 3) extendiendo el esquema.

## Alternativas consideradas

- **`sqlx::migrate!` + macros `query!`** — pros: verificación en compilación,
  versionado de migraciones. Contras: requiere `DATABASE_URL`/datos offline en el
  build y la CI; sobredimensionado para el esquema actual. Reabrible cuando el
  esquema crezca y las migraciones incrementales aporten.
- **Conexión ansiosa (`connect`)** — descartada: es `async`, obligaría a abrir la
  DB fuera del `init` de la UI o a crear un runtime temporal, con el riesgo de
  atar el pool a un runtime que luego se destruye.
- **DB global única (no por proyecto)** — el modelo elegido sí es global, pero las
  sesiones se particionan por `project_id`; se evita una DB por carpeta para no
  dispersar el historial.

## Consecuencias

- Build y CI no necesitan base de datos ni `DATABASE_URL`; el coste es perder la
  verificación de SQL en compilación (mitigado con el test de ida y vuelta de
  `store`).
- `Store` es `Clone` (envuelve un pool con recuento de referencias) y se comparte
  a las tareas async sin fricción; encaja con el patrón Tokio↔GLib existente.
- Añadir *parts* estructurados (Fase 3) implicará migrar el esquema; al no haber
  versionado formal habrá que introducir migraciones idempotentes o, llegado el
  caso, adoptar `sqlx::migrate!` (reabriendo este ADR).
- El historial se agrupa por worktree, alineado con el modelo de OpenCode.
