# Find and fix a dependency cycle

The user's marimo app `examples/demo/cycle.py` never finishes running: its
cells spin forever. This doc shows how the AI finds the cause and fixes it.

> **User asks the AI:** *"My marimo app examples/demo/cycle.py is stuck — cells never finish. What is wrong?"*

What follows is the real conversation between the AI (the LLM) and tpmcp, captured
from the server. The LLM sends tool calls and sees the answers as markdown.

## Step 1: Check the file without touching it (`notebook_check`)

`notebook_check` validates a notebook and writes nothing. The LLM sends:

````json
{
  "name": "notebook_check",
  "arguments": {
    "path": "examples/demo/cycle.py"
  }
}
````

tpmcp answers:

````markdown
notebook_check: examples/demo/cycle.py
format: marimo 0.25.1 · revision: afabafff15ed6aaf
1 finding(s): 1 error(s), 0 warning(s), 0 info

- error cell 0 `cycle`: cells [0, 1] form a dependency cycle via a, b
````

In marimo every cell is a function. Cell 0 needs `b` from cell 1 and cell 1
needs `a` from cell 0: they wait for each other forever. The check only reads.

## Step 2: Look at the two cells (`notebook_read`)

The LLM sends:

````json
{
  "name": "notebook_read",
  "arguments": {
    "path": "examples/demo/cycle.py"
  }
}
````

tpmcp answers:

````markdown
notebook: examples/demo/cycle.py
revision: afabafff15ed6aaf
cells: 2 (2 code, 0 markdown) · marimo 0.25.1

### [0] code · 1 lines
```python
a = b + 1
```

### [1] code · 1 lines
```python
b = a + 1
```
````

The AI sees the loop and picks a fix: cell 1 should produce `b` on its own
instead of deriving it from `a`.

## Step 3: Break the cycle (`cell_set_source`)

The LLM replaces all of cell 1, sending the revision from the read:

````json
{
  "name": "cell_set_source",
  "arguments": {
    "path": "examples/demo/cycle.py",
    "cell": 1,
    "source": "b = 10",
    "revision": "afabafff15ed6aaf"
  }
}
````

tpmcp answers:

````markdown
updated source of cell 1
path: examples/demo/cycle.py
revision: 4bd5ac6f7271df02

### [1] code · 1 lines
```python
   1 | b = 10
```
````

The edit went through and the file got a new revision, `4bd5ac6f7271df02`.

## Step 4: Verify the fix (`notebook_check`)

The LLM sends the same check as in Step 1:

````json
{
  "name": "notebook_check",
  "arguments": {
    "path": "examples/demo/cycle.py"
  }
}
````

tpmcp answers:

````markdown
notebook_check: examples/demo/cycle.py
format: marimo 0.25.1 · revision: 4bd5ac6f7271df02
2 finding(s): 0 error(s), 0 warning(s), 2 info

- info cell 0 `stale-return`: cell 0 returns [a] but marimo would regenerate []; the return is refreshed on marimo's next save
- info cell 1 `stale-signature`: cell 1 takes (a) but marimo would regenerate (); the signature is refreshed on marimo's next save
````

Zero errors — the cycle is gone. The two info findings report drift: cell 1
still says `def _(a):` although it no longer uses `a`. tpmcp does not rewrite
signatures (marimo refreshes them on its next save); the file runs fine.

## Step 5: Lint the file (`ruff_check`)

The LLM sends:

````json
{
  "name": "ruff_check",
  "arguments": {
    "path": "examples/demo/cycle.py"
  }
}
````

tpmcp answers:

````markdown
ruff check: examples/demo/cycle.py
revision: 4bd5ac6f7271df02
no issues found
````

The same ruff that lints plain `.py` files also lints marimo files: no issues.

## Result

The file changed like this (`diff -u` against the original):

````diff
--- a/examples/demo/cycle.py
+++ b/examples/demo/cycle.py
@@ -12,7 +12,7 @@
 
 @app.cell
 def _(a):
-    b = a + 1
+    b = 10
     return (b,)
 
 
````

Two `notebook_check` calls found and verified the problem; one `cell_set_source`
call fixed it. The file never left its exact layout: the diff shows one line.

## The input file

`examples/demo/cycle.py` before the session:

````python
import marimo

__generated_with = "0.25.1"
app = marimo.App()


@app.cell
def _(b):
    a = b + 1
    return (a,)


@app.cell
def _(a):
    b = a + 1
    return (b,)


if __name__ == "__main__":
    app.run()
````
