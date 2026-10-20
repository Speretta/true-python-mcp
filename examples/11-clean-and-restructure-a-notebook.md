# Clean up and restructure a notebook

The user has `examples/demo/cleanup.ipynb`, a notebook they are about to
commit. It has stored outputs they do not want in git, a "Findings" note that
was typed as a code cell, and that note left in the middle of the code. This
example walks four of the cell tools in one pass: clear the outputs, tag the
parameters cell, convert the note to markdown, and move it to the end.

> **User asks the AI:** *"Tidy examples/demo/cleanup.ipynb before I commit it:
> clear the outputs, tag the parameters cell, and make that 'Findings' note a
> markdown cell at the end."*

What follows is the real conversation between the AI (the LLM) and tpmcp,
captured from the server. The LLM sends tool calls and sees the answers as
markdown.

## Step 1: Read the notebook (`notebook_read`)

The LLM sends:

````json
{
  "name": "notebook_read",
  "arguments": {
    "path": "examples/demo/cleanup.ipynb"
  }
}
````

tpmcp answers:

````markdown
notebook: examples/demo/cleanup.ipynb
revision: e57ffacfbdb62a48
cells: 4 (3 code, 1 markdown) · nbformat 4.5

### [0] markdown · 3 lines · id 3aa1b2c3
```markdown
# Cleanup demo

A notebook to tidy before committing.
```

### [1] code · exec 1 · 2 lines · id 5bb4c5d6
```python
x = 42
print(x)
```
outputs(1): stream(stdout)

### [2] code · exec 2 · 2 lines · id 7cc8d9e0
```python
## Findings
- the peak is at x = 3
```
outputs(1): stream(stdout)

### [3] code · exec 3 · 2 lines · id 9ee1f2a3
```python
y = x + 1
print(y)
```
outputs(1): stream(stdout)
````

Three code cells, each with a stored `stream(stdout)` output, and the
"Findings" note in cell 2 — typed as code, but it is markdown text.

## Step 2: Strip the outputs (`cell_clear_outputs`)

Omitting `cell` clears every code cell in one call. The LLM sends:

````json
{
  "name": "cell_clear_outputs",
  "arguments": {
    "path": "examples/demo/cleanup.ipynb",
    "revision": "e57ffacfbdb62a48"
  }
}
````

tpmcp answers:

````markdown
cleared outputs of 3 code cells
path: examples/demo/cleanup.ipynb
revision: 77b022742fd1f6ca
````

Each code cell's `outputs` becomes `[]` and its `execution_count` becomes
`null`, per the nbformat spec — the notebook is now clean to commit.

## Step 3: Tag the parameters cell (`cell_set_metadata`)

The `x = 42` cell is the notebook's parameter, so the AI tags it the way
papermill expects. The LLM sends:

````json
{
  "name": "cell_set_metadata",
  "arguments": {
    "path": "examples/demo/cleanup.ipynb",
    "cell": 1,
    "metadata": {
      "tags": [
        "parameters"
      ]
    },
    "revision": "77b022742fd1f6ca"
  }
}
````

tpmcp answers:

````markdown
replaced metadata of cell 1
path: examples/demo/cleanup.ipynb
revision: 106e8529a2564318

previous metadata:
```json
{}
```
````

`cell_set_metadata` is a full replace, not a patch, and it echoes the
previous object (`{}`) so the change can be undone. Only the metadata moved —
the cell's source, `id`, and `outputs` are untouched.

## Step 4: Turn the note into markdown (`cell_set_type`)

The LLM converts cell 2, the "Findings" note, from code to markdown:

````json
{
  "name": "cell_set_type",
  "arguments": {
    "path": "examples/demo/cleanup.ipynb",
    "cell": 2,
    "cell_type": "markdown",
    "revision": "106e8529a2564318"
  }
}
````

tpmcp answers:

````markdown
cell 2 is now markdown
path: examples/demo/cleanup.ipynb
revision: 02dda097c585818d
````

Converting code to markdown keeps the source text as-is and drops the cell's
`outputs` and `execution_count`, which markdown cells do not carry. The
`## Findings` text now renders as a markdown heading and a list.

## Step 5: Move the note to the end (`cell_move`)

The LLM moves the note (still cell 2) to the end of the notebook:

````json
{
  "name": "cell_move",
  "arguments": {
    "path": "examples/demo/cleanup.ipynb",
    "from": 2,
    "to": 3,
    "revision": "02dda097c585818d"
  }
}
````

tpmcp answers:

````markdown
moved cell 2 to index 3
path: examples/demo/cleanup.ipynb
revision: 41c58f4236497c44
````

`to` is interpreted after removing the cell: with four cells, `from: 2, to: 3`
removes cell 2 (leaving three) and inserts it at index 3 — the very end.

## Step 6: Verify the new shape (`notebook_read`)

The LLM sends the same read as Step 1:

````json
{
  "name": "notebook_read",
  "arguments": {
    "path": "examples/demo/cleanup.ipynb"
  }
}
````

tpmcp answers:

````markdown
notebook: examples/demo/cleanup.ipynb
revision: 41c58f4236497c44
cells: 4 (2 code, 2 markdown) · nbformat 4.5

### [0] markdown · 3 lines · id 3aa1b2c3
```markdown
# Cleanup demo

A notebook to tidy before committing.
```

### [1] code · 2 lines · id 5bb4c5d6
```python
x = 42
print(x)
```

### [2] code · 2 lines · id 9ee1f2a3
```python
y = x + 1
print(y)
```

### [3] markdown · 2 lines · id 7cc8d9e0
```markdown
## Findings
- the peak is at x = 3
```
````

Two code cells and two markdown cells, no `exec` markers and no
`outputs(…)` lines — the outputs are gone, and the note is now a markdown
cell at the end.

## Result

The notebook changed like this (`diff -u` against the original):

````diff
--- a/examples/demo/cleanup.ipynb
+++ b/examples/demo/cleanup.ipynb
@@ -12,18 +12,14 @@
   },
   {
    "cell_type": "code",
-   "execution_count": 1,
+   "execution_count": null,
    "id": "5bb4c5d6",
-   "metadata": {},
-   "outputs": [
-    {
-     "name": "stdout",
-     "output_type": "stream",
-     "text": [
-      "42\n"
-     ]
-    }
-   ],
+   "metadata": {
+    "tags": [
+     "parameters"
+    ]
+   },
+   "outputs": [],
    "source": [
     "x = 42\n",
     "print(x)"
@@ -31,40 +27,22 @@
   },
   {
    "cell_type": "code",
-   "execution_count": 2,
-   "id": "7cc8d9e0",
+   "execution_count": null,
+   "id": "9ee1f2a3",
    "metadata": {},
-   "outputs": [
-    {
-     "name": "stdout",
-     "output_type": "stream",
-     "text": [
-      "done\n"
-     ]
-    }
-   ],
+   "outputs": [],
    "source": [
-    "## Findings\n",
-    "- the peak is at x = 3"
+    "y = x + 1\n",
+    "print(y)"
    ]
   },
   {
-   "cell_type": "code",
-   "execution_count": 3,
-   "id": "9ee1f2a3",
+   "cell_type": "markdown",
+   "id": "7cc8d9e0",
    "metadata": {},
-   "outputs": [
-    {
-     "name": "stdout",
-     "output_type": "stream",
-     "text": [
-      "43\n"
-     ]
-    }
-   ],
    "source": [
-    "y = x + 1\n",
-    "print(y)"
+    "## Findings\n",
+    "- the peak is at x = 3"
    ]
   }
  ],
````

Every change is one the four tools made: the `tags` metadata on cell 1, the
cleared `outputs` and `execution_count` on the code cells, and the note
re-typed as markdown and moved to the end. Each cell kept its `id`, and the
notebook metadata never changed.

## The input file

`examples/demo/cleanup.ipynb` before the session:

````json
{
 "cells": [
  {
   "cell_type": "markdown",
   "id": "3aa1b2c3",
   "metadata": {},
   "source": [
    "# Cleanup demo\n",
    "\n",
    "A notebook to tidy before committing."
   ]
  },
  {
   "cell_type": "code",
   "execution_count": 1,
   "id": "5bb4c5d6",
   "metadata": {},
   "outputs": [
    {
     "name": "stdout",
     "output_type": "stream",
     "text": [
      "42\n"
     ]
    }
   ],
   "source": [
    "x = 42\n",
    "print(x)"
   ]
  },
  {
   "cell_type": "code",
   "execution_count": 2,
   "id": "7cc8d9e0",
   "metadata": {},
   "outputs": [
    {
     "name": "stdout",
     "output_type": "stream",
     "text": [
      "done\n"
     ]
    }
   ],
   "source": [
    "## Findings\n",
    "- the peak is at x = 3"
   ]
  },
  {
   "cell_type": "code",
   "execution_count": 3,
   "id": "9ee1f2a3",
   "metadata": {},
   "outputs": [
    {
     "name": "stdout",
     "output_type": "stream",
     "text": [
      "43\n"
     ]
    }
   ],
   "source": [
    "y = x + 1\n",
    "print(y)"
   ]
  }
 ],
 "metadata": {
  "kernelspec": {
   "display_name": "Python 3",
   "language": "python",
   "name": "python3"
  }
 },
 "nbformat": 4,
 "nbformat_minor": 5
}
````
