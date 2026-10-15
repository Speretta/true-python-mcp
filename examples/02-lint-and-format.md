# Lint and format a plain Python file

The user has a plain Python file at `examples/demo/messy.py`. No notebook is
involved — this is an ordinary `.py` file. It has unused imports, an unused
local variable, and messy spacing. The user wants the AI to lint it, apply the
safe fixes, and format it.

> **User asks the AI:** *"Lint examples/demo/messy.py, apply the safe fixes,
> and format it."*

What follows is the real conversation between the AI (the LLM) and tpmcp,
captured from the server. The LLM sends tool calls and sees the answers as
markdown.

## Step 1 — Lint the file (`ruff_check`)

The LLM sends:

````json
{
  "name": "ruff_check",
  "arguments": {
    "path": "examples/demo/messy.py"
  }
}
````

tpmcp answers:

````markdown
ruff check: examples/demo/messy.py
revision: e27ce6a2ecf6cfa0
3 diagnostic(s), 3 fixable

1:8 F401 `os` imported but unused [fixable: safe]
2:8 F401 `sys` imported but unused [fixable: safe]
10:5 F841 Local variable `unused` is assigned to but never used [fixable: unsafe]
````

The answer lists three findings and reports the current revision on the
`revision: e27ce6a2ecf6cfa0` line. Every tpmcp response reports the revision,
so the next call can chain from it. Note the two labels: removing an unused
import is a safe fix, but removing the unused variable is marked unsafe,
because deleting code can change behavior.

## Step 2 — Apply the safe fixes (`ruff_check` with `fix`)

The LLM sends:

````json
{
  "name": "ruff_check",
  "arguments": {
    "path": "examples/demo/messy.py",
    "fix": true,
    "revision": "e27ce6a2ecf6cfa0"
  }
}
````

tpmcp answers:

````markdown
ruff check: examples/demo/messy.py
applied automatic fixes · revision: f34960ad3fe50f4a
1 diagnostic(s), 1 fixable

8:5 F841 Local variable `unused` is assigned to but never used [fixable: unsafe]
````

`fix: true` applies only the safe fixes: the two unused imports are gone. The
F841 finding stays, and it moved from line 10 to line 8 because the file is
now two lines shorter. tpmcp does not delete that variable on its own — that
would be an unsafe fix. The AI decides what to do with it, and the honest
choice is to report it to the user instead of silently deleting code. The new
revision appears right in the `applied automatic fixes · revision: …` line
(`f34960ad3fe50f4a`); that is the revision the next call must carry.

## Step 3 — Format the file (`ruff_format`)

The LLM sends:

````json
{
  "name": "ruff_format",
  "arguments": {
    "path": "examples/demo/messy.py",
    "revision": "f34960ad3fe50f4a"
  }
}
````

tpmcp answers:

````markdown
ruff format: examples/demo/messy.py
formatted · revision: 14aa2ccd320f5d80
````

This call chains from Step 2, not Step 1: it carries `f34960ad3fe50f4a`, the
revision the fix call returned. With the right revision the format succeeds,
the spacing is normalized, and the answer reports yet another revision
(`14aa2ccd320f5d80`) for whatever comes next.

## Result

The file changed like this (`diff -u` against the original):

````diff
--- a/examples/demo/messy.py
+++ b/examples/demo/messy.py
@@ -1,11 +1,7 @@
-import os
-import sys
-
-
 def greet(name):
-    return "hello "+name
+    return "hello " + name
 
 
-def  main( ):
+def main():
     unused = 42
-    print(greet( "world" ))
+    print(greet("world"))
````

The two unused imports are gone and the spacing is fixed, in exactly the lines
the tools touched. The `unused = 42` line stays: F841 is an unsafe fix, so it
is left for a human decision, and the AI tells the user about it in its final
answer. Every step chained revisions — read, fix, format — so each write
landed on a file state the AI had actually seen.

## The input file

`examples/demo/messy.py` before the session:

````python
import os
import sys


def greet(name):
    return "hello "+name


def  main( ):
    unused = 42
    print(greet( "world" ))
````
