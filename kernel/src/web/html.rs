//! A forgiving HTML reader. It turns a page into a flat list of items
//! (styled text, line breaks, rules, form fields) that the layout code
//! wraps into lines. There is no CSS or JavaScript; tags decide the look.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
    pub underline: bool,
    /// 1 to 6 for <h1> to <h6>, 0 for normal text.
    pub heading: u8,
    /// Index into `Document::links`.
    pub link: Option<u32>,
    /// Smaller grey text (<small>, image captions).
    pub faint: bool,
}

#[derive(Clone, Debug)]
pub enum Item {
    Text(String, Style),
    /// End the current line.
    Newline,
    /// End the current line and leave a gap of about this many
    /// sixteenths of a line (paragraphs, headings).
    Block(u8),
    /// Left margin for following lines, in levels (lists, quotes).
    Indent(u8),
    Center(bool),
    Rule,
    /// A form field the user can type into: form, field index.
    Input(u32, u32),
    /// A submit button: form, label.
    Button(u32, String),
}

#[derive(Clone, Debug)]
pub struct Field {
    pub name: String,
    pub value: String,
    pub placeholder: String,
}

#[derive(Clone, Debug, Default)]
pub struct Form {
    pub action: String,
    pub post: bool,
    pub fields: Vec<Field>,
}

#[derive(Default)]
pub struct Document {
    pub title: String,
    pub items: Vec<Item>,
    pub links: Vec<String>,
    pub forms: Vec<Form>,
    /// From <base href>, if the page sets one.
    pub base: Option<String>,
}

/// Elements whose content is never shown.
const HIDDEN: &[&str] = &[
    "head", "script", "style", "template", "svg", "math", "iframe", "object", "canvas", "select",
    "datalist", "button", "video", "audio", "map", "dialog", "textarea",
];
/// Elements with raw text content (no tags inside).
const RAW: &[&str] = &["script", "style", "title", "textarea", "xmp"];
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

#[derive(Clone)]
struct Open {
    tag: String,
    style: Style,
    hidden: bool,
    pre: bool,
    indent: u8,
    center: bool,
    /// Ordered list counter.
    counter: Option<u32>,
}

struct Builder {
    doc: Document,
    stack: Vec<Open>,
    /// The last text ended with a space (or we are at a line start).
    space: bool,
    at_line_start: bool,
    form: Option<u32>,
    indent: u8,
    center: bool,
    in_title: bool,
    button_label: Option<String>,
}

pub fn parse(bytes: &[u8], content_type: &str) -> Document {
    let text = decode(bytes, content_type);
    if content_type.starts_with("text/plain") {
        let mut doc = Document::default();
        for line in text.lines() {
            doc.items.push(Item::Text(
                line.to_string(),
                Style {
                    mono: true,
                    ..Style::default()
                },
            ));
            doc.items.push(Item::Newline);
        }
        return doc;
    }
    let mut b = Builder {
        doc: Document::default(),
        stack: Vec::new(),
        space: true,
        at_line_start: true,
        form: None,
        indent: 0,
        center: false,
        in_title: false,
        button_label: None,
    };
    b.run(&text);
    let title = collapse(&b.doc.title);
    b.doc.title = title;
    b.doc
}

impl Builder {
    fn run(&mut self, s: &str) {
        let bytes = s.as_bytes();
        let mut i = 0;
        let mut text_start = 0;
        while i < bytes.len() {
            if bytes[i] != b'<' {
                i += 1;
                continue;
            }
            let rest = &s[i..];
            let next = bytes.get(i + 1).copied().unwrap_or(0);
            if rest.starts_with("<!--") {
                self.text(&s[text_start..i]);
                i = match rest.find("-->") {
                    Some(e) => i + e + 3,
                    None => bytes.len(),
                };
                text_start = i;
            } else if next == b'!' || next == b'?' {
                self.text(&s[text_start..i]);
                i = match rest.find('>') {
                    Some(e) => i + e + 1,
                    None => bytes.len(),
                };
                text_start = i;
            } else if next == b'/' || next.is_ascii_alphabetic() {
                self.text(&s[text_start..i]);
                let (end, tag) = read_tag(s, i);
                i = end;
                text_start = i;
                if tag.closing {
                    self.end_tag(&tag.name);
                } else {
                    let raw = RAW.contains(&tag.name.as_str());
                    let name = tag.name.clone();
                    self.start_tag(tag);
                    if raw {
                        // everything up to the matching end tag is text
                        let close = find_ci(&s[i..], &alloc::format!("</{}", name));
                        let end = close.map_or(bytes.len(), |c| i + c);
                        let content = &s[i..end];
                        match name.as_str() {
                            "title" => {
                                if self.doc.title.is_empty() {
                                    self.doc.title = decode_entities(content);
                                }
                            }
                            "xmp" => self.text(content),
                            _ => {}
                        }
                        i = end;
                        text_start = i;
                    }
                }
            } else {
                i += 1;
            }
        }
        self.text(&s[text_start..]);
    }

    fn top(&self) -> Option<&Open> {
        self.stack.last()
    }

    fn style(&self) -> Style {
        self.top().map_or(Style::default(), |o| o.style)
    }

    fn hidden(&self) -> bool {
        self.top().is_some_and(|o| o.hidden)
    }

    fn pre(&self) -> bool {
        self.top().is_some_and(|o| o.pre)
    }

    fn push(&mut self, item: Item) {
        self.doc.items.push(item);
    }

    fn text(&mut self, raw: &str) {
        if raw.is_empty() || self.hidden() || self.in_title {
            return;
        }
        let text = decode_entities(raw);
        if let Some(label) = &mut self.button_label {
            label.push_str(&text);
            return;
        }
        let style = self.style();
        if self.pre() {
            // a newline right after <pre> is not shown
            let text = match self.doc.items.last() {
                Some(Item::Block(_)) | Some(Item::Indent(_)) | Some(Item::Center(_)) => text
                    .strip_prefix("\r")
                    .unwrap_or(&text)
                    .strip_prefix('\n')
                    .unwrap_or(&text)
                    .to_string(),
                _ => text,
            };
            let mut first = true;
            for line in text.split('\n') {
                if !first {
                    self.push(Item::Newline);
                }
                first = false;
                let line = line.replace('\t', "    ").replace('\r', "");
                if !line.is_empty() {
                    self.push(Item::Text(line, style));
                    self.at_line_start = false;
                }
            }
            self.space = false;
            return;
        }
        let mut out = String::new();
        for c in text.chars() {
            if c.is_whitespace() && c != '\u{a0}' {
                if !self.space {
                    out.push(' ');
                    self.space = true;
                }
            } else {
                out.push(c);
                self.space = false;
            }
        }
        if out.is_empty() {
            return;
        }
        if self.at_line_start {
            let trimmed = out.trim_start_matches(' ');
            if trimmed.is_empty() {
                return;
            }
            out = trimmed.to_string();
        }
        self.at_line_start = false;
        // merge with the previous run of the same style
        if let Some(Item::Text(prev, prev_style)) = self.doc.items.last_mut() {
            if *prev_style == style {
                prev.push_str(&out);
                return;
            }
        }
        self.push(Item::Text(out, style));
    }

    fn block(&mut self, gap: u8) {
        if self.hidden() {
            return;
        }
        // look past layout switches for the last line break
        let mut last = self.doc.items.len();
        while last > 0 && matches!(self.doc.items[last - 1], Item::Indent(_) | Item::Center(_)) {
            last -= 1;
        }
        if last < self.doc.items.len() {
            match self.doc.items.get_mut(last.wrapping_sub(1)) {
                Some(Item::Block(g)) => *g = (*g).max(gap),
                Some(Item::Newline) | None => {}
                Some(_) => {
                    let item = if gap > 0 {
                        Item::Block(gap)
                    } else {
                        Item::Newline
                    };
                    self.doc.items.insert(last, item);
                }
            }
            self.space = true;
            self.at_line_start = true;
            return;
        }
        match self.doc.items.last_mut() {
            Some(Item::Block(g)) => *g = (*g).max(gap),
            Some(Item::Newline) if gap > 0 => {
                self.doc.items.pop();
                self.push(Item::Block(gap));
            }
            Some(Item::Newline) => {}
            None => {}
            _ => self.push(if gap > 0 {
                Item::Block(gap)
            } else {
                Item::Newline
            }),
        }
        self.space = true;
        self.at_line_start = true;
    }

    fn newline(&mut self) {
        if self.hidden() {
            return;
        }
        self.push(Item::Newline);
        self.space = true;
        self.at_line_start = true;
    }

    fn set_layout(&mut self) {
        let indent = self.top().map_or(0, |o| o.indent);
        let center = self.top().is_some_and(|o| o.center);
        if indent != self.indent {
            self.indent = indent;
            self.push(Item::Indent(indent));
        }
        if center != self.center {
            self.center = center;
            self.push(Item::Center(center));
        }
    }

    fn start_tag(&mut self, tag: Tag) {
        let name = tag.name.as_str();
        let parent = self.top().cloned().unwrap_or(Open {
            tag: String::new(),
            style: Style::default(),
            hidden: false,
            pre: false,
            indent: 0,
            center: false,
            counter: None,
        });
        let mut open = Open {
            tag: tag.name.clone(),
            counter: None,
            ..parent.clone()
        };

        // implied end tags
        if matches!(
            name,
            "p" | "li" | "dt" | "dd" | "tr" | "td" | "th" | "option"
        ) {
            let closes: &[&str] = match name {
                "li" => &["li"],
                "dt" | "dd" => &["dt", "dd"],
                "tr" => &["tr", "td", "th"],
                "td" | "th" => &["td", "th"],
                "option" => &["option"],
                _ => &["p"],
            };
            if self.top().is_some_and(|o| closes.contains(&o.tag.as_str())) {
                let top = self.top().unwrap().tag.clone();
                self.end_tag(&top);
            }
        }

        let hidden_attr = tag.attr("hidden").is_some()
            || tag.attr("aria-hidden") == Some("true")
            || tag.attr("style").is_some_and(|s| {
                let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
                let s = s.to_ascii_lowercase();
                s.contains("display:none") || s.contains("visibility:hidden")
            });
        if HIDDEN.contains(&name) || hidden_attr {
            open.hidden = true;
        }

        match name {
            "b" | "strong" => open.style.bold = true,
            "i" | "em" | "cite" | "var" | "dfn" => open.style.italic = true,
            "u" | "ins" => open.style.underline = true,
            "code" | "kbd" | "samp" | "tt" => open.style.mono = true,
            "small" | "figcaption" | "sub" | "sup" => open.style.faint = true,
            "center" => open.center = true,
            "pre" | "listing" | "plaintext" | "xmp" => {
                open.pre = true;
                open.style.mono = true;
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                open.style.heading = name.as_bytes()[1] - b'0';
                open.style.bold = true;
            }
            "ul" | "ol" | "menu" | "dir" => {
                open.indent = parent.indent.saturating_add(1).min(8);
                if name == "ol" {
                    open.counter =
                        Some(tag.attr("start").and_then(|s| s.parse().ok()).unwrap_or(1));
                }
            }
            "blockquote" | "dd" => open.indent = parent.indent.saturating_add(1).min(8),
            "a" => {
                if let Some(href) = tag.attr("href") {
                    let href = decode_entities(href);
                    open.style.link = Some(self.doc.links.len() as u32);
                    self.doc.links.push(href);
                }
            }
            "base" => {
                if let Some(href) = tag.attr("href") {
                    self.doc.base = Some(decode_entities(href));
                }
            }
            "title" => {}
            "form" => {
                self.form = Some(self.doc.forms.len() as u32);
                self.doc.forms.push(Form {
                    action: decode_entities(tag.attr("action").unwrap_or("")),
                    post: tag
                        .attr("method")
                        .is_some_and(|m| m.eq_ignore_ascii_case("post")),
                    fields: Vec::new(),
                });
            }
            _ => {}
        }
        if let Some(align) = tag.attr("align") {
            if align.eq_ignore_ascii_case("center") && is_block(name) {
                open.center = true;
            }
        }

        let hidden = self.hidden();
        if !VOID.contains(&name) {
            self.stack.push(open);
            if self.stack.len() > 512 {
                self.stack.remove(0);
            }
        }
        if hidden {
            if name == "title" {
                self.in_title = false;
            }
            return;
        }

        // what the tag puts on the page
        match name {
            "br" => self.newline(),
            "hr" => {
                self.block(0);
                self.push(Item::Rule);
                self.at_line_start = true;
            }
            "p" | "blockquote" | "pre" | "table" | "ul" | "ol" | "dl" | "figure" | "form"
            | "fieldset" | "address" | "details" => {
                self.block(8);
                self.set_layout();
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.block(12);
                self.set_layout();
            }
            "li" => {
                self.block(0);
                self.set_layout();
                let parent_counter = self
                    .stack
                    .iter_mut()
                    .rev()
                    .find(|o| matches!(o.tag.as_str(), "ul" | "ol" | "menu" | "dir"))
                    .map(|o| {
                        let c = o.counter;
                        if let Some(n) = &mut o.counter {
                            *n += 1;
                        }
                        c
                    });
                let marker = match parent_counter {
                    Some(Some(n)) => alloc::format!("{}. ", n),
                    _ => String::from("• "),
                };
                let style = Style {
                    link: None,
                    ..self.style()
                };
                self.push(Item::Text(marker, style));
                self.at_line_start = false;
                self.space = true;
            }
            "img" => {
                let alt = tag.attr("alt").map(decode_entities).unwrap_or_default();
                let alt = collapse(&alt);
                if !alt.is_empty() {
                    let style = Style {
                        faint: true,
                        ..self.style()
                    };
                    let text = alloc::format!("[{}]", alt);
                    if !self.space && !self.at_line_start {
                        self.text(" ");
                    }
                    self.push(Item::Text(text, style));
                    self.at_line_start = false;
                    self.space = false;
                }
            }
            "td" | "th" => {
                if !self.space && !self.at_line_start {
                    self.push(Item::Text(String::from("  "), self.style()));
                    self.space = true;
                }
            }
            "input" => self.input(&tag),
            "button" => {
                // the label is collected until </button>
                self.button_label = Some(String::new());
                if let Some(top) = self.stack.last_mut() {
                    top.hidden = false;
                }
            }
            "title" => self.in_title = true,
            _ if is_block(name) => {
                self.block(0);
                self.set_layout();
            }
            _ => {}
        }
    }

    fn input(&mut self, tag: &Tag) {
        let Some(form) = self.form else {
            return;
        };
        let kind = tag.attr("type").unwrap_or("text").to_ascii_lowercase();
        let name = decode_entities(tag.attr("name").unwrap_or(""));
        let value = decode_entities(tag.attr("value").unwrap_or(""));
        let fields = &mut self.doc.forms[form as usize].fields;
        match kind.as_str() {
            "hidden" => fields.push(Field {
                name,
                value,
                placeholder: String::new(),
            }),
            "text" | "search" | "email" | "url" | "number" | "tel" | "password" => {
                let index = fields.len() as u32;
                fields.push(Field {
                    name,
                    value,
                    placeholder: decode_entities(tag.attr("placeholder").unwrap_or("")),
                });
                self.push(Item::Input(form, index));
                self.at_line_start = false;
                self.space = false;
            }
            "submit" | "image" => {
                let label = if value.is_empty() {
                    String::from("Submit")
                } else {
                    value
                };
                self.push(Item::Button(form, label));
                self.at_line_start = false;
                self.space = false;
            }
            "checkbox" | "radio" if tag.attr("checked").is_some() => fields.push(Field {
                name,
                value: if value.is_empty() {
                    String::from("on")
                } else {
                    value
                },
                placeholder: String::new(),
            }),
            _ => {}
        }
    }

    fn end_tag(&mut self, name: &str) {
        let Some(pos) = self.stack.iter().rposition(|o| o.tag == name) else {
            if name == "br" {
                self.newline();
            } else if name == "p" {
                self.block(8);
            }
            return;
        };
        let was_hidden = self.stack[pos].hidden;
        let parent_hidden = pos > 0 && self.stack[pos - 1].hidden;
        self.stack.truncate(pos);
        if name == "title" {
            self.in_title = false;
        }
        if name == "form" {
            self.form = None;
        }
        if name == "button" {
            if let Some(label) = self.button_label.take() {
                if let Some(form) = self.form {
                    if !parent_hidden {
                        let label = collapse(&label);
                        let label = if label.is_empty() {
                            String::from("Submit")
                        } else {
                            label
                        };
                        self.push(Item::Button(form, label));
                        self.at_line_start = false;
                        self.space = false;
                    }
                }
            }
            return;
        }
        if was_hidden || self.hidden() {
            return;
        }
        match name {
            "p" | "blockquote" | "pre" | "table" | "ul" | "ol" | "dl" | "figure" | "form"
            | "fieldset" | "address" | "details" => {
                self.block(8);
                self.set_layout();
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.block(8);
                self.set_layout();
            }
            _ if is_block(name) => {
                self.block(0);
                self.set_layout();
            }
            _ => {}
        }
    }
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "ul"
            | "ol"
            | "li"
            | "table"
            | "tr"
            | "blockquote"
            | "pre"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "nav"
            | "main"
            | "aside"
            | "form"
            | "dl"
            | "dt"
            | "dd"
            | "figure"
            | "figcaption"
            | "address"
            | "center"
            | "details"
            | "summary"
            | "fieldset"
            | "legend"
            | "body"
            | "caption"
            | "tbody"
            | "thead"
            | "tfoot"
            | "hgroup"
            | "menu"
            | "dir"
            | "noscript"
    )
}

struct Tag {
    name: String,
    closing: bool,
    attrs: Vec<(String, String)>,
}

impl Tag {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Read a tag starting at `<`. Returns the index after `>` and the tag.
fn read_tag(s: &str, start: usize) -> (usize, Tag) {
    let b = s.as_bytes();
    let mut i = start + 1;
    let closing = b.get(i) == Some(&b'/');
    if closing {
        i += 1;
    }
    let name_start = i;
    while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' && b[i] != b'/' {
        i += 1;
    }
    let name = s[name_start..i].to_ascii_lowercase();
    let mut attrs = Vec::new();
    loop {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        if b[i] == b'>' {
            i += 1;
            break;
        }
        let an_start = i;
        while i < b.len()
            && !b[i].is_ascii_whitespace()
            && b[i] != b'='
            && b[i] != b'>'
            && b[i] != b'/'
        {
            i += 1;
        }
        let attr_name = s[an_start..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let quote = b[i];
                i += 1;
                let v_start = i;
                while i < b.len() && b[i] != quote {
                    i += 1;
                }
                value = s[v_start..i].to_string();
                i = (i + 1).min(b.len());
            } else {
                let v_start = i;
                while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                    i += 1;
                }
                value = s[v_start..i].to_string();
            }
        }
        if attr_name.is_empty() {
            i += 1;
            continue;
        }
        attrs.push((attr_name, value));
    }
    // keep on a char boundary (attribute values may hold UTF-8)
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    (
        i,
        Tag {
            name,
            closing,
            attrs,
        },
    )
}

fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    let h = hay.as_bytes();
    let n = needle.as_bytes();
    if n.len() > h.len() {
        return None;
    }
    (0..=h.len() - n.len()).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// Collapse runs of whitespace and trim.
pub fn collapse(s: &str) -> String {
    let mut out = String::new();
    for word in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Bytes to text: UTF-8, or Windows-1251 / Latin-1 when the page says so.
fn decode(bytes: &[u8], content_type: &str) -> String {
    let mut charset = content_type.split("charset=").nth(1).map(|c| {
        c.trim_matches(|c: char| c == '"' || c == ';' || c.is_whitespace())
            .to_ascii_lowercase()
    });
    if charset.is_none() {
        // look for <meta charset> near the start
        let head = &bytes[..bytes.len().min(4096)];
        let head = String::from_utf8_lossy(head).to_ascii_lowercase();
        if let Some(pos) = head.find("charset=") {
            let rest = &head[pos + 8..];
            let rest = rest.trim_start_matches(['"', '\'']);
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                .unwrap_or(rest.len());
            charset = Some(rest[..end].to_string());
        }
    }
    match charset.as_deref() {
        Some("windows-1251") | Some("cp1251") => bytes.iter().map(|&b| cp1251(b)).collect(),
        Some("iso-8859-1") | Some("latin1") | Some("windows-1252") => {
            bytes.iter().map(|&b| b as char).collect()
        }
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn cp1251(b: u8) -> char {
    match b {
        0..=0x7f => b as char,
        0xc0..=0xff => char::from_u32(0x410 + (b - 0xc0) as u32).unwrap_or('?'),
        0xa8 => 'Ё',
        0xb8 => 'ё',
        0xa0 => '\u{a0}',
        0xab => '«',
        0xbb => '»',
        0x96 => '–',
        0x97 => '—',
        0x85 => '…',
        0x93 => '“',
        0x94 => '”',
        0xb9 => '№',
        _ => '?',
    }
}

/// Replace `&amp;`-style character references.
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos..];
        let end = rest[1..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '#'))
            .map(|e| e + 1)
            .unwrap_or(rest.len());
        let name = &rest[1..end];
        let decoded = if let Some(num) = name.strip_prefix('#') {
            let code = if let Some(hex) = num.strip_prefix(['x', 'X']) {
                u32::from_str_radix(hex, 16).ok()
            } else {
                num.parse().ok()
            };
            code.and_then(char::from_u32)
        } else {
            named_entity(name)
        };
        match decoded {
            Some(c) if end > 1 => {
                out.push(c);
                rest = &rest[end..];
                if rest.starts_with(';') {
                    rest = &rest[1..];
                }
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn named_entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" | "AMP" => '&',
        "lt" | "LT" => '<',
        "gt" | "GT" => '>',
        "quot" | "QUOT" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "laquo" => '«',
        "raquo" => '»',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "bdquo" => '„',
        "middot" => '·',
        "bull" => '•',
        "times" => '×',
        "divide" => '÷',
        "deg" => '°',
        "plusmn" => '±',
        "sect" => '§',
        "para" => '¶',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "cent" => '¢',
        "larr" => '←',
        "rarr" => '→',
        "uarr" => '↑',
        "darr" => '↓',
        "shy" => '\u{ad}',
        "zwnj" | "zwj" | "lrm" | "rlm" => '\u{200b}',
        "thinsp" | "ensp" | "emsp" => ' ',
        "iexcl" => '¡',
        "iquest" => '¿',
        "frac12" => '½',
        "frac14" => '¼',
        "eacute" => 'é',
        "egrave" => 'è',
        "aacute" => 'á',
        "agrave" => 'à',
        "ouml" => 'ö',
        "uuml" => 'ü',
        "auml" => 'ä',
        "szlig" => 'ß',
        "ccedil" => 'ç',
        "ntilde" => 'ñ',
        "check" => '✓',
        _ => return None,
    })
}
