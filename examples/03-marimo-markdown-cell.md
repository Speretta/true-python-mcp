# Edit a markdown cell in a marimo notebook

The user has a small marimo app at `examples/demo/plan.py`. A marimo notebook is an ordinary Python file, but tpmcp shows it as cells, the way the marimo editor does. The user wants a new title in the markdown cell.

> **User asks the AI:** *"In my marimo app examples/demo/plan.py, change the title from 'Sales plan' to 'Revenue plan'."*

What follows is the real conversation between the AI (the LLM) and tpmcp, captured from the server. The LLM sends tool calls and sees the answers as markdown.

## Step 1: Read the notebook (`notebook_read`)

The LLM sends:
````json
{
  "name": "notebook_read",
  "arguments": {
    "path": "examples/demo/plan.py"
  }
}
````

tpmcp answers:
````markdown
notebook: examples/demo/plan.py
revision: 80b86cd3d49ef040
cells: 4 (3 code, 1 markdown) · marimo 0.25.1

### [0] markdown · 3 lines
```markdown
# Sales plan

A tiny marimo app used in the tpmcp examples.
```

### [1] code · 1 lines
```python
import marimo as mo
```

### [2] code · 1 lines
```python
mo.md(f"Revenue: **{revenue}**")
```

### [3] code · 1 lines
```python
revenue = 1200
```
````

In the file, cell 0 is stored as a `mo.md(r"""...""")` call. tpmcp hides that wrapper and shows only the markdown text, so the AI works with plain text. The answer also carries a revision — a short hash of the file state — and every write must present the current one.

## Step 2: Change the title (`cell_edit`)

The LLM sends:
````json
{
  "name": "cell_edit",
  "arguments": {
    "path": "examples/demo/plan.py",
    "cell": 0,
    "old_string": "# Sales plan",
    "new_string": "# Revenue plan",
    "revision": "80b86cd3d49ef040"
  }
}
````

tpmcp answers:
````markdown
edited cell 0
path: examples/demo/plan.py
revision: 26b329bf8de08218

```diff
-# Sales plan
+# Revenue plan
 
 A tiny marimo app used in the tpmcp examples.
```
````

`cell_edit` replaces an exact string inside one cell. The cell-level diff shows that only the title line changed. The answer carries the new revision (`26b329bf8de08218`).

## Result

The file changed like this (`diff -u` against the original):
````diff
--- a/examples/demo/plan.py
+++ b/examples/demo/plan.py
@@ -7,7 +7,7 @@
 @app.cell(hide_code=True)
 def _(mo):
     mo.md(r"""
-    # Sales plan
+    # Revenue plan
 
     A tiny marimo app used in the tpmcp examples.
     """)
````

tpmcp re-wrapped the text exactly the way marimo itself would write it: only the title line changed in the diff, and the wrapper, the indentation, the other cells and the rest of the file are byte-identical. A marimo user can open the file and see the new title immediately. The AI reports back that the title is now "Revenue plan" and that nothing else moved.

## The input file

`examples/demo/plan.py` before the session:
````python
import marimo

__generated_with = "0.25.1"
app = marimo.App()


@app.cell(hide_code=True)
def _(mo):
    mo.md(r"""
    # Sales plan

    A tiny marimo app used in the tpmcp examples.
    """)
    return


@app.cell
def _():
    import marimo as mo

    return (mo,)


@app.cell
def _(mo, revenue):
    mo.md(f"Revenue: **{revenue}**")
    return


@app.cell
def _():
    revenue = 1200
    return (revenue,)


if __name__ == "__main__":
    app.run()
````
