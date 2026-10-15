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
