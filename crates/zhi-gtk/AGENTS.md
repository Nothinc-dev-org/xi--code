# AGENTS.md — zhi-gtk

> Implementado: vista de chat con streaming (Fase 1), sidebar de sesiones
> persistidas y colapsable (Fase 2), render de tools + permisos embebidos (Fase 3),
> panel de cambios con diff (Fase 4), paleta de comandos (Ctrl+P), render de
> tablas Markdown y layout responsive. Lee `/AGENTS.md`, `docs/architecture.md` y
> `docs/decisions/0002-gtk4-libadwaita-relm4.md` antes de tocar este crate.

## Responsabilidad

La **app de escritorio**: UI con GTK4 + libadwaita + Relm4. Es el binario del
proyecto. **Solo presentación e interacción**; no contiene lógica de negocio.

- Ventana principal (libadwaita), sidebar de sesiones (colapsable), vista de
  chat, panel de cambios con diff unificado, render de markdown del stream
  (prosa + bloques de código + tablas), composición de mensajes y adjuntos.
- Paleta de comandos (Ctrl+P) con búsqueda y acciones.
- Diálogos de resolución de permisos (preguntar/permitir/denegar).
- Selección de proyecto/worktree, agente y modelo (filtrado por proveedores
  conectados).
- Layout responsive: panel de cambios visible solo si la ventana ≥ 1180px.

## Depende de

Solo `zhi-core`. La UI envía **comandos** al motor y recibe **eventos**; no llama
nunca directamente a `zhi-provider`/`zhi-tool`/`zhi-mcp`/`zhi-lsp`.

## El patrón crítico: Tokio ↔ GLib

GTK no es thread-safe y su loop (GLib) vive en el hilo principal. El motor corre
en un runtime Tokio en otros hilos. Por tanto:

- La UI **nunca bloquea** el hilo de GLib esperando al motor.
- Comandos UI→motor y eventos motor→UI viajan por canales (`async_channel`),
  puenteados al loop de GLib (los `Worker`/comandos de Relm4 reciben los eventos
  del motor como mensajes y actualizan el estado del componente).
- Los widgets se tocan **solo** desde el hilo de UI, en respuesta a esos mensajes.
- El stream de tokens del LLM se aplica incrementalmente como mensajes Relm4.

## Render del mensaje del asistente

El cuerpo de la burbuja del asistente es un `gtk::Box` vertical que se rellena
con bloques heterogéneos:

- **Prosa**: `Label` con markup Pango (negrita, cursiva, headings, listas,
  inline code) generado a partir del Markdown.
- **Bloques de código** (fences ```` ``` ````): tarjeta independiente con el
  lenguaje opcional en cabecera, código monoespaciado seleccionable dentro de
  un `ScrolledWindow` horizontal y un botón flotante (`gtk::Overlay`) en la
  esquina inferior derecha que copia el texto crudo al portapapeles.

Durante el streaming, mientras el markdown puede estar a medias, el cuerpo
muestra un único `Label` con texto plano que se va actualizando con cada
delta. Al cerrar el segmento (siguiente `ToolStarted` o `TurnFinished`), el
cuerpo se vacía y se rellena con los bloques renderizados.

El tokenizador vive en `src/markdown.rs` (`parse_blocks`) y consume
`pulldown-cmark`; los bloques se materializan a widgets en `main.rs`
(`fill_with_blocks`, `make_code_block`).

## Toast

Un único `gtk::Revealer` montado como `add_overlay` del `gtk::Overlay` raíz,
con `halign=Center, valign=Start`. Se dispara con `Msg::Toast(text)` y se
auto-oculta tras un timeout corto; un toast nuevo cancela el timeout previo.
Hoy lo usa el botón de copiar de los bloques de código ("Texto Copiado").

## Sidebar de sesiones colapsable

Dos `gtk::Box`互斥: `sessions_sidebar_collapsed` (solo botón de toggle con
icono `sidebar-show-symbolic`) y `sessions_sidebar_expanded` (header bar con
título "Sesiones", botón "Nueva sesión" y botón de colapsar). Toggle por
`Msg::ToggleSessionsSidebar`. El estado `sessions_sidebar_collapsed` se
persiste en memoria (no en DB). El sidebar colapsado tiene `size_request`
de 48px; el expandido usa el ancho por defecto.

## Panel de cambios

`gtk::Revealer` con transición `SlideLeft` situado a la derecha del chat.
Visible solo si `wide_layout` (ventana ≥ `CHANGES_PANEL_BREAKPOINT` = 1180px)
y hay contenido en `changes_patch`. El diff se obtiene de forma asíncrona
(`request_changes_patch`): si la sesión tiene `session_base_snapshot`, usa
`Snapshots::patch(hash)`; si no, usa `Snapshots::worktree_patch()` como
fallback. El diff se parsea en `parse_patch_files` (retorna `Vec<DiffFile>`
con `DiffLine` por cada línea) y se renderiza como tarjetas por archivo con
líneas coloreadas (`.diff-line-addition` / `.diff-line-deletion`). Barra de
navegación flotante (`changes_nav_bar`) con botones prev/next y label con
ruta del archivo actual y conteo.

## Paleta de comandos

`adw::MessageDialog` con `gtk::SearchEntry` y `gtk::ListBox` filtrable.
Se abre con Ctrl+P (ShortcutController global en la ventana). Acciones
disponibles (`CommandPaletteAction`): SelectModel, ConnectProvider,
NewSession, ToggleThinking, Quit. Cierre con Escape o Ctrl+P. El foco
se devuelve al `entry` al cerrar.

## Render de tablas

`markdown::Block::Table { headers, rows }` parseado con
`pulldown_cmark::Options::ENABLE_TABLES`. Se materializa como `gtk::Grid`
dentro de un `gtk::Frame` con cabeceras en negrita (`.heading`) y scroll
horizontal (`gtk::PolicyType::Automatic`). Las celdas son `gtk::Label`
seleccionable con `xalign=0.0`.

## Layout responsive

`add_tick_callback` en la ventana detecta cambios de ancho y emite
`Msg::WindowWidthChanged(width)`. Si `width >= CHANGES_PANEL_BREAKPOINT`
(1180px), `wide_layout = true` y se muestra el panel de cambios junto al
chat. El panel tiene `size_request` de `CHANGES_PANEL_WIDTH` (420px). Si
la ventana es estrecha, el panel se oculta y el chat usa todo el ancho.

## Invariantes

- Cero lógica de negocio aquí: si aparece, va a `zhi-core`.
- Ninguna llamada bloqueante en el hilo de UI.
- Errores de borde con `anyhow` (este crate es binario/borde).
