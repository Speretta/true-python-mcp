import marimo

__generated_with = "0.25.1"
app = marimo.App()


@app.cell
def _():
    config = {"a": 1}
    return (config,)


@app.cell
def _():
    config = {"b": 2}
    return (config,)


@app.cell
def _(config):
    print(config)
    return


if __name__ == "__main__":
    app.run()
