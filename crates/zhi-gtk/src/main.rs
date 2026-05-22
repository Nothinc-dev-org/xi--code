//! Binario de la app de escritorio xiě-code (GTK4 + libadwaita + Relm4).
//!
//! Fase 1 (MVP de chat): vista de chat con DeepSeek y streaming de la respuesta.
//! Fase 2 (persistencia): sidebar de sesiones respaldadas en SQLite (`zhi-core`),
//! creación de sesiones nuevas y reanudación de existentes.
//! Fase 3 (tools y permisos): el motor invoca tools; la UI muestra tarjetas de
//! ejecución y resuelve permisos con un diálogo. La lógica de dominio vive en
//! `zhi-core`; este crate es solo presentación. Ver `crates/zhi-gtk/AGENTS.md`.

mod markdown;

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use futures::StreamExt;
use relm4::adw::prelude::*;
use relm4::{adw, gtk, Component, ComponentParts, ComponentSender, RelmApp, RelmWidgetExt};
use zhi_core::{
    AgentEvent, Engine, Message, PermissionDecision, PermissionRequest, PermissionResolver, Role,
    Session, SessionMeta, Store, ToolContext,
};

const APP_ID: &str = "ai.xiecode.App";
/// Máximo de caracteres mostrados de la salida de una tool (la UI no es un visor).
const TOOL_OUTPUT_MAX: usize = 4000;

struct App {
    /// `None` si no se pudo inicializar el motor (p. ej. falta la API key).
    engine: Option<Engine>,
    /// `None` si no se pudo abrir la base de datos.
    store: Option<Store>,
    /// Directorio de trabajo del proyecto activo (worktree de las tools).
    workdir: PathBuf,
    /// Proyecto activo (directorio de trabajo); se resuelve en el arranque.
    project_id: Option<i64>,
    /// Sesiones del proyecto, de la más reciente a la más antigua (orden de fila).
    sessions: Vec<SessionMeta>,
    /// Sesión seleccionada actualmente.
    current_session: Option<i64>,
    /// Historial en memoria de la sesión activa.
    session: Session,
    /// Label del mensaje del asistente que se está transmitiendo ahora mismo.
    streaming_label: Option<gtk::Label>,
    /// Label de salida de la tarjeta de tool en ejecución.
    tool_output: Option<gtk::Label>,
    /// Texto acumulado del segmento de texto en curso (markdown sin renderizar).
    partial: String,
    busy: bool,
}

#[derive(Debug)]
enum Msg {
    /// Arranque completado: proyecto resuelto y sesiones cargadas.
    Bootstrapped {
        project_id: i64,
        sessions: Vec<SessionMeta>,
    },
    /// El usuario seleccionó la fila `index` del sidebar.
    SelectIndex(i32),
    /// Llegó el historial de la sesión seleccionada.
    SessionLoaded(Vec<Message>),
    /// Crear una sesión nueva.
    NewSession,
    /// Se creó una sesión nueva.
    SessionCreated(SessionMeta),
    /// Se renombró una sesión (al enviar su primer mensaje).
    Renamed { id: i64, title: String },
    /// El usuario envía un prompt.
    Send(String),
    /// Llega un fragmento de texto del asistente.
    Delta(String),
    /// El agente va a ejecutar una tool.
    ToolStarted { name: String, arguments: String },
    /// Una tool terminó con su salida.
    ToolFinished { output: String, ok: bool },
    /// El motor pide autorización para una tool con efectos.
    PermissionRequested {
        request: PermissionRequest,
        reply: tokio::sync::oneshot::Sender<PermissionDecision>,
    },
    /// El turno terminó: mensajes producidos para persistir y extender la sesión.
    TurnFinished(Vec<Message>),
    /// El turno falló.
    Failed(String),
}

/// Resolver de permisos respaldado por la UI: envía la solicitud al hilo de GLib
/// y espera la decisión por un canal `oneshot`. Ver ADR-0007.
struct UiPermissions {
    sender: ComponentSender<App>,
}

#[async_trait::async_trait]
impl PermissionResolver for UiPermissions {
    async fn resolve(&self, request: PermissionRequest) -> PermissionDecision {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.sender
            .input(Msg::PermissionRequested { request, reply: tx });
        // Si el canal se cierra sin respuesta, denegar por seguridad.
        rx.await.unwrap_or(PermissionDecision::Deny)
    }
}

#[relm4::component]
// La macro de Relm4 inicializa los widgets con `#[name]` con un valor dummy que
// reasigna después; eso dispara `unused_assignments` en el código generado.
#[allow(unused_assignments)]
impl Component for App {
    type Init = ();
    type Input = Msg;
    type Output = ();
    type CommandOutput = ();

    view! {
        adw::ApplicationWindow {
            set_title: Some("xiě-code"),
            set_default_width: 1040,
            set_default_height: 680,

            gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,

                // ── Sidebar de sesiones ──────────────────────────────────────
                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_size_request: (260, -1),

                    adw::HeaderBar {
                        // Los controles de ventana (min/max/cerrar) viven solo en
                        // la cabecera del área de conversación, no en el sidebar.
                        set_show_start_title_buttons: false,
                        set_show_end_title_buttons: false,
                        #[wrap(Some)]
                        set_title_widget = &adw::WindowTitle {
                            set_title: "Sesiones",
                        },
                        pack_start = &gtk::Button {
                            set_icon_name: "list-add-symbolic",
                            set_tooltip_text: Some("Nueva sesión"),
                            connect_clicked[sender] => move |_| {
                                sender.input(Msg::NewSession);
                            },
                        },
                    },

                    gtk::ScrolledWindow {
                        set_vexpand: true,
                        set_hscrollbar_policy: gtk::PolicyType::Never,

                        #[name = "session_list"]
                        gtk::ListBox {
                            set_selection_mode: gtk::SelectionMode::Single,
                            add_css_class: "navigation-sidebar",
                            connect_row_selected[sender] => move |_, row| {
                                if let Some(row) = row {
                                    sender.input(Msg::SelectIndex(row.index()));
                                }
                            },
                        },
                    },
                },

                gtk::Separator {
                    set_orientation: gtk::Orientation::Vertical,
                },

                // ── Área de conversación ─────────────────────────────────────
                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_hexpand: true,

                    adw::HeaderBar,

                    #[name = "scroller"]
                    gtk::ScrolledWindow {
                        set_vexpand: true,
                        set_hscrollbar_policy: gtk::PolicyType::Never,

                        #[name = "chat_list"]
                        #[wrap(Some)]
                        set_child = &gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_spacing: 16,
                            set_margin_all: 16,
                        },
                    },

                    gtk::Box {
                        set_orientation: gtk::Orientation::Horizontal,
                        set_spacing: 6,
                        set_margin_all: 12,

                        #[name = "entry"]
                        gtk::Entry {
                            set_hexpand: true,
                            set_placeholder_text: Some("Escribe un mensaje…"),
                            connect_activate[sender] => move |entry| {
                                let text = entry.text().trim().to_string();
                                if !text.is_empty() {
                                    sender.input(Msg::Send(text));
                                    entry.set_text("");
                                }
                            },
                        },

                        #[name = "send_button"]
                        gtk::Button {
                            set_label: "Enviar",
                            add_css_class: "suggested-action",
                            connect_clicked[sender, entry] => move |_| {
                                let text = entry.text().trim().to_string();
                                if !text.is_empty() {
                                    sender.input(Msg::Send(text));
                                    entry.set_text("");
                                }
                            },
                        },
                    },
                },
            },
        }
    }

    fn init(
        _init: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let store = match Store::connect_default() {
            Ok(store) => Some(store),
            Err(err) => {
                tracing::error!(%err, "no se pudo abrir la base de datos");
                None
            }
        };

        let workdir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        let model = App {
            engine: Engine::from_env().ok(),
            store: store.clone(),
            workdir: workdir.clone(),
            project_id: None,
            sessions: Vec::new(),
            current_session: None,
            session: Session::new(),
            streaming_label: None,
            tool_output: None,
            partial: String::new(),
            busy: false,
        };

        let widgets = view_output!();

        if model.engine.is_none() {
            append_bubble(
                &widgets.chat_list,
                Role::System,
                "No se encontró DEEPSEEK_API_KEY en el entorno. Expórtala y reinicia la app.",
            );
        }

        // Arranque asíncrono: migrar el esquema, resolver el proyecto (directorio
        // de trabajo) y listar sus sesiones. El resultado vuelve por `sender`.
        if let Some(store) = store {
            let project_path = workdir.display().to_string();
            relm4::spawn(async move {
                match bootstrap(&store, &project_path).await {
                    Ok((project_id, sessions)) => {
                        sender.input(Msg::Bootstrapped {
                            project_id,
                            sessions,
                        });
                    }
                    Err(err) => sender.input(Msg::Failed(err.to_string())),
                }
            });
        } else {
            append_bubble(
                &widgets.chat_list,
                Role::System,
                "No se pudo abrir el almacén de sesiones; el historial no se guardará.",
            );
        }

        update_controls(&model, &widgets);
        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: Self::Input,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        let was_busy = self.busy;

        match message {
            Msg::Bootstrapped {
                project_id,
                sessions,
            } => {
                self.project_id = Some(project_id);
                self.sessions = sessions;
                if self.sessions.is_empty() {
                    sender.input(Msg::NewSession);
                } else {
                    rebuild_session_list(&widgets.session_list, &self.sessions, 0);
                }
            }

            Msg::SelectIndex(index) => {
                if self.busy {
                    return;
                }
                let Some(meta) = self.sessions.get(index as usize) else {
                    return;
                };
                let id = meta.id;
                if self.current_session == Some(id) {
                    return; // Ya está cargada (p. ej. selección programática).
                }
                self.current_session = Some(id);
                self.streaming_label = None;
                self.tool_output = None;
                self.partial.clear();
                clear_chat(&widgets.chat_list);

                if let Some(store) = self.store.clone() {
                    let sender = sender.clone();
                    relm4::spawn(async move {
                        match store.load_messages(id).await {
                            Ok(messages) => sender.input(Msg::SessionLoaded(messages)),
                            Err(err) => sender.input(Msg::Failed(err.to_string())),
                        }
                    });
                }
            }

            Msg::SessionLoaded(messages) => {
                clear_chat(&widgets.chat_list);
                render_history(&widgets.chat_list, &messages);
                self.session = Session::from_messages(messages);
            }

            Msg::NewSession => {
                let (Some(store), Some(project_id)) = (self.store.clone(), self.project_id) else {
                    return;
                };
                let sender = sender.clone();
                relm4::spawn(async move {
                    match store.create_session(project_id, "Nueva sesión").await {
                        Ok(meta) => sender.input(Msg::SessionCreated(meta)),
                        Err(err) => sender.input(Msg::Failed(err.to_string())),
                    }
                });
            }

            Msg::SessionCreated(meta) => {
                let id = meta.id;
                self.sessions.insert(0, meta);
                self.current_session = Some(id);
                self.session = Session::new();
                self.streaming_label = None;
                self.tool_output = None;
                self.partial.clear();
                clear_chat(&widgets.chat_list);
                rebuild_session_list(&widgets.session_list, &self.sessions, 0);
            }

            Msg::Renamed { id, title } => {
                if let Some(meta) = self.sessions.iter_mut().find(|m| m.id == id) {
                    meta.title = title;
                }
                let selected = self
                    .current_session
                    .and_then(|cur| self.sessions.iter().position(|m| m.id == cur))
                    .unwrap_or(0);
                rebuild_session_list(&widgets.session_list, &self.sessions, selected as i32);
            }

            Msg::Send(text) => {
                let Some(engine) = self.engine.clone() else {
                    return;
                };
                let (Some(store), Some(session_id)) = (self.store.clone(), self.current_session)
                else {
                    return;
                };
                if self.busy {
                    return;
                }

                let first_message = self.session.is_empty();
                append_bubble(&widgets.chat_list, Role::User, &text);
                self.session.push_user(&text);

                // Persistir el mensaje del usuario; si es el primero, renombrar la
                // sesión con un resumen para que sea reconocible en el sidebar.
                {
                    let store = store.clone();
                    let text = text.clone();
                    let sender = sender.clone();
                    relm4::spawn(async move {
                        if let Err(err) = store
                            .append_message(session_id, &Message::user(&text))
                            .await
                        {
                            tracing::error!(%err, "no se pudo guardar el mensaje del usuario");
                            return;
                        }
                        if first_message {
                            let title = summarize_title(&text);
                            if store.rename_session(session_id, &title).await.is_ok() {
                                sender.input(Msg::Renamed {
                                    id: session_id,
                                    title,
                                });
                            }
                        }
                    });
                }

                self.streaming_label = None;
                self.tool_output = None;
                self.partial.clear();
                self.busy = true;

                // Lanzar el bucle de agente: consume el stream de eventos y los
                // reenvía como mensajes Relm4 (patrón Tokio↔GLib).
                let ctx = ToolContext::new(self.workdir.clone());
                let resolver: Arc<dyn PermissionResolver> = Arc::new(UiPermissions {
                    sender: sender.clone(),
                });
                let history = self.session.history();
                let sender = sender.clone();
                relm4::spawn(async move {
                    let mut stream = engine.run_turn(history, ctx, resolver);
                    while let Some(event) = stream.next().await {
                        match event {
                            Ok(AgentEvent::Delta(d)) => sender.input(Msg::Delta(d)),
                            Ok(AgentEvent::ToolStarted { name, arguments }) => {
                                sender.input(Msg::ToolStarted { name, arguments })
                            }
                            Ok(AgentEvent::ToolFinished { output, ok, .. }) => {
                                sender.input(Msg::ToolFinished { output, ok })
                            }
                            Ok(AgentEvent::Turn(messages)) => {
                                sender.input(Msg::TurnFinished(messages))
                            }
                            Err(err) => {
                                sender.input(Msg::Failed(err.to_string()));
                                return;
                            }
                        }
                    }
                });
            }

            Msg::Delta(delta) => {
                self.partial.push_str(&delta);
                let label = self
                    .streaming_label
                    .get_or_insert_with(|| append_bubble(&widgets.chat_list, Role::Assistant, ""));
                // Texto plano mientras llega: el markdown puede estar a medias.
                label.set_text(&self.partial);
            }

            Msg::ToolStarted { name, arguments } => {
                // Cierra el segmento de texto previo (si lo hay) renderizando markdown.
                if let Some(label) = self.streaming_label.take() {
                    if !self.partial.is_empty() {
                        label.set_markup(&markdown::to_pango(&self.partial));
                    }
                }
                self.partial.clear();
                let output = append_tool_card(&widgets.chat_list, &name, &arguments, "Ejecutando…");
                self.tool_output = Some(output);
            }

            Msg::ToolFinished { output, ok } => {
                if let Some(label) = self.tool_output.take() {
                    set_tool_output(&label, &output, ok);
                }
            }

            Msg::PermissionRequested { request, reply } => {
                if let Some(label) = &self.tool_output {
                    label.set_text("Esperando permiso del usuario…");
                }
                append_permission_controls(&widgets.chat_list, &request.tool_name, reply);
            }

            Msg::TurnFinished(messages) => {
                if let Some(label) = self.streaming_label.take() {
                    label.set_markup(&markdown::to_pango(&self.partial));
                }
                self.partial.clear();
                self.tool_output = None;
                self.busy = false;
                self.session.extend(messages.clone());

                if let (Some(store), Some(session_id)) = (self.store.clone(), self.current_session)
                {
                    relm4::spawn(async move {
                        for msg in &messages {
                            if let Err(err) = store.append_message(session_id, msg).await {
                                tracing::error!(%err, "no se pudo guardar un mensaje del turno");
                            }
                        }
                    });
                }
            }

            Msg::Failed(err) => {
                if let Some(label) = self.streaming_label.take() {
                    label.set_markup(&error_markup(&err));
                } else {
                    append_bubble(&widgets.chat_list, Role::System, &format!("Error: {err}"));
                }
                self.tool_output = None;
                self.partial.clear();
                self.busy = false;
            }
        }

        update_controls(self, widgets);
        if was_busy && !self.busy {
            widgets.entry.grab_focus();
        }
        scroll_to_bottom(&widgets.scroller);
    }
}

/// Migra el esquema, resuelve el proyecto y lista sus sesiones.
async fn bootstrap(store: &Store, project_path: &str) -> zhi_core::Result<(i64, Vec<SessionMeta>)> {
    store.migrate().await?;
    let project_id = store.get_or_create_project(project_path).await?;
    let sessions = store.list_sessions(project_id).await?;
    Ok((project_id, sessions))
}

/// Habilita/inhabilita el envío según haya motor, sesión y no estemos ocupados.
fn update_controls(model: &App, widgets: &AppWidgets) {
    let ready = model.engine.is_some() && model.current_session.is_some() && !model.busy;
    widgets.entry.set_sensitive(ready);
    widgets.send_button.set_sensitive(ready);
}

/// Resumen corto para el título de una sesión a partir de su primer mensaje.
fn summarize_title(text: &str) -> String {
    let trimmed = text.trim();
    let summary: String = trimmed.chars().take(40).collect();
    if trimmed.chars().count() > 40 {
        format!("{}…", summary.trim_end())
    } else {
        summary
    }
}

/// Reconstruye las filas del sidebar y selecciona la fila `selected` (la
/// selección programática se neutraliza en `Msg::SelectIndex`).
fn rebuild_session_list(list: &gtk::ListBox, sessions: &[SessionMeta], selected: i32) {
    clear_list(list);
    for meta in sessions {
        let label = gtk::Label::new(Some(&meta.title));
        label.set_xalign(0.0);
        label.set_margin_all(8);
        label.set_max_width_chars(28);
        label.set_ellipsize(relm4::gtk::pango::EllipsizeMode::End);
        list.append(&label);
    }
    if let Some(row) = list.row_at_index(selected) {
        list.select_row(Some(&row));
    }
}

fn clear_list(list: &gtk::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

fn clear_chat(chat_list: &gtk::Box) {
    while let Some(child) = chat_list.first_child() {
        chat_list.remove(&child);
    }
}

/// Renderiza un historial cargado: burbujas de texto y tarjetas de tool con su
/// salida (los resultados `Role::Tool` se fusionan en la tarjeta por id).
fn render_history(chat_list: &gtk::Box, messages: &[Message]) {
    use std::collections::HashMap;
    let outputs: HashMap<&str, &str> = messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .filter_map(|m| m.tool_call_id.as_deref().map(|id| (id, m.content.as_str())))
        .collect();

    for message in messages {
        match message.role {
            Role::User | Role::System => {
                append_bubble(chat_list, message.role, &message.content);
            }
            Role::Assistant => {
                if !message.content.is_empty() {
                    let label = append_bubble(chat_list, Role::Assistant, &message.content);
                    label.set_markup(&markdown::to_pango(&message.content));
                }
                for call in &message.tool_calls {
                    let output = outputs.get(call.id.as_str()).copied().unwrap_or("");
                    let label = append_tool_card(
                        chat_list,
                        &call.function.name,
                        &call.function.arguments,
                        output,
                    );
                    set_tool_output(&label, output, true);
                }
            }
            Role::Tool => {} // ya fusionado en la tarjeta de la llamada
        }
    }
}

/// Añade una burbuja de mensaje al chat y devuelve el label de su contenido (para
/// poder actualizarlo durante el streaming).
fn append_bubble(chat_list: &gtk::Box, role: Role, content: &str) -> gtk::Label {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
    row.set_halign(gtk::Align::Fill);

    let author = gtk::Label::new(Some(role_name(role)));
    author.set_xalign(0.0);
    author.add_css_class("dim-label");
    author.add_css_class("caption-heading");

    let body = gtk::Label::new(Some(content));
    body.set_xalign(0.0);
    body.set_wrap(true);
    body.set_selectable(true);
    body.set_halign(gtk::Align::Start);

    row.append(&author);
    row.append(&body);
    chat_list.append(&row);

    body
}

/// Añade una tarjeta de ejecución de tool y devuelve el label de su salida.
fn append_tool_card(chat_list: &gtk::Box, name: &str, args: &str, initial: &str) -> gtk::Label {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
    card.add_css_class("card");
    card.set_margin_all(4);

    let header = gtk::Label::new(Some(&format!("🔧 {name}")));
    header.set_xalign(0.0);
    header.set_margin_top(8);
    header.set_margin_start(8);
    header.set_margin_end(8);
    header.add_css_class("heading");

    let args_quote = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    args_quote.set_margin_start(8);
    args_quote.set_margin_end(8);

    let quote_line = gtk::Separator::new(gtk::Orientation::Vertical);
    quote_line.add_css_class("accent");

    let args_label = gtk::Label::new(Some(args));
    args_label.set_xalign(0.0);
    args_label.set_wrap(true);
    args_label.set_selectable(true);
    args_label.add_css_class("monospace");
    args_label.add_css_class("dim-label");

    args_quote.append(&quote_line);
    args_quote.append(&args_label);

    let output = gtk::Label::new(Some(initial));
    output.set_xalign(0.0);
    output.set_wrap(true);
    output.set_selectable(true);
    output.set_margin_all(8);
    output.add_css_class("monospace");

    card.append(&header);
    card.append(&args_quote);
    card.append(&output);
    chat_list.append(&card);

    output
}

fn append_permission_controls(
    chat_list: &gtk::Box,
    tool_name: &str,
    reply: tokio::sync::oneshot::Sender<PermissionDecision>,
) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.set_margin_start(8);
    row.set_margin_end(8);
    row.set_margin_bottom(8);

    let label = gtk::Label::new(Some(&format!("Permitir ejecutar {tool_name}?")));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.add_css_class("dim-label");

    let deny = gtk::Button::with_label("Denegar");
    let allow = gtk::Button::with_label("Permitir");
    allow.add_css_class("suggested-action");

    let reply = Rc::new(Cell::new(Some(reply)));
    allow.connect_clicked({
        let deny = deny.clone();
        let allow = allow.clone();
        let label = label.clone();
        let reply = reply.clone();
        move |_| {
            allow.set_sensitive(false);
            deny.set_sensitive(false);
            if let Some(tx) = reply.take() {
                let _ = tx.send(PermissionDecision::Allow);
                label.set_text("Permiso concedido.");
            }
        }
    });

    deny.connect_clicked({
        let deny = deny.clone();
        let allow = allow.clone();
        let label = label.clone();
        let reply = reply.clone();
        move |_| {
            allow.set_sensitive(false);
            deny.set_sensitive(false);
            if let Some(tx) = reply.take() {
                let _ = tx.send(PermissionDecision::Deny);
                label.set_text("Permiso denegado.");
            }
        }
    });

    row.append(&label);
    row.append(&deny);
    row.append(&allow);
    chat_list.append(&row);
}

/// Vuelca la salida de una tool (truncada) en su label, con color según éxito.
fn set_tool_output(label: &gtk::Label, output: &str, ok: bool) {
    let shown = truncate(output, TOOL_OUTPUT_MAX);
    if ok {
        label.set_text(&shown);
    } else {
        label.set_markup(&error_markup(&shown));
    }
}

/// Recorta `s` a `max` caracteres añadiendo una marca de truncado.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let head: String = s.chars().take(max).collect();
        format!("{head}\n… (salida truncada)")
    } else {
        s.to_string()
    }
}

/// Markup Pango en rojo para mensajes de error, con el texto escapado.
fn error_markup(text: &str) -> String {
    format!(
        "<span foreground=\"#e01b24\">{}</span>",
        relm4::gtk::glib::markup_escape_text(text).as_str()
    )
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::User => "Tú",
        Role::Assistant => "xiě-code",
        Role::System => "Sistema",
        Role::Tool => "Tool",
    }
}

/// Desplaza el scroll al final tras aplicar el layout pendiente.
fn scroll_to_bottom(scroller: &gtk::ScrolledWindow) {
    let adjustment = scroller.vadjustment();
    relm4::gtk::glib::idle_add_local_once(move || {
        adjustment.set_value(adjustment.upper());
    });
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let app = RelmApp::new(APP_ID);
    app.run::<App>(());
}
