//! Conversion of marimo notebooks: what converting one to another format
//! leaves behind.

use super::{MarimoNotebook, Segment};

impl MarimoNotebook {
    /// What converting this file into another format leaves behind: literal
    /// text outside cells (module comments, PEP 723 dependency blocks, stray
    /// code) and `marimo.App(...)` configuration. One warning per category,
    /// only when present, so a conversion never drops them silently.
    pub(crate) fn conversion_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        let mut stray_example: Option<String> = None;
        for segment in &self.segments {
            let Segment::Lit(text) = segment else {
                continue;
            };
            if let Some(configuration) = app_configuration(text) {
                warnings.push(format!(
                    "marimo.App configuration ({configuration}) is not carried over"
                ));
            }
            for line in text.lines() {
                if !line.trim().is_empty() && !is_scaffold_line(line) {
                    stray_example.get_or_insert_with(|| truncate_example(line));
                }
            }
        }
        if let Some(example) = stray_example {
            warnings.push(format!(
                "top-level lines outside cells (e.g. `{example}`) are not carried over"
            ));
        }
        warnings
    }
}

/// The arguments of a `marimo.App(...)` call in a literal segment, when it
/// carries any configuration.
fn app_configuration(text: &str) -> Option<String> {
    let after = text.split("marimo.App(").nth(1)?;
    let arguments = after.split(')').next().unwrap_or("").trim();
    (!arguments.is_empty()).then(|| arguments.to_string())
}

/// Lines of the standard file scaffold: the imports, the version stamp, the
/// app declaration, the run guard and the PEP 723 header marimo writes above
/// the import. Everything else in a literal segment is user content a
/// conversion would leave behind.
fn is_scaffold_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed == "import marimo"
        || trimmed.starts_with("__generated_with")
        || trimmed.starts_with("app = marimo.App")
        || trimmed.starts_with("if __name__")
        || trimmed == "app.run()"
        // A PEP 723 inline-metadata block. The closing `# ///` is also the
        // prefix of the opening `# /// script`, so match the opening form
        // first; `# requires-python =` and `# dependencies =` cover the
        // single-line key/value shape this project emits.
        || trimmed == "# /// script"
        || trimmed == "# ///"
        || trimmed.starts_with("# requires-python =")
        || trimmed.starts_with("# dependencies =")
}

/// One literal line shortened for a warning message (byte-safe).
fn truncate_example(line: &str) -> String {
    let trimmed = line.trim();
    let end = trimmed.floor_char_boundary(60);
    if end < trimmed.len() {
        format!("{}…", &trimmed[..end])
    } else {
        trimmed.to_string()
    }
}
