import marimo

__generated_with = "0.25.1"
app = marimo.App()

with app.setup:
    import marimo as mo

    banner = mo.md(payload)


@app.cell
def _():
    payload = "# Title"
    return (payload,)


if __name__ == "__main__":
    app.run()
