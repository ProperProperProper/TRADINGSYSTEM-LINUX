# Archived Rust OpenD SDK comparison

> Historical comparison from 2026-10-18 for the archived Rust bot under `bot/`. It is not a current SDK compatibility audit and does not describe the Python Moomoo OpenD clients used by Gainers and ORB.

The comparison checked protocol framing, keep-alive, encryption, push handling, and reconnect behavior against the then-installed SDK. Its proposed follow-up order and quota assumptions applied to the old custom Rust client. Do not use it to determine current live feed behavior, entitlements, or Moomoo API limits. For the active bots, inspect the pinned Python SDK integration, `papercompmoomoo/src/papercompmoomoo/moomoo_feed.py`, and current runtime telemetry.
