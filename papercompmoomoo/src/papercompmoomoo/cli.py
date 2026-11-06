"""Command-line entry point for papercompmoomoo."""


def main() -> None:
    from .bot import main as run_bot

    run_bot()


if __name__ == "__main__":
    main()
