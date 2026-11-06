"""Keep the Moomoo-only simulated competition bot alive at all hours."""

from __future__ import annotations

import logging
import subprocess
import sys
import time

from .runtime import enter_project

LOGGER = logging.getLogger(__name__)


def main() -> None:
    enter_project()
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    while True:
        # Start the account/audit gates immediately after a restart. The legacy
        # unit suite has stale assumptions and used to add up to five minutes
        # of delay before the live Moomoo connection was even attempted.
        LOGGER.info("Starting current simulated competition bot")
        process = subprocess.Popen([sys.executable, "-m", "papercompmoomoo.bot"])
        return_code = process.wait()
        LOGGER.error("Bot stopped with exit code %s; restarting in 5 seconds", return_code)
        time.sleep(5)


if __name__ == "__main__":
    main()
