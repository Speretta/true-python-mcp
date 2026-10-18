# Convert a Jupyter notebook to marimo

The user has `examples/demo/survey.ipynb`, a small analysis notebook with the
shape real notebooks tend to have: a markdown intro, a `!pip install` cell, a
plot with a stored PNG, and a raw cell holding a SQL note. They want it as a
marimo app, and as a markdown document for a report. This example shows
`notebook_create` with `from_path`: every cell that cannot convert exactly is
kept and reported, and the AI repairs the two cells marimo cannot run.

> **User asks the AI:** *"Convert examples/demo/survey.ipynb into a marimo app,
> and give me a markdown version for the report too."*

What follows is the real conversation between the AI (the LLM) and tpmcp,
captured from the server. The LLM sends tool calls and sees the answers as
markdown.

## Step 1: Survey the source (`notebook_read`)

The LLM sends:
````json
{
  "name": "notebook_read",
  "arguments": {
    "path": "examples/demo/survey.ipynb",
    "detail": "outline",
    "diagnostics": "check"
  }
}
````

tpmcp answers:
````markdown
notebook: examples/demo/survey.ipynb
revision: 2e7d52367fdbea33
cells: 6 (4 code, 1 markdown, 1 other) · nbformat 4.5

### [0] markdown · 3 lines · # Survey analysis

### [1] code · 5 lines · import pandas as pd

### [2] code · 1 lines · !pip install seaborn

### [3] code · 3 lines · import seaborn as sns

### [4] raw · 3 lines · SELECT segment, AVG(rating)

### [5] code · 2 lines · total = len(responses)

diagnostics: 0 finding(s)
no findings: the notebook is valid
````

One call gives the shape of the notebook (one row per cell, no bodies) and its
structural health. The AI can already see the two cells that will not survive
a conversion cleanly: the `!pip install` shell escape in cell 2 and the raw
SQL note in cell 4.

## Step 2: Convert (`notebook_create` with `from_path`)

The LLM sends:
````json
{
  "name": "notebook_create",
  "arguments": {
    "path": "examples/demo/survey.py",
    "from_path": "examples/demo/survey.ipynb"
  }
}
````

tpmcp answers:
````markdown
created notebook with 6 cells from examples/demo/survey.ipynb
path: examples/demo/survey.py
revision: 6f79f664e35aa1f9

warnings:
- cell 2: Jupyter magic `!pip` has no marimo equivalent; kept verbatim, marimo will report a syntax error
- cell 4: raw cells have no marimo equivalent; kept as a code cell
- outputs and execution counts are not stored in marimo files
- added a first cell `import marimo as mo`: markdown cells need a definition of `mo` to run in marimo
````

Nothing is dropped silently. Each warning tells the AI what to do next: the
magic cell is kept verbatim and will read back as broken, the raw cell became
a code cell, the outputs stayed behind (marimo files do not store them), and
tpmcp added one scaffold cell of its own — the `import marimo as mo` that the
markdown cell's `mo.md(...)` call needs at runtime. The scaffold cell is why
the file has 7 cells while the response counts the 6 converted ones.

## Step 3: Read the converted file (`notebook_read`)

The LLM sends:
````json
{
  "name": "notebook_read",
  "arguments": {
    "path": "examples/demo/survey.py"
  }
}
````

tpmcp answers:
````markdown
notebook: examples/demo/survey.py
revision: 6f79f664e35aa1f9
cells: 7 (6 code, 1 markdown) · marimo 0.25.1

### [0] code · 1 lines
```python
import marimo as mo
```

### [1] markdown · 3 lines
```markdown
# Survey analysis

A tiny Jupyter notebook used in the tpmcp examples.
```

### [2] code · 5 lines
```python
import pandas as pd

responses = pd.DataFrame(
    {"rating": [4, 5, 3, 5, 4], "segment": ["a", "b", "a", "c", "b"]}
)
```

### [3] code (broken) · 4 lines
```python
@app.cell
def _():
    !pip install seaborn
    return
```

### [4] code · 3 lines
```python
import seaborn as sns

plot = sns.barplot(x="segment", y="rating", data=responses)
```

### [5] code (broken) · 6 lines
```python
@app.cell
def _():
    SELECT segment, AVG(rating)
    FROM responses
    GROUP BY segment
    return
```

### [6] code · 2 lines
```python
total = len(responses)
print(total)
```
````

The file has a syntax error (`!pip` is not Python), but one broken region
never locks the whole notebook: tpmcp degrades the two unconvertible cells to
read-only `(broken)` blocks and keeps the other five editable. The broken
blocks are shown verbatim, so the AI sees exactly what to fix.

## Step 4: Try to fix the magic cell in place (`cell_edit`)

The LLM sends:
````json
{
  "name": "cell_edit",
  "arguments": {
    "path": "examples/demo/survey.py",
    "cell": 3,
    "old_string": "!pip install seaborn",
    "new_string": "# seaborn ships with the environment",
    "revision": "6f79f664e35aa1f9"
  }
}
````

tpmcp refuses (isError=true):
````markdown
cell 3 has a syntax error and is read-only; repair its Python syntax to edit it here
````

A broken block has no parsed structure, so surgical edits into it are
refused — the refusal changed nothing on disk. The repair path is to replace
the cell: delete it and insert a valid one at the same index.

## Step 5: Delete the broken cell (`cell_delete`)

The LLM sends:
````json
{
  "name": "cell_delete",
  "arguments": {
    "path": "examples/demo/survey.py",
    "cell": 3,
    "revision": "6f79f664e35aa1f9"
  }
}
````

tpmcp answers:
````markdown
deleted code cell 3
path: examples/demo/survey.py
revision: 3a3af29b00334c9d
````

## Step 6: Insert the replacement (`cell_insert`)

The LLM sends:
````json
{
  "name": "cell_insert",
  "arguments": {
    "path": "examples/demo/survey.py",
    "index": 3,
    "cell_type": "code",
    "source": "# seaborn ships with the environment; marimo has no shell escape",
    "revision": "3a3af29b00334c9d"
  }
}
````

tpmcp answers:
````markdown
inserted code cell at index 3
path: examples/demo/survey.py
revision: 050c1a4e8841567a
````

## Step 7: Locate the remaining broken cell (`notebook_read`)

The LLM sends:
````json
{
  "name": "notebook_read",
  "arguments": {
    "path": "examples/demo/survey.py",
    "detail": "outline",
    "format": "json"
  }
}
````

tpmcp answers (structuredContent):
````json
{"cell_count":7,"cells":[{"cell_type":"code","first_line":"import marimo as mo","index":0,"kind":null,"line_count":1},{"cell_type":"markdown","first_line":"# Survey analysis","index":1,"kind":null,"line_count":3},{"cell_type":"code","first_line":"import pandas as pd","index":2,"kind":null,"line_count":5},{"cell_type":"code","first_line":"# seaborn ships with the environment; marimo has no shell escape","index":3,"kind":null,"line_count":1},{"cell_type":"code","first_line":"import seaborn as sns","index":4,"kind":null,"line_count":3},{"cell_type":"code","first_line":"@app.cell","index":5,"kind":"broken","line_count":6},{"cell_type":"code","first_line":"total = len(responses)","index":6,"kind":null,"line_count":2}],"format":"marimo 0.25.1","path":"examples/demo/survey.py","range":[0,7],"revision":"050c1a4e8841567a"}
````

The delete shifted the indexes, so the AI re-reads before acting — the
outline's `kind` field shows the SQL note is now the broken cell 5.

## Step 8: Try to retype it (`cell_set_type`)

The SQL note belongs in a markdown cell, so the LLM tries the direct route:
````json
{
  "name": "cell_set_type",
  "arguments": {
    "path": "examples/demo/survey.py",
    "cell": 5,
    "cell_type": "markdown",
    "revision": "050c1a4e8841567a"
  }
}
````

tpmcp refuses (isError=true):
````markdown
cell 5 is a marimo broken block; its type cannot be changed
````

Same rule as step 4: broken blocks are read-only. Delete and reinsert as
markdown instead.

## Step 9: Delete and reinsert as markdown (`cell_delete`, `cell_insert`)

The LLM sends (JSON format this time — every tool speaks both):
````json
{
  "name": "cell_delete",
  "arguments": {
    "path": "examples/demo/survey.py",
    "cell": 5,
    "revision": "050c1a4e8841567a",
    "format": "json"
  }
}
````

tpmcp answers:
````json
{"cell":5,"deleted_type":"code","ok":true,"path":"examples/demo/survey.py","revision":"1d979caffd20a8e9","summary":"deleted code cell 5"}
````

The LLM sends:
````json
{
  "name": "cell_insert",
  "arguments": {
    "path": "examples/demo/survey.py",
    "index": 5,
    "cell_type": "markdown",
    "source": "SELECT segment, AVG(rating)\nFROM responses\nGROUP BY segment",
    "revision": "1d979caffd20a8e9",
    "format": "json"
  }
}
````

tpmcp answers:
````json
{"cell_type":"markdown","index":5,"ok":true,"path":"examples/demo/survey.py","revision":"57cc699fb3b931c6","summary":"inserted markdown cell at index 5"}
````

## Step 10: Verify the repaired file (`notebook_check`)

The LLM sends:
````json
{
  "name": "notebook_check",
  "arguments": {
    "path": "examples/demo/survey.py"
  }
}
````

tpmcp answers:
````markdown
notebook_check: examples/demo/survey.py
format: marimo 0.25.1 · revision: 57cc699fb3b931c6
no findings: the notebook is valid
````

No syntax errors, no graph problems: the file parses as a whole again and
every cell is editable.

## Step 11: The markdown version (`notebook_create`, `.md` target)

The LLM sends:
````json
{
  "name": "notebook_create",
  "arguments": {
    "path": "examples/demo/survey.md",
    "from_path": "examples/demo/survey.ipynb"
  }
}
````

tpmcp answers:
````markdown
created notebook with 6 cells from examples/demo/survey.ipynb
path: examples/demo/survey.md
revision: 522c3be57632c9c9

warnings:
- cell 3: binary outputs not exported (image/png)
````

The `.md` export renders markdown cells verbatim, fences the code, and keeps
text outputs as blockquotes — for example the plot cell's:
````markdown
outputs:
> execute_result image/png: <96 bytes> | text/plain:
>     <Axes: xlabel='segment', ylabel='rating'>
````

The PNG itself has no place in a standalone markdown file, so it is dropped —
and reported, like everything else.

## Result

`examples/demo/survey.py` after the repairs — a valid marimo app, in the
layout marimo's own serializer writes (the `mo` scaffold cell exports
`(mo,)`, the pandas cell exports `(responses,)`, and the cells that use it
take it as a parameter):
````python
import marimo

__generated_with = "0.25.1"
app = marimo.App()


@app.cell
def _():
    import marimo as mo

    return (mo,)


@app.cell(hide_code=True)
def _(mo):
    mo.md(r"""
    # Survey analysis

    A tiny Jupyter notebook used in the tpmcp examples.
    """)
    return


@app.cell
def _():
    import pandas as pd

    responses = pd.DataFrame(
        {"rating": [4, 5, 3, 5, 4], "segment": ["a", "b", "a", "c", "b"]}
    )
    return (responses,)


@app.cell
def _():
    # seaborn ships with the environment; marimo has no shell escape
    return


@app.cell
def _(responses):
    import seaborn as sns

    plot = sns.barplot(x="segment", y="rating", data=responses)
    return


@app.cell(hide_code=True)
def _(mo):
    mo.md(r"""
    SELECT segment, AVG(rating)
    FROM responses
    GROUP BY segment
    """)
    return


@app.cell
def _(responses):
    total = len(responses)
    print(total)
    return


if __name__ == "__main__":
    app.run()
````

Every value in the source notebook either survived, was reported in a
warning, or both: the dataframes and plot code converted exactly, the shell
escape and the raw SQL note were replaced with working equivalents by the
AI, and the only lost bytes are the ones marimo files and markdown documents
have no room for — outputs, execution counts, and the PNG, all named in the
warnings.

## The input file

`examples/demo/survey.ipynb` before the session (generated with the real
`nbformat` library):
````json
{
 "cells": [
  {
   "cell_type": "markdown",
   "id": "3dd19e1b",
   "metadata": {},
   "source": [
    "# Survey analysis\n",
    "\n",
    "A tiny Jupyter notebook used in the tpmcp examples."
   ]
  },
  {
   "cell_type": "code",
   "execution_count": 1,
   "id": "b75cdbd4",
   "metadata": {},
   "outputs": [],
   "source": [
    "import pandas as pd\n",
    "\n",
    "responses = pd.DataFrame(\n",
    "    {\"rating\": [4, 5, 3, 5, 4], \"segment\": [\"a\", \"b\", \"a\", \"c\", \"b\"]}\n",
    ")"
   ]
  },
  {
   "cell_type": "code",
   "execution_count": 2,
   "id": "147aa72d",
   "metadata": {},
   "outputs": [
    {
     "name": "stdout",
     "output_type": "stream",
     "text": [
      "Successfully installed seaborn-0.13.2\n"
     ]
    }
   ],
   "source": [
    "!pip install seaborn"
   ]
  },
  {
   "cell_type": "code",
   "execution_count": 3,
   "id": "f7a6c92b",
   "metadata": {},
   "outputs": [
    {
     "data": {
      "image/png": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==",
      "text/plain": [
       "<Axes: xlabel='segment', ylabel='rating'>"
      ]
     },
     "execution_count": 3,
     "metadata": {},
     "output_type": "execute_result"
    }
   ],
   "source": [
    "import seaborn as sns\n",
    "\n",
    "plot = sns.barplot(x=\"segment\", y=\"rating\", data=responses)"
   ]
  },
  {
   "cell_type": "raw",
   "id": "916fd09f",
   "metadata": {},
   "source": [
    "SELECT segment, AVG(rating)\n",
    "FROM responses\n",
    "GROUP BY segment"
   ]
  },
  {
   "cell_type": "code",
   "execution_count": 4,
   "id": "f6156070",
   "metadata": {},
   "outputs": [
    {
     "name": "stdout",
     "output_type": "stream",
     "text": [
      "5\n"
     ]
    }
   ],
   "source": [
    "total = len(responses)\n",
    "print(total)"
   ]
  }
 ],
 "metadata": {},
 "nbformat": 4,
 "nbformat_minor": 5
}
````
