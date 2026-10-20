# Lint and format a Jupyter notebook

The user has a small Jupyter notebook at `examples/demo/messy.ipynb`. Its
code cell has an unused `import os` and messy spacing. This is the same ruff
that example 02 ran on a plain `.py` file, now pointed at a notebook: the
user wants it linted, the safe fix applied, and the cell formatted — with the
stored output and metadata left exactly as they are.

> **User asks the AI:** *"Lint examples/demo/messy.ipynb, apply the safe fix,
> and format it."*

What follows is the real conversation between the AI (the LLM) and tpmcp,
captured from the server. The LLM sends tool calls and sees the answers as
markdown.

## Step 1: Lint the notebook (`ruff_check`)

The LLM sends:

````json
{
  "name": "ruff_check",
  "arguments": {
    "path": "examples/demo/messy.ipynb"
  }
}
````

tpmcp answers:

````markdown
ruff check: examples/demo/messy.ipynb
revision: a9949b5317b716b1
1 diagnostic(s), 1 fixable

cell 1 1:8 F401 `os` imported but unused [fixable: safe]
````

`ruff_check` accepts a notebook exactly like a plain file, but on a notebook
each diagnostic is cell-mapped: `cell 1` is the 0-based index of the code cell
(the second cell), and `1:8` is the line and column *inside that cell*, not
inside the file. The finding is an unused import, and removing an import is a
safe fix. The `revision:` line is the fingerprint every later write must
chain from.

## Step 2: Apply the safe fix (`ruff_check` with `fix`)

The LLM sends:

````json
{
  "name": "ruff_check",
  "arguments": {
    "path": "examples/demo/messy.ipynb",
    "fix": true,
    "revision": "a9949b5317b716b1"
  }
}
````

tpmcp answers:

````markdown
ruff check: examples/demo/messy.ipynb
applied automatic fixes · revision: c204e80ee0821c4d
no issues found
````

`fix: true` removes the unused import. The one finding here is a safe fix, and
that is all tpmcp applies on its own — anything unsafe stays for a human. The
new revision appears right in the `applied automatic fixes · revision: …` line
(`c204e80ee0821c4d`).

## Step 3: See what the formatter would do (`ruff_format` with `check`)

The spacing in the code cell is still messy, but before writing anything the
AI asks for a dry run:

````json
{
  "name": "ruff_format",
  "arguments": {
    "path": "examples/demo/messy.ipynb",
    "check": true
  }
}
````

tpmcp answers:

````markdown
ruff format (dry run): examples/demo/messy.ipynb
would reformat
revision: c204e80ee0821c4d
````

`check: true` is a dry run: it needs no revision because it only reads, and
it reports `would reformat` without writing. A notebook has no plain-text
diff to show, so the dry run says *what* would happen rather than printing
one. It still returns the current revision, so the in-place call chains
straight from it.

## Step 4: Format the notebook (`ruff_format`)

The LLM sends the in-place call, carrying the revision from the dry run:

````json
{
  "name": "ruff_format",
  "arguments": {
    "path": "examples/demo/messy.ipynb",
    "revision": "c204e80ee0821c4d"
  }
}
````

tpmcp answers:

````markdown
ruff format: examples/demo/messy.ipynb
formatted · revision: 2489d448a38eec0c
````

## Step 5: Verify the notebook is clean (`ruff_check`)

The LLM sends the same check as Step 1:

````json
{
  "name": "ruff_check",
  "arguments": {
    "path": "examples/demo/messy.ipynb"
  }
}
````

tpmcp answers:

````markdown
ruff check: examples/demo/messy.ipynb
revision: 2489d448a38eec0c
no issues found
````

No diagnostics — the same tool that found the unused import now confirms the
notebook is clean.

## Result

The notebook changed like this (`diff -u` against the original):

````diff
--- a/examples/demo/messy.ipynb
+++ b/examples/demo/messy.ipynb
@@ -25,10 +25,8 @@
     }
    ],
    "source": [
-    "import os\n",
-    "\n",
-    "x  =  1\n",
-    "print( x )"
+    "x = 1\n",
+    "print(x)"
    ]
   }
  ],
````

One cell changed, and only its `source` list: the unused import is gone and
the spacing is normalized. The cell's stored `stream(stdout)` output, its
`execution_count`, its `id`, and its `metadata` are byte-for-byte untouched —
`ruff_format` reformats a notebook the way the cell tools edit one, so git
shows exactly the lines that changed.

## The input file

`examples/demo/messy.ipynb` before the session:

````json
{
 "cells": [
  {
   "cell_type": "markdown",
   "id": "9a1b2c3d",
   "metadata": {},
   "source": [
    "# Messy notebook\n",
    "\n",
    "A notebook in the tpmcp examples."
   ]
  },
  {
   "cell_type": "code",
   "execution_count": 1,
   "id": "4e5f6a7b",
   "metadata": {},
   "outputs": [
    {
     "name": "stdout",
     "output_type": "stream",
     "text": [
      "hello\n"
     ]
    }
   ],
   "source": [
    "import os\n",
    "\n",
    "x  =  1\n",
    "print( x )"
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
