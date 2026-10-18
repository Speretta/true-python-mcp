//! Canonical-form boundary tests for the Jupyter pipeline.
//!
//! "Byte-exact" means identity on files already in nbformat/VS Code layout;
//! other layouts are normalized once by the first write, then stable forever.
//! These tests pin that honest boundary (correctness audit F7), the version
//! field error messages (F16) and the nbformat-aligned output predicates
//! (F17).
//!
//! Fixtures (additive; the existing corpus is untouched):
//! - `tests/data/crlf_structured.ipynb` — `tests/data/nbformat_rich.ipynb`
//!   with every line ending converted to CRLF, as a `core.autocrlf` checkout
//!   produces. Generated with:
//!   `sed 's/$/\r/' tests/data/nbformat_rich.ipynb > tests/data/crlf_structured.ipynb`
//!   Every raw LF byte in an `.ipynb` is structural JSON whitespace —
//!   newlines inside string values are escaped `\n` — so a line-level rewrite
//!   cannot alter the data.
//! - `tests/data/exotic_numbers.ipynb` — canonical layout carrying a `1e+30`
//!   float and integers beyond 2^64 inside free-form metadata, where nbformat
//!   accepts any JSON value. The `1e+30` spelling round-trips verbatim under
//!   the current serde_json (it matches Python's `repr`); the beyond-u64
//!   integers are what normalize on first write.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test assertion idiom
mod common;

use common::{ground_truth_python, run_python};
use tpmcp::notebook::{JupyterNotebook, Notebook, Severity};

fn read(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("cannot read {path}: {error}"))
}

/// (severity, cell, code) triples of a document's findings.
fn codes(content: &str) -> Vec<(Severity, Option<usize>, String)> {
    JupyterNotebook::parse(content)
        .unwrap()
        .check()
        .into_iter()
        .map(|finding| (finding.severity, finding.cell, finding.code))
        .collect()
}

/// A CRLF-structured checkout parses, and the first write normalizes it to
/// the exact LF canonical bytes — a documented one-time whole-file rewrite,
/// idempotent afterwards.
#[test]
fn crlf_structured_notebook_normalizes_to_lf_once() {
    let original = read("tests/data/crlf_structured.ipynb");
    assert!(original.contains("\r\n"), "fixture must be CRLF-structured");

    let normalized = JupyterNotebook::parse(&original)
        .unwrap()
        .to_string_canonical();
    // The normalization lands exactly on the LF corpus file it was generated
    // from, and no CR survives anywhere.
    assert_eq!(normalized, read("tests/data/nbformat_rich.ipynb"));
    assert!(!normalized.contains('\r'));

    let again = JupyterNotebook::parse(&normalized)
        .unwrap()
        .to_string_canonical();
    assert_eq!(again, normalized, "normalization must be idempotent");
}

/// Exotic number forms (float exponent `1e+30`, integers beyond u64) parse
/// fine, normalize on first write through serde_json's number handling, and
/// are stable afterwards. Serializer behavior is deliberately unchanged here;
/// this documents where the byte-exact claim ends.
#[test]
fn exotic_numbers_normalize_once_then_are_stable() {
    let original = read("tests/data/exotic_numbers.ipynb");
    assert!(original.contains("1e+30"), "fixture carries 1e+30");
    assert!(
        original.contains("18446744073709551616"),
        "fixture carries 2^64"
    );

    let normalized = JupyterNotebook::parse(&original)
        .unwrap()
        .to_string_canonical();
    assert_ne!(
        normalized, original,
        "first write normalizes exotic number forms"
    );
    // serde_json reads integers beyond u64 as f64, which re-emit in the
    // shortest round-trip float form; the `1e+30` spelling already IS that
    // form (it matches Python's repr), so it round-trips verbatim:
    assert!(
        normalized.contains("\"big_int\": 1.8446744073709552e+19"),
        "{normalized}"
    );
    assert!(
        normalized.contains("\"u64_plus_one\": 1.8446744073709552e+19"),
        "{normalized}"
    );
    assert!(normalized.contains("\"bigger_int\": 1e+20"), "{normalized}");
    assert!(normalized.contains("\"float_form\": 1e+30"), "{normalized}");
    assert!(normalized.contains("\"planck_scale\": 1e+30"), "{normalized}");

    let again = JupyterNotebook::parse(&normalized)
        .unwrap()
        .to_string_canonical();
    assert_eq!(again, normalized, "normalization must be idempotent");

    // A float spelled WITHOUT the '+' is the same number in a non-canonical
    // form: it normalizes once to serde_json's spelling, then stabilizes.
    let inline = r#"{"nbformat": 4, "nbformat_minor": 5, "metadata": {"n": 1e30}, "cells": []}"#;
    let once = JupyterNotebook::parse(inline).unwrap().to_string_canonical();
    assert!(once.contains("\"n\": 1e+30"), "{once}");
    let twice = JupyterNotebook::parse(&once).unwrap().to_string_canonical();
    assert_eq!(twice, once);

    // The fixture is schema-clean on both sides of the normalization.
    assert!(codes(&original).is_empty(), "original has findings");
    assert!(codes(&normalized).is_empty(), "normalized has findings");
}

/// F16: a version field that is present but not an integer (e.g. the JSON
/// float `4.0`) must report the value found, not claim the field is missing.
#[test]
fn float_version_fields_report_the_value_found() {
    let error = JupyterNotebook::parse(
        r#"{"nbformat": 4.0, "nbformat_minor": 5, "cells": [], "metadata": {}}"#,
    )
    .unwrap_err();
    assert!(error.contains("nbformat"), "{error}");
    assert!(error.contains("4.0"), "{error}");
    assert!(
        !error.contains("missing"),
        "a present field must not be reported missing: {error}"
    );

    let error = JupyterNotebook::parse(r#"{"nbformat": 4, "nbformat_minor": 5.0, "cells": []}"#)
        .unwrap_err();
    assert!(error.contains("nbformat_minor"), "{error}");
    assert!(error.contains("5.0"), "{error}");
    assert!(!error.contains("missing"), "{error}");

    // Non-numeric present values quote what was found.
    let error = JupyterNotebook::parse(r#"{"nbformat": "4", "nbformat_minor": 5, "cells": []}"#)
        .unwrap_err();
    assert!(error.contains(r#""4""#), "{error}");
    assert!(!error.contains("missing"), "{error}");

    // Genuinely missing fields keep the long-standing text (also asserted by
    // the frozen tests/roundtrip.rs), and integer versions keep theirs.
    let error = JupyterNotebook::parse(r#"{"nbformat_minor": 5, "cells": []}"#).unwrap_err();
    assert!(
        error.contains(r#"missing integer field "nbformat""#),
        "{error}"
    );
    let error =
        JupyterNotebook::parse(r#"{"nbformat": 3, "nbformat_minor": 0, "cells": []}"#).unwrap_err();
    assert!(error.contains("unsupported nbformat version 3"), "{error}");
}

/// A minimal nbformat 4.5 notebook whose single code cell carries `output`
/// as its only output.
fn notebook_with_output(output: &str) -> String {
    format!(
        r#"{{
 "cells": [
  {{
   "cell_type": "code",
   "execution_count": 1,
   "id": "aaaa0000",
   "metadata": {{}},
   "outputs": [
    {output}
   ],
   "source": [
    "x = 1\n"
   ]
  }}
 ],
 "metadata": {{}},
 "nbformat": 4,
 "nbformat_minor": 5
}}
"#
    )
}

/// F17 ground truth (nbformat 5.11.1, v4.5 schema): `execute_result`
/// accepts `"execution_count": null` (`{"type": ["integer", "null"],
/// "minimum": 0}`), so our old is_u64-only predicate was a false positive.
const EXECUTE_RESULT_NULL_COUNT: &str =
    r#"{"execution_count": null, "data": {"text/plain": ["1"]}, "metadata": {}, "output_type": "execute_result"}"#;
/// The official schema types stream `name` as a plain string — "stdout,
/// stderr" is only its description — so any string is valid.
const STREAM_ODD_NAME: &str = r#"{"name": "stdoutput", "output_type": "stream", "text": ["hi\n"]}"#;
/// Non-string names are invalid ("42 is not of type 'string'").
const STREAM_NUMERIC_NAME: &str = r#"{"name": 42, "output_type": "stream", "text": ["hi\n"]}"#;

/// F17: the aligned predicates, deterministically.
#[test]
fn output_shapes_follow_the_official_schema() {
    // null prompt number on execute_result is valid.
    assert!(codes(&notebook_with_output(EXECUTE_RESULT_NULL_COUNT)).is_empty());
    // Any string stream name is valid.
    assert!(codes(&notebook_with_output(STREAM_ODD_NAME)).is_empty());
    // A non-string stream name flags exactly one outputs error.
    let findings = JupyterNotebook::parse(&notebook_with_output(STREAM_NUMERIC_NAME))
        .unwrap()
        .check();
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert_eq!(findings[0].code, "outputs");
    assert!(
        findings[0].message.contains("invalid \"name\""),
        "{findings:?}"
    );
    // Wrong-typed and missing execute_result on execute_result still flag.
    let string_count = EXECUTE_RESULT_NULL_COUNT.replace("null", r#""7""#);
    assert_eq!(
        codes(&notebook_with_output(&string_count)).len(),
        1,
        "string execution_count must flag"
    );
    let missing_count = EXECUTE_RESULT_NULL_COUNT.replace(r#""execution_count": null, "#, "");
    assert_eq!(
        codes(&notebook_with_output(&missing_count)).len(),
        1,
        "missing execution_count must flag"
    );
}

/// F17 live ground truth: nbformat.validate's verdict on the exact documents
/// our predicates judge must agree with our Error findings — the same
/// contract the frozen fixture loop in tests/check.rs enforces.
#[test]
fn output_shapes_agree_with_nbformat_validate() {
    let Some(py) = ground_truth_python() else {
        return;
    };
    const VALIDATE: &str = r#"
import sys
import nbformat
try:
    nb = nbformat.read(sys.argv[1], as_version=4)
    nbformat.validate(nb)
    print("valid")
except nbformat.ValidationError:
    print("invalid")
"#;
    let dir = std::env::temp_dir().join(format!("tpmcp-canonical-forms-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, content, expect_valid) in [
        (
            "execute_result_null_count.ipynb",
            notebook_with_output(EXECUTE_RESULT_NULL_COUNT),
            true,
        ),
        (
            "stream_odd_name.ipynb",
            notebook_with_output(STREAM_ODD_NAME),
            true,
        ),
        (
            "stream_numeric_name.ipynb",
            notebook_with_output(STREAM_NUMERIC_NAME),
            false,
        ),
    ] {
        let path = dir.join(name);
        std::fs::write(&path, &content).unwrap();
        let verdict = run_python(&py, VALIDATE, &[path.to_str().unwrap()]);
        assert_eq!(
            verdict.trim() == "valid",
            expect_valid,
            "{name}: nbformat's verdict changed: {}",
            verdict.trim()
        );
        let has_errors = codes(&content)
            .iter()
            .any(|(severity, _, _)| *severity == Severity::Error);
        assert_eq!(
            verdict.trim() == "invalid",
            has_errors,
            "{name}: nbformat says {}, we say errors={has_errors}",
            verdict.trim()
        );
    }
}

/// Style-14 pin: clearing all outputs visits code cells once, in ascending
/// index order, and leaves non-code cells alone.
#[test]
fn clear_outputs_all_clears_in_index_order() {
    let mut notebook = JupyterNotebook::parse(&read("tests/data/schema_issues.ipynb")).unwrap();
    let cleared = notebook.clear_outputs(None).unwrap();
    // schema_issues.ipynb: cells 1 and 2 are the code cells.
    assert_eq!(cleared, vec![1, 2]);
    for index in cleared {
        assert_eq!(notebook.cells[index].extra["outputs"], serde_json::json!([]));
        assert_eq!(
            notebook.cells[index].extra["execution_count"],
            serde_json::Value::Null
        );
    }
    // The markdown cell keeps whatever it (invalidly) carried.
    assert!(notebook.cells[3].extra.contains_key("outputs"));
    assert!(notebook.cells[3].extra.contains_key("execution_count"));
}
