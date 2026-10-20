def format_price(price: float) -> str:
    return f"${price:.2f}"


def main() -> None:
    print("calculating")
    price: float = "9.99"
    print(format_price(price))
