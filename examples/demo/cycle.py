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
