# Find one value in a big notebook — and verify the whole thing

The user has `examples/demo/report.ipynb`, a 40-cell monthly report, and one
value in it changed. Reading 40 cell bodies to find it would burn tokens for
nothing, so the AI navigates with the outline and the query view, hits a
revision conflict because the user edits the same file in VS Code mid-session,
recovers, and verifies the result — findings and lint included — in the same
single read.

> **User asks the AI:** *"In examples/demo/report.ipynb the tax rate changed:
> set it to 0.21, and make sure the notebook is still clean."*

What follows is the real conversation between the AI (the LLM) and tpmcp,
captured from the server. The LLM sends tool calls and sees the answers as
markdown.

## Step 1: The shape of the notebook (`notebook_read`, `detail=outline`)

The LLM sends:
````json
{
  "name": "notebook_read",
  "arguments": {
    "path": "examples/demo/report.ipynb",
    "detail": "outline"
  }
}
````

tpmcp answers — one row per cell instead of 40 fenced bodies:
````markdown
notebook: examples/demo/report.ipynb
revision: 3954eebcbd18aaec
cells: 40 (36 code, 4 markdown) · nbformat 4.5

### [0] markdown · 3 lines · # Monthly report

### [1] code · 1 lines · import statistics

### [2] code · 1 lines · base = 100

### [3] markdown · 1 lines · ## Assumptions

### [4] code · 1 lines · tax_rate = 0.19

### [5] code · 2 lines · revenue_q1 = base * (1 + 0.05 * 1)

### [6] code · 1 lines · cost_q1 = base * 0.6

### [7] code · 1 lines · margin_q1 = revenue_q1 - cost_q1

### [8] code · 2 lines · net_q1 = revenue_q1 * (1 - tax_rate)

### [9] code · 2 lines · revenue_q2 = base * (1 + 0.05 * 2)

### [10] code · 1 lines · cost_q2 = base * 0.6

### [11] code · 1 lines · margin_q2 = revenue_q2 - cost_q2

### [12] code · 2 lines · net_q2 = revenue_q2 * (1 - tax_rate)

### [13] code · 2 lines · revenue_q3 = base * (1 + 0.05 * 3)

### [14] code · 1 lines · cost_q3 = base * 0.6

### [15] code · 1 lines · margin_q3 = revenue_q3 - cost_q3

### [16] code · 2 lines · net_q3 = revenue_q3 * (1 - tax_rate)

### [17] code · 2 lines · revenue_q4 = base * (1 + 0.05 * 4)

### [18] code · 1 lines · cost_q4 = base * 0.6

### [19] code · 1 lines · margin_q4 = revenue_q4 - cost_q4

### [20] code · 2 lines · net_q4 = revenue_q4 * (1 - tax_rate)

### [21] code · 2 lines · revenue_q5 = base * (1 + 0.05 * 5)

### [22] code · 1 lines · cost_q5 = base * 0.6

### [23] code · 1 lines · margin_q5 = revenue_q5 - cost_q5

### [24] code · 2 lines · net_q5 = revenue_q5 * (1 - tax_rate)

### [25] code · 2 lines · revenue_q6 = base * (1 + 0.05 * 6)

### [26] code · 1 lines · cost_q6 = base * 0.6

### [27] code · 1 lines · margin_q6 = revenue_q6 - cost_q6

### [28] code · 2 lines · net_q6 = revenue_q6 * (1 - tax_rate)

### [29] code · 2 lines · revenue_q7 = base * (1 + 0.05 * 7)

### [30] code · 1 lines · cost_q7 = base * 0.6

### [31] code · 1 lines · margin_q7 = revenue_q7 - cost_q7

### [32] code · 2 lines · net_q7 = revenue_q7 * (1 - tax_rate)

### [33] code · 2 lines · revenue_q8 = base * (1 + 0.05 * 8)

### [34] code · 1 lines · cost_q8 = base * 0.6

### [35] code · 1 lines · margin_q8 = revenue_q8 - cost_q8

### [36] code · 2 lines · net_q8 = revenue_q8 * (1 - tax_rate)

### [37] markdown · 1 lines · ## Summary

### [38] code · 2 lines · total_revenue = revenue_q1 + revenue_q2 + revenue_q3 + revenue_q4 + revenue_q5 +

### [39] markdown · 1 lines · _Generated for the tpmcp examples._
````

Each row is the cell's index, type, line count and first line (long lines are
cut at a fixed budget — cell 38's sum is truncated). The AI can see the
`tax_rate` definition at cell 4 without reading a single body.

## Step 2: Page through matches (`notebook_read`, `query` + `limit`)

The LLM sends:
````json
{
  "name": "notebook_read",
  "arguments": {
    "path": "examples/demo/report.ipynb",
    "query": "revenue_q",
    "limit": 5
  }
}
````

tpmcp answers:
````markdown
notebook: examples/demo/report.ipynb
revision: 3954eebcbd18aaec
cells: 40 (36 code, 4 markdown) · nbformat 4.5
query: "revenue_q"
matches: 25 cell(s), 33 line(s)

### [5] code · 2 match(es)
   1 | revenue_q1 = base * (1 + 0.05 * 1)
   2 | print(revenue_q1)

### [7] code · 1 match(es)
   1 | margin_q1 = revenue_q1 - cost_q1

### [8] code · 1 match(es)
   1 | net_q1 = revenue_q1 * (1 - tax_rate)

### [9] code · 2 match(es)
   1 | revenue_q2 = base * (1 + 0.05 * 2)
   2 | print(revenue_q2)

### [11] code · 1 match(es)
   1 | margin_q2 = revenue_q2 - cost_q2

20 more matching cell(s): page with start/limit or narrow the query
````

`query` returns only the matching cells and lines, with the 1-based line
numbers `cell_set_source` ranges use. `start`/`limit` page through the match
list — 25 cells match `revenue_q`, so the first page of 5 comes with a
steering line for the other 20.

## Step 3: Find the definition (`notebook_read`, `query`)

The LLM sends:
````json
{
  "name": "notebook_read",
  "arguments": {
    "path": "examples/demo/report.ipynb",
    "query": "tax_rate",
    "limit": 3
  }
}
````

tpmcp answers:
````markdown
notebook: examples/demo/report.ipynb
revision: 3954eebcbd18aaec
cells: 40 (36 code, 4 markdown) · nbformat 4.5
query: "tax_rate"
matches: 9 cell(s), 9 line(s)

### [4] code · 1 match(es)
   1 | tax_rate = 0.19

### [8] code · 1 match(es)
   1 | net_q1 = revenue_q1 * (1 - tax_rate)

### [12] code · 1 match(es)
   1 | net_q2 = revenue_q2 * (1 - tax_rate)

6 more matching cell(s): page with start/limit or narrow the query
````

Nine cells mention `tax_rate`; the first match is the definition
(`tax_rate = 0.19`, cell 4) and the rest just use it, so cell 4 is the only
edit. The query response also carries the file revision — query views chain
into mutations exactly like full reads.

## Meanwhile, in VS Code

Before the AI's edit lands, the user retitles the notebook in VS Code. The
title line in the file changes to `# Monthly report (draft)`. The AI does not
see it.

## Step 4: The stale edit is refused (`cell_set_source`)

The LLM sends:
````json
{
  "name": "cell_set_source",
  "arguments": {
    "path": "examples/demo/report.ipynb",
    "cell": 4,
    "source": "tax_rate = 0.21",
    "revision": "3954eebcbd18aaec"
  }
}
````

tpmcp refuses (isError=true):
````markdown
revision conflict for examples/demo/report.ipynb: expected 3954eebcbd18aaec, file is now at 52614b24a6b1e18c; re-read the file and retry with its current revision
````

The machine-readable part (structuredContent):
````json
{"current_revision": "52614b24a6b1e18c", "error": "conflict", "expected_revision": "3954eebcbd18aaec", "message": "revision conflict for examples/demo/report.ipynb: expected 3954eebcbd18aaec, file is now at 52614b24a6b1e18c; re-read the file and retry with its current revision", "path": "examples/demo/report.ipynb"}
````

The revision came from step 3, before the user's save — so the write is
refused and nothing on disk changes. The error carries the current revision,
which is all the AI needs to recover.

## Step 5: Re-query for a fresh revision (`notebook_read`)

The LLM sends:
````json
{
  "name": "notebook_read",
  "arguments": {
    "path": "examples/demo/report.ipynb",
    "query": "tax_rate",
    "limit": 1
  }
}
````

tpmcp answers:
````markdown
notebook: examples/demo/report.ipynb
revision: 52614b24a6b1e18c
cells: 40 (36 code, 4 markdown) · nbformat 4.5
query: "tax_rate"
matches: 9 cell(s), 9 line(s)

### [4] code · 1 match(es)
   1 | tax_rate = 0.19

8 more matching cell(s): page with start/limit or narrow the query
````

One small query replaces the re-read: it confirms cell 4 still holds
`tax_rate = 0.19` (the user's edit touched the title, not the tax rate) and
returns the fresh revision.

## Step 6: Retry the edit (`cell_set_source`)

The LLM sends:
````json
{
  "name": "cell_set_source",
  "arguments": {
    "path": "examples/demo/report.ipynb",
    "cell": 4,
    "source": "tax_rate = 0.21",
    "revision": "52614b24a6b1e18c"
  }
}
````

tpmcp answers:
````markdown
updated source of cell 4
path: examples/demo/report.ipynb
revision: 9421a0d00b66e6bc

### [4] code · 1 lines
```python
   1 | tax_rate = 0.21
```
````

## Step 7: Verify in the same read (`notebook_read`, `diagnostics=check+lint`)

The user also asked that the notebook stay clean. Instead of a separate
`notebook_check` + `ruff_check` round-trip, the AI rides the diagnostics on
the next read — with `limit=3`, because only the header and the findings
matter now:

The LLM sends:
````json
{
  "name": "notebook_read",
  "arguments": {
    "path": "examples/demo/report.ipynb",
    "detail": "outline",
    "start": 0,
    "limit": 3,
    "diagnostics": "check+lint"
  }
}
````

tpmcp answers:
````markdown
notebook: examples/demo/report.ipynb
revision: 9421a0d00b66e6bc
cells: 40 (36 code, 4 markdown) · nbformat 4.5

### [0] markdown · 3 lines · # Monthly report (draft)

### [1] code · 1 lines · import statistics

### [2] code · 1 lines · base = 100

diagnostics: 0 finding(s), 1 lint issue(s)

- lint cell 1 1:8 F401 `statistics` imported but unused [fixable: safe]
````

The notebook is structurally valid (0 findings), and the lint half — ruff
running over the same snapshot, mapped to cell indexes — flags one real
leftover: cell 1 imports `statistics` and nothing in the notebook uses it.
The outline rows also confirm the user's `(draft)` retitle survived: the
refused write of step 4 touched nothing.

## Step 8: Remove the unused import (`cell_delete`)

The LLM sends:
````json
{
  "name": "cell_delete",
  "arguments": {
    "path": "examples/demo/report.ipynb",
    "cell": 1,
    "revision": "9421a0d00b66e6bc"
  }
}
````

tpmcp answers:
````markdown
deleted code cell 1
path: examples/demo/report.ipynb
revision: f4a598f6c0a15eb3
````

## Step 9: Final verification (`notebook_read`)

The LLM sends:
````json
{
  "name": "notebook_read",
  "arguments": {
    "path": "examples/demo/report.ipynb",
    "detail": "outline",
    "start": 0,
    "limit": 2,
    "diagnostics": "check+lint"
  }
}
````

tpmcp answers:
````markdown
notebook: examples/demo/report.ipynb
revision: f4a598f6c0a15eb3
cells: 39 (35 code, 4 markdown) · nbformat 4.5

### [0] markdown · 3 lines · # Monthly report (draft)

### [1] code · 1 lines · base = 100

diagnostics: 0 finding(s), 0 lint issue(s)
no findings: the notebook is valid
````

39 cells, zero findings, zero lint issues.

## Result

The file changed exactly where it should (`diff -u` against the original):
````diff
--- a/examples/demo/report.ipynb
+++ b/examples/demo/report.ipynb
@@ -5,23 +5,13 @@
    "id": "abd56ec5",
    "metadata": {},
    "source": [
-    "# Monthly report\n",
+    "# Monthly report (draft)\n",
     "\n",
     "Forty cells of very important numbers."
    ]
   },
   {
    "cell_type": "code",
-   "execution_count": 1,
-   "id": "3a946cdd",
-   "metadata": {},
-   "outputs": [],
-   "source": [
-    "import statistics"
-   ]
-  },
-  {
-   "cell_type": "code",
    "execution_count": 2,
    "id": "68058b3e",
    "metadata": {},
@@ -45,7 +35,7 @@
    "metadata": {},
    "outputs": [],
    "source": [
-    "tax_rate = 0.19"
+    "tax_rate = 0.21"
    ]
   },
   {
````

All three edits survived: the user's retitle, the new tax rate, and the
removed unused import. The whole flow — find the cell, edit it, verify the
notebook — cost two mutating round-trips plus reads that double as
navigation and verification.

## The input file

`examples/demo/report.ipynb` (40 cells, generated with the real `nbformat`
library, outputs stored as real executions produce them): a markdown title,
an unused `import statistics`, `base = 100`, the `tax_rate = 0.19`
definition, then eight quarters of revenue/cost/margin/net cells — every
`net_qN` computed from `tax_rate` — and a summary. Too long to quote here;
`git checkout examples/demo` restores it after you replay the flow.
