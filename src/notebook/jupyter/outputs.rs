//! nbformat cell outputs, decoded.
//!
//! A Jupyter cell's `outputs` is an array of output objects; this module
//! owns their JSON key knowledge (`output_type`, `name`, `text`, `data`,
//! `ename`, `evalue`, `traceback`) and the string-or-lines normalization.
//! Renderers and search format the decoded [`OutputView`] and never spell
//! the keys themselves.

use serde_json::Value;

/// One decoded output object.
pub struct OutputView<'a> {
    raw: &'a Value,
}

/// One decoded MIME part of a `data` bundle: `text/plain` carries its
/// text, every other payload only its byte count.
pub enum MimePart<'a> {
    Plain(String),
    Binary { mime: &'a str, bytes: usize },
}

impl<'a> OutputView<'a> {
    /// Decode one output object.
    pub fn new(output: &'a Value) -> Self {
        Self { raw: output }
    }

    /// The `output_type` as shown: `unknown` when the field is absent.
    pub fn kind(&self) -> &'a str {
        self.raw
            .get("output_type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    }

    /// The MIME keys of the `data` bundle; `None` when there is none.
    pub fn data_mimes(&self) -> Option<Vec<&'a str>> {
        match self.raw.get("data") {
            Some(Value::Object(data)) => Some(data.keys().map(String::as_str).collect()),
            _ => None,
        }
    }

    /// The stream name as stored (`stdout`/`stderr`); `None` for
    /// non-stream outputs and for streams without a name.
    pub fn name(&self) -> Option<&'a str> {
        self.raw.get("name").and_then(Value::as_str)
    }

    /// The stream name the payload view prints: `stdout` when absent.
    pub fn stream_name(&self) -> &'a str {
        self.name().unwrap_or("stdout")
    }

    /// The `text` payload normalized to one string.
    pub fn text(&self) -> String {
        normalized(self.raw.get("text"))
    }

    /// The `text/plain` payload of the `data` bundle, normalized like
    /// [`Self::text`].
    pub fn plain_text(&self) -> String {
        normalized(self.raw.get("data").and_then(|data| data.get("text/plain")))
    }

    /// The identity of an error output: `(ename, evalue)`. `None` for
    /// every other output type.
    pub fn error_identity(&self) -> Option<(String, String)> {
        (self.kind() == "error").then(|| {
            (
                self.field("ename").unwrap_or_default().to_string(),
                self.field("evalue").unwrap_or_default().to_string(),
            )
        })
    }

    /// The traceback lines of an error output (non-string entries
    /// dropped); empty for every other output type.
    pub fn traceback_lines(&self) -> Vec<&'a str> {
        match self.raw.get("traceback") {
            Some(Value::Array(lines)) => lines.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        }
    }

    /// The traceback as the full payload view prints it: the lines joined
    /// with newlines and stripped of ANSI escapes.
    pub fn traceback_display(&self) -> String {
        strip_ansi(&self.traceback_lines().join("\n"))
    }

    /// The traceback as the searchable text carries it: the stored lines
    /// concatenated (they carry no terminators) and ANSI-stripped.
    fn traceback_text(&self) -> String {
        strip_ansi(&normalized(self.raw.get("traceback")))
    }

    /// The MIME parts of the `data` bundle decoded, in the bundle's own
    /// key order.
    pub fn mime_parts(&self) -> Vec<MimePart<'a>> {
        let Some(Value::Object(data)) = self.raw.get("data") else {
            return Vec::new();
        };
        data.iter()
            .map(|(mime, payload)| {
                if mime == "text/plain" {
                    MimePart::Plain(normalized(Some(payload)))
                } else {
                    MimePart::Binary {
                        mime,
                        bytes: payload.as_str().map(str::len).unwrap_or(0),
                    }
                }
            })
            .collect()
    }

    /// The searchable text of the output (stream text, `text/plain`
    /// payloads, or an error's name, value and traceback), so an
    /// output-scope search matches what a reader sees rather than the JSON
    /// encoding.
    pub fn search_text(&self) -> String {
        match self.kind() {
            "stream" => self.text(),
            "error" => {
                let (name, value) = self.error_identity().unwrap_or_default();
                format!("{name}: {value}\n{}", self.traceback_text())
            }
            _ => strip_ansi(&self.plain_text()),
        }
    }

    fn field(&self, key: &str) -> Option<&'a str> {
        self.raw.get(key).and_then(Value::as_str)
    }
}

/// `"text"`-like fields may be a JSON string or an array of lines;
/// normalize to one string. Array entries are concatenated because
/// nbformat lines carry their own terminators.
fn normalized(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(lines)) => lines
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .concat(),
        _ => String::new(),
    }
}

/// Remove ANSI escape sequences from tracebacks so they don't burn tokens.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            // CSI: ESC [ ... final byte in @..~
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(next) = chars.next()
                    && !('@'..='~').contains(&next)
                {}
            }
        } else {
            out.push(character);
        }
    }
    out
}
