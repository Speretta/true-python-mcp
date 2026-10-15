# The file changes while the AI is working

The user asks the AI to change `examples/demo/plan.py`, a marimo notebook. While the AI is working, the user edits the same file in VS Code. This example shows the revision guard: tpmcp refuses one unsafe write, and the AI recovers on its own.

> **User asks the AI:** *"In examples/demo/plan.py, set the revenue to 1500."*

What follows is the real conversation between the AI (the LLM) and tpmcp, captured from the server. The LLM sends tool calls and sees the answers as markdown.

## Step 1 — Read one cell (`notebook_read`)

The LLM sends:
````json
{
  "name": "notebook_read",
  "arguments": {
    "path": "examples/demo/plan.py",
    "start": 3,
    "limit": 1
  }
}
````

tpmcp answers:
````markdown
notebook: examples/demo/plan.py
revision: 80b86cd3d49ef040
cells: 4 (3 code, 1 markdown) · marimo 0.25.1

### [3] code · 1 lines
```python
revenue = 1200
```
````

A read can show a window instead of the whole notebook: `start: 3` with `limit: 1` returns only cell 3. The answer carries revision `80b86cd3d49ef040`, a short hash of the file state; the coming edit must present it.

## Meanwhile, in VS Code

Before the AI's edit lands, the user types a comment at the end of the same file in VS Code. The AI does not see it. This line is appended to the file:
````text
# edited in VS Code while the AI was thinking
````

## Step 2 — Set the new value (`cell_set_source`)

The LLM sends:
````json
{
  "name": "cell_set_source",
  "arguments": {
    "path": "examples/demo/plan.py",
    "cell": 3,
    "source": "revenue = 1500",
    "revision": "80b86cd3d49ef040"
  }
}
````

tpmcp refuses the change (isError=true) and explains why:
````markdown
revision conflict for examples/demo/plan.py: expected 80b86cd3d49ef040, file is now at aad84c8f283906a6; re-read the file and retry with its current revision
````

The machine-readable part (structuredContent):
````json
{"current_revision":"aad84c8f283906a6","error":"conflict","expected_revision":"80b86cd3d49ef040","message":"revision conflict for examples/demo/plan.py: expected 80b86cd3d49ef040, file is now at aad84c8f283906a6; re-read the file and retry with its current revision","path":"examples/demo/plan.py"}
````

The AI sent the old revision, so the revision guard refused the write. The refused call changed nothing on disk: the user comment is still there, and cell 3 still says `revenue = 1200`. The error also arrives in machine-readable form, with `"error": "conflict"` and the current revision, so the AI knows what to do next.

## Step 3 — Re-read the cell (`notebook_read`)

The LLM sends:
````json
{
  "name": "notebook_read",
  "arguments": {
    "path": "examples/demo/plan.py",
    "start": 3,
    "limit": 1
  }
}
````

tpmcp answers:
````markdown
notebook: examples/demo/plan.py
revision: aad84c8f283906a6
cells: 4 (3 code, 1 markdown) · marimo 0.25.1

### [3] code · 1 lines
```python
revenue = 1200
```
````

The fresh read returns revision `aad84c8f283906a6`, the same one the error reported. Cell 3 still contains `revenue = 1200`, so the AI keeps its plan and only updates the revision.

## Step 4 — Retry the edit (`cell_set_source`)

The LLM sends:
````json
{
  "name": "cell_set_source",
  "arguments": {
    "path": "examples/demo/plan.py",
    "cell": 3,
    "source": "revenue = 1500",
    "revision": "aad84c8f283906a6"
  }
}
````

tpmcp answers:
````markdown
updated source of cell 3
path: examples/demo/plan.py
revision: 423a32b2c63058af

### [3] code · 1 lines
```python
   1 | revenue = 1500
```
````

With the current revision the write succeeds. The answer shows the new cell source, with a line number, and the next revision `423a32b2c63058af`.

## Result

The file changed like this (`diff -u` against the original):
````diff
--- a/examples/demo/plan.py
+++ b/examples/demo/plan.py
@@ -29,9 +29,11 @@
 
 @app.cell
 def _():
-    revenue = 1200
+    revenue = 1500
     return (revenue,)
 
 
 if __name__ == "__main__":
     app.run()
+
+# edited in VS Code while the AI was thinking
````

Both changes survived: the AI set `revenue = 1500`, and the user comment is still at the end of the file. Nothing was overwritten, because the refused call wrote nothing and the retry touched only cell 3. This is the revision guard doing its job, and the AI recovered with no help from the user.

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
