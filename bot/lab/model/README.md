# Local-model snapshot

This directory versions the lab's trained model files alongside the bot and lab source code. `v1.json` and `v2.json` contain fitted ridge weights, features, calibration and training metadata; `current` selects v2. `monitor.json` and `vol_track.json` preserve the model's validation record. These are exports from the local lab runtime, not synthetic fixtures or trading settings.

The running lab continues to read and write its configured runtime `root/model` directory. On a fresh installation, copy these files into that directory only if it has no newer model; do not overwrite a model that has trained since this snapshot. Refresh and commit this snapshot after later training if that new version should be published. The candle databases, live order/account state and logs remain outside Git; they are not model artifacts. No deep-model champion files existed when this snapshot was taken.

Models are research-only and are not automatically promoted into paper trading.
