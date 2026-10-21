# Type-check a Python file and a notebook

The user has the same pricing helper in two forms: a plain module
`examples/demo/ty_bug.py` and a Jupyter notebook `examples/demo/ty_bug.ipynb`.
Both annotate a variable as `float` and assign it the string `"9.99"`. The user
wants the AI to type-check both with `ty_check` and fix the notebook.

> **User asks the AI:** *"Type-check examples/demo/ty_bug.py and
> examples/demo/ty_bug.ipynb, then fix the notebook."*

What follows is the real conversation between the AI (the LLM) and tpmcp,
captured from the server. The LLM sends tool calls and sees the answers as
markdown.

## Step 1: Type-check the module (`ty_check`)

`ty_check` runs Astral's type checker on the file (compiled in, no
interpreter, no subprocess) and writes nothing. The LLM sends:

````json
{
  "name": "ty_check",
  "arguments": {
    "path": "examples/demo/ty_bug.py"
  }
}
````

tpmcp answers:

````markdown
ty check: examples/demo/ty_bug.py
revision: 48d8e3328fd98d1d
1 diagnostic(s)

7:20 invalid-assignment Object of type `Literal["9.99"]` is not assignable to `float`
````

The answer names the file, reports the current revision on the
`revision: 48d8e3328fd98d1d` line (a fingerprint of the file contents) and
lists one diagnostic: `invalid-assignment` at line 7, column 20. The message
names both types: the `Literal["9.99"]` string is not assignable to the `float`
annotation.

## Step 2: Type-check the notebook (`ty_check`)

The same check on the notebook:

````json
{
  "name": "ty_check",
  "arguments": {
    "path": "examples/demo/ty_bug.ipynb"
  }
}
````

tpmcp answers:

````markdown
ty check: examples/demo/ty_bug.ipynb
revision: ab44d46c50b3e283
1 diagnostic(s)

cell 2 2:16 invalid-assignment Object of type `Literal["9.99"]` is not assignable to `float`
````

Same code, same message, but the position is written for a notebook. `cell 2`
is the 0-based cell index, and `2:16` is the line and column inside that cell,
not inside the whole file. Cell 2 is the third cell (indexes start at 0), and
its second line is the `price: float = "9.99"` assignment.

## Step 3: Read the offending cell (`notebook_read`)

The AI reads the notebook before editing, so it sees the exact cell it is about
to change:

````json
{
  "name": "notebook_read",
  "arguments": {
    "path": "examples/demo/ty_bug.ipynb"
  }
}
````

tpmcp answers:

````markdown
notebook: examples/demo/ty_bug.ipynb
revision: ab44d46c50b3e283
cells: 3 (2 code, 1 markdown) · nbformat 4.5

### [0] markdown · 1 lines · id 0e42b380
```markdown
# Pricing helper
```

### [1] code · 2 lines · id 431c582c
```python
def format_price(price: float) -> str:
    return f"${price:.2f}"
```

### [2] code · 3 lines · id 1c22426d
```python
print("calculating")
price: float = "9.99"
print(format_price(price))
```
````

The read confirms the layout. Cell 2 holds three lines, and its second line is
exactly the assignment the diagnostic pointed at (cell-relative line 2). The
read returns the same revision as the check (`ab44d46c50b3e283`), so the next
call can chain from it.

## Step 4: Fix the cell (`cell_set_source`)

The LLM replaces the whole cell (the string becomes a float), carrying the
revision from the read:

````json
{
  "name": "cell_set_source",
  "arguments": {
    "path": "examples/demo/ty_bug.ipynb",
    "cell": 2,
    "source": "print(\"calculating\")\nprice: float = 9.99\nprint(format_price(price))",
    "revision": "ab44d46c50b3e283"
  }
}
````

tpmcp answers:

````markdown
updated source of cell 2
path: examples/demo/ty_bug.ipynb
revision: fe042f360f796138

### [2] code · 3 lines
```python
   1 | print("calculating")
   2 | price: float = 9.99
   3 | print(format_price(price))
```
````

The edit landed and the file got a new revision, `fe042f360f796138`. Only
cell 2 changed.

## Step 5: Verify the fix (`ty_check`)

The LLM sends the same check as Step 2:

````json
{
  "name": "ty_check",
  "arguments": {
    "path": "examples/demo/ty_bug.ipynb"
  }
}
````

tpmcp answers:

````markdown
ty check: examples/demo/ty_bug.ipynb
revision: fe042f360f796138
no type errors found
````

No diagnostics: `no type errors found`. The same tool that found the error now
confirms it is gone. `examples/demo/ty_bug.py` is left as it is: `ty_check` is
read-only and tpmcp's cell tools edit notebooks, not plain modules, so the AI
reports the module's `7:20 invalid-assignment` line to the user instead of
rewriting their file.

## Result

The notebook changed like this (`diff -u` against the original):

````diff
--- a/examples/demo/ty_bug.ipynb
+++ b/examples/demo/ty_bug.ipynb
@@ -27,7 +27,7 @@
    "outputs": [],
    "source": [
     "print(\"calculating\")\n",
-    "price: float = \"9.99\"\n",
+    "price: float = 9.99\n",
     "print(format_price(price))"
    ]
   }
````

One line, one cell: the byte-exact edit the notebook tools always make.
`examples/demo/ty_bug.py` is unchanged on disk; the AI handed the user its
`7:20 invalid-assignment` line to fix in their editor. Two `ty_check` calls
found and verified the error; one `cell_set_source` call fixed it.

## The input files

`examples/demo/ty_bug.py` before the session:

````python
def format_price(price: float) -> str:
    return f"${price:.2f}"


def main() -> None:
    print("calculating")
    price: float = "9.99"
    print(format_price(price))
````

`examples/demo/ty_bug.ipynb` before the session:

````json
{
 "cells": [
  {
   "cell_type": "markdown",
   "id": "0e42b380",
   "metadata": {},
   "source": [
    "# Pricing helper"
   ]
  },
  {
   "cell_type": "code",
   "execution_count": null,
   "id": "431c582c",
   "metadata": {},
   "outputs": [],
   "source": [
    "def format_price(price: float) -> str:\n",
    "    return f\"${price:.2f}\""
   ]
  },
  {
   "cell_type": "code",
   "execution_count": null,
   "id": "1c22426d",
   "metadata": {},
   "outputs": [],
   "source": [
    "print(\"calculating\")\n",
    "price: float = \"9.99\"\n",
    "print(format_price(price))"
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
