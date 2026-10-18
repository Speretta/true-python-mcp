import marimo

__generated_with = "0.25.1"
app = marimo.App()


@app.cell
def _():
    sql = """
SELECT 1
"""
    print(sql)
    return


@app.cell
def _():
    note = """
        kept
    """
    print(note)
    return


if __name__ == "__main__":
    app.run()
