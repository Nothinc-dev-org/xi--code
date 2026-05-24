# AGENTS.md — zhi-gtk

> Implementado: vista de chat con streaming (Fase 1), sidebar de sesiones
> persistidas (Fase 2) y render de tools + permisos embebidos (Fase 3). Lee
> `/AGENTS.md`, `docs/architecture.md` y
> `docs/decisions/0002-gtk4-libadwaita-relm4.md` antes de tocar este crate.

## Responsabilidad

La **app de escritorio**: UI con GTK4 + libadwaita + Relm4. Es el binario del
proyecto. **Solo presentación e interacción**; no contiene lógica de negocio.

- Ventana principal (libadwaita), lista de sesiones, vista de chat, render de
  markdown del stream, composición de mensajes y adjuntos.
- Diálogos de resolución de permisos (preguntar/permitir/denegar).
- Selección de proyecto/worktree, agente y modelo.

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

## Invariantes

- Cero lógica de negocio aquí: si aparece, va a `zhi-core`.
- Ninguna llamada bloqueante en el hilo de UI.
- Errores de borde con `anyhow` (este crate es binario/borde).
