# Set up another host

TradingSystem contains two Moomoo simulated paper bots that share one `COMPETITION` account. A Git checkout does not contain credentials, account identifiers, runtime databases, market history, or broker fills. Do not expect GitHub to restore trading state.

## Requirements

- Linux with the existing user-service setup, Rust/Cargo, and Python.
- Moomoo OpenD running locally and authenticated for simulated trading. The bots connect to `127.0.0.1:11111`.
- The pinned Python dependencies listed in `versions.lock`.

## Setup

1. Clone the repository and read the root [`README.md`](../README.md) and [`AGENTS.md`](../AGENTS.md).
2. Configure the `gainer` and `orb` account mappings in the ignored local file `papercompmoomoo/data/account_ids.json`; both must resolve to the same Competition account. Do not add the file to Git or share its numeric value in logs, screenshots, or documentation.
3. Install the Python package and Rust components using the repository setup instructions. Do not start a second bot supervisor if one is already installed.
4. Verify the Gainers and ORB services separately, then open the local dashboard at `http://127.0.0.1:8790`.

Transfer runtime databases and immutable trading evidence only through an approved private backup. Preserve each strategy's separate data and keep the shared configured account in `SIMULATE` mode.
