# Create a notebook from scratch

The user wants a brand-new Jupyter notebook, not a conversion of an existing
one. `notebook_create` with `cells` makes it from scratch — the path's
`.ipynb` extension picks the Jupyter format — and the same call again is
refused because the file now exists.

> **User asks the AI:** *"Create examples/demo/fresh.ipynb with a title cell
> and a `x = 1` code cell."*

What follows is the real conversation between the AI (the LLM) and tpmcp,
captured from the server. The LLM sends tool calls and sees the answers as
markdown.

## Step 1: Create the notebook (`notebook_create` with `cells`)

The LLM sends:

````json
{
  "name": "notebook_create",
  "arguments": {
    "path": "examples/demo/fresh.ipynb",
    "cells": [
      {
        "cell_type": "markdown",
        "source": "# Fresh notebook\n\nCreated from scratch with notebook_create."
      },
      {
        "cell_type": "code",
        "source": "x = 1\nprint(x)"
      }
    ]
  }
}
````

tpmcp answers:

````markdown
created notebook with 2 cells
path: examples/demo/fresh.ipynb
revision: bac331a9a9284c49
````

`cells` (not `from_path`) is the fresh-create path. The extension picks
nbformat 4.5 Jupyter, the markdown cell becomes a markdown cell, and the code
cell gets the empty `outputs` and `execution_count: null` a never-run cell
carries. tpmcp generates each new cell's `id` on creation, so this revision
is unique to this run — replaying the same call yields a different one
(unlike the fixture-based walkthroughs, whose revisions are fixed by their
fixed content).

## Step 2: The file already exists (`notebook_create` refuses)

The LLM sends the same call a second time:

````json
{
  "name": "notebook_create",
  "arguments": {
    "path": "examples/demo/fresh.ipynb",
    "cells": [
      {
        "cell_type": "markdown",
        "source": "# Fresh notebook\n\nCreated from scratch with notebook_create."
      },
      {
        "cell_type": "code",
        "source": "x = 1\nprint(x)"
      }
    ]
  }
}
````

tpmcp refuses (isError=true):

````markdown
file already exists: examples/demo/fresh.ipynb
````

The machine-readable part (structuredContent):

````json
{"error": "already_exists", "message": "file already exists: examples/demo/fresh.ipynb", "path": "examples/demo/fresh.ipynb"}
````

`notebook_create` is create-only: it never overwrites, so it refuses the
moment the path exists. To change a notebook that already exists, use the
cell tools instead.

## What was written

`examples/demo/fresh.ipynb`, created by Step 1. The two cell `id`s are
generated on creation, so they (and the revision above) differ on every run:

````json
{
 "cells": [
  {
   "cell_type": "markdown",
   "id": "738bd727",
   "metadata": {},
   "source": [
    "# Fresh notebook\n",
    "\n",
    "Created from scratch with notebook_create."
   ]
  },
  {
   "cell_type": "code",
   "execution_count": null,
   "id": "942112f3",
   "metadata": {},
   "outputs": [],
   "source": [
    "x = 1\n",
    "print(x)"
   ]
  }
 ],
 "metadata": {},
 "nbformat": 4,
 "nbformat_minor": 5
}
````

This file is the example's product, not a checked-in input — remove it before
replaying (`rm examples/demo/fresh.ipynb`), or Step 1 will refuse instead of
creating.
