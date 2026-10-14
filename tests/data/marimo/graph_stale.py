import marimo

__generated_with = "0.25.1"
app = marimo.App()


@app.cell
def _():
    alpha = 1
    return


@app.cell
def _(alpha):
    beta = alpha + 1
    return


@app.cell
def _():
    gamma = 3
    return (gamma,)


@app.cell
def _():
    delta = beta * 2
    return (delta,)


if __name__ == "__main__":
    app.run()
