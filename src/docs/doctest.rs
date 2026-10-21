//! Doctest extraction from a resolved docstring.
//!
//! A docstring's examples are written as an interactive session: a `>>>`
//! primary prompt, optional `...` continuation prompts, and the indented lines
//! of expected output that follow. This module turns that prose back into the
//! individual examples so `py_docs` can surface them separately from the
//! signature and prose. It is a pure string scan — no parsing, no execution —
//! and returns an empty list when a docstring has none (an honest empty list,
//! never a `None`).

/// Extracts each doctest example from a docstring. One example per `>>>`
/// prompt; a `...` continuation line or an indented (leading-whitespace)
/// expected-output line is grouped under it, and trailing blank lines are
/// trimmed. Leading indentation is normalized off every line, so a docstring
/// indented to its function body still yields flush-left `>>>` examples. Lines
/// that are neither prompts, continuations nor indented output end the current
/// example, matching how doctest prose and prompts interleave.
pub(crate) fn extract_doctests(text: &str) -> Vec<String> {
    let mut examples = Vec::new();
    let mut current: Vec<&str> = Vec::new();

    for line in text.lines() {
        let stripped = line.trim_start();
        if is_prompt(stripped) {
            finish(&mut examples, &mut current);
            current.push(stripped);
        } else if !current.is_empty() && (is_continuation(stripped) || is_output(line)) {
            current.push(stripped);
        } else {
            finish(&mut examples, &mut current);
        }
    }
    finish(&mut examples, &mut current);
    examples
}

/// A `>>>` primary prompt, either bare or followed by a statement.
fn is_prompt(line: &str) -> bool {
    line == ">>>" || line.starts_with(">>> ")
}

/// A `...` continuation prompt, either bare or followed by a statement.
fn is_continuation(line: &str) -> bool {
    line == "..." || line.starts_with("... ")
}

/// A line of expected output: blank (part of the example, trimmed later if it
/// trails) or indented relative to the prompt.
fn is_output(line: &str) -> bool {
    line.is_empty() || line.starts_with(char::is_whitespace)
}

/// Commits the in-progress example, dropping any trailing blank lines, and
/// resets the buffer for the next prompt.
fn finish(examples: &mut Vec<String>, current: &mut Vec<&str>) {
    if current.is_empty() {
        return;
    }
    while current.last().is_some_and(|line| line.trim().is_empty()) {
        current.pop();
    }
    if current.is_empty() {
        return;
    }
    examples.push(current.join("\n"));
    current.clear();
}

#[cfg(test)]
mod tests {
    use super::extract_doctests;

    #[test]
    fn empty_and_prose_only_docstrings_have_no_doctests() {
        assert!(extract_doctests("").is_empty());
        assert!(extract_doctests("Just prose, no examples.").is_empty());
    }

    #[test]
    fn single_example_with_indented_output() {
        let text = "Add two numbers.\n\n    >>> add(1, 2)\n    3\n";
        assert_eq!(extract_doctests(text), vec![">>> add(1, 2)\n3"]);
    }

    #[test]
    fn continuation_lines_are_grouped() {
        let text = "    >>> for i in range(3):\n    ...     print(i)\n    0\n    1\n    2\n";
        assert_eq!(
            extract_doctests(text),
            vec![">>> for i in range(3):\n...     print(i)\n0\n1\n2"]
        );
    }

    #[test]
    fn multiple_examples_are_split() {
        let text = ">>> add(1, 2)\n  3\n>>> add(0, 0)\n  0\n";
        assert_eq!(
            extract_doctests(text),
            vec![">>> add(1, 2)\n3", ">>> add(0, 0)\n0"]
        );
    }

    #[test]
    fn trailing_blanks_are_trimmed() {
        let text = ">>> add(1, 2)\n  3\n\n\n";
        assert_eq!(extract_doctests(text), vec![">>> add(1, 2)\n3"]);
    }

    #[test]
    fn non_indented_prose_ends_an_example() {
        // The flush-left note after the output is prose, not output, so the
        // example stops at the (indented) `3` and the note is not swallowed.
        let text = "    >>> add(1, 2)\n    3\n\nReturns the sum.\n";
        assert_eq!(extract_doctests(text), vec![">>> add(1, 2)\n3"]);
    }
}
