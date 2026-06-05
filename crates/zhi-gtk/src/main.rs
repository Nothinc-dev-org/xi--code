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
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use futures::StreamExt;
use relm4::adw::prelude::*;
use relm4::{adw, gtk, Component, ComponentParts, ComponentSender, RelmApp, RelmWidgetExt};
use zhi_core::{
    oauth, AgentEvent, AgentKind, AuthInfo, AuthStore, Catalog, Engine, Message, ModelRef,
    PermissionDecision, PermissionRequest, PermissionResolver, Role, Session, SessionMeta,
    Snapshots, Store, ToolContext,
};

const APP_ID: &str = "ai.xiecode.App";
/// Máximo de caracteres mostrados de la salida de una tool (la UI no es un visor).
const TOOL_OUTPUT_MAX: usize = 4000;
const CHANGES_PANEL_BREAKPOINT: i32 = 1180;
const CHANGES_PANEL_WIDTH: i32 = 420;

struct App {
    /// Motor del agente. Existe siempre: la falta de credenciales se reporta al
    /// ejecutar un turno, no al arrancar (ver [ADR-0008]).
    engine: Engine,
    /// `None` si no se pudo abrir la base de datos.
    store: Option<Store>,
    /// Snapshots del worktree (Fase 3c). `None` hasta `SnapshotsReady`, o
    /// permanente si `git` no está disponible.
    snapshots: Option<Snapshots>,
    /// Directorio de trabajo del proyecto activo (worktree de las tools).
    workdir: PathBuf,
    /// Proyecto activo (directorio de trabajo); se resuelve en el arranque.
    project_id: Option<i64>,
    /// Sesiones del proyecto, de la más reciente a la más antigua (orden de fila).
    sessions: Vec<SessionMeta>,
    /// Sesión seleccionada actualmente.
    current_session: Option<i64>,
    /// Perfil del agente activo en la sesión actual. Persistido por sesión;
    /// las nuevas heredan el valor activo en el momento de su creación.
    current_agent: AgentKind,
    /// Modelo activo en la sesión actual. Persistido por sesión (igual que el
    /// agente); las nuevas heredan el valor activo en el momento de su creación.
    /// Arranca con `zhi_provider::default_model()`.
    current_model: String,
    /// Historial en memoria de la sesión activa.
    session: Session,
    /// Burbuja del asistente en streaming: el `label` recibe texto plano
    /// incremental; al cerrar el segmento, su `body` se vacía y se rellena
    /// con los bloques renderizados (prosa + bloques de código con copy).
    streaming_bubble: Option<StreamingBubble>,
    /// Label de salida de la tarjeta de tool en ejecución.
    tool_output: Option<gtk::Label>,
    /// Tarjeta de tool en ejecución (para colgar de ella el botón "Revertir"
    /// al cerrar el paso).
    tool_card: Option<gtk::Box>,
    /// Tarjeta del paso ya cerrado que está esperando el `message_id` del
    /// snapshot recién persistido para colgarle el botón "Revertir". Vive solo
    /// entre `TurnFinished` y `SnapshotPersisted`; sobrevive a un `Send` que
    /// llegue en medio (sin esto, el reset de `tool_card` por el siguiente
    /// turno se llevaría por delante el botón).
    revertible_card: Option<gtk::Box>,
    /// Snapshot tomado para el paso actual del agente: `(hash, message_index)`,
    /// donde `message_index` apunta al mensaje del asistente del paso dentro
    /// del `Vec<Message>` que llegará en `TurnFinished`.
    pending_snapshot: Option<(String, usize)>,
    /// `message_id` (DB) → `hash`. Repoblado al cargar una sesión y extendido
    /// tras cada turno.
    message_snapshots: HashMap<i64, String>,
    /// Primer snapshot con cambios de la sesión activa; base para el panel de cambios.
    session_base_snapshot: Option<String>,
    /// Diff renderizable del worktree actual respecto a `session_base_snapshot`.
    changes_patch: String,
    /// Índice del archivo actualmente enfocado en el nav del panel de cambios.
    changes_nav_index: usize,
    /// Rutas de los archivos del diff actual, para el nav.
    changes_nav_files: Vec<String>,
    /// `true` si el viewport tiene espacio para mostrar chat + cambios.
    wide_layout: bool,
    /// Texto acumulado del segmento de texto en curso (markdown sin renderizar).
    partial: String,
    /// Visibilidad global del *chain of thought*. Solo aplica si el modelo
    /// activo es razonador; el botón ojo es el toggle. Mientras `false`, las
    /// tarjetas de reasoning se ven colapsadas (spinner + resumen).
    show_reasoning: bool,
    /// Tarjeta de reasoning en streaming. Vive desde el primer `ReasoningDelta`
    /// hasta `ReasoningFinished`.
    reasoning_card: Option<ReasoningCard>,
    /// Texto acumulado del bloque de reasoning en curso.
    reasoning_partial: String,
    /// Todas las tarjetas de reasoning del chat (streaming + historial), para
    /// aplicar el toggle global al estado expandido/colapsado.
    reasoning_cards: Vec<ReasoningCard>,
    /// Duraciones de razonamiento del turno en curso, en el orden en que las
    /// emite el motor. Al persistir el `TurnFinished` se asignan a los
    /// mensajes de asistente que llevan `reasoning` por orden de aparición.
    reasoning_ms_queue: Vec<u64>,
    sessions_sidebar_collapsed: bool,
    command_palette_dialog: Option<adw::MessageDialog>,
    /// Timeout activo que oculta el toast. Si llega un toast nuevo antes de
    /// que dispare, se cancela el anterior para reiniciar el contador.
    toast_timeout: Option<gtk::glib::SourceId>,
    busy: bool,
}

/// Burbuja del asistente mientras se transmite: `label` acumula texto plano y,
/// al cerrar el segmento, los bloques renderizados se appendean a `body` (que
/// se vacía primero).
#[derive(Clone)]
struct StreamingBubble {
    body: gtk::Box,
    label: gtk::Label,
}

/// Widgets de una tarjeta de *chain of thought*. La tarjeta es siempre visible
/// con su encabezado; lo que cambia con el toggle global es la visibilidad de
/// `body` (el texto largo). El `spinner` solo se anima durante streaming.
#[derive(Clone)]
struct ReasoningCard {
    spinner: gtk::Spinner,
    summary: gtk::Label,
    body: gtk::Label,
}

impl ReasoningCard {
    fn apply_visibility(&self, show: bool) {
        self.body.set_visible(show);
    }
}

#[derive(Debug)]
enum Msg {
    /// Arranque completado: proyecto resuelto y sesiones cargadas.
    Bootstrapped {
        project_id: i64,
        sessions: Vec<SessionMeta>,
    },
    /// El manager de snapshots terminó de abrirse (puede no estar disponible).
    SnapshotsReady(Snapshots),
    /// El usuario seleccionó la fila `index` del sidebar.
    SelectIndex(i32),
    /// Llegó el historial de la sesión seleccionada con sus snapshots y las
    /// duraciones de razonamiento por mensaje (`message_id → ms`).
    SessionLoaded {
        messages: Vec<(i64, Message)>,
        snapshots: HashMap<i64, String>,
        reasoning_ms: HashMap<i64, u64>,
    },
    /// Crear una sesión nueva.
    NewSession,
    /// Colapsar/des-colapsar el sidebar de sesiones.
    ToggleSessionsSidebar,
    /// Se creó una sesión nueva.
    SessionCreated(SessionMeta),
    /// El usuario pidió eliminar una sesión desde el menú contextual del sidebar.
    DeleteSessionRequest(i64),
    /// El usuario confirmó la eliminación en el diálogo.
    DeleteSessionConfirmed(i64),
    /// La sesión se eliminó del store: limpiar UI y, si era la activa, abrir
    /// otra si la hay.
    SessionDeleted(i64),
    /// Se renombró una sesión (al enviar su primer mensaje).
    Renamed { id: i64, title: String },
    /// El usuario cambió el agente activo desde el selector.
    AgentChanged(AgentKind),
    /// Alternar entre Build y Plan (atajo Shift+Tab desde el campo de entrada).
    ToggleAgent,
    /// El usuario pulsó el botón de modelo de la top toolbar.
    OpenModelPicker,
    /// El usuario abrió la paleta de comandos (Ctrl+P).
    OpenCommandPalette,
    /// La paleta de comandos se cerró.
    CommandPaletteClosed,
    /// El usuario seleccionó un modelo en el modal.
    ModelChanged(String),
    /// El usuario pulsó el botón de configuración (icono).
    OpenSettings,
    /// El usuario pulsó "Connect" en el modal de configuración.
    OpenConnectProvider,
    /// El usuario eligió un proveedor para conectar.
    ConnectProvider(String),
    /// El flujo OAuth de OpenAI ya tiene `AuthInfo`; persistir y notificar.
    OauthOpenAiCompleted(AuthInfo),
    /// El flujo OAuth de OpenAI falló (timeout, CSRF, denegación, etc.).
    OauthOpenAiFailed(String),
    /// El usuario desconectó un proveedor desde Configuración.
    DisconnectProvider(String),
    /// El usuario alternó la visibilidad global del *chain of thought* con el
    /// botón ojo (solo presente si el modelo activo es razonador).
    ToggleReasoning(bool),
    /// El usuario envía un prompt.
    Send(String),
    /// Llega un fragmento de texto del asistente.
    Delta(String),
    /// Llega un fragmento del *chain of thought* del paso.
    ReasoningDelta(String),
    /// El bloque de razonamiento del paso terminó (con su duración medida).
    ReasoningFinished { ms: u64 },
    /// El motor capturó un snapshot del worktree para el paso actual.
    StepSnapshot { hash: String, message_index: usize },
    /// El snapshot del último turno ya está persistido bajo `message_id`;
    /// listo para colgar el botón "Revertir" de la última tarjeta del paso.
    SnapshotPersisted { message_id: i64, hash: String },
    /// Llegó el diff del worktree respecto a la base de cambios de la sesión.
    ChangesPatchLoaded(String),
    /// Navegar al archivo anterior en el panel de cambios.
    ChangesNavPrev,
    /// Navegar al archivo siguiente en el panel de cambios.
    ChangesNavNext,
    /// Cambió el ancho disponible de la ventana.
    WindowWidthChanged(i32),
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
    /// El usuario pidió revertir el snapshot asociado al mensaje `message_id`.
    Revert(i64),
    /// Llegó la lista de archivos que cambiarán al revertir; pedir confirmación.
    RevertPreview { hash: String, files: Vec<PathBuf> },
    /// La restauración terminó: recargar la sesión activa.
    RevertDone,
    /// El turno falló.
    Failed(String),
    /// Muestra un toast flotante (centro-arriba) durante ~1.6 s.
    Toast(String),
    /// Cerrar la aplicación.
    Quit,
}

#[derive(Clone, Copy)]
enum CommandPaletteAction {
    SelectModel,
    ConnectProvider,
    NewSession,
    ToggleThinking,
    Quit,
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

            gtk::Overlay {
                // Toast flotante centro-arriba: usado, por ejemplo, al copiar
                // un bloque de código. Aparece con `Msg::Toast` y se oculta
                // tras un timeout corto.
                #[name = "toast"]
                add_overlay = &gtk::Revealer {
                    set_halign: gtk::Align::Center,
                    set_valign: gtk::Align::Start,
                    set_margin_top: 12,
                    set_can_target: false,
                    set_transition_type: gtk::RevealerTransitionType::SlideDown,
                    set_transition_duration: 180,
                    set_reveal_child: false,

                    gtk::Box {
                        add_css_class: "osd",
                        add_css_class: "toolbar",
                        set_margin_all: 4,

                        #[name = "toast_label"]
                        gtk::Label {
                            set_label: "",
                            set_margin_start: 12,
                            set_margin_end: 12,
                            set_margin_top: 4,
                            set_margin_bottom: 4,
                        },
                    },
                },

                #[wrap(Some)]
                set_child = &gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,

                // ── Sidebar de sesiones ──────────────────────────────────────
                #[name = "sessions_sidebar"]
                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_hexpand: false,

                    #[name = "sessions_sidebar_collapsed"]
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_visible: false,

                        gtk::Button {
                            set_icon_name: "sidebar-show-symbolic",
                            add_css_class: "flat",
                            set_margin_top: 6,
                            set_margin_start: 6,
                            set_margin_end: 6,
                            set_tooltip_text: Some("Mostrar sesiones"),
                            connect_clicked[sender] => move |_| {
                                sender.input(Msg::ToggleSessionsSidebar);
                            },
                        },
                    },

                    #[name = "sessions_sidebar_expanded"]
                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,

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
                            pack_end = &gtk::Button {
                                set_icon_name: "sidebar-show-right-symbolic",
                                add_css_class: "flat",
                                set_tooltip_text: Some("Ocultar sesiones"),
                                connect_clicked[sender] => move |_| {
                                    sender.input(Msg::ToggleSessionsSidebar);
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
                },

                gtk::Separator {
                    set_orientation: gtk::Orientation::Vertical,
                },

                // ── Área de conversación ─────────────────────────────────────
                gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                    set_hexpand: true,

                    adw::HeaderBar,

                    // Toolbar de la sección de chat (no es chrome de ventana):
                    // queda fija sobre el scroll del chat, con el selector de
                    // modelo alineado a la derecha. Spacer expansivo antes del
                    // botón para empujarlo al borde derecho sin que el hitbox
                    // del botón ocupe toda la fila.
                    gtk::Box {
                        set_orientation: gtk::Orientation::Horizontal,
                        set_margin_top: 2,
                        set_margin_bottom: 2,
                        set_margin_start: 8,
                        set_margin_end: 8,

                        gtk::Box {
                            set_hexpand: true,
                        },

                        #[name = "model_button"]
                        gtk::Button {
                            set_label: "Modelo",
                            add_css_class: "flat",
                            set_tooltip_text: Some("Cambiar modelo"),
                            connect_clicked[sender] => move |_| {
                                tracing::debug!("model_button: click");
                                sender.input(Msg::OpenModelPicker);
                            },
                        },

                        // Toggle global de visibilidad del *chain of thought*.
                        // Solo se muestra cuando el modelo activo es razonador
                        // (visibilidad gestionada en `update_controls`).
                        #[name = "reasoning_button"]
                        gtk::ToggleButton {
                            set_icon_name: "view-conceal-symbolic",
                            add_css_class: "flat",
                            set_visible: false,
                            set_tooltip_text: Some("Mostrar pensamientos"),
                            connect_toggled[sender] => move |b| {
                                sender.input(Msg::ToggleReasoning(b.is_active()));
                            },
                        },

                        #[name = "settings_button"]
                        gtk::Button {
                            set_icon_name: "preferences-system-symbolic",
                            add_css_class: "flat",
                            set_tooltip_text: Some("Configuración"),
                            connect_clicked[sender] => move |_| {
                                sender.input(Msg::OpenSettings);
                            },
                        },
                    },

                    gtk::Separator {
                        set_orientation: gtk::Orientation::Horizontal,
                    },

                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_vexpand: true,

                    gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_hexpand: true,

                        #[name = "scroller"]
                        gtk::ScrolledWindow {
                            set_vexpand: true,
                            set_hscrollbar_policy: gtk::PolicyType::Automatic,

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

                            gtk::Box {
                                set_orientation: gtk::Orientation::Horizontal,
                                add_css_class: "linked",

                                #[name = "agent_build"]
                                gtk::ToggleButton {
                                    set_label: "Build",
                                    set_active: true,
                                    add_css_class: "agent-build",
                                    set_tooltip_text: Some(
                                        "Acceso completo: lee, escribe, ejecuta (Shift+Tab para alternar)",
                                    ),
                                    connect_toggled[sender] => move |b| {
                                        if b.is_active() {
                                            sender.input(Msg::AgentChanged(AgentKind::Build));
                                        }
                                    },
                                },

                                #[name = "agent_plan"]
                                gtk::ToggleButton {
                                    set_label: "Plan",
                                    add_css_class: "agent-plan",
                                    set_tooltip_text: Some(
                                        "Solo lectura: lee y propone, no modifica (Shift+Tab para alternar)",
                                    ),
                                    connect_toggled[sender] => move |b| {
                                        if b.is_active() {
                                            sender.input(Msg::AgentChanged(AgentKind::Plan));
                                        }
                                    },
                                },
                            },

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

                    #[name = "changes_separator"]
                    gtk::Separator {
                        set_orientation: gtk::Orientation::Vertical,
                        set_visible: false,
                    },

                    // ── Panel de cambios ─────────────────────────────────────
                    #[name = "changes_panel"]
                    gtk::Revealer {
                        set_transition_type: gtk::RevealerTransitionType::SlideLeft,
                        set_transition_duration: 180,
                        set_reveal_child: false,
                        set_visible: false,

                        #[wrap(Some)]
                        set_child = &gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_size_request: (CHANGES_PANEL_WIDTH, -1),

                            gtk::Box {
                                set_orientation: gtk::Orientation::Horizontal,
                                set_margin_top: 8,
                                set_margin_bottom: 8,
                                set_margin_start: 12,
                                set_margin_end: 12,

                                gtk::Label {
                                    set_label: "Cambios",
                                    add_css_class: "heading",
                                },
                            },

                            gtk::Separator {
                                set_orientation: gtk::Orientation::Horizontal,
                            },

                            gtk::Overlay {
                                gtk::ScrolledWindow {
                                    set_vexpand: true,
                                    set_hscrollbar_policy: gtk::PolicyType::Automatic,

                                    #[name = "changes_list"]
                                    #[wrap(Some)]
                                    set_child = &gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_hexpand: true,
                                        set_spacing: 8,
                                        set_margin_all: 12,
                                        set_margin_bottom: 20,
                                    },
                                },

                                #[name = "changes_nav_bar"]
                                add_overlay = &gtk::Box {
                                    set_orientation: gtk::Orientation::Horizontal,
                                    set_halign: gtk::Align::Center,
                                    set_valign: gtk::Align::Start,
                                    set_margin_top: 6,
                                    set_spacing: 4,
                                    add_css_class: "changes-nav-bar",
                                    set_visible: false,

                                    #[name = "changes_nav_prev"]
                                    gtk::Button {
                                        set_icon_name: "go-previous-symbolic",
                                        add_css_class: "flat",
                                        add_css_class: "circular",
                                        connect_clicked[sender] => move |_| {
                                            sender.input(Msg::ChangesNavPrev);
                                        },
                                    },

                                    #[name = "changes_nav_label"]
                                    gtk::Label {
                                        set_ellipsize: relm4::gtk::pango::EllipsizeMode::Middle,
                                        add_css_class: "caption",
                                    },

                                    #[name = "changes_nav_next"]
                                    gtk::Button {
                                        set_icon_name: "go-next-symbolic",
                                        add_css_class: "flat",
                                        add_css_class: "circular",
                                        connect_clicked[sender] => move |_| {
                                            sender.input(Msg::ChangesNavNext);
                                        },
                                    },
                                },
                            },
                        },
                    },
                },
                },
            },
        }
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

        // Catálogo: snapshot embebido al instante (la app nunca arranca sin
        // catálogo). El refresco contra models.dev corre en background.
        let catalog = Arc::new(Catalog::load().openai_compatible());
        let auth = AuthStore::load_default();
        let engine = Engine::new(catalog.clone(), auth);
        let current_model = catalog
            .default_model()
            .map(|r| r.to_string())
            .unwrap_or_default();
        let model = App {
            engine,
            store: store.clone(),
            snapshots: None,
            workdir: workdir.clone(),
            project_id: None,
            sessions: Vec::new(),
            current_session: None,
            current_agent: AgentKind::default(),
            current_model,
            session: Session::new(),
            streaming_bubble: None,
            tool_output: None,
            tool_card: None,
            revertible_card: None,
            pending_snapshot: None,
            message_snapshots: HashMap::new(),
            session_base_snapshot: None,
            changes_patch: String::new(),
            changes_nav_index: 0,
            changes_nav_files: Vec::new(),
            wide_layout: false,
            partial: String::new(),
            show_reasoning: false,
            reasoning_card: None,
            reasoning_partial: String::new(),
            reasoning_cards: Vec::new(),
            reasoning_ms_queue: Vec::new(),
            sessions_sidebar_collapsed: false,
            command_palette_dialog: None,
            toast_timeout: None,
            busy: false,
        };

        let widgets = view_output!();

        // Enlaza los dos toggles como grupo radio (uno activo a la vez).
        widgets.agent_plan.set_group(Some(&widgets.agent_build));

        {
            let sender = sender.clone();
            let last_width = Rc::new(Cell::new(0));
            root.add_tick_callback(move |window, _| {
                let width = window.allocated_width();
                if last_width.get() != width {
                    last_width.set(width);
                    sender.input(Msg::WindowWidthChanged(width));
                }
                gtk::glib::ControlFlow::Continue
            });
        }

        // Atajo Shift+Tab en el campo de entrada para alternar Build/Plan. Se
        // registra como `ShortcutController` local al Entry: solo dispara con
        // el campo enfocado y devuelve `Stop` para no propagarse a la
        // navegación por foco de GTK.
        {
            let controller = gtk::ShortcutController::new();
            let trigger =
                gtk::ShortcutTrigger::parse_string("<Shift>Tab").expect("trigger Shift+Tab válido");
            let sender_action = sender.clone();
            let action = gtk::CallbackAction::new(move |_, _| {
                sender_action.input(Msg::ToggleAgent);
                gtk::glib::Propagation::Stop
            });
            controller.add_shortcut(gtk::Shortcut::new(Some(trigger), Some(action)));
            widgets.entry.add_controller(controller);
        }

        // Paleta de comandos al estilo OpenCode. Se registra global a la ventana
        // para que funcione aunque el foco esté dentro del campo de entrada.
        {
            let controller = gtk::ShortcutController::new();
            controller.set_scope(gtk::ShortcutScope::Global);
            let trigger =
                gtk::ShortcutTrigger::parse_string("<Control>p").expect("trigger Ctrl+P válido");
            let sender_action = sender.clone();
            let action = gtk::CallbackAction::new(move |_, _| {
                sender_action.input(Msg::OpenCommandPalette);
                gtk::glib::Propagation::Stop
            });
            controller.add_shortcut(gtk::Shortcut::new(Some(trigger), Some(action)));
            root.add_controller(controller);
        }

        if !any_provider_key_present(&catalog) {
            append_bubble(
                &widgets.chat_list,
                Role::System,
                "No hay ninguna clave de proveedor en el entorno. Puedes navegar \
                 el catálogo y elegir modelo; al enviar un mensaje, la app te \
                 indicará la variable que falta exportar.",
            );
        }

        // Refresco del catálogo en background: descarga models.dev a la cache
        // si está stale. La app sigue usando el catálogo ya cargado; el JSON
        // fresco se aplica en el próximo arranque (igual que OpenCode).
        spawn_catalog_refresh();

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
        root: &Self::Root,
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
                    rebuild_session_list(&widgets.session_list, &self.sessions, 0, &sender);
                }

                // Inicializa el manager de snapshots para este proyecto. El
                // shadow vive en `$XDG_DATA_HOME/xiě-code/snapshots/<id>`.
                let workdir = self.workdir.clone();
                let git_dir = snapshots_dir(project_id);
                let sender = sender.clone();
                relm4::spawn(async move {
                    match Snapshots::open(workdir, git_dir).await {
                        Ok(snap) => sender.input(Msg::SnapshotsReady(snap)),
                        Err(err) => tracing::warn!(%err, "no se pudo abrir snapshots"),
                    }
                });
            }

            Msg::SnapshotsReady(snap) => {
                if !snap.available() {
                    tracing::warn!("snapshots deshabilitados: el botón «Revertir» no aparecerá");
                }
                self.snapshots = Some(snap);
                request_changes_patch(self, &sender);
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
                self.current_agent = meta.agent;
                self.current_model = meta.model.clone().unwrap_or_else(|| {
                    self.engine
                        .catalog()
                        .default_model()
                        .map(|r| r.to_string())
                        .unwrap_or_default()
                });
                self.streaming_bubble = None;
                self.tool_output = None;
                self.tool_card = None;
                self.revertible_card = None;
                self.pending_snapshot = None;
                self.message_snapshots.clear();
                self.session_base_snapshot = None;
                self.changes_patch.clear();
                self.partial.clear();
                clear_chat(&widgets.chat_list);

                if let Some(store) = self.store.clone() {
                    let sender = sender.clone();
                    relm4::spawn(async move {
                        let messages = match store.load_messages(id).await {
                            Ok(m) => m,
                            Err(err) => {
                                sender.input(Msg::Failed(err.to_string()));
                                return;
                            }
                        };
                        let snapshots = store.load_snapshots(id).await.unwrap_or_default();
                        let reasoning_ms =
                            store.load_reasoning_durations(id).await.unwrap_or_default();
                        sender.input(Msg::SessionLoaded {
                            messages,
                            snapshots,
                            reasoning_ms,
                        });
                    });
                }
            }

            Msg::SessionLoaded {
                messages,
                snapshots,
                reasoning_ms,
            } => {
                clear_chat(&widgets.chat_list);
                self.reasoning_cards.clear();
                self.reasoning_card = None;
                self.reasoning_partial.clear();
                self.reasoning_ms_queue.clear();
                self.session_base_snapshot = messages
                    .iter()
                    .find_map(|(id, _)| snapshots.get(id).cloned());
                if self.session_base_snapshot.is_none() {
                    self.changes_patch.clear();
                    clear_chat(&widgets.changes_list);
                }
                let cards = render_history(
                    &widgets.chat_list,
                    &messages,
                    &snapshots,
                    &reasoning_ms,
                    self.show_reasoning,
                    &sender,
                );
                self.reasoning_cards = cards;
                self.message_snapshots = snapshots;
                self.session =
                    Session::from_messages(messages.into_iter().map(|(_, m)| m).collect());
                request_changes_patch(self, &sender);
            }

            Msg::NewSession => {
                let (Some(store), Some(project_id)) = (self.store.clone(), self.project_id) else {
                    return;
                };
                let agent = self.current_agent;
                let model = (!self.current_model.is_empty()).then(|| self.current_model.clone());
                let sender = sender.clone();
                relm4::spawn(async move {
                    match store
                        .create_session(project_id, "Nueva sesión", agent, model.as_deref())
                        .await
                    {
                        Ok(meta) => sender.input(Msg::SessionCreated(meta)),
                        Err(err) => sender.input(Msg::Failed(err.to_string())),
                    }
                });
            }

            Msg::ToggleSessionsSidebar => {
                self.sessions_sidebar_collapsed = !self.sessions_sidebar_collapsed;
            }

            Msg::SessionCreated(meta) => {
                let id = meta.id;
                self.current_agent = meta.agent;
                if let Some(model) = meta.model.clone() {
                    self.current_model = model;
                }
                self.sessions.insert(0, meta);
                self.current_session = Some(id);
                self.session = Session::new();
                self.streaming_bubble = None;
                self.tool_output = None;
                self.partial.clear();
                self.session_base_snapshot = None;
                self.changes_patch.clear();
                clear_chat(&widgets.changes_list);
                clear_chat(&widgets.chat_list);
                rebuild_session_list(&widgets.session_list, &self.sessions, 0, &sender);
            }

            Msg::DeleteSessionRequest(session_id) => {
                if self.busy {
                    return;
                }
                let Some(meta) = self.sessions.iter().find(|m| m.id == session_id) else {
                    return;
                };
                let title = meta.title.clone();
                let sender_dialog = sender.clone();
                show_delete_session_dialog(root, &title, move |confirmed| {
                    if confirmed {
                        sender_dialog.input(Msg::DeleteSessionConfirmed(session_id));
                    }
                });
            }

            Msg::DeleteSessionConfirmed(session_id) => {
                let Some(store) = self.store.clone() else {
                    return;
                };
                let sender = sender.clone();
                relm4::spawn(async move {
                    match store.delete_session(session_id).await {
                        Ok(()) => sender.input(Msg::SessionDeleted(session_id)),
                        Err(err) => sender.input(Msg::Failed(err.to_string())),
                    }
                });
            }

            Msg::SessionDeleted(session_id) => {
                self.sessions.retain(|m| m.id != session_id);
                let was_active = self.current_session == Some(session_id);
                if was_active {
                    self.current_session = None;
                    self.session = Session::new();
                    self.streaming_bubble = None;
                    self.tool_output = None;
                    self.tool_card = None;
                    self.revertible_card = None;
                    self.pending_snapshot = None;
                    self.message_snapshots.clear();
                    self.session_base_snapshot = None;
                    self.changes_patch.clear();
                    clear_chat(&widgets.changes_list);
                    self.partial.clear();
                    clear_chat(&widgets.chat_list);
                }
                let selected = if was_active && !self.sessions.is_empty() {
                    0
                } else {
                    self.current_session
                        .and_then(|cur| self.sessions.iter().position(|m| m.id == cur))
                        .map(|i| i as i32)
                        .unwrap_or(-1)
                };
                rebuild_session_list(&widgets.session_list, &self.sessions, selected, &sender);
                if was_active && !self.sessions.is_empty() {
                    sender.input(Msg::SelectIndex(0));
                }
            }

            Msg::ToggleAgent => {
                let kind = match self.current_agent {
                    AgentKind::Build => AgentKind::Plan,
                    AgentKind::Plan => AgentKind::Build,
                };
                sender.input(Msg::AgentChanged(kind));
            }

            Msg::AgentChanged(kind) => {
                if self.current_agent == kind {
                    return; // Eco de la sincronización programática del toggle.
                }
                self.current_agent = kind;
                if let (Some(store), Some(id)) = (self.store.clone(), self.current_session) {
                    relm4::spawn(async move {
                        if let Err(err) = store.set_session_agent(id, kind).await {
                            tracing::warn!(%err, "no se pudo persistir el agente");
                        }
                    });
                }
                if let Some(meta) = self
                    .sessions
                    .iter_mut()
                    .find(|m| Some(m.id) == self.current_session)
                {
                    meta.agent = kind;
                }
            }

            Msg::OpenModelPicker => {
                let connected: HashSet<String> = self.engine.auth().all().into_keys().collect();
                let options = catalog_model_options(self.engine.catalog(), &connected);
                tracing::debug!(current = %self.current_model, count = options.len(), "modelo: abriendo selector");
                let current = self.current_model.clone();
                let sender_dialog = sender.clone();
                show_model_picker_dialog(root, &current, &options, move |chosen| {
                    sender_dialog.input(Msg::ModelChanged(chosen));
                });
            }

            Msg::OpenCommandPalette => {
                if let Some(dialog) = self.command_palette_dialog.take() {
                    dialog.close();
                    return;
                }
                let sender_dialog = sender.clone();
                let sender_closed = sender.clone();
                let show_reasoning = self.show_reasoning;
                let dialog = show_command_palette_dialog(
                    root,
                    show_reasoning,
                    move |action| match action {
                        CommandPaletteAction::SelectModel => {
                            sender_dialog.input(Msg::OpenModelPicker)
                        }
                        CommandPaletteAction::ConnectProvider => {
                            sender_dialog.input(Msg::OpenConnectProvider)
                        }
                        CommandPaletteAction::NewSession => sender_dialog.input(Msg::NewSession),
                        CommandPaletteAction::ToggleThinking => {
                            sender_dialog.input(Msg::ToggleReasoning(!show_reasoning))
                        }
                        CommandPaletteAction::Quit => sender_dialog.input(Msg::Quit),
                    },
                    move || sender_closed.input(Msg::CommandPaletteClosed),
                );
                self.command_palette_dialog = Some(dialog);
            }

            Msg::CommandPaletteClosed => {
                self.command_palette_dialog = None;
                widgets.entry.grab_focus();
            }

            Msg::ModelChanged(model) => {
                if self.current_model == model {
                    return;
                }
                self.current_model = model.clone();
                if let (Some(store), Some(id)) = (self.store.clone(), self.current_session) {
                    let model_for_store = model.clone();
                    relm4::spawn(async move {
                        if let Err(err) = store.set_session_model(id, &model_for_store).await {
                            tracing::warn!(%err, "no se pudo persistir el modelo");
                        }
                    });
                }
                if let Some(meta) = self
                    .sessions
                    .iter_mut()
                    .find(|m| Some(m.id) == self.current_session)
                {
                    meta.model = Some(model);
                }
            }

            Msg::OpenSettings => {
                let sender_dialog = sender.clone();
                let connected: Vec<(String, String)> = self
                    .engine
                    .auth()
                    .all()
                    .into_iter()
                    .map(|(id, info)| {
                        let kind = match info {
                            AuthInfo::Oauth { .. } => "OAuth (ChatGPT)",
                            AuthInfo::Api { .. } => "API key",
                        };
                        (id, kind.to_string())
                    })
                    .collect();
                show_settings_dialog(root, &connected, move |action| match action {
                    SettingsAction::Connect => sender_dialog.input(Msg::OpenConnectProvider),
                    SettingsAction::Disconnect(id) => {
                        sender_dialog.input(Msg::DisconnectProvider(id))
                    }
                });
            }

            Msg::OpenConnectProvider => {
                let sender_dialog = sender.clone();
                let connected: HashSet<String> = self.engine.auth().all().into_keys().collect();
                let providers: Vec<(String, String)> = self
                    .engine
                    .catalog()
                    .providers()
                    .map(|p| (p.id.clone(), p.name.clone()))
                    .collect();
                show_connect_provider_dialog(root, &providers, &connected, move |provider_id| {
                    sender_dialog.input(Msg::ConnectProvider(provider_id));
                });
            }

            Msg::ConnectProvider(provider_id) => match provider_id.as_str() {
                "openai" => {
                    let sender = sender.clone();
                    show_oauth_waiting_dialog(root);
                    relm4::spawn(async move {
                        match oauth::OpenAiOauth::start_browser_flow().await {
                            Ok(flow) => {
                                if let Err(e) = open_in_browser(&flow.authorize_url) {
                                    sender.input(Msg::OauthOpenAiFailed(format!(
                                        "no se pudo abrir el navegador: {e}"
                                    )));
                                    return;
                                }
                                match flow.await_completion().await {
                                    Ok(info) => sender.input(Msg::OauthOpenAiCompleted(info)),
                                    Err(e) => sender.input(Msg::OauthOpenAiFailed(e.to_string())),
                                }
                            }
                            Err(e) => sender.input(Msg::OauthOpenAiFailed(e.to_string())),
                        }
                    });
                }
                other => {
                    // Para proveedores sin flujo OAuth, ofrecer entrada manual
                    // de API key. Esta es la rama por defecto y la única hoy
                    // disponible para todo lo que no sea OpenAI.
                    let provider_id = other.to_string();
                    let provider_id_for_closure = provider_id.clone();
                    let auth = self.engine.auth().clone();
                    let sender_dialog = sender.clone();
                    show_api_key_dialog(root, &provider_id, move |key| {
                        let auth = auth.clone();
                        let provider_id = provider_id_for_closure.clone();
                        let sender = sender_dialog.clone();
                        relm4::spawn(async move {
                            if let Err(e) = auth
                                .set(
                                    provider_id.clone(),
                                    AuthInfo::Api {
                                        key,
                                        metadata: None,
                                    },
                                )
                                .await
                            {
                                sender.input(Msg::Failed(format!("auth: {e}")));
                                return;
                            }
                            sender.input(Msg::Toast(format!("Cuenta «{provider_id}» conectada")));
                        });
                    });
                }
            },

            Msg::OauthOpenAiCompleted(info) => {
                let auth = self.engine.auth().clone();
                let sender = sender.clone();
                relm4::spawn(async move {
                    if let Err(e) = auth.set("openai", info).await {
                        sender.input(Msg::Failed(format!("auth: {e}")));
                        return;
                    }
                    sender.input(Msg::Toast("Cuenta OpenAI conectada".to_string()));
                });
            }

            Msg::OauthOpenAiFailed(err) => {
                append_bubble(
                    &widgets.chat_list,
                    Role::System,
                    &format!("La conexión con OpenAI falló: {err}"),
                );
            }

            Msg::DisconnectProvider(provider_id) => {
                let auth = self.engine.auth().clone();
                let sender = sender.clone();
                let id = provider_id.clone();
                relm4::spawn(async move {
                    if let Err(e) = auth.remove(&id).await {
                        sender.input(Msg::Failed(format!("auth: {e}")));
                        return;
                    }
                    sender.input(Msg::Toast(format!("«{id}» desconectado")));
                });
            }

            Msg::Renamed { id, title } => {
                if let Some(meta) = self.sessions.iter_mut().find(|m| m.id == id) {
                    meta.title = title;
                }
                let selected = self
                    .current_session
                    .and_then(|cur| self.sessions.iter().position(|m| m.id == cur))
                    .unwrap_or(0);
                rebuild_session_list(
                    &widgets.session_list,
                    &self.sessions,
                    selected as i32,
                    &sender,
                );
            }

            Msg::Send(text) => {
                let engine = self.engine.clone();
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

                self.streaming_bubble = None;
                self.tool_output = None;
                self.tool_card = None;
                self.pending_snapshot = None;
                self.partial.clear();
                self.busy = true;

                // Lanzar el bucle de agente: consume el stream de eventos y los
                // reenvía como mensajes Relm4 (patrón Tokio↔GLib).
                let ctx = ToolContext::new(self.workdir.clone());
                let resolver: Arc<dyn PermissionResolver> = Arc::new(UiPermissions {
                    sender: sender.clone(),
                });
                let history = self.session.history();
                let snapshots = self.snapshots.clone();
                let agent = self.current_agent;
                let model = self.current_model.clone();
                let sender = sender.clone();
                relm4::spawn(async move {
                    let mut stream =
                        engine.run_turn(agent, model, history, ctx, snapshots, resolver);
                    while let Some(event) = stream.next().await {
                        match event {
                            Ok(AgentEvent::Delta(d)) => sender.input(Msg::Delta(d)),
                            Ok(AgentEvent::ReasoningDelta(d)) => {
                                sender.input(Msg::ReasoningDelta(d))
                            }
                            Ok(AgentEvent::ReasoningFinished { duration_ms }) => {
                                sender.input(Msg::ReasoningFinished { ms: duration_ms })
                            }
                            Ok(AgentEvent::StepSnapshot {
                                hash,
                                message_index,
                            }) => sender.input(Msg::StepSnapshot {
                                hash,
                                message_index,
                            }),
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
                let bubble = self
                    .streaming_bubble
                    .get_or_insert_with(|| append_assistant_streaming(&widgets.chat_list));
                // Texto plano mientras llega: el markdown puede estar a medias.
                bubble.label.set_text(&self.partial);
            }

            Msg::ReasoningDelta(delta) => {
                self.reasoning_partial.push_str(&delta);
                let show = self.show_reasoning;
                let card = self.reasoning_card.get_or_insert_with(|| {
                    let c = append_reasoning_card(&widgets.chat_list, show, "Pensando…", true);
                    self.reasoning_cards.push(c.clone());
                    c
                });
                card.body.set_text(&self.reasoning_partial);
            }

            Msg::ReasoningFinished { ms } => {
                self.reasoning_ms_queue.push(ms);
                if let Some(card) = self.reasoning_card.take() {
                    finalize_reasoning_card(&card, ms);
                }
                self.reasoning_partial.clear();
            }

            Msg::ToggleReasoning(active) => {
                self.show_reasoning = active;
                for card in &self.reasoning_cards {
                    card.apply_visibility(active);
                }
            }

            Msg::ToolStarted { name, arguments } => {
                // Cierra el segmento de texto previo (si lo hay) renderizando markdown.
                if let Some(bubble) = self.streaming_bubble.take() {
                    if !self.partial.is_empty() {
                        fill_with_blocks(&bubble.body, &self.partial, &sender);
                    }
                }
                self.partial.clear();
                let (card, output) =
                    append_tool_card(&widgets.chat_list, &name, &arguments, "Ejecutando…");
                self.tool_output = Some(output);
                self.tool_card = Some(card);
            }

            Msg::StepSnapshot {
                hash,
                message_index,
            } => {
                self.pending_snapshot = Some((hash, message_index));
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
                if let Some(bubble) = self.streaming_bubble.take() {
                    fill_with_blocks(&bubble.body, &self.partial, &sender);
                }
                self.partial.clear();
                self.tool_output = None;
                // Defensivo: si el step cerró sin `ReasoningFinished`, deja la
                // card abierta en streaming. La sellamos con duración 0.
                if let Some(card) = self.reasoning_card.take() {
                    finalize_reasoning_card(&card, 0);
                }
                self.reasoning_partial.clear();
                let reasoning_ms = std::mem::take(&mut self.reasoning_ms_queue);
                let pending = self.pending_snapshot.take();
                // Mover la card a `revertible_card` para que sobreviva a un
                // posible `Send` que llegue antes de `SnapshotPersisted`.
                if pending.is_some() {
                    self.revertible_card = self.tool_card.take();
                }
                self.busy = false;
                self.session.extend(messages.clone());

                if let (Some(store), Some(session_id)) = (self.store.clone(), self.current_session)
                {
                    let sender = sender.clone();
                    relm4::spawn(async move {
                        let mut ids: Vec<i64> = Vec::with_capacity(messages.len());
                        for msg in &messages {
                            match store.append_message(session_id, msg).await {
                                Ok(id) => ids.push(id),
                                Err(err) => {
                                    tracing::error!(%err, "no se pudo guardar un mensaje del turno");
                                    return;
                                }
                            }
                        }
                        // Asigna cada duración medida en este turno al siguiente
                        // assistant message que llevaba reasoning. El motor las
                        // emite en orden de paso, así que el zip respeta el orden.
                        let mut durations = reasoning_ms.into_iter();
                        for (msg, &id) in messages.iter().zip(ids.iter()) {
                            if msg.role == Role::Assistant
                                && msg.reasoning.as_deref().is_some_and(|s| !s.is_empty())
                            {
                                if let Some(ms) = durations.next() {
                                    if let Err(err) = store.set_message_reasoning_ms(id, ms).await {
                                        tracing::warn!(%err, "no se pudo persistir la duración del reasoning");
                                    }
                                }
                            }
                        }
                        if let Some((hash, idx)) = pending {
                            if let Some(&message_id) = ids.get(idx) {
                                if let Err(err) =
                                    store.set_message_snapshot(message_id, &hash).await
                                {
                                    tracing::warn!(%err, "no se pudo persistir el snapshot");
                                    return;
                                }
                                sender.input(Msg::SnapshotPersisted { message_id, hash });
                            }
                        }
                    });
                }
            }

            Msg::SnapshotPersisted { message_id, hash } => {
                if self.session_base_snapshot.is_none() {
                    self.session_base_snapshot = Some(hash.clone());
                }
                self.message_snapshots.insert(message_id, hash);
                let card = self
                    .revertible_card
                    .take()
                    .or_else(|| self.tool_card.take());
                match card {
                    Some(card) if card.parent().is_some() => {
                        attach_revert_button(&card, message_id, &sender);
                    }
                    Some(_) => {
                        // La tarjeta fue desligada (sesión recargada o
                        // cambiada): pinta el botón como una fila propia al
                        // final del chat para no perder la acción.
                        attach_revert_button(&widgets.chat_list, message_id, &sender);
                    }
                    None => {
                        // No había card del paso (caso raro); cuelga el botón
                        // al final del chat para que el usuario lo encuentre.
                        attach_revert_button(&widgets.chat_list, message_id, &sender);
                    }
                }
                request_changes_patch(self, &sender);
            }

            Msg::ChangesPatchLoaded(patch) => {
                self.changes_patch = patch;
                let files = parse_patch_files(&self.changes_patch);
                self.changes_nav_files = files.iter().map(|f| f.path.clone()).collect();
                self.changes_nav_index = 0;
                render_changes_patch(&widgets.changes_list, &files);
                let has_nav = self.changes_nav_files.len() > 1;
                widgets.changes_nav_bar.set_visible(has_nav);
                if has_nav {
                    update_changes_nav_label(widgets, self);
                }
            }

            Msg::ChangesNavPrev => {
                if self.changes_nav_files.is_empty() || self.changes_nav_index == 0 {
                    return;
                }
                self.changes_nav_index -= 1;
                update_changes_nav_label(widgets, self);
                scroll_to_changes_file(widgets, self.changes_nav_index);
            }

            Msg::ChangesNavNext => {
                if self.changes_nav_files.is_empty()
                    || self.changes_nav_index + 1 >= self.changes_nav_files.len()
                {
                    return;
                }
                self.changes_nav_index += 1;
                update_changes_nav_label(widgets, self);
                scroll_to_changes_file(widgets, self.changes_nav_index);
            }

            Msg::WindowWidthChanged(width) => {
                self.wide_layout = width >= CHANGES_PANEL_BREAKPOINT;
            }

            Msg::Revert(message_id) => {
                if self.busy {
                    return;
                }
                let (Some(snap), Some(hash)) = (
                    self.snapshots.clone(),
                    self.message_snapshots.get(&message_id).cloned(),
                ) else {
                    return;
                };
                let sender = sender.clone();
                relm4::spawn(async move {
                    match snap.patch_files(&hash).await {
                        Ok(files) => sender.input(Msg::RevertPreview { hash, files }),
                        Err(err) => sender.input(Msg::Failed(err.to_string())),
                    }
                });
            }

            Msg::RevertPreview { hash, files } => {
                let Some(snap) = self.snapshots.clone() else {
                    return;
                };
                let files_for_restore = files.clone();
                let sender_dialog = sender.clone();
                show_revert_dialog(root, &files, move |confirmed| {
                    if !confirmed {
                        return;
                    }
                    let snap = snap.clone();
                    let hash = hash.clone();
                    let files = files_for_restore.clone();
                    let sender = sender_dialog.clone();
                    relm4::spawn(async move {
                        match snap.restore(&hash, &files).await {
                            Ok(()) => sender.input(Msg::RevertDone),
                            Err(err) => sender.input(Msg::Failed(err.to_string())),
                        }
                    });
                });
            }

            Msg::RevertDone => {
                // Recarga la sesión activa: el chat ya refleja la conversación
                // como estaba; el cambio visible está en el FS del worktree.
                if let Some(id) = self.current_session {
                    if let Some(idx) = self.sessions.iter().position(|m| m.id == id) {
                        // Forzamos recarga.
                        self.current_session = None;
                        sender.input(Msg::SelectIndex(idx as i32));
                    }
                }
            }

            Msg::Failed(err) => {
                if let Some(bubble) = self.streaming_bubble.take() {
                    bubble.label.set_markup(&error_markup(&err));
                } else {
                    append_bubble(&widgets.chat_list, Role::System, &format!("Error: {err}"));
                }
                self.tool_output = None;
                self.tool_card = None;
                if let Some(card) = self.reasoning_card.take() {
                    finalize_reasoning_card(&card, 0);
                }
                self.reasoning_partial.clear();
                self.reasoning_ms_queue.clear();
                self.partial.clear();
                self.busy = false;
            }

            Msg::Toast(text) => {
                if let Some(id) = self.toast_timeout.take() {
                    id.remove();
                }
                widgets.toast_label.set_label(&text);
                widgets.toast.set_reveal_child(true);
                let revealer = widgets.toast.clone();
                self.toast_timeout = Some(gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(1600),
                    move || {
                        revealer.set_reveal_child(false);
                    },
                ));
            }

            Msg::Quit => {
                if let Some(app) = root.application() {
                    app.quit();
                } else {
                    root.close();
                }
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

/// Habilita/inhabilita el envío según haya sesión y no estemos ocupados, y
/// refleja el agente activo en los toggles (sin disparar el handler: el
/// `AgentChanged` neutraliza el eco comparando con `current_agent`). El botón
/// de modelo muestra el modelo activo y solo se inhabilita durante un turno:
/// puede abrirse aunque no haya clave de proveedor en el entorno (la falta se
/// reporta al enviar; ver [ADR-0008]).
fn update_controls(model: &App, widgets: &AppWidgets) {
    widgets
        .sessions_sidebar_expanded
        .set_visible(!model.sessions_sidebar_collapsed);
    widgets
        .sessions_sidebar_collapsed
        .set_visible(model.sessions_sidebar_collapsed);
    widgets.sessions_sidebar.set_size_request(
        if model.sessions_sidebar_collapsed {
            48
        } else {
            -1
        },
        -1,
    );

    let show_changes = model.wide_layout && !model.changes_patch.trim().is_empty();
    widgets.changes_separator.set_visible(show_changes);
    widgets.changes_panel.set_visible(show_changes);
    widgets.changes_panel.set_reveal_child(show_changes);

    let ready = model.current_session.is_some() && !model.busy;
    widgets.entry.set_sensitive(ready);
    widgets.send_button.set_sensitive(ready);

    let toggles_enabled = model.current_session.is_some() && !model.busy;
    widgets.agent_build.set_sensitive(toggles_enabled);
    widgets.agent_plan.set_sensitive(toggles_enabled);
    match model.current_agent {
        AgentKind::Build => widgets.agent_build.set_active(true),
        AgentKind::Plan => widgets.agent_plan.set_active(true),
    }

    let model_label: String = if model.current_model.is_empty() {
        "Modelo".to_string()
    } else {
        // Mostrar solo `model_id` (parte tras el slash) si viene como par.
        ModelRef::parse(&model.current_model)
            .map(|r| r.model_id)
            .unwrap_or_else(|| model.current_model.clone())
    };
    widgets.model_button.set_label(&model_label);
    widgets.model_button.set_sensitive(!model.busy);

    // Botón ojo: solo visible si el modelo activo es razonador. El estado e
    // ícono reflejan el toggle global. `block_signal`/`unblock_signal` no es
    // necesario porque el `set_active` con el valor que ya tiene es no-op y
    // si difiere, el handler emite `ToggleReasoning(b.is_active())` con el
    // mismo valor que ya hay en el estado: el update se vuelve idempotente.
    let reasoning_visible = ModelRef::parse(&model.current_model)
        .or_else(|| model.engine.catalog().resolve_legacy(&model.current_model))
        .map(|r| model.engine.catalog().is_reasoning_model(&r))
        .unwrap_or(false);
    widgets.reasoning_button.set_visible(reasoning_visible);
    if reasoning_visible {
        widgets.reasoning_button.set_active(model.show_reasoning);
        let (icon, tooltip) = if model.show_reasoning {
            ("view-reveal-symbolic", "Ocultar pensamientos")
        } else {
            ("view-conceal-symbolic", "Mostrar pensamientos")
        };
        widgets.reasoning_button.set_icon_name(icon);
        widgets.reasoning_button.set_tooltip_text(Some(tooltip));
    }
}

fn request_changes_patch(model: &App, sender: &ComponentSender<App>) {
    let Some(snap) = model.snapshots.clone() else {
        sender.input(Msg::ChangesPatchLoaded(String::new()));
        return;
    };
    let hash = model.session_base_snapshot.clone();
    let sender = sender.clone();
    relm4::spawn(async move {
        let patch = match hash {
            Some(hash) => snap.patch(&hash).await,
            None => snap.worktree_patch().await,
        };
        match patch {
            Ok(patch) => sender.input(Msg::ChangesPatchLoaded(patch)),
            Err(err) => sender.input(Msg::Failed(err.to_string())),
        }
    });
}

struct DiffFile {
    path: String,
    lines: Vec<DiffLine>,
}

struct DiffLine {
    old: Option<usize>,
    new: Option<usize>,
    kind: DiffLineKind,
    text: String,
}

enum DiffLineKind {
    Context,
    Addition,
    Deletion,
}

fn render_changes_patch(container: &gtk::Box, files: &[DiffFile]) {
    clear_chat(container);
    for file in files {
        container.append(&make_diff_file_section(file));
    }
}

fn update_changes_nav_label(widgets: &AppWidgets, model: &App) {
    let total = model.changes_nav_files.len();
    if total == 0 {
        return;
    }
    let path = &model.changes_nav_files[model.changes_nav_index];
    let current = model.changes_nav_index + 1;
    let markup = format!(
        "<b>{}</b> ({}/{})",
        relm4::gtk::glib::markup_escape_text(path),
        current,
        total
    );
    widgets.changes_nav_label.set_markup(&markup);
    widgets
        .changes_nav_prev
        .set_sensitive(model.changes_nav_index > 0);
    widgets
        .changes_nav_next
        .set_sensitive(model.changes_nav_index + 1 < total);
}

fn scroll_to_changes_file(widgets: &AppWidgets, index: usize) {
    let mut child = widgets.changes_list.first_child();
    for _ in 0..index {
        child = child.and_then(|c| c.next_sibling());
    }
    let Some(target) = child else { return };
    let Some((_, y)) = target.translate_coordinates(&widgets.changes_list, 0.0, 0.0) else {
        return;
    };
    // Subir la cadena de padres hasta encontrar el ScrolledWindow.
    let mut ancestor = widgets.changes_list.parent();
    while let Some(ref widget) = ancestor {
        if let Some(sw) = widget.downcast_ref::<gtk::ScrolledWindow>() {
            sw.vadjustment().set_value(y.max(0.0));
            return;
        }
        ancestor = widget.parent();
    }
}

fn parse_patch_files(patch: &str) -> Vec<DiffFile> {
    let mut files = Vec::new();
    let mut current: Option<DiffFile> = None;
    let mut old_line: Option<usize> = None;
    let mut new_line: Option<usize> = None;

    for line in patch.lines() {
        if let Some(path) = line.strip_prefix("diff --git ").and_then(parse_diff_path) {
            if let Some(file) = current.take().filter(|f| !f.lines.is_empty()) {
                files.push(file);
            }
            current = Some(DiffFile {
                path,
                lines: Vec::new(),
            });
            old_line = None;
            new_line = None;
            continue;
        }

        if line.starts_with("@@") {
            if let Some((old, new)) = parse_hunk_lines(line) {
                old_line = Some(old);
                new_line = Some(new);
            }
            continue;
        }

        if line.starts_with("+++")
            || line.starts_with("---")
            || line.starts_with("index ")
            || line.starts_with("new file mode ")
            || line.starts_with("deleted file mode ")
        {
            continue;
        }

        if let Some(text) = line.strip_prefix('+') {
            let new = new_line.unwrap_or(0);
            if let Some(file) = &mut current {
                file.lines.push(DiffLine {
                    old: None,
                    new: Some(new),
                    kind: DiffLineKind::Addition,
                    text: text.to_string(),
                });
            }
            new_line = new_line.map(|n| n + 1);
            continue;
        }

        if let Some(text) = line.strip_prefix('-') {
            let old = old_line.unwrap_or(0);
            if let Some(file) = &mut current {
                file.lines.push(DiffLine {
                    old: Some(old),
                    new: None,
                    kind: DiffLineKind::Deletion,
                    text: text.to_string(),
                });
            }
            old_line = old_line.map(|n| n + 1);
            continue;
        }

        if let Some(text) = line.strip_prefix(' ') {
            let old = old_line.unwrap_or(0);
            let new = new_line.unwrap_or(0);
            if let Some(file) = &mut current {
                file.lines.push(DiffLine {
                    old: Some(old),
                    new: Some(new),
                    kind: DiffLineKind::Context,
                    text: text.to_string(),
                });
            }
            old_line = old_line.map(|n| n + 1);
            new_line = new_line.map(|n| n + 1);
        }
    }

    if let Some(file) = current.take().filter(|f| !f.lines.is_empty()) {
        files.push(file);
    }
    files
}

fn parse_diff_path(line: &str) -> Option<String> {
    line.split_whitespace()
        .nth(1)
        .map(|s| s.strip_prefix("b/").unwrap_or(s).to_string())
}

fn make_diff_file_section(file: &DiffFile) -> gtk::Widget {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("card");
    card.set_hexpand(true);

    let header = gtk::Label::new(Some(&file.path));
    header.set_xalign(0.0);
    header.set_selectable(true);
    header.add_css_class("heading");
    header.set_margin_top(8);
    header.set_margin_bottom(8);
    header.set_margin_start(10);
    header.set_margin_end(10);
    card.append(&header);

    for line in &file.lines {
        card.append(&make_diff_line(line));
    }

    card.upcast()
}

fn make_diff_line(line: &DiffLine) -> gtk::Widget {
    let old = line
        .old
        .map(|n| format!("{n:>4}"))
        .unwrap_or_else(|| "    ".to_string());
    let new = line
        .new
        .map(|n| format!("{n:>4}"))
        .unwrap_or_else(|| "    ".to_string());
    let (mark, css_class) = match line.kind {
        DiffLineKind::Context => (" ", None),
        DiffLineKind::Addition => ("+", Some("diff-line-addition")),
        DiffLineKind::Deletion => ("-", Some("diff-line-deletion")),
    };
    let text = relm4::gtk::glib::markup_escape_text(&line.text);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.set_hexpand(true);
    if let Some(css_class) = css_class {
        row.add_css_class(css_class);
    }

    let label = gtk::Label::new(None);
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_wrap(false);
    label.set_selectable(true);
    label.add_css_class("monospace");
    label.set_margin_start(8);
    label.set_margin_end(8);
    label.set_markup(&format!("{old} {new} {mark} {text}"));
    row.append(&label);
    row.upcast()
}

fn parse_hunk_lines(line: &str) -> Option<(usize, usize)> {
    let mut parts = line.split_whitespace();
    parts.next()?;
    let old = parse_hunk_start(parts.next()?)?;
    let new = parse_hunk_start(parts.next()?)?;
    Some((old, new))
}

fn parse_hunk_start(part: &str) -> Option<usize> {
    part.get(1..)?.split(',').next()?.parse().ok()
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
/// selección programática se neutraliza en `Msg::SelectIndex`). Engancha en
/// cada fila un menú contextual con la opción de eliminar.
fn rebuild_session_list(
    list: &gtk::ListBox,
    sessions: &[SessionMeta],
    selected: i32,
    sender: &ComponentSender<App>,
) {
    clear_list(list);
    for meta in sessions {
        let label = gtk::Label::new(Some(&meta.title));
        label.set_xalign(0.0);
        label.set_margin_all(8);
        label.set_max_width_chars(28);
        label.set_ellipsize(relm4::gtk::pango::EllipsizeMode::End);
        list.append(&label);
        // `ListBox::append` envuelve el hijo en una `ListBoxRow`: ese es el
        // widget al que adjuntamos el gesto de clic derecho.
        if let Some(row) = label.parent() {
            attach_session_context_menu(&row, meta.id, sender);
        }
    }
    if let Some(row) = list.row_at_index(selected) {
        list.select_row(Some(&row));
    }
}

/// Adjunta a la fila del sidebar un `Popover` con la acción de eliminar y un
/// `GestureClick` que lo despliega al hacer clic derecho.
fn attach_session_context_menu(row: &gtk::Widget, session_id: i64, sender: &ComponentSender<App>) {
    let popover = gtk::Popover::new();
    popover.set_has_arrow(false);
    popover.set_parent(row);

    let delete = gtk::Button::with_label("Eliminar");
    delete.add_css_class("flat");
    delete.add_css_class("destructive-action");
    {
        let popover = popover.clone();
        let sender = sender.clone();
        delete.connect_clicked(move |_| {
            popover.popdown();
            sender.input(Msg::DeleteSessionRequest(session_id));
        });
    }
    popover.set_child(Some(&delete));

    // `set_parent` mantiene el popover en el árbol de widgets de la fila;
    // GTK exige desadosarlo antes de que la fila se finalice (al reconstruir
    // el sidebar). Lo hacemos en el destructor del wrapper, atado al
    // lifecycle de la fila vía `unrealize`.
    row.connect_unrealize({
        let popover = popover.clone();
        move |_| popover.unparent()
    });

    let gesture = gtk::GestureClick::new();
    gesture.set_button(gtk::gdk::BUTTON_SECONDARY);
    gesture.connect_pressed(move |_, _, x, y| {
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.popup();
    });
    row.add_controller(gesture);
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

/// Renderiza un historial cargado: tarjetas de reasoning (si las hay) → burbuja
/// de texto del asistente → tarjetas de tool con su salida. Los resultados
/// `Role::Tool` se fusionan en la tarjeta por id; los mensajes con snapshot
/// reciben un botón "Revertir" en su última tarjeta. Devuelve las tarjetas de
/// reasoning creadas para que el toggle global pueda actuar sobre ellas.
fn render_history(
    chat_list: &gtk::Box,
    messages: &[(i64, Message)],
    snapshots: &HashMap<i64, String>,
    reasoning_ms: &HashMap<i64, u64>,
    show_reasoning: bool,
    sender: &ComponentSender<App>,
) -> Vec<ReasoningCard> {
    let outputs: HashMap<&str, &str> = messages
        .iter()
        .filter(|(_, m)| m.role == Role::Tool)
        .filter_map(|(_, m)| m.tool_call_id.as_deref().map(|id| (id, m.content.as_str())))
        .collect();

    let mut cards: Vec<ReasoningCard> = Vec::new();

    for (message_id, message) in messages {
        match message.role {
            Role::User | Role::System => {
                append_bubble(chat_list, message.role, &message.content);
            }
            Role::Assistant => {
                if let Some(reasoning) = message.reasoning.as_deref().filter(|s| !s.is_empty()) {
                    let ms = reasoning_ms.get(message_id).copied().unwrap_or(0);
                    let summary = reasoning_summary(ms);
                    let card = append_reasoning_card(chat_list, show_reasoning, &summary, false);
                    card.body.set_text(reasoning);
                    cards.push(card);
                }
                if !message.content.is_empty() {
                    append_assistant_blocks(chat_list, &message.content, sender);
                }
                let mut last_card: Option<gtk::Box> = None;
                for call in &message.tool_calls {
                    let output = outputs.get(call.id.as_str()).copied().unwrap_or("");
                    let (card, label) = append_tool_card(
                        chat_list,
                        &call.function.name,
                        &call.function.arguments,
                        output,
                    );
                    set_tool_output(&label, output, true);
                    last_card = Some(card);
                }
                if snapshots.contains_key(message_id) {
                    if let Some(card) = last_card {
                        attach_revert_button(&card, *message_id, sender);
                    }
                }
            }
            Role::Tool => {} // ya fusionado en la tarjeta de la llamada
        }
    }

    cards
}

/// Añade una tarjeta de *chain of thought* al final del chat. `show_body`
/// controla la visibilidad inicial del cuerpo (lo aplica el toggle global);
/// `spinner_active` activa el spinner cuando la tarjeta está en streaming.
fn append_reasoning_card(
    chat_list: &gtk::Box,
    show_body: bool,
    summary_text: &str,
    spinner_active: bool,
) -> ReasoningCard {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
    card.add_css_class("card");
    card.set_margin_all(4);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    header.set_margin_top(8);
    header.set_margin_bottom(4);
    header.set_margin_start(8);
    header.set_margin_end(8);

    let icon = gtk::Label::new(Some("🧠"));
    let spinner = gtk::Spinner::new();
    spinner.set_visible(spinner_active);
    if spinner_active {
        spinner.start();
    }
    let summary = gtk::Label::new(Some(summary_text));
    summary.set_xalign(0.0);
    summary.add_css_class("dim-label");

    header.append(&icon);
    header.append(&spinner);
    header.append(&summary);

    let body = gtk::Label::new(None);
    body.set_xalign(0.0);
    body.set_wrap(true);
    body.set_selectable(true);
    body.set_margin_start(8);
    body.set_margin_end(8);
    body.set_margin_bottom(8);
    body.add_css_class("dim-label");
    body.set_visible(show_body);

    card.append(&header);
    card.append(&body);
    chat_list.append(&card);

    ReasoningCard {
        spinner,
        summary,
        body,
    }
}

/// Cierra una tarjeta en streaming: detiene el spinner y reemplaza el resumen
/// por la duración medida.
fn finalize_reasoning_card(card: &ReasoningCard, ms: u64) {
    card.spinner.stop();
    card.spinner.set_visible(false);
    card.summary.set_text(&reasoning_summary(ms));
}

/// Texto del resumen colapsado de una tarjeta de reasoning. Con duración:
/// "Pensamiento · 4.2 s"; sin duración: "Pensamiento".
fn reasoning_summary(ms: u64) -> String {
    if ms == 0 {
        "Pensamiento".to_string()
    } else {
        format!("Pensamiento · {}", format_duration(ms))
    }
}

/// Formatea una duración en ms para mostrarla al usuario: ms hasta 1 s, "X.Y s"
/// hasta 1 min, "Xm Ys" más allá.
fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else {
        let mins = ms / 60_000;
        let secs = (ms % 60_000) / 1000;
        format!("{mins}m {secs}s")
    }
}

/// Ubicación del shadow git de un proyecto. Vive bajo el directorio de datos
/// XDG, separado por `project_id` para que las sesiones del mismo worktree
/// compartan objetos.
fn snapshots_dir(project_id: i64) -> PathBuf {
    let base = match std::env::var_os("XDG_DATA_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => {
            let home = std::env::var_os("HOME").unwrap_or_default();
            PathBuf::from(home).join(".local/share")
        }
    };
    base.join("xiě-code")
        .join("snapshots")
        .join(project_id.to_string())
}

/// Burbuja de texto plano (User/System). Devuelve el `Label` interno por si el
/// llamador necesita mutarlo después.
fn append_bubble(chat_list: &gtk::Box, role: Role, content: &str) -> gtk::Label {
    let row = build_bubble_shell(chat_list, role);
    let body = make_prose_label();
    body.set_text(content);
    row.append(&body);
    body
}

/// Burbuja del asistente en streaming: contenedor vertical con un único `Label`
/// que va recibiendo texto plano incremental. Al cerrar el segmento, el body
/// se vacía y se rellena con bloques (prosa + code-blocks con botón copy).
fn append_assistant_streaming(chat_list: &gtk::Box) -> StreamingBubble {
    let row = build_bubble_shell(chat_list, Role::Assistant);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
    body.set_halign(gtk::Align::Fill);
    let label = make_prose_label();
    body.append(&label);
    row.append(&body);
    StreamingBubble { body, label }
}

/// Burbuja del asistente del historial: igual que el streaming pero ya
/// renderizada en bloques desde el principio.
fn append_assistant_blocks(chat_list: &gtk::Box, markdown: &str, sender: &ComponentSender<App>) {
    let row = build_bubble_shell(chat_list, Role::Assistant);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
    body.set_halign(gtk::Align::Fill);
    fill_with_blocks(&body, markdown, sender);
    row.append(&body);
}

/// Construye el "shell" común de una burbuja (row + autor) y lo appendea al
/// chat. El llamador appendea el body al row resultante.
fn build_bubble_shell(chat_list: &gtk::Box, role: Role) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
    row.set_halign(gtk::Align::Fill);
    let author = gtk::Label::new(Some(role_name(role)));
    author.set_xalign(0.0);
    author.add_css_class("dim-label");
    author.add_css_class("caption-heading");
    row.append(&author);
    chat_list.append(&row);
    row
}

fn make_prose_label() -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_selectable(true);
    label.set_halign(gtk::Align::Start);
    label
}

/// Vacía `body` y lo rellena con los bloques renderizados de `markdown`.
fn fill_with_blocks(body: &gtk::Box, markdown: &str, sender: &ComponentSender<App>) {
    while let Some(child) = body.first_child() {
        body.remove(&child);
    }
    for block in markdown::parse_blocks(markdown) {
        match block {
            markdown::Block::Prose(markup) => {
                let label = make_prose_label();
                label.set_markup(&markup);
                body.append(&label);
            }
            markdown::Block::Code { lang, text } => {
                body.append(&make_code_block(lang.as_deref(), &text, sender));
            }
            markdown::Block::Table { headers, rows } => {
                body.append(&make_table_block(&headers, &rows));
            }
        }
    }
}

/// Tarjeta de bloque de código: lenguaje opcional arriba, código monoespaciado
/// con scroll horizontal y un botón de copiar flotante en la esquina inferior
/// derecha.
fn make_code_block(lang: Option<&str>, text: &str, sender: &ComponentSender<App>) -> gtk::Widget {
    let frame = gtk::Frame::new(None);
    frame.add_css_class("card");

    let stack = gtk::Box::new(gtk::Orientation::Vertical, 0);

    if let Some(lang) = lang.filter(|l| !l.is_empty()) {
        let header = gtk::Label::new(Some(lang));
        header.set_xalign(0.0);
        header.add_css_class("dim-label");
        header.add_css_class("caption");
        header.set_margin_top(4);
        header.set_margin_start(8);
        header.set_margin_end(8);
        stack.append(&header);
    }

    let code = gtk::Label::new(Some(text));
    code.set_xalign(0.0);
    code.set_selectable(true);
    code.set_wrap(false);
    code.add_css_class("monospace");
    code.set_margin_top(6);
    code.set_margin_bottom(6);
    code.set_margin_start(8);
    code.set_margin_end(8);
    code.set_halign(gtk::Align::Start);

    let scroll = gtk::ScrolledWindow::new();
    scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
    scroll.set_child(Some(&code));

    let copy = gtk::Button::from_icon_name("edit-copy-symbolic");
    copy.add_css_class("flat");
    copy.add_css_class("circular");
    copy.set_tooltip_text(Some("Copiar"));
    copy.set_halign(gtk::Align::End);
    copy.set_valign(gtk::Align::End);
    copy.set_margin_end(4);
    copy.set_margin_bottom(4);
    let text_owned = text.to_string();
    let sender = sender.clone();
    copy.connect_clicked(move |btn| {
        btn.clipboard().set_text(&text_owned);
        sender.input(Msg::Toast("Texto Copiado".into()));
    });

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&scroll));
    overlay.add_overlay(&copy);

    stack.append(&overlay);
    frame.set_child(Some(&stack));
    frame.upcast()
}

/// Tabla renderizada como `gtk::Grid` con cabeceras en negrita y bordes sutiles.
fn make_table_block(headers: &[String], rows: &[Vec<String>]) -> gtk::Widget {
    let frame = gtk::Frame::new(None);
    frame.add_css_class("card");
    frame.set_margin_top(8);
    frame.set_margin_bottom(8);
    frame.set_hexpand(true);

    let grid = gtk::Grid::new();
    grid.set_column_spacing(12);
    grid.set_row_spacing(4);
    grid.set_margin_top(8);
    grid.set_margin_bottom(8);
    grid.set_margin_start(12);
    grid.set_margin_end(12);

    for (col, header) in headers.iter().enumerate() {
        let label = gtk::Label::new(Some(header));
        label.set_xalign(0.0);
        label.set_selectable(true);
        label.set_halign(gtk::Align::Start);
        label.add_css_class("heading");
        grid.attach(&label, col as i32, 0, 1, 1);
    }

    for (row_idx, row) in rows.iter().enumerate() {
        for (col_idx, cell) in row.iter().enumerate() {
            let label = gtk::Label::new(Some(cell));
            label.set_xalign(0.0);
            label.set_selectable(true);
            label.set_halign(gtk::Align::Start);
            grid.attach(&label, col_idx as i32, (row_idx + 1) as i32, 1, 1);
        }
    }

    let scroll = gtk::ScrolledWindow::new();
    scroll.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
    scroll.set_min_content_width(0);
    scroll.set_propagate_natural_width(false);
    scroll.set_child(Some(&grid));

    frame.set_child(Some(&scroll));
    frame.upcast()
}

/// Añade una tarjeta de ejecución de tool y devuelve la tarjeta y el label de
/// su salida. La tarjeta se devuelve para colgarle después un botón "Revertir".
fn append_tool_card(
    chat_list: &gtk::Box,
    name: &str,
    args: &str,
    initial: &str,
) -> (gtk::Box, gtk::Label) {
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

    let args_label = gtk::Label::new(Some(&format_tool_invocation(name, args)));
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

    (card, output)
}

/// Renderiza la invocación de una tool como una línea estilo comando de consola
/// en lugar del JSON crudo (`grep "patrón" .` en vez de
/// `{"path":".","pattern":"patrón"}`). Si la tool no es conocida o los argumentos
/// no parsean, cae a `name <json>` para no perder información.
fn format_tool_invocation(name: &str, args_json: &str) -> String {
    let Ok(args) = serde_json::from_str::<serde_json::Value>(args_json) else {
        return format!("{name} {args_json}");
    };
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("");
    match name {
        "bash" => format!("$ {}", s("command")),
        "read_file" => format!("read_file {}", shell_quote(s("path"))),
        "write_file" => format!("write_file {}", shell_quote(s("path"))),
        "edit_file" => {
            let mut out = format!("edit_file {}", shell_quote(s("path")));
            if args
                .get("replace_all")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                out.push_str(" --replace-all");
            }
            out
        }
        "list_dir" => format!("list_dir {}", shell_quote(s("path"))),
        "glob" => format!("glob {}", shell_quote(s("pattern"))),
        "grep" => {
            let path = s("path");
            if path.is_empty() {
                format!("grep {}", shell_quote(s("pattern")))
            } else {
                format!("grep {} {}", shell_quote(s("pattern")), shell_quote(path))
            }
        }
        _ => format!("{name} {args_json}"),
    }
}

/// Entrecomilla un valor para su exhibición estilo shell. Si solo contiene
/// caracteres seguros (alfa-num + un puñado de símbolos habituales en rutas y
/// patrones), se deja tal cual; en otro caso se envuelve en `'…'` escapando las
/// comillas simples internas (estilo POSIX).
fn shell_quote(value: &str) -> String {
    let safe = !value.is_empty()
        && value.bytes().all(|b| {
            matches!(b,
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9'
                | b'-' | b'_' | b'.' | b'/' | b':' | b'@' | b'%' | b'+' | b',')
        });
    if safe {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// Cuelga un botón "Revertir" al final de la tarjeta de tool. Al pulsarlo, envía
/// `Msg::Revert(message_id)`; el handler abre el diálogo de confirmación.
fn attach_revert_button(card: &gtk::Box, message_id: i64, sender: &ComponentSender<App>) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.set_halign(gtk::Align::End);
    row.set_margin_all(8);

    let button = gtk::Button::with_label("Revertir");
    button.add_css_class("flat");
    let sender = sender.clone();
    button.connect_clicked(move |_| sender.input(Msg::Revert(message_id)));

    row.append(&button);
    card.append(&row);
}

/// Muestra un `adw::MessageDialog` listando los archivos que cambiarán y, si el
/// usuario confirma, invoca `on_response(true)`. `MessageDialog` está disponible
/// desde libadwaita 1.2 (en 1.5 lo reemplaza `AlertDialog`; migrar cuando se
/// suba la feature en Fase 6, junto con `NavigationSplitView`).
fn show_revert_dialog(
    parent: &adw::ApplicationWindow,
    files: &[PathBuf],
    on_response: impl Fn(bool) + 'static,
) {
    let body = if files.is_empty() {
        "El worktree coincide con el snapshot; no hay archivos que cambiar.".to_string()
    } else {
        let list = files
            .iter()
            .take(20)
            .map(|p| format!("• {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n");
        let extra = files.len().saturating_sub(20);
        if extra == 0 {
            format!("Se sobrescribirán los siguientes archivos:\n\n{list}")
        } else {
            format!("Se sobrescribirán los siguientes archivos:\n\n{list}\n… y {extra} más")
        }
    };

    let dialog = adw::MessageDialog::new(Some(parent), Some("Revertir cambios"), Some(&body));
    dialog.add_response("cancel", "Cancelar");
    dialog.add_response("revert", "Revertir");
    dialog.set_response_appearance("revert", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    dialog.connect_response(None, move |_dialog, response| {
        on_response(response == "revert");
    });
    dialog.present();
}

fn show_command_palette_dialog(
    parent: &adw::ApplicationWindow,
    show_reasoning: bool,
    on_select: impl Fn(CommandPaletteAction) + 'static,
    on_close: impl Fn() + 'static,
) -> adw::MessageDialog {
    let dialog = adw::MessageDialog::new(Some(parent), Some("Comandos"), None);
    dialog.add_response("cancel", "Cancelar");
    dialog.add_response("apply", "Abrir");
    dialog.set_response_appearance("apply", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("apply"));
    dialog.set_close_response("cancel");

    {
        let controller = gtk::ShortcutController::new();
        for shortcut in ["Escape", "<Control>p"] {
            let trigger = gtk::ShortcutTrigger::parse_string(shortcut).expect("trigger válido");
            let dialog_for_shortcut = dialog.clone();
            let action = gtk::CallbackAction::new(move |_, _| {
                dialog_for_shortcut.response("cancel");
                gtk::glib::Propagation::Stop
            });
            controller.add_shortcut(gtk::Shortcut::new(Some(trigger), Some(action)));
        }
        dialog.add_controller(controller);
    }

    let container = gtk::Box::new(gtk::Orientation::Vertical, 6);
    container.set_size_request(420, -1);
    container.set_margin_top(4);

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Buscar comando…"));
    container.append(&search);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.add_css_class("boxed-list");

    let thinking_label = if show_reasoning {
        "Ocultar Thinking"
    } else {
        "Mostrar Thinking"
    };

    let commands: Rc<Vec<(CommandPaletteAction, &str, &str)>> = Rc::new(vec![
        (
            CommandPaletteAction::SelectModel,
            "Seleccionar Modelo",
            "Cambiar el modelo activo",
        ),
        (
            CommandPaletteAction::ConnectProvider,
            "Conectar Proveedor",
            "Agregar una cuenta o API key",
        ),
        (
            CommandPaletteAction::NewSession,
            "Nueva Sesión",
            "Crear una conversación nueva",
        ),
        (
            CommandPaletteAction::ToggleThinking,
            thinking_label,
            "Alternar visibilidad del razonamiento",
        ),
        (CommandPaletteAction::Quit, "Salir", "Cerrar xiě-code"),
    ]);

    for (idx, (_, title, description)) in commands.iter().enumerate() {
        let row = gtk::ListBoxRow::new();
        let row_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
        row_box.set_margin_top(8);
        row_box.set_margin_bottom(8);
        row_box.set_margin_start(12);
        row_box.set_margin_end(12);

        let title_label = gtk::Label::new(Some(title));
        title_label.set_xalign(0.0);
        title_label.add_css_class("heading");
        row_box.append(&title_label);

        let description_label = gtk::Label::new(Some(description));
        description_label.set_xalign(0.0);
        description_label.add_css_class("dim-label");
        row_box.append(&description_label);

        row.set_child(Some(&row_box));
        unsafe {
            row.set_data::<usize>("idx", idx);
            row.set_data::<Rc<String>>(
                "needle",
                Rc::new(format!("{title} {description}").to_lowercase()),
            );
        }
        list.append(&row);
    }

    if let Some(first) = list.row_at_index(0) {
        list.select_row(Some(&first));
    }

    let commands_len = commands.len();
    {
        let search_for_filter = search.clone();
        list.set_filter_func(move |row| {
            let needle = search_for_filter.text().to_string();
            if needle.is_empty() {
                return true;
            }
            let needle = needle.to_lowercase();
            unsafe { row.data::<Rc<String>>("needle") }
                .map(|ptr| {
                    let s = unsafe { ptr.as_ref() };
                    s.contains(&needle)
                })
                .unwrap_or(true)
        });
        let list_for_search = list.clone();
        search.connect_search_changed(move |search| {
            list_for_search.invalidate_filter();
            let needle = search.text().to_string().to_lowercase();
            for idx in 0..commands_len {
                let Some(row) = list_for_search.row_at_index(idx as i32) else {
                    continue;
                };
                let matches = unsafe { row.data::<Rc<String>>("needle") }
                    .map(|ptr| {
                        let stored = unsafe { ptr.as_ref() };
                        needle.is_empty() || stored.contains(&needle)
                    })
                    .unwrap_or(false);
                if matches {
                    list_for_search.select_row(Some(&row));
                    return;
                }
            }
            list_for_search.unselect_all();
        });
    }

    {
        let dialog_for_activate = dialog.clone();
        list.connect_row_activated(move |list, row| {
            list.select_row(Some(row));
            dialog_for_activate.response("apply");
        });
    }

    {
        let dialog_for_search = dialog.clone();
        search.connect_activate(move |_| {
            dialog_for_search.response("apply");
        });
    }

    container.append(&list);
    dialog.set_extra_child(Some(&container));

    let list_for_response = list.clone();
    dialog.connect_response(None, move |dialog, response| {
        on_close();
        dialog.close();
        if response != "apply" {
            return;
        }
        let Some(row) = list_for_response.selected_row() else {
            return;
        };
        let Some(ptr) = (unsafe { row.data::<usize>("idx") }) else {
            return;
        };
        if let Some((action, _, _)) = commands.get(unsafe { *ptr.as_ref() }) {
            on_select(*action);
        }
    });

    dialog.present();
    search.grab_focus();
    dialog
}

/// Una opción del picker de modelos: `value` es el `provider/model` que se
/// persiste y se envía al motor; `label` es la cadena visible.
struct ModelOption {
    value: String,
    label: String,
}

/// Construye la lista de modelos de proveedores conectados, excluyendo los
/// marcados como `deprecated`.
fn catalog_model_options(catalog: &Catalog, connected: &HashSet<String>) -> Vec<ModelOption> {
    catalog
        .providers()
        .filter(|p| connected.contains(&p.id))
        .flat_map(|p| {
            p.models
                .values()
                .filter(|m| m.status != Some(zhi_core::ModelStatus::Deprecated))
                .map(move |m| {
                    let label_model = m.name.as_deref().unwrap_or(&m.id);
                    ModelOption {
                        value: ModelRef::new(&p.id, &m.id).to_string(),
                        label: format!("{label_model}  ·  {}", p.name),
                    }
                })
        })
        .collect()
}

/// `true` si al menos uno de los proveedores del catálogo filtrado tiene su
/// API key en el entorno.
fn any_provider_key_present(catalog: &Catalog) -> bool {
    catalog.providers().any(|p| p.has_api_key())
}

/// Lanza una tarea Tokio que refresca la cache de `models.dev` cuando está
/// stale, y la repite cada `REFRESH_INTERVAL`. La sesión actual seguirá con
/// el catálogo ya cargado en memoria; el JSON fresco se usará al próximo
/// arranque (mismo patrón que OpenCode). Respeta `XIE_DISABLE_MODELS_FETCH`.
fn spawn_catalog_refresh() {
    if std::env::var_os("XIE_DISABLE_MODELS_FETCH").is_some() {
        return;
    }
    relm4::spawn(async move {
        loop {
            if !Catalog::cache_is_fresh() {
                match Catalog::fetch_and_cache().await {
                    Ok(_) => tracing::debug!("models.dev: cache refrescada"),
                    Err(e) => tracing::warn!(error = %e, "no se pudo refrescar models.dev"),
                }
            }
            tokio::time::sleep(zhi_core::catalog_internals::REFRESH_INTERVAL).await;
        }
    });
}

/// Modal de selección de modelo. Lista los modelos del catálogo (miles)
/// dentro de un `ScrolledWindow` con altura acotada y un `SearchEntry` para
/// filtrar por substring (insensible a mayúsculas). Al activar una fila o
/// pulsar "Aplicar" se invoca `on_select(provider/model)`; cancelar no
/// invoca nada.
fn show_model_picker_dialog(
    parent: &adw::ApplicationWindow,
    current: &str,
    options: &[ModelOption],
    on_select: impl Fn(String) + 'static,
) {
    let dialog = adw::MessageDialog::new(Some(parent), Some("Modelo"), None);
    dialog.add_response("cancel", "Cancelar");
    dialog.add_response("apply", "Aplicar");
    dialog.set_response_appearance("apply", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("apply"));
    dialog.set_close_response("cancel");

    let container = gtk::Box::new(gtk::Orientation::Vertical, 6);
    container.set_margin_top(4);
    container.set_size_request(420, -1);

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Buscar modelo o proveedor…"));
    container.append(&search);

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroller.set_propagate_natural_height(true);
    // Altura acotada: con miles de modelos, sin esto el diálogo se sale de
    // la pantalla. `max_content_height` requiere `propagate_natural_height`.
    scroller.set_min_content_height(360);
    scroller.set_max_content_height(480);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.add_css_class("boxed-list");
    scroller.set_child(Some(&list));
    container.append(&scroller);
    dialog.set_extra_child(Some(&container));

    let values: Rc<Vec<String>> = Rc::new(options.iter().map(|o| o.value.clone()).collect());
    let options_len = options.len();
    let current_idx = options.iter().position(|o| o.value == current);

    for (idx, option) in options.iter().enumerate() {
        let row = gtk::ListBoxRow::new();
        // Guardamos el índice en el widget como hijo `Label` invisible; más
        // simple y robusto que usar GObject data en este punto.
        let label = gtk::Label::new(Some(&option.label));
        label.set_xalign(0.0);
        label.set_margin_top(8);
        label.set_margin_bottom(8);
        label.set_margin_start(12);
        label.set_margin_end(12);
        row.set_child(Some(&label));
        // Etiqueta minúscula como búsqueda preprocesada para el filtro.
        let needle_target: Rc<String> = Rc::new(option.label.to_lowercase());
        unsafe {
            row.set_data::<usize>("idx", idx);
            row.set_data::<Rc<String>>("needle", needle_target);
        }
        list.append(&row);
    }

    // Seleccionar el modelo activo al abrir, manteniendo el foco en el buscador.
    if let Some(idx) = current_idx.or((!options.is_empty()).then_some(0)) {
        if let Some(row) = list.row_at_index(idx as i32) {
            list.select_row(Some(&row));
        }
    }

    // Filtro por substring sobre la etiqueta preprocesada (minúsculas).
    {
        let search_for_filter = search.clone();
        list.set_filter_func(move |row| {
            let needle = search_for_filter.text().to_string();
            if needle.is_empty() {
                return true;
            }
            let needle = needle.to_lowercase();
            let stored = unsafe { row.data::<Rc<String>>("needle") };
            stored
                .map(|ptr| {
                    let s = unsafe { ptr.as_ref() };
                    s.contains(&needle)
                })
                .unwrap_or(true)
        });
        let list_for_search = list.clone();
        search.connect_search_changed(move |search| {
            list_for_search.invalidate_filter();
            if let Some(row) = first_matching_model_row(
                &list_for_search,
                &search.text().to_string().to_lowercase(),
                options_len,
            ) {
                list_for_search.select_row(Some(&row));
            } else {
                list_for_search.unselect_all();
            }
        });
    }

    // Activar una fila (Enter o doble clic) selecciona y emite "apply": el
    // `connect_response` de abajo se encarga del resto.
    {
        let dialog_for_activate = dialog.clone();
        list.connect_row_activated(move |list, row| {
            list.select_row(Some(row));
            dialog_for_activate.response("apply");
        });
    }

    {
        let dialog_for_search = dialog.clone();
        search.connect_activate(move |_| {
            dialog_for_search.response("apply");
        });
    }

    // Botón "Aplicar": aplica la fila seleccionada actualmente.
    let list_for_response = list.clone();
    let search_for_response = search.clone();
    dialog.connect_response(None, move |dialog, response| {
        if response != "apply" {
            return;
        }
        let needle = search_for_response.text().to_string().to_lowercase();
        let row = list_for_response
            .selected_row()
            .filter(|row| model_row_matches(row, &needle))
            .or_else(|| first_matching_model_row(&list_for_response, &needle, options_len));
        let Some(row) = row else {
            return;
        };
        let Some(ptr) = (unsafe { row.data::<usize>("idx") }) else {
            return;
        };
        let idx = unsafe { *ptr.as_ref() };
        if let Some(chosen) = values.get(idx) {
            on_select(chosen.clone());
            dialog.close();
        }
    });

    dialog.present();
    gtk::glib::idle_add_local_once(move || {
        search.grab_focus();
    });
}

fn first_matching_model_row(
    list: &gtk::ListBox,
    needle: &str,
    options_len: usize,
) -> Option<gtk::ListBoxRow> {
    (0..options_len).find_map(|idx| {
        let row = list.row_at_index(idx as i32)?;
        model_row_matches(&row, needle).then_some(row)
    })
}

fn model_row_matches(row: &gtk::ListBoxRow, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    unsafe { row.data::<Rc<String>>("needle") }
        .map(|ptr| {
            let stored = unsafe { ptr.as_ref() };
            stored.contains(needle)
        })
        .unwrap_or(false)
}

/// Acción que el modal de Configuración devuelve al callback.
enum SettingsAction {
    Connect,
    Disconnect(String),
}

/// Modal "Configuración". Muestra las cuentas conectadas con un botón de
/// desconectar por fila y un botón general "Conectar proveedor" abajo.
fn show_settings_dialog(
    parent: &adw::ApplicationWindow,
    connected: &[(String, String)],
    on_action: impl Fn(SettingsAction) + 'static,
) {
    let on_action = Rc::new(on_action);
    let dialog = adw::MessageDialog::new(Some(parent), Some("Configuración"), None);
    dialog.add_response("close", "Cerrar");
    dialog.set_default_response(Some("close"));
    dialog.set_close_response("close");

    let container = gtk::Box::new(gtk::Orientation::Vertical, 12);
    container.set_size_request(420, -1);
    container.set_margin_top(4);

    let section_title = gtk::Label::new(Some("Cuentas"));
    section_title.set_xalign(0.0);
    section_title.add_css_class("heading");
    container.append(&section_title);

    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    if connected.is_empty() {
        let row = gtk::ListBoxRow::new();
        row.set_selectable(false);
        let label = gtk::Label::new(Some(
            "No hay cuentas conectadas. Pulsa «Conectar proveedor» para empezar.",
        ));
        label.set_xalign(0.0);
        label.set_wrap(true);
        label.set_margin_top(10);
        label.set_margin_bottom(10);
        label.set_margin_start(12);
        label.set_margin_end(12);
        row.set_child(Some(&label));
        list.append(&row);
    } else {
        for (id, kind) in connected {
            let row = gtk::ListBoxRow::new();
            row.set_selectable(false);
            let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            hbox.set_margin_top(8);
            hbox.set_margin_bottom(8);
            hbox.set_margin_start(12);
            hbox.set_margin_end(8);
            let label = gtk::Label::new(Some(&format!("{id} · {kind}")));
            label.set_xalign(0.0);
            label.set_hexpand(true);
            hbox.append(&label);
            let disconnect = gtk::Button::with_label("Desconectar");
            disconnect.add_css_class("flat");
            disconnect.add_css_class("destructive-action");
            let id_clone = id.clone();
            let dialog_for_close = dialog.clone();
            let on_action_clone = on_action.clone();
            disconnect.connect_clicked(move |_| {
                on_action_clone(SettingsAction::Disconnect(id_clone.clone()));
                dialog_for_close.close();
            });
            hbox.append(&disconnect);
            row.set_child(Some(&hbox));
            list.append(&row);
        }
    }
    container.append(&list);

    let connect = gtk::Button::with_label("Conectar proveedor");
    connect.add_css_class("suggested-action");
    let on_action_for_connect = on_action.clone();
    let dialog_for_connect = dialog.clone();
    connect.connect_clicked(move |_| {
        on_action_for_connect(SettingsAction::Connect);
        dialog_for_connect.close();
    });
    container.append(&connect);

    dialog.set_extra_child(Some(&container));
    dialog.present();
}

/// Modal "Conectar proveedor". Lista los proveedores del catálogo y al
/// confirmar invoca `on_select(provider_id)`. OpenAI dispara el flujo OAuth;
/// el resto cae a entrada manual de API key.
fn show_connect_provider_dialog(
    parent: &adw::ApplicationWindow,
    providers: &[(String, String)],
    connected_ids: &HashSet<String>,
    on_select: impl Fn(String) + 'static,
) {
    let dialog = adw::MessageDialog::new(Some(parent), Some("Conectar proveedor"), None);
    dialog.add_response("cancel", "Cancelar");
    dialog.add_response("apply", "Continuar");
    dialog.set_response_appearance("apply", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("apply"));
    dialog.set_close_response("cancel");

    let container = gtk::Box::new(gtk::Orientation::Vertical, 6);
    container.set_size_request(420, -1);
    container.set_margin_top(4);

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Buscar proveedor…"));
    container.append(&search);

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroller.set_propagate_natural_height(true);
    scroller.set_min_content_height(320);
    scroller.set_max_content_height(420);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.add_css_class("boxed-list");

    // Pone OpenAI primero (es el único con OAuth implementado) y el resto en
    // orden del catálogo. Una pequeña etiqueta indica el método disponible.
    let mut sorted: Vec<&(String, String)> = providers.iter().collect();
    sorted.sort_by_key(|(id, _)| if id == "openai" { 0 } else { 1 });

    let connected_ids: Rc<HashSet<String>> = Rc::new(connected_ids.iter().cloned().collect());
    let values: Rc<Vec<String>> = Rc::new(sorted.iter().map(|(id, _)| id.clone()).collect());
    for (idx, (id, name)) in sorted.iter().enumerate() {
        let is_connected = connected_ids.contains(id.as_str());
        let row = gtk::ListBoxRow::new();
        row.set_selectable(!is_connected);
        row.set_activatable(!is_connected);
        row.set_sensitive(!is_connected);

        let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        hbox.set_margin_top(8);
        hbox.set_margin_bottom(8);
        hbox.set_margin_start(12);
        hbox.set_margin_end(12);

        let text_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text_box.set_hexpand(true);
        let title = gtk::Label::new(Some(name));
        title.set_xalign(0.0);
        title.add_css_class("heading");
        text_box.append(&title);
        let subtitle = gtk::Label::new(Some(if id == "openai" {
            "OAuth (cuenta ChatGPT Pro/Plus)"
        } else {
            "API key"
        }));
        subtitle.set_xalign(0.0);
        subtitle.add_css_class("dim-label");
        text_box.append(&subtitle);
        hbox.append(&text_box);

        if is_connected {
            let check = gtk::Image::from_icon_name("object-select-symbolic");
            check.add_css_class("success");
            check.set_tooltip_text(Some("Proveedor conectado"));
            hbox.append(&check);
        }

        row.set_child(Some(&hbox));
        let needle: Rc<String> = Rc::new(format!("{} {}", id, name).to_lowercase());
        unsafe {
            row.set_data::<usize>("idx", idx);
            row.set_data::<Rc<String>>("needle", needle);
        }
        list.append(&row);
    }
    if let Some(idx) = sorted
        .iter()
        .position(|(id, _)| !connected_ids.contains(id.as_str()))
    {
        if let Some(row) = list.row_at_index(idx as i32) {
            list.select_row(Some(&row));
        }
    }

    {
        let search_for_filter = search.clone();
        list.set_filter_func(move |row| {
            let needle = search_for_filter.text().to_string();
            if needle.is_empty() {
                return true;
            }
            let needle = needle.to_lowercase();
            unsafe { row.data::<Rc<String>>("needle") }
                .map(|ptr| {
                    let s = unsafe { ptr.as_ref() };
                    s.contains(&needle)
                })
                .unwrap_or(true)
        });
        let list_for_search = list.clone();
        search.connect_search_changed(move |_| {
            list_for_search.invalidate_filter();
        });
    }

    scroller.set_child(Some(&list));
    container.append(&scroller);
    dialog.set_extra_child(Some(&container));

    {
        let dialog_for_activate = dialog.clone();
        let connected_for_activate = connected_ids.clone();
        let values_for_activate = values.clone();
        list.connect_row_activated(move |list, row| {
            let Some(ptr) = (unsafe { row.data::<usize>("idx") }) else {
                return;
            };
            let idx = unsafe { *ptr.as_ref() };
            if values_for_activate
                .get(idx)
                .is_some_and(|id| connected_for_activate.contains(id))
            {
                return;
            }
            list.select_row(Some(row));
            dialog_for_activate.response("apply");
        });
    }

    let list_for_response = list.clone();
    let connected_for_response = connected_ids.clone();
    dialog.connect_response(None, move |_, response| {
        if response != "apply" {
            return;
        }
        let Some(row) = list_for_response.selected_row() else {
            return;
        };
        let Some(ptr) = (unsafe { row.data::<usize>("idx") }) else {
            return;
        };
        let idx = unsafe { *ptr.as_ref() };
        if let Some(id) = values.get(idx) {
            if connected_for_response.contains(id) {
                return;
            }
            on_select(id.clone());
        }
    });

    dialog.present();
}

/// Modal de espera del callback OAuth. Solo informativo: el callback llega
/// al servidor local y dispara `OauthOpenAiCompleted` / `OauthOpenAiFailed`,
/// que muestran un toast / mensaje en el chat.
fn show_oauth_waiting_dialog(parent: &adw::ApplicationWindow) {
    let dialog = adw::MessageDialog::new(
        Some(parent),
        Some("Conectando con OpenAI"),
        Some(
            "Se ha abierto tu navegador para completar la autorización. \
             Vuelve a esta ventana cuando termines; te avisaremos del resultado.\n\n\
             Si nada se abre, copia esta URL en el navegador manualmente:\n\
             http://127.0.0.1:1455/auth/callback",
        ),
    );
    dialog.add_response("close", "Entendido");
    dialog.set_default_response(Some("close"));
    dialog.set_close_response("close");
    dialog.present();
}

/// Modal de entrada manual de API key para un proveedor.
fn show_api_key_dialog(
    parent: &adw::ApplicationWindow,
    provider_id: &str,
    on_confirm: impl Fn(String) + 'static,
) {
    let dialog = adw::MessageDialog::new(
        Some(parent),
        Some(&format!("API key · {provider_id}")),
        Some("Pega aquí tu API key. Se guardará en auth.json con permisos 0600."),
    );
    dialog.add_response("cancel", "Cancelar");
    dialog.add_response("save", "Guardar");
    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("save"));
    dialog.set_close_response("cancel");

    let entry = gtk::PasswordEntry::new();
    entry.set_show_peek_icon(true);
    entry.set_margin_top(6);
    dialog.set_extra_child(Some(&entry));

    let entry_for_response = entry.clone();
    dialog.connect_response(None, move |_, response| {
        if response != "save" {
            return;
        }
        let text = entry_for_response.text().trim().to_string();
        if !text.is_empty() {
            on_confirm(text);
        }
    });
    dialog.present();
}

/// Lanza `xdg-open` (Linux) para abrir una URL en el navegador del usuario.
fn open_in_browser(url: &str) -> std::io::Result<()> {
    std::process::Command::new("xdg-open")
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Diálogo destructivo de confirmación para eliminar una sesión.
fn show_delete_session_dialog(
    parent: &adw::ApplicationWindow,
    title: &str,
    on_response: impl Fn(bool) + 'static,
) {
    let body = format!("¿Eliminar la sesión «{title}»?\n\nSe borrarán también todos sus mensajes.");
    let dialog = adw::MessageDialog::new(Some(parent), Some("Eliminar sesión"), Some(&body));
    dialog.add_response("cancel", "Cancelar");
    dialog.add_response("delete", "Eliminar");
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.connect_response(None, move |_dialog, response| {
        on_response(response == "delete");
    });
    dialog.present();
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

/// CSS de la app. Solo el estado activo (`:checked`) lleva color; el inactivo
/// queda con el estilo neutro del tema para preservar la pista visual de cuál
/// está seleccionado en el segmented control Build/Plan.
const APP_CSS: &str = "
    .agent-build:checked {
        background-image: none;
        background-color: #1c71d8;
        color: white;
    }
    .agent-plan:checked {
        background-image: none;
        background-color: #daa520;
        color: white;
    }
    .diff-line-addition {
        background-color: #143d2a;
    }
    .diff-line-deletion {
        background-color: #4a1f24;
    }
    .permission-status {
        border-radius: 999px;
        padding: 2px 8px;
        color: white;
        font-weight: 700;
    }
    .permission-approved {
        background-color: #2e7d32;
    }
    .permission-rejected {
        background-color: #b3261e;
    }
    .changes-nav-bar {
        background-color: alpha(@window_bg_color, 0.85);
        border-radius: 12px;
        padding: 2px 4px;
        border: 1px solid alpha(@borders, 0.5);
    }
";

fn install_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(APP_CSS);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let app = RelmApp::new(APP_ID);
    install_css();
    app.run::<App>(());
}
