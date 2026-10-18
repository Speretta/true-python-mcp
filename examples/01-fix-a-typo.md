# Fix a typo in a Jupyter cell

The user has a small Jupyter notebook at `examples/demo/demo.ipynb`. The second
cell prints the text `hello wrold` — a typo in the source code. The user wants
the AI to fix that one word and leave everything else alone.

> **User asks the AI:** *"There is a typo in the second cell of
> examples/demo/demo.ipynb — fix it."*

What follows is the real conversation between the AI (the LLM) and tpmcp,
captured from the server. The LLM sends tool calls and sees the answers as
markdown.

## Step 1: Read the notebook and get the revision (`notebook_read`)

The LLM sends:

````json
{
  "name": "notebook_read",
  "arguments": {
    "path": "examples/demo/demo.ipynb"
  }
}
````

tpmcp answers:

````markdown
notebook: examples/demo/demo.ipynb
revision: eefc2ea99b5fe065
cells: 3 (2 code, 1 markdown) · nbformat 4.5

### [0] markdown · 3 lines · id 0e42b380
```markdown
# Demo

A tiny Jupyter notebook used in the tpmcp examples.
```

### [1] code · exec 1 · 1 lines · id 431c582c
```python
print("hello wrold")
```
outputs(1): stream(stdout)

### [2] code · 3 lines · id 1c22426d
```python
import os

x = 1 + 1
```
````

Two details matter in this answer. First, cell indexes are 0-based: the
"second cell" the user means is cell `1`, and it holds the typo. Second, the
answer reports a revision (`eefc2ea99b5fe065`). The revision is a fingerprint
of the file contents, and every change must carry it — that way tpmcp never
writes over a file that changed since the AI last looked at it.

## Step 2: Replace the typo (`cell_edit`)

The LLM sends:

````json
{
  "name": "cell_edit",
  "arguments": {
    "path": "examples/demo/demo.ipynb",
    "cell": 1,
    "old_string": "wrold",
    "new_string": "world",
    "revision": "eefc2ea99b5fe065"
  }
}
````

tpmcp answers:

````markdown
edited cell 1
path: examples/demo/demo.ipynb
revision: 9cbe7cce7d6827cd

```diff
-print("hello wrold")
+print("hello world")
```
````

The tool replaced the exact string `wrold` with `world` inside cell 1, and the
diff in the answer proves it: one line out, one line in. The call carried the
revision from Step 1, and the answer returns a new revision
(`9cbe7cce7d6827cd`) for any next call.

## Result

The file changed like this (`diff -u` against the original):

````diff
--- a/examples/demo/demo.ipynb
+++ b/examples/demo/demo.ipynb
@@ -25,7 +25,7 @@
     }
    ],
    "source": [
-    "print(\"hello wrold\")"
+    "print(\"hello world\")"
    ]
   },
   {
````

Only the source line changed. The cell's stored output still says
`hello wrold`, and the other cells, all outputs, ids, and metadata are
untouched. This is expected: tpmcp never runs code, it edits the notebook file
as data. The stored output will refresh the next time the user runs the
notebook. The AI reports back: typo fixed in cell 1, one line changed, nothing
else touched.

## The input file

`examples/demo/demo.ipynb` before the session:

````json
{
 "cells": [
  {
   "cell_type": "markdown",
   "id": "0e42b380",
   "metadata": {},
   "source": [
    "# Demo\n",
    "\n",
    "A tiny Jupyter notebook used in the tpmcp examples."
   ]
  },
  {
   "cell_type": "code",
   "execution_count": 1,
   "id": "431c582c",
   "metadata": {},
   "outputs": [
    {
     "name": "stdout",
     "output_type": "stream",
     "text": [
      "hello wrold\n"
     ]
    }
   ],
   "source": [
    "print(\"hello wrold\")"
   ]
  },
  {
   "cell_type": "code",
   "execution_count": null,
   "id": "1c22426d",
   "metadata": {},
   "outputs": [],
   "source": [
    "import os\n",
    "\n",
    "x = 1 + 1"
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
