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
    # Bench fixture

    A fixed notebook used by the tpmcp benchmark. Every stack gets the
    same content in its own format. The outputs below are static text:
    no server re-runs them during the benchmark.
    """)
    return


@app.cell
def _():
    import json
    import math

    return json, math


@app.cell(hide_code=True)
def _(mo):
    mo.md(r"""
    ## Data
    """)
    return


@app.cell
def _():
    values = [3, 1, 4, 1, 5, 9, 2, 6]
    return (values,)


@app.cell
def _():
    greeting = "hello bench"
    return


@app.cell
def _(values):
    total = sum(values)
    total
    return (total,)


@app.cell(hide_code=True)
def _(mo):
    mo.md(r"""
    ## Computations
    """)
    return


@app.cell
def _(values):
    for v in values:
        print(v * 2)
    return


@app.cell
def _(total, values):
    mean = total / len(values)
    mean
    return (mean,)


@app.cell
def _(values):
    squares = [v * v for v in values]
    squares
    return


@app.cell(hide_code=True)
def _(mo):
    mo.md(r"""
    ## Summary
    """)
    return


@app.cell
def _(mean, total):
    payload = {"total": total, "mean": mean}
    return (payload,)


@app.cell
def _(json, payload):
    json.dumps(payload, sort_keys=True)
    return


@app.cell
def _(math, mean):
    math.isclose(mean, 3.875)
    return


@app.cell(hide_code=True)
def _(mo):
    mo.md(r"""
    ## Extra cells to give the notebook some weight
    """)
    return


@app.cell
def _(total):
    extra_0 = 0 * total
    extra_0
    return


@app.cell
def _(total):
    extra_1 = 1 * total
    extra_1
    return


@app.cell
def _(total):
    extra_2 = 2 * total
    extra_2
    return


@app.cell
def _(total):
    extra_3 = 3 * total
    extra_3
    return


@app.cell
def _(total):
    extra_4 = 4 * total
    extra_4
    return


@app.cell
def _():
    print("bench complete")
    return


if __name__ == "__main__":
    app.run()
