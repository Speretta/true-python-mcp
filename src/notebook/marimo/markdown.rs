//! Markdown cells in marimo notebooks.
//!
//! A markdown cell is an `@app.cell` block whose body is a single bare
//! `mo.md("...")` call with a constant string (or an f-string without
//! interpolations). We mirror marimo's own compiler and serializer:
//! extraction follows `_extract_markdown` (rstrip lines, dedent, strip) and
//! re-wrapping follows `format_markdown`/`construct_markdown_call` (always
//! `"""` quotes, original prefix tag kept, `{`/`}` doubled for f-strings,
//! `""` escaped).

use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;

/// A detected `mo.md(...)` cell body.
pub(super) struct Detected {
    /// Markdown text as marimo exposes it (dedented, stripped).
    pub text: String,
    /// String prefix tag in source order: the leading (max two) characters
    /// from `r`, `t`, `f` — what marimo's serializer preserves on rewrite.
    pub tag: String,
    /// Comment lines between the signature and the `mo.md(` call; kept
    /// verbatim across edits (marimo preserves everything outside the call).
    pub comments: String,
}

/// Detect a markdown cell from an `@app.cell` function body (mirrors
/// `_extract_markdown`: a single bare `mo.md(const-string)` expression, with
/// an optional trailing `return`).
pub(super) fn detect(src: &str, body: &[Stmt], body_start: usize) -> Option<Detected> {
    let statements: Vec<&Stmt> = body
        .iter()
        .take_while(|stmt| !matches!(stmt, Stmt::Return(_)))
        .collect();
    if statements.len() != 1 || body.len() > statements.len() + 1 {
        return None;
    }
    let Stmt::Expr(expr) = statements[0] else {
        return None;
    };
    let Expr::Call(call) = &*expr.value else {
        return None;
    };
    // marimo requires the callee to be literally `mo.md`.
    let Expr::Attribute(attribute) = &*call.func else {
        return None;
    };
    if attribute.attr.as_str() != "md" {
        return None;
    }
    let Expr::Name(name) = &*attribute.value else {
        return None;
    };
    if name.id.as_str() != "mo" {
        return None;
    }
    // Exactly one positional argument that is a constant string; an f-string
    // counts only when it has a single literal element (no interpolations).
    if call.arguments.args.len() != 1 {
        return None;
    }
    let argument = &call.arguments.args[0];
    let value = match argument {
        Expr::StringLiteral(literal) => literal.value.to_str().to_string(),
        // CPython folds the argument to a JoinedStr; marimo accepts it only
        // when it holds a single constant element. That maps to exactly one
        // f-string part (no implicit concatenation) with one literal element.
        Expr::FString(fstring) => {
            let parts: Vec<_> = fstring.value.iter().collect();
            match parts.as_slice() {
                [ruff_python_ast::FStringPartRef::FString(part)] => match &part.elements[..] {
                    [ruff_python_ast::InterpolatedStringElement::Literal(literal)] => {
                        literal.value.to_string()
                    }
                    _ => return None,
                },
                _ => return None,
            }
        }
        _ => return None,
    };
    let expr_start = statements[0].range().start().to_usize();
    let comments = src[body_start..super::parse::line_start(src, expr_start)].to_string();
    Some(Detected {
        text: extract(&value),
        tag: tag_from_source(src, argument),
        comments,
    })
}

/// The prefix tag marimo's serializer keeps: up to two leading characters
/// from `r`/`t`/`f` (case preserved), stopping at the first other character.
fn tag_from_source(src: &str, argument: &Expr) -> String {
    let start = argument.range().start().to_usize();
    let mut tag = String::new();
    for character in src[start..].chars() {
        if tag.chars().count() == 2 || !matches!(character, 'r' | 'R' | 't' | 'T' | 'f' | 'F') {
            break;
        }
        tag.push(character);
    }
    tag
}

/// marimo's `_extract_markdown` post-processing: rstrip every line, dedent
/// the first line separately from the rest, join with a newline, strip.
pub(super) fn extract(value: &str) -> String {
    let lines: Vec<&str> = value.split('\n').map(str::trim_end).collect();
    let first = dedent_common_margin(lines[0]);
    let rest = dedent_common_margin(&lines[1..].join("\n"));
    format!("{first}\n{rest}").trim().to_string()
}

/// `textwrap.dedent`: whitespace-only lines become empty and are ignored for
/// the margin; the margin is the longest common leading-whitespace prefix of
/// the remaining lines (compared literally, so mixed tabs/spaces cancel).
fn dedent_common_margin(text: &str) -> String {
    let lines: Vec<&str> = text
        .split('\n')
        .map(|line| if line.trim().is_empty() { "" } else { line })
        .collect();
    let margin = lines
        .iter()
        .filter(|line| !line.is_empty())
        .map(|line| &line[..line.len() - line.trim_start().len()])
        .fold(None::<&str>, |margin, indent| match margin {
            None => Some(indent),
            Some(margin) if indent.starts_with(margin) => Some(margin),
            Some(margin) if margin.starts_with(indent) => Some(indent),
            Some(_) => Some(""),
        })
        .unwrap_or("");
    if margin.is_empty() {
        return lines.join("\n");
    }
    lines
        .iter()
        .map(|line| line.strip_prefix(margin).unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Re-wrap markdown content as a `mo.md` call body, mirroring marimo's
/// `format_markdown` + `construct_markdown_call` + body indentation:
/// braces are doubled for f-strings, `""` is escaped, the quote is always
/// `"""`, the tag is preserved, and every non-blank line gets the body
/// indent. Returns the block tail: body + trailing return.
pub(super) fn wrap_call(content: &str, indent: &str, tag: &str) -> String {
    let mut text = content.trim().to_string();
    if tag.contains('f') || tag.contains('F') {
        text = text.replace('{', "{{").replace('}', "}}");
    }
    text = text.replace("\"\"", "\"\\\"");
    let call = format!("mo.md({tag}\"\"\"\n{text}\n\"\"\")");
    super::indent_lines(&call, indent)
}
