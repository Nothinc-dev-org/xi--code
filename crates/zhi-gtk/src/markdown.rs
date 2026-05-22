//! Conversión de Markdown a marcado Pango para renderizar en `gtk::Label`.
//!
//! Pango solo soporta un subconjunto de formato (negrita, cursiva, monoespaciado…),
//! así que mapeamos los elementos comunes y descartamos el resto. El texto se
//! escapa siempre para no romper el marcado.

use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use relm4::gtk::glib;

/// Convierte una cadena Markdown en marcado Pango seguro.
pub fn to_pango(markdown: &str) -> String {
    let mut out = String::new();

    for event in Parser::new(markdown) {
        match event {
            Event::Start(Tag::Strong) => out.push_str("<b>"),
            Event::Start(Tag::Emphasis) => out.push_str("<i>"),
            Event::Start(Tag::Heading { .. }) => out.push_str("<b>"),
            Event::Start(Tag::CodeBlock(_)) => out.push_str("<tt>"),
            Event::Start(Tag::Item) => out.push_str("• "),

            Event::End(TagEnd::Strong) => out.push_str("</b>"),
            Event::End(TagEnd::Emphasis) => out.push_str("</i>"),
            Event::End(TagEnd::Heading(_)) => out.push_str("</b>\n"),
            Event::End(TagEnd::CodeBlock) => out.push_str("</tt>\n"),
            Event::End(TagEnd::Paragraph) => out.push_str("\n\n"),
            Event::End(TagEnd::Item) => out.push('\n'),

            Event::Text(text) => out.push_str(glib::markup_escape_text(&text).as_str()),
            Event::Code(code) => {
                out.push_str("<tt>");
                out.push_str(glib::markup_escape_text(&code).as_str());
                out.push_str("</tt>");
            }
            Event::SoftBreak => out.push(' '),
            Event::HardBreak => out.push('\n'),

            _ => {}
        }
    }

    out.trim_end().to_string()
}
