//! Tokeniza un mensaje Markdown en bloques renderizables por la UI.
//!
//! Los bloques de código se entregan en crudo (con su lenguaje opcional)
//! para que la vista los pinte como widgets propios con botón de copiar.
//! El resto se devuelve como prosa en marcado Pango ya escapado.

use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};
use relm4::gtk::glib;

pub enum Block {
    /// Texto convertido a marcado Pango. Apto para `Label::set_markup`.
    Prose(String),
    /// Bloque de código con su lenguaje (si la cerca lo declaró) y su texto literal.
    Code { lang: Option<String>, text: String },
}

pub fn parse_blocks(markdown: &str) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut prose = String::new();
    let mut code: Option<(Option<String>, String)> = None;

    let flush_prose = |prose: &mut String, out: &mut Vec<Block>| {
        let trimmed = prose.trim_end();
        if !trimmed.is_empty() {
            out.push(Block::Prose(trimmed.to_string()));
        }
        prose.clear();
    };

    for event in Parser::new(markdown) {
        if let Some((_, buf)) = code.as_mut() {
            match event {
                Event::Text(t) => buf.push_str(&t),
                Event::End(TagEnd::CodeBlock) => {
                    let (lang, mut text) = code.take().unwrap();
                    if text.ends_with('\n') {
                        text.pop();
                    }
                    out.push(Block::Code { lang, text });
                }
                _ => {}
            }
            continue;
        }

        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                flush_prose(&mut prose, &mut out);
                let lang = match kind {
                    CodeBlockKind::Fenced(l) if !l.is_empty() => Some(l.into_string()),
                    _ => None,
                };
                code = Some((lang, String::new()));
            }

            Event::Start(Tag::Strong) => prose.push_str("<b>"),
            Event::Start(Tag::Emphasis) => prose.push_str("<i>"),
            Event::Start(Tag::Heading { .. }) => prose.push_str("<b>"),
            Event::Start(Tag::Item) => prose.push_str("• "),

            Event::End(TagEnd::Strong) => prose.push_str("</b>"),
            Event::End(TagEnd::Emphasis) => prose.push_str("</i>"),
            Event::End(TagEnd::Heading(_)) => prose.push_str("</b>\n"),
            Event::End(TagEnd::Paragraph) => prose.push_str("\n\n"),
            Event::End(TagEnd::Item) => prose.push('\n'),

            Event::Text(text) => prose.push_str(glib::markup_escape_text(&text).as_str()),
            Event::Code(code) => {
                prose.push_str("<tt>");
                prose.push_str(glib::markup_escape_text(&code).as_str());
                prose.push_str("</tt>");
            }
            Event::SoftBreak => prose.push(' '),
            Event::HardBreak => prose.push('\n'),

            _ => {}
        }
    }

    flush_prose(&mut prose, &mut out);
    out
}
