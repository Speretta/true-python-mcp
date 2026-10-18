//! Parsing marimo `.py` notebooks into byte-exact segments.
//!
//! A marimo file is ordinary Python: a header (`import marimo`,
//! `__generated_with`, `app = marimo.App(...)`), a sequence of cell blocks
//! (`@app.cell def _(): ...`), and a run guard. We walk the top-level AST and
//! slice the source into literal segments (everything verbatim) and cell
//! segments. An unedited cell serializes from its captured block bytes, so a
//! parsed-then-serialized file is byte-identical to the original.

use ruff_python_ast::{Decorator, Expr, ModModule, Stmt, StmtFunctionDef};
use ruff_python_parser::{Parsed, parse_module};
use ruff_text_size::Ranged;

use super::{CellMeta, Kind, MarimoNotebook, Segment};
use crate::notebook::{Cell, SourceText};

/// Cheap marker check mirroring marimo's own detection: both `import marimo`
/// and `marimo.App` must appear. Structural validation happens in [`parse`].
pub fn is_marimo(content: &str) -> bool {
    content.contains("import marimo") && content.contains("marimo.App")
}

/// Parse a marimo file. Falls back to a line-based scanner when the file has a
/// syntax error, so a single broken cell does not make the whole notebook
/// uneditable (matching `marimo edit --watch`).
pub fn parse(src: &str) -> Result<MarimoNotebook, String> {
    match parse_module(src) {
        Ok(parsed) => parse_structured(src, &parsed),
        Err(_) => super::scan::scan(src),
    }
}

fn parse_structured(src: &str, parsed: &Parsed<ModModule>) -> Result<MarimoNotebook, String> {
    let mut segments = Vec::new();
    let mut cells = Vec::new();
    let mut metas = Vec::new();
    let mut version = None;
    let mut seen_import = false;
    let mut seen_app = false;
    let mut pos = 0usize;

    for stmt in parsed.syntax().body.iter() {
        let start = stmt.range().start().to_usize();
        let end = stmt.range().end().to_usize();
        if start > pos {
            segments.push(Segment::Lit(src[pos..start].to_string()));
        }
        match classify(stmt) {
            Top::AppCell(func) => {
                let (cell, meta) = app_cell(src, func, start, end);
                push_cell(&mut segments, &mut cells, &mut metas, cell, meta);
            }
            Top::Setup => {
                let (cell, meta) = setup_cell(src, stmt, start, end);
                push_cell(&mut segments, &mut cells, &mut metas, cell, meta);
            }
            Top::ReadOnly => {
                let (cell, meta) = read_only_cell(src, stmt, start, end);
                push_cell(&mut segments, &mut cells, &mut metas, cell, meta);
            }
            Top::Literal => {
                inspect_literal(stmt, &mut seen_import, &mut seen_app, &mut version);
                segments.push(Segment::Lit(src[start..end].to_string()));
            }
        }
        pos = end;
    }
    if pos < src.len() {
        segments.push(Segment::Lit(src[pos..].to_string()));
    }
    finish(cells, metas, segments, version, seen_import, seen_app)
}

/// Shared finalization: validate the marimo markers and assemble.
pub(super) fn finish(
    cells: Vec<Cell>,
    metas: Vec<CellMeta>,
    segments: Vec<Segment>,
    version: Option<String>,
    seen_import: bool,
    seen_app: bool,
) -> Result<MarimoNotebook, String> {
    if !seen_import || !seen_app {
        return Err(
            "not a marimo notebook: expected `import marimo` and `app = marimo.App(...)`"
                .to_string(),
        );
    }
    Ok(MarimoNotebook {
        cells,
        metas,
        segments,
        version,
    })
}

pub(super) fn push_cell(
    segments: &mut Vec<Segment>,
    cells: &mut Vec<Cell>,
    metas: &mut Vec<CellMeta>,
    cell: Cell,
    meta: CellMeta,
) {
    cells.push(cell);
    metas.push(meta);
    segments.push(Segment::Cell);
}

pub(super) enum Top<'a> {
    AppCell(&'a StmtFunctionDef),
    Setup,
    ReadOnly,
    Literal,
}

pub(super) fn classify(stmt: &Stmt) -> Top<'_> {
    match stmt {
        Stmt::FunctionDef(func) => match app_decorator_attr(func) {
            Some("cell") => Top::AppCell(func),
            Some("function") => Top::ReadOnly,
            _ => Top::Literal,
        },
        Stmt::ClassDef(class) => match class.decorator_list.iter().find_map(decorator_app_attr) {
            Some("class_definition") => Top::ReadOnly,
            _ => Top::Literal,
        },
        Stmt::With(with) => {
            if is_setup(with) {
                Top::Setup
            } else {
                Top::Literal
            }
        }
        Stmt::Expr(expr) => {
            if is_unparsable_cell(&expr.value) {
                Top::ReadOnly
            } else {
                Top::Literal
            }
        }
        _ => Top::Literal,
    }
}

/// The `app.<attr>` name of a cell's decorator, if any.
fn app_decorator_attr(func: &StmtFunctionDef) -> Option<&str> {
    func.decorator_list.iter().find_map(decorator_app_attr)
}

fn decorator_app_attr(decorator: &Decorator) -> Option<&str> {
    let mut expr = &decorator.expression;
    if let Expr::Call(call) = expr {
        expr = &call.func;
    }
    app_attr(expr)
}

/// Match `<name>.<attr>` where the value is `app`; returns `attr`.
fn app_attr(expr: &Expr) -> Option<&str> {
    if let Expr::Attribute(attribute) = expr
        && let Expr::Name(value) = &*attribute.value
        && value.id.as_str() == "app"
    {
        return Some(attribute.attr.id.as_str());
    }
    None
}

fn is_setup(with: &ruff_python_ast::StmtWith) -> bool {
    with.items.first().is_some_and(|item| {
        let mut expr = &item.context_expr;
        if let Expr::Call(call) = expr {
            expr = &call.func;
        }
        app_attr(expr) == Some("setup")
    })
}

fn is_unparsable_cell(expr: &Expr) -> bool {
    if let Expr::Call(call) = expr {
        return app_attr(&call.func) == Some("_unparsable_cell");
    }
    false
}

/// An `@app.cell` block: capture the verbatim block, the decorator+signature
/// header, the body indent, and the trailing `return` (regenerated by marimo,
/// so stripped from the editable source).
pub(super) fn app_cell(
    src: &str,
    func: &StmtFunctionDef,
    start: usize,
    end: usize,
) -> (Cell, CellMeta) {
    let body = &func.body;
    let first_start = body.first().map_or(end, |s| s.range().start().to_usize());
    let first_line = line_start(src, first_start);
    let indent = src[first_line..first_start].to_string();
    // The AST skips comments, so walk back over any comment lines directly
    // above the first statement to keep them part of the editable body.
    let body_line_start = absorb_leading_comments(src, first_line);

    let (body_end, return_suffix) = if let Some(Stmt::Return(ret)) = body.last() {
        let ret_line = line_start(src, ret.range().start().to_usize());
        (ret_line, Some(src[ret_line..end].to_string()))
    } else {
        (end, None)
    };

    let header = src[start..body_line_start].to_string();
    let raw_body = &src[body_line_start..body_end];
    let (cell_type, source, indent_memory, md) = match super::markdown::detect(src, body, body_line_start)
    {
        Some(detected) => (
            "markdown".to_string(),
            detected.text,
            None,
            Some(super::MdInfo {
                tag: detected.tag,
                comments: detected.comments,
            }),
        ),
        None => {
            let (source, indent_memory) = super::indent::strip_fixed_indent(raw_body, &indent);
            ("code".to_string(), source, indent_memory, None)
        }
    };
    (
        Cell {
            cell_type,
            source: SourceText::Lines(SourceText::split_canonical(&source)),
            extra: Default::default(),
        },
        CellMeta {
            kind: Kind::AppCell,
            block: src[start..end].to_string(),
            header,
            indent,
            indent_memory,
            return_suffix,
            md,
            edited: false,
        },
    )
}

/// Walk backwards from `line_start` over contiguous comment lines, returning
/// the start of the earliest one (so leading comments stay in the body).
fn absorb_leading_comments(src: &str, line_start_offset: usize) -> usize {
    let mut current = line_start_offset;
    while current > 0 {
        let prev = line_start(src, current - 1);
        if prev >= current {
            break;
        }
        let line = src[prev..current].trim_end_matches(['\n', '\r']);
        if line.trim_start().starts_with('#') {
            current = prev;
        } else {
            break;
        }
    }
    current
}

/// A `with app.setup:` block: editable body, no trailing return.
pub(super) fn setup_cell(src: &str, stmt: &Stmt, start: usize, end: usize) -> (Cell, CellMeta) {
    let (body, header_end) = match stmt {
        Stmt::With(with) => {
            let first = with
                .body
                .first()
                .map_or(end, |s| s.range().start().to_usize());
            (&with.body[..], line_start(src, first))
        }
        _ => (&[][..], end),
    };
    let first_start = body.first().map_or(end, |s| s.range().start().to_usize());
    let indent = src[header_end..first_start].to_string();
    let header = src[start..header_end].to_string();
    let (source, indent_memory) = super::indent::strip_fixed_indent(&src[header_end..end], &indent);
    (
        Cell {
            cell_type: "code".to_string(),
            source: SourceText::Lines(SourceText::split_canonical(&source)),
            extra: marimo_extra("setup", None),
        },
        CellMeta {
            kind: Kind::Setup,
            block: src[start..end].to_string(),
            header,
            indent,
            indent_memory,
            return_suffix: None,
            md: None,
            edited: false,
        },
    )
}

/// `@app.function`, `@app.class_definition`, `app._unparsable_cell(...)`:
/// shown verbatim, editing not supported (the whole block is the source).
pub(super) fn read_only_cell(src: &str, stmt: &Stmt, start: usize, end: usize) -> (Cell, CellMeta) {
    let kind = match stmt {
        Stmt::FunctionDef(_) => Kind::Function,
        Stmt::ClassDef(_) => Kind::Class,
        _ => Kind::Unparsable,
    };
    let block = src[start..end].to_string();
    (
        Cell {
            cell_type: "code".to_string(),
            source: SourceText::Lines(SourceText::split_canonical(&block)),
            extra: marimo_extra(kind.label(), None),
        },
        CellMeta {
            kind,
            block,
            header: String::new(),
            indent: String::new(),
            indent_memory: None,
            return_suffix: None,
            md: None,
            edited: false,
        },
    )
}

pub(super) fn inspect_literal(
    stmt: &Stmt,
    seen_import: &mut bool,
    seen_app: &mut bool,
    version: &mut Option<String>,
) {
    match stmt {
        Stmt::Import(import) => {
            if import.names.iter().any(|a| a.name.as_str() == "marimo") {
                *seen_import = true;
            }
        }
        Stmt::Assign(assign) => {
            if let Some(Expr::Name(target)) = assign.targets.first() {
                match target.id.as_str() {
                    "app" => {
                        if let Expr::Call(call) = &*assign.value
                            && let Expr::Attribute(attr) = &*call.func
                            && attr.attr.id.as_str() == "App"
                        {
                            *seen_app = true;
                        }
                    }
                    "__generated_with" => {
                        if let Expr::StringLiteral(literal) = &*assign.value {
                            *version = Some(literal.value.to_str().to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

pub(super) fn marimo_extra(
    kind: &str,
    name: Option<&str>,
) -> std::collections::BTreeMap<String, serde_json::Value> {
    let mut extra = std::collections::BTreeMap::new();
    let mut info = serde_json::Map::new();
    info.insert("kind".into(), serde_json::Value::String(kind.to_string()));
    if let Some(name) = name {
        info.insert("name".into(), serde_json::Value::String(name.to_string()));
    }
    extra.insert("marimo".into(), serde_json::Value::Object(info));
    extra
}

pub(super) fn line_start(src: &str, offset: usize) -> usize {
    src[..offset].rfind('\n').map_or(0, |index| index + 1)
}
