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

/// Renders the return type off a signature label (`(…) -> str` → `str`),
/// scanning for the top-level ` -> ` arrow so a callable return type like
/// `(int) -> str` is not split at its inner arrow.
fn return_type_of(label: &str) -> Option<String> {
    let bytes = label.as_bytes();
    let mut depth = 0usize;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b'-' if depth == 0
                && bytes.get(index + 1) == Some(&b'>')
                && bytes.get(index + 2) == Some(&b' ') =>
            {
                return Some(label[index + 3..].trim().to_string());
            }
            _ => {}
        }
        index += 1;
    }
    None
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
    let bytes = label.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'*' && bytes.get(index + 1) == Some(&b',') {
            return Some(index);
        }
    }
    None
}

/// Classifies one parameter's kind from ty's flags plus the inferred position.
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
/// `False`). A `=` never appears in an annotation, so the first one marks the
/// default.
fn param_default(label: &str) -> Option<String> {
    label
        .split_once('=')
        .map(|(_, default)| default.trim().to_string())
}
