//! Signature rendering: split `call_signature_details` output into structured
//! [`DocParam`]s and extract the rendered return type from the signature label.

use ty_python_semantic::types::ProgramEnvironment;
use ty_python_semantic::types::ide_support::{CallSignatureDetails, CallSignatureParameter};

use crate::ty::TyDb;

use super::DocParam;

/// Renders the callable output: one signature per overload and the structured
/// return type/parameters off the first overload.
pub(super) fn callable_output<'db>(
    db: &'db TyDb,
    env: &ProgramEnvironment<'db>,
    details: &[CallSignatureDetails<'db>],
    resolved_name: &str,
) -> (Vec<String>, Option<String>, Vec<DocParam>) {
    let mut signatures = Vec::with_capacity(details.len());
    let mut return_type = None;
    let mut parameters = Vec::new();
    for (index, detail) in details.iter().enumerate() {
        signatures.push(format!("def {resolved_name}{}", detail.label));
        if index == 0 {
            return_type = return_type_of(&detail.label);
            parameters = doc_params(db, env, detail);
        }
    }
    (signatures, return_type, parameters)
}

/// Renders the return type off a signature label (`(…) -> str` → `str`).
fn return_type_of(label: &str) -> Option<String> {
    // why: ty keeps the structured return type private (`Signature::return_ty`
    // is `pub(crate)`), so the rendered `label` is the only public source. The
    // string-aware scan keeps a `Literal["-> "]` annotation from matching the
    // arrow, but it is still a parse of a rendered string, not the type itself.
    top_level_match(label, b"-> ").map(|index| label[index + 3..].trim().to_string())
}

/// Splits a rendered signature's parameters into [`DocParam`]s, classifying
/// each parameter's kind from the variadic/positional flags and its position
/// relative to the keyword-only `*` marker in the label.
fn doc_params<'db>(
    db: &'db TyDb,
    env: &ProgramEnvironment<'db>,
    details: &CallSignatureDetails<'db>,
) -> Vec<DocParam> {
    let label = &details.label;
    let bare_star = find_bare_star(label);
    let mut after_variadic = false;
    let mut search_from = 0usize;
    details
        .parameters
        .iter()
        .map(|param| {
            let position = label[search_from..]
                .find(&param.label)
                .map(|offset| search_from + offset);
            if let Some(position) = position {
                search_from = position + param.label.len();
            }
            let after_star =
                position.is_some_and(|position| bare_star.is_some_and(|star| position > star));
            let kind = param_kind(param, after_variadic, after_star);
            after_variadic |= param.is_variadic;
            DocParam {
                name: param.name.trim_start_matches('*').to_string(),
                type_: param.ty.display(db, env).to_string(),
                kind,
                default: param_default(&param.label),
            }
        })
        .collect()
}

/// The byte offset of the bare `*` keyword-only separator in a signature label
/// (`*` immediately followed by `,`), distinguishing it from `*args` (followed
/// by the name) and `**kwargs` (followed by another `*`).
fn find_bare_star(label: &str) -> Option<usize> {
    // why: `CallSignatureParameter` has no `is_keyword_only` flag, so the bare
    // `*` separator (only present in the rendered label) is the sole signal
    // for keyword-only vs positional-or-keyword.
    let bytes = label.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'*' && bytes.get(index + 1) == Some(&b',') {
            return Some(index);
        }
    }
    None
}

/// Classifies one parameter's kind from ty's flags plus the inferred position.
/// The positional-only/variadic flags come straight off the structured
/// [`CallSignatureParameter`]; only `keyword_only` (vs `positional_or_keyword`)
/// needs inference, because ty does not expose an `is_keyword_only` flag. The
/// position relative to the `*` separator / `*args` is the only signal.
fn param_kind(
    param: &CallSignatureParameter<'_>,
    after_variadic: bool,
    after_star: bool,
) -> &'static str {
    if param.is_variadic {
        "variadic"
    } else if param.is_keyword_variadic {
        "keyword_variadic"
    } else if param.is_positional_only {
        "positional_only"
    } else if after_variadic || after_star {
        "keyword_only"
    } else {
        "positional_or_keyword"
    }
}

/// The rendered default off a parameter label (`skipkeys: bool = False` →
/// `False`).
fn param_default(label: &str) -> Option<String> {
    // why: the default *literal* is not exposed on `CallSignatureParameter`
    // (it carries only the rendered `label`, `name`, `ty` and the variadic /
    // positional-only flags), so the `=` split is the honest fallback. Scanning
    // for the first top-level `=` keeps `x: Literal["a=b"] = "a=b"` from
    // splitting at the `=` inside the annotation.
    top_level_match(label, b"=").map(|index| label[index + 1..].trim().to_string())
}

/// The byte offset of the first occurrence of `needle` at bracket depth 0 and
/// outside a string literal. Shared by [`return_type_of`] and [`param_default`],
/// the two places a rendered signature label is split; a string-aware scan is
/// the honest fallback for values ty keeps private. Single- and double-quoted
/// strings with `\` escapes are tracked (triple-quoted literals do not occur in
/// rendered signatures).
fn top_level_match(label: &str, needle: &[u8]) -> Option<usize> {
    let bytes = label.as_bytes();
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            // Skip the escaped character after a backslash, so `\"` inside a
            // string does not read as the closing quote.
            Some(_) if byte == b'\\' => index += 1,
            Some(q) if byte == q => quote = None,
            Some(_) => {}
            None => match byte {
                b'\'' | b'"' => quote = Some(byte),
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                _ if depth == 0 && bytes[index..].starts_with(needle) => return Some(index),
                _ => {}
            },
        }
        index += 1;
    }
    None
}
