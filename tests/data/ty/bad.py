x: int = "not an int"


def bad_return() -> int:
    return "not an int"


def add(a: int, b: int) -> int:
    return a + b


add(1, "two")
