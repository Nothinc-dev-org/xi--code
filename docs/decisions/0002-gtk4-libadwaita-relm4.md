# ADR-0002: UI con GTK4 + libadwaita + Relm4

- **Estado:** aceptado
- **Fecha:** 2026-05-21

## Contexto

El requerimiento fija GTK4 como toolkit de UI. En Rust hay varias formas de
estructurar una app GTK4: usar `gtk4-rs` directamente (imperativo, mucho
boilerplate de señales y estado mutable compartido) o un framework reactivo
encima.

## Decisión

- **`gtk4-rs`** como binding base de GTK4.
- **`libadwaita`** (`libadwaita-rs`) para componentes y estilo modernos del
  ecosistema GNOME (ventanas adaptativas, listas, hojas, temas claro/oscuro).
- **Relm4** como framework de arquitectura de UI: componentes con estado,
  mensajes y actualización al estilo Elm, que encaja bien con un motor dirigido
  por eventos y con el puente async.

## Alternativas consideradas

- **`gtk4-rs` puro** — máxima cercanía a la API, pero gestión de estado tediosa y
  propensa a errores en una UI con mucho estado dinámico (streaming, sesiones).
- **Relm4 (elegida)** — abstracción ergonómica sobre `gtk4-rs`; los componentes
  reciben mensajes, lo que mapea de forma natural con los eventos del motor.
- **Otros toolkits** (Iced, egui, Slint) — descartados: el requerimiento exige
  GTK4 explícitamente.

## Consecuencias

- La UI se organiza en componentes Relm4 que reciben eventos del motor como
  mensajes; el patrón Tokio↔GLib se integra con los `Worker`/comandos de Relm4.
- Dependencia de las librerías de sistema de GTK4/libadwaita en build y empaquetado.
- Estética y comportamiento alineados con GNOME; en otros entornos de escritorio
  sigue funcionando vía GTK.
