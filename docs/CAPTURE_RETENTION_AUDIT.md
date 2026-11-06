# Capture and retention audit (historical)

> Snapshot of implementation work, not a current completeness or service-health report. It predates removal of the live market-data converter. Do not use its old converter workflow, restart status, test counts, or listed gaps as current operating facts.

The audit recorded additions to the decision journal, SQLite row-history protections, and `telemetry.py` append-only broker/feed event capture. It also documented historical gaps around event-to-order linkage, full-session feed continuity, reconnect recovery, disk-growth measurement, news data, and clock-offset measurement. Historical missing observations cannot be reconstructed or backfilled as if they had been captured live.

Current telemetry and evidence behavior must be checked in the active Gainer/ORB sources and on the host. The old converter is retired; candidate-scoped Moomoo ingestion is owned by the bots. SQLite immutability triggers protect application history but are not a substitute for filesystem controls or backups.
