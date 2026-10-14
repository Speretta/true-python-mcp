import marimo

__generated_with = "0.25.1"
app = marimo.App()


@app.cell
def _(y):
    x = y + 1
    return (x,)


@app.cell
def _(x):
    y = x + 1
    return (y,)


if __name__ == "__main__":
    app.run()
