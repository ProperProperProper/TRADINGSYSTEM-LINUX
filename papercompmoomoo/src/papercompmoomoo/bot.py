"""High-momentum Moomoo paper-trading bot.

This module is deliberately restricted to Moomoo's simulated environment.
It does not support real-account order placement.
"""

from __future__ import annotations

import logging
import math
import os
import json
import sqlite3
import time
from urllib.request import urlopen
from dataclasses import dataclass
from datetime import date, datetime, timedelta, timezone
from pathlib import Path

from .account import KIND as ACCOUNT_KIND, STANDARD_KIND, pick_account
from .account_ids import account_id
from .shared_account import bind_order, owner_of as shared_owner_of, owned_symbols, release, reserve, sync as sync_shared_account
from .shared_movers import publish as publish_shared_movers
from .auditor import evaluate
from .deploy import code_hash
from .journal import Journal
from .moomoo_feed import MoomooFeed
from . import board as messages
from . import brain as brain_mod
from . import darwin_qos
from . import gating
from . import microsense
from . import dblog
from . import owner_rules
from . import planner
from . import swings
from .candlestore import CandleStore
from .signals import (PRE_OPEN_REMARK, WORKING_ORDER_STATUSES, orders_to_cancel, average_daily_volume, candidate_tape_activity, may_trade, nasdaq_listed_symbols, order_gate, size_order, symbol_profile)
from futu import (
    OpenQuoteContext,
    OpenSecTradeContext,
    ModifyOrderOp,
    OrderType,
    RET_OK,
    TrdEnv,
    TrdMarket,
    TrdSide,
    Session,
)


LOGGER = logging.getLogger(__name__)
MAX_CANDIDATE_QUOTE_AGE_MS = 30_000

def candidate_quote_age_ms(quote: dict, now: datetime) -> int | None:
    """Return the older of Moomoo trade-quote and executable-book provider ages."""
    stamp=quote.get('time') if isinstance(quote,dict) else None
    if not stamp:return None
    try:
        value=datetime.fromisoformat(str(stamp))
        if value.tzinfo is None:value=value.replace(tzinfo=planner.NEW_YORK)
        ages=[int((now.astimezone(timezone.utc)-value.astimezone(timezone.utc)).total_seconds()*1000)]
        if 'book_time' in quote:
            book_stamp=quote.get('book_time')
            if not book_stamp:return None
            book_value=datetime.fromisoformat(str(book_stamp))
            if book_value.tzinfo is None:book_value=book_value.replace(tzinfo=planner.NEW_YORK)
            ages.append(int((now.astimezone(timezone.utc)-book_value.astimezone(timezone.utc)).total_seconds()*1000))
        return max(ages)
    except (TypeError,ValueError,OverflowError):
        return None


EXIT_FILL_GRACE_SECONDS = 120.0  # an exit's fill can lag: for this long the account still shows the shares


@dataclass(frozen=True)
class Settings:
    """Runtime strategy and connection settings."""

    host: str = "127.0.0.1"
    port: int = 11111
    symbols: tuple[str, ...] = (
        # Leveraged/index volatility vehicles.
        "US.SOXL",
        "US.SOXS",
        "US.TQQQ",
        "US.SQQQ",
        "US.LABU",
        "US.LABD",
        "US.NUGT",
        "US.DUST",
        "US.BITU",
        "US.BITX",
        # Liquid high-beta technology and semiconductor names.
        "US.NVDA",
        "US.TSLA",
        "US.AMD",
        "US.ARM",
        "US.SMCI",
        "US.MU",
        "US.AVGO",
        "US.PLTR",
        "US.MSTR",
        "US.HOOD",
        # Crypto, mining, and trading-platform momentum names.
        "US.MARA",
        "US.RIOT",
        "US.CLSK",
        "US.HUT",
        "US.COIN",
        "US.CIFR",
        # Speculative growth, space, quantum, and EV names.
        "US.RKLB",
        "US.ASTS",
        "US.IONQ",
        "US.QBTS",
        "US.RGTI",
        "US.U",
        "US.RIVN",
        "US.LCID",
        "US.OKLO",
        # Retail/meme and event-driven momentum names.
        "US.GME",
        "US.AMC",
        "US.CVNA",
        "US.DJT",
        "US.BBAI",
        # Additional liquid momentum and event-driven names.
        "US.AAPL",
        "US.MSFT",
        "US.AMZN",
        "US.GOOGL",
        "US.META",
        "US.NFLX",
        "US.BABA",
        "US.PDD",
        "US.NIO",
        "US.XPEV",
        "US.LULU",
        "US.NKE",
        "US.SNAP",
        "US.PINS",
        "US.SOFI",
        "US.PYPL",
        "US.SQ",
        "US.TOST",
        "US.DKNG",
        "US.ROKU",
        "US.RDDT",
        "US.TWLO",
        "US.DASH",
        "US.AFRM",
        "US.UPST",
        "US.OPEN",
        "US.HIMS",
        "US.TEM",
        "US.SOUN",
        "US.AI",
        "US.BE",
        "US.PLUG",
        "US.ENVX",
        "US.JOBY",
        "US.WKHS",
        "US.CHPT",
        "US.FUBO",
        "US.WBD",
        "US.PARA",
        "US.VFC",
        "US.BYND",
        "US.W",
        "US.ETSY",
        "US.IREN",
        "US.KOSS",
        "US.BILI",
        "US.BEKE",
        "US.KWEB",
        "US.YINN",
        "US.YANG",
        "US.TMF",
        "US.TMV",
        "US.FAS",
        "US.FAZ",
        "US.SARK",
        "US.DRIP",
        "US.UDOW",
        "US.SDOG",
        "US.TNA",
        "US.TZA",
    )
    baseline_sessions: int = 21
    candle_interval: str = "1m"
    order_quantity: int = 100
    target_leverage: float = 2.0  # owner 2026-10-21: 2x leverage (it was 11x)
    max_position_funds: float | None = None  # size from live shared buying power and risk unless explicitly configured
    allocated_capital: float | None = None  # User-configured virtual capital limit (e.g. $5000)
    account_kind: str = ACCOUNT_KIND
    buying_power_buffer: float = 0.01
    max_open_positions: int = 15  # owner 2026-10-21: up to 15 positions (the daily plan's size)
    audit_seconds: int = 300  # the bot audits itself this often (and at every start)
    stale_order_seconds: int = 30  # unfilled entry orders expire quickly; do not leave an entry working unattended
    risk_per_trade: float = 0.005  # loss budget per trade as a share of equity, divided by the symbol's own stop distance
    bar_participation: float = 0.10  # at most this share of the last closed bar's volume ...
    daily_participation: float = 0.02  # ... and of the average daily volume
    min_price: float = 0.01  # penny stocks allowed; size_order protects share count
    max_position_pct: float = 0.40
    max_orders_per_day: int = 200
    plan_enabled: bool = os.environ.get("PAPERCOMP_PLAN", "1") != "0"  # the daily routine (planner.py)
    plan_size: int = 15  # candidates bought at the open (owner 2026-10-21: at most 15)
    plan_lead_hours: float = 5.5  # start premarket candidate orders at 04:00 ET, when US premarket opens
    plan_retry_max: int = 6
    snapshot_reuse_seconds: float = 2.5  # an account read this recent is reused (reconcile and the order that follows it are one read)
    accinfo_min_gap_seconds: float = 3.1  # Moomoo allows 10 account reads per 30 seconds: reads are spaced so a burst of orders (15 candidates) is never refused for frequency
    plan_pre_retry_seconds: int = 30  # refresh pre-open plan orders no faster than their short working-order timeout
    plan_retry_seconds: int = 10
    plan_pre_order_grace_seconds: int = 30  # an unfilled pre-open order is cancelled at the open; RTH re-evaluates before entry
    stop_loss_pct: float = 2.0
    trailing_stop_pct: float = 3.0
    take_profit_pct: float = 0.0
    discover_exchange_universe: bool = True
    baseline_cache_file: str = "data/baselines.json"  # only read once, to keep the work an earlier version did
    baseline_max_age_days: int = 7
    history_db: str = "data/common1m.sqlite3"
    journal_db: str = "data/journal.sqlite3"
    live_shortlist_size: int = 300
    poll_seconds: int = 5
    watch_seconds: int = 20  # how often the account is re-read while something is held or an order is working
    scan_settle_seconds: int = 20  # settle time for a newly closed one-minute bar
    request_pause_seconds: int = 1


SESSION_BARS = 390  # one-minute bars in a regular US session


def live_quote_profile(frame, quote: dict | None, now: datetime, baseline: float | None) -> tuple[dict, float]:
    """Sizing inputs from candidate-scoped Moomoo bars and its current Moomoo quote."""
    profile = symbol_profile(frame[frame.index + timedelta(minutes=1) <= now]) if frame is not None and not frame.empty else {"last_bar_volume": 0.0, "typical_bar_volume": 0.0, "median_range": 0.0}
    live_volume = float((quote or {}).get("volume") or 0.0)
    if profile["last_bar_volume"] <= 0 and live_volume > 0:
        per_bar = live_volume / SESSION_BARS
        profile = {**profile, "last_bar_volume": per_bar, "typical_bar_volume": per_bar}
    if not baseline or baseline <= 0:
        baseline = live_volume
    return profile, float(baseline or 0.0)


def planner_remark() -> str:
    return PRE_OPEN_REMARK


class MomentumPaperBot:
    """Sense native one-minute gainers and submit simulated limit entries."""

    def __init__(self, settings: Settings | None = None) -> None:
        self.settings = settings or Settings()
        if self.settings.candle_interval != "1m" or "1m" not in self.settings.history_db:
            raise ValueError("the gainer bot requires native one-minute data")
        self.feed = MoomooFeed(OpenQuoteContext(host=self.settings.host, port=self.settings.port), minutes=1)  # Moomoo live data for candidates and positions, used sparingly
        self.exit_sent: dict[str, float] = {}  # symbol -> monotonic time of the last exit order
        self.trade_context = OpenSecTradeContext(
            filter_trdmarket=TrdMarket.US,
            host=self.settings.host,
            port=self.settings.port,
        )
        # Candles and baselines live on disk (SQLite), updated with only the new bars; nothing large is kept in memory between scans.
        from .telemetry import RecordedContext, emit
        self.trade_context = RecordedContext(self.trade_context, 'gainer_trade')
        emit('bot_configuration', vars(self.settings), actor='gainer')
        from .telemetry import install_handlers
        install_handlers(self.trade_context, trade=True)
        self.COMMON_DB = Path(self.settings.history_db)
        self.store = CandleStore(self.settings.history_db, bar_ms=60_000)
        self.board = messages.connect()  # the agents' message board (board.py)
        owner_rules.pin_hard_rules(self.board)
        brain_mod.LIVE = True
        self.starting_capital: float = self.settings.allocated_capital if self.settings.allocated_capital is not None else 100_000.0
        self.guard = owner_rules.CapitalGuard()  # the owner's circuit breaker: no new orders past the loss limits
        self.journal = Journal(self.settings.journal_db)  # what it saw and did, for analysis (TradingSystem reads it)
        self.entered_symbols: set[str] = set()
        self.working_orders: set[str] = set()
        self.order_reprice_attempts: dict[int, int] = {}
        self.acc_id: int | None = None  # the competition account, resolved from the account list; None = trade nothing
        # The paper-trading competition is for Nasdaq: orders go only to Nasdaq-listed symbols. NYSE (and the rest of the universe) is still downloaded and scanned.
        self.nasdaq_symbols: frozenset[str] = self.load_nasdaq_cache()
        self.symbols = self.load_exchange_universe() if self.settings.discover_exchange_universe else self.settings.symbols
        self.positions: dict[str, dict[str, float | datetime | int]] = {}
        self.order_day = date.today()
        self.orders_today = 0
        self.phase_cache: tuple[float, str] = (0.0, "closed")
        self.swing_cache: dict = {}
        self.status_path = Path("data/status.json")
        self.clear_stale_status()
        self.write_status(state="connected", universe=len(self.symbols), trading_exchange="NASDAQ", nasdaq_symbols=len(self.nasdaq_symbols), code_hash=code_hash())

    STALE_STATUS_KEYS = ("account", "watchlist", "last_candidates", "positions_live", "last_scanned_bar", "batch", "batches", "error", "phase", "time")

    def clear_stale_status(self) -> None:
        """A new process starts from what it reads itself: whatever the previous one left in status.json (account, candidates, live positions) is removed, not shown as current."""
        try:
            current = json.loads(self.status_path.read_text())
        except (OSError, ValueError):
            return
        if any(k in current for k in self.STALE_STATUS_KEYS):
            self.status_path.write_text(json.dumps({k: v for k, v in current.items() if k not in self.STALE_STATUS_KEYS}, indent=2, default=str))

    def resolve_account(self) -> bool:
        """Find the trading account matching settings.account_kind (defaulting to the configured Moomoo COMPETITION account)."""
        ret, accounts = self.trade_context.get_acc_list()
        found = pick_account(accounts, self.settings.account_kind) if ret == RET_OK else None
        if found != account_id("gainer"):
            found = None
        if found != self.acc_id:
            LOGGER.warning("Trading account: %s", f"Account {found} ({self.settings.account_kind})" if found else f"no {self.settings.account_kind} simulated US account found; trading nothing")
        self.acc_id = found
        self.write_status(account_kind=self.settings.account_kind, acc_id=found, trading_exchange="NASDAQ", allocated_capital=self.virtual_trading_balance())
        return found is not None

    def baseline_cutoff(self) -> str:
        return (date.today() - timedelta(days=self.settings.baseline_max_age_days)).isoformat()

    def write_status(self, **updates: object) -> None:
        """Write dashboard state atomically so it is always readable."""
        try:
            current = json.loads(self.status_path.read_text()) if self.status_path.exists() else {}
        except (OSError, json.JSONDecodeError):
            current = {}
        current.update(updates)
        if updates.get("state") in ("connected", "scanning", "outside_regular_session"):
            current.pop("error", None)  # a message the supervisor left while the bot was blocked ("tests failed") must not outlive the block
        now_iso = datetime.now(timezone.utc).isoformat()
        current.update({
            "phase": self.phase_cache[1],
            "time": now_iso,
            "orders_today": self.orders_today,
            "open_positions": list(self.positions),
            "baseline_count": self.store.baseline_count(self.baseline_cutoff()),
            "capital_guard": self.guard.summary(),
            "updated_at": now_iso,
        })
        self.status_path.parent.mkdir(parents=True, exist_ok=True)
        temporary = self.status_path.with_suffix(".tmp")
        temporary.write_text(json.dumps(current, indent=2, default=str))
        from .telemetry import preserve_file
        preserve_file(self.status_path)
        temporary.replace(self.status_path)

    @staticmethod
    def load_nasdaq_cache() -> frozenset[str]:
        """The tradable Nasdaq-listed symbols found the last time the listing was read."""
        for p in ("data/nasdaq_common.json", "papercompmoomoo/data/nasdaq_common.json"):
            path = Path(p)
            if path.exists():
                try:
                    return frozenset(json.loads(path.read_text()))
                except (OSError, ValueError, TypeError):
                    pass
        return frozenset()

    def load_exchange_universe(self) -> tuple[str, ...]:
        """Discover current Nasdaq common symbols from Nasdaq Trader."""
        import pandas as pd
        from io import StringIO
        from urllib.request import urlopen

        universe_path = Path("data/universe.json")
        if not universe_path.parent.exists():
            universe_path = Path("papercompmoomoo/data/universe.json")
        found: set[str] = set()
        nasdaq_found: set[str] = set()
        try:
            nasdaq = pd.read_csv(
                StringIO(urlopen("https://www.nasdaqtrader.com/dynamic/symdir/nasdaqlisted.txt", timeout=15).read().decode()),
                sep="|",
            )
            nasdaq_found = set(nasdaq_listed_symbols(nasdaq))
            nasdaq = nasdaq[nasdaq["Test Issue"] == "N"]
            found.update(nasdaq["Symbol"].dropna().astype(str))
        except Exception:
            if universe_path.exists():
                try:
                    cached = tuple(json.loads(universe_path.read_text()))
                    LOGGER.warning("Using cached exchange universe of %d symbols", len(cached))
                    return cached
                except (OSError, ValueError, TypeError):
                    pass
            LOGGER.exception("Exchange-universe discovery failed; using configured fallback list")
            return self.settings.symbols

        symbols = tuple(f"US.{symbol}" for symbol in sorted(found) if symbol.isalnum() and len(symbol) <= 5)
        universe_path.parent.mkdir(parents=True, exist_ok=True)
        universe_path.write_text(json.dumps(symbols))
        self.nasdaq_symbols = frozenset(nasdaq_found & set(symbols))
        for out_dir in (Path("data"), Path("papercompmoomoo/data")):
            if out_dir.exists():
                (out_dir / "nasdaq.json").write_text(json.dumps(sorted(self.nasdaq_symbols)))
                (out_dir / "nasdaq_common.json").write_text(json.dumps(sorted(self.nasdaq_symbols)))
        LOGGER.info("Discovered %d Nasdaq symbols (%d tradable)", len(symbols), len(self.nasdaq_symbols))
        return symbols or self.settings.symbols

    def import_earlier_baselines(self) -> int:
        """Keep the work of the earlier version: its cache (today's average volumes) becomes disk baselines, so those symbols are not downloaded again."""
        try:
            cached = json.loads(Path(self.settings.baseline_cache_file).read_text())
            if cached.get("date") != date.today().isoformat() or cached.get("interval") != self.settings.candle_interval:
                return 0
            symbols = cached["symbols"]
        except (OSError, KeyError, TypeError, ValueError):
            return 0
        known = self.store.fresh_symbols(self.baseline_cutoff())
        added = 0
        for code, value in symbols.items():
            ticker = code.removeprefix("US.")
            if ticker not in known:
                self.store.set_baseline(ticker, float(value[0]), cached["date"])
                added += 1
        return added

    def record_baseline(self, ticker: str) -> None:
        """Average daily volume of one symbol, from its stored bars (21 complete sessions)."""
        frame = self.store.moomoo_frame(ticker, datetime.now(timezone.utc) - timedelta(days=45))
        average = average_daily_volume(frame, self.settings.baseline_sessions, minimum=1)  # whatever history exists: a new listing is traded on its few sessions
        # 0.0 = checked but too little history (a new or thin listing): not asked for again until the baselines are refreshed next week
        self.store.set_baseline(ticker, average or 0.0, date.today().isoformat())

    def ensure_baselines(self) -> None:
        """Refresh baselines only from Moomoo candles already stored locally.

        Candidate discovery uses Moomoo's live leaderboard RVOL first, so a
        missing historical baseline never triggers a second market-data
        provider or blocks the live candidate path.
        """
        tickers = [symbol.removeprefix("US.") for symbol in self.symbols]
        imported = self.import_earlier_baselines()
        todo = [t for t in tickers if t not in self.store.fresh_symbols(self.baseline_cutoff())]
        for offset in range(0, len(todo), 100):
            for ticker in todo[offset:offset + 100]:
                if not self.store.moomoo_frame(ticker).empty:
                    self.record_baseline(ticker)
            if self.acc_id is not None:
                self.account_snapshot(max_age=0.0)
            self.write_status(
                state="scanning",
                baseline_source="moomoo_only",
                baseline_count=self.store.baseline_count(self.baseline_cutoff()),
                baseline_scanned=min(offset + 100, len(todo)),
                baseline_total=len(todo),
            )
        LOGGER.info(
            "Moomoo-only baselines ready: %d symbols (%d imported cache entries); missing symbols wait for candidate capture",
            self.store.baseline_count(self.baseline_cutoff()), imported,
        )

    def broker_max_buy(self, symbol: str, price: float) -> float | None:
        """The most shares the account may buy of ``symbol`` now, cash and margin (the broker's own figure); None when it cannot be read."""
        if self.acc_id is None:
            return None
        ret, info = self.trade_context.acctradinginfo_query(order_type=OrderType.MARKET, code=symbol, price=price, trd_env=TrdEnv.SIMULATE, acc_id=self.acc_id)
        if ret != RET_OK or info.empty:
            return None
        try:
            return float(info.iloc[0]["max_cash_and_margin_buy"])
        except (KeyError, TypeError, ValueError):
            return None

    def submit_market_order(self, symbol: str, side, quantity: int, remark: str):
        """Submit a MARKET order only after Moomoo confirms the regular session."""
        if self.acc_id is None:
            return -1, "no competition account: nothing is ordered"
        if quantity <= 0 or quantity != int(quantity):
            return -1, f"refusing an order for {quantity} shares"
        local = datetime.now(planner.NEW_YORK)
        state = str(self.feed.market_state() or "").split(".")[-1].upper()
        if (local.weekday() >= 5 or not (planner.OPEN_TIME <= local.time() < planner.CLOSE_TIME)
                or state not in planner.OPEN_STATES):
            return -1, f"market order refused outside confirmed regular session ({state or 'unknown'})"
        return self.trade_context.place_order(price=0, qty=int(quantity), code=symbol, order_type=OrderType.MARKET, trd_side=side,
                                              trd_env=TrdEnv.SIMULATE, acc_id=self.acc_id, remark=remark)

    TERMS_FILE = Path("data/scout_terms.json")

    def load_terms(self) -> dict:
        try:
            return json.loads(self.TERMS_FILE.read_text())
        except (OSError, ValueError):
            return {}

    def save_terms(self, terms: dict) -> None:
        tmp = self.TERMS_FILE.with_suffix(".tmp")
        tmp.parent.mkdir(parents=True, exist_ok=True)
        tmp.write_text(json.dumps(terms))
        from .telemetry import preserve_file
        preserve_file(self.TERMS_FILE)
        tmp.replace(self.TERMS_FILE)

    def load_live_policy(self) -> dict:
        """Dynamically load active multi-model consensus policy with hot-reload."""
        try:
            from .deliberation import load_active_policy
            return load_active_policy()
        except Exception:
            return {}


    # ---- monitor Moomoo gainers and place approved premarket orders from 04:00 ET; retry unfilled entries at the regular open ----
    COMMON_DB = Path("data/common1m.sqlite3")

    def market_phase(self, now: datetime) -> str:
        """'pre', 'open', 'after' or 'closed' from OpenD (cached for a few seconds); the clock only when OpenD cannot say."""
        stamp, phase = self.phase_cache
        if time.monotonic() - stamp < 10:
            return phase
        phase = planner.phase_from_state(self.feed.market_state(), now)
        if phase != self.phase_cache[1]:
            self.phase_cache = (time.monotonic(), phase)
            self.write_status(phase=phase, state="scanning" if phase in ("open", "pre") else "outside_regular_session")
        else:
            self.phase_cache = (time.monotonic(), phase)
        return phase

    def set_terms(self, symbol: str, terms: dict, source: str, **meta: object) -> None:
        saved = self.load_terms()
        saved[symbol] = {**terms, "_meta": {**saved.get(symbol, {}).get("_meta", {}), "source": source, **meta}}
        self.save_terms(saved)

    def build_plan(self, now: datetime, session: str) -> dict:
        """Initialize the session plan for the NASDAQ Hottest Gainers Bot.

        When the static MooComp selector is disabled (the default for the Gainers Bot),
        starts with an empty locked session plan whose candidates are populated dynamically
        in real time by ``discover_intraday_candidates`` from OpenD's live top movers.
        """
        now_ms = int(now.timestamp() * 1000)
        if not getattr(self.settings, "moocomp_static_plan", False) and planner.momentum_selection is planner._default_momentum_selection:
            evidence = {
                "schema": planner.MOMENTUM_SCHEMA,
                "strategy": "nasdaq_hottest_gainers_live",
                "data_cutoff_day": int(now.timestamp() // 86400),
                "data_cutoff": session,
                "lookback_sessions": 0,
            }
            plan = planner.new_plan(session, [], now_ms, "after_close", evidence)
            planner.save_plan(plan)
            self.journal.plan_event(now_ms, session, None, "plan_built", json.dumps({**evidence, "chosen": []}))
            LOGGER.warning("Gainers session plan initialized for %s (live hottest gainers discovery active)", session)
            self.say("planner", f"NASDAQ Hottest Gainers session {session} ready for live top movers discovery.", topic="plan", kind="decision")
            return plan
        selection = planner.momentum_selection(session, self.settings.plan_size, set(self.nasdaq_symbols))
        chosen = []
        for row in selection["candidates"]:
            history = swings.history_for_plan(self.COMMON_DB, row["symbol"].removeprefix("US."), before_ms=now_ms)
            # Backtests are research/teaching evidence only. Live gainer
            # execution never waits for or imports a backtest champion.
            terms = swings.default_terms(history)
            if not terms:
                self.journal.plan_event(now_ms, session, row["symbol"], "candidate_deferred",
                    "symbol-specific Moomoo volatility evidence is insufficient to set protective levels")
                continue
            # Override terms with walk-forward calibrated Alpha Strategy brackets:
            if "take_profit_pct" in row:
                terms["take_profit_pct"] = round(float(row["take_profit_pct"]) * 100.0, 2)
            if "stop_loss_pct" in row:
                terms["stop_pct"] = round(float(row["stop_loss_pct"]) * 100.0, 2)
            if "staged_scale" in row and row["staged_scale"]:
                terms["staged_scale"] = row["staged_scale"]
            floor_reflex = float(row.get("thomasai_floor_reflex", 1.0))
            bear_pen = float(row.get("thomasai_bear_penalty", 1.0))
            adaptive_hold = max(45.0, min(390.0, 360.0 / ((floor_reflex * bear_pen) ** 0.5)))
            terms["hold_minutes"] = round(adaptive_hold, 1)
            if floor_reflex > 1.0:
                terms["floor_reflex_weight"] = floor_reflex
                terms["break_even"] = True
                base_trail = float(terms.get("trail_pct", 2.0))
                terms["trail_pct"] = round(min(base_trail, base_trail / (floor_reflex ** 0.5)), 3)
            guidance_parts = []
            if row.get("thomasai_guided"):
                guidance_parts.append(f"Conviction: {row.get('thomasai_conviction', 1.0):.2f}")
                if row.get("thomasai_bear_penalty", 1.0) > 1.2:
                    guidance_parts.append(f"Bear Sentry: {row.get('thomasai_bear_penalty'):.2f}")
                if floor_reflex > 1.0:
                    guidance_parts.append(f"Floor Reflex: {floor_reflex:.2f} (Trail {terms['trail_pct']}%)")
            guidance = f"[ThomasAi {' | '.join(guidance_parts)}] " if guidance_parts else ""
            if selection["strategy"] == planner.TUNED_STRATEGY:
                why_desc = f"{guidance}{row['symbol']}: own tuned lookback {row['observations']}s, score {row['score']:.4f}, own calibrated stop {terms['stop_pct']}%, tp {terms['take_profit_pct']}%, trail {terms['trail_pct']}%"
            else:
                why_desc = f"{guidance}{row['symbol']} rank {row['rank']}: own calibrated stop {terms['stop_pct']}%, tp {terms['take_profit_pct']}%, trail {terms['trail_pct']}%"
            chosen.append({
                "symbol": row["symbol"],
                "why": why_desc,
                "terms": terms,
                "score": row["score"],
                "rank": row["rank"],
                "weight": row.get("weight", 1.0),
                "take_profit_pct": row.get("take_profit_pct", 0.05),
                "stop_loss_pct": row.get("stop_loss_pct", 0.05),
                "observations": row["observations"],
                "lookback": row.get("lookback", row["observations"]),
                "threshold": row.get("threshold", 0.0),
                "thomasai_guided": row.get("thomasai_guided", True),
                "thomasai_conviction": row.get("thomasai_conviction", 1.0),
                "thomasai_bear_penalty": row.get("thomasai_bear_penalty", 1.0),
                "thomasai_floor_reflex": row.get("thomasai_floor_reflex", 1.0),
            })
        evidence = {key: selection[key] for key in ("schema", "strategy", "data_cutoff_day", "data_cutoff", "lookback_sessions")}
        plan = planner.new_plan(session, chosen, now_ms, "after_close", evidence)
        planner.save_plan(plan)
        self.journal.plan_event(now_ms, session, None, "plan_built", json.dumps({**evidence, "chosen": [c["symbol"] for c in chosen]}))
        for c in chosen:
            self.journal.plan_event(now_ms, session, c["symbol"], "candidate", json.dumps({**evidence, "why": c["why"], "terms": c["terms"], "score": c["score"],
                                                                                             "rank": c["rank"], "observations": c["observations"]}))
        LOGGER.warning("Plan for %s: %s", session, [c["symbol"] for c in chosen])
        self.say("planner", f"Momentum plan for {session}: " + "; ".join(f"{c['symbol'][3:]} (stop {c['terms'].get('stop_pct')}, trail {c['terms'].get('trail_pct')}, tp {c['terms'].get('take_profit_pct')}): {c['why']}" for c in chosen), topic="plan", kind="decision")
        return plan

    def refresh_pre_open(self, plan: dict, now: datetime) -> None:
        """From 04:00 ET, vet candidates and submit ETH SIMULATE limits when OpenD confirms premarket."""
        now_ms = int(now.timestamp() * 1000)
        first = plan.get("stage", "pre_open") != "pre_open"
        plan["stage"] = "pre_open"
        snapshot = self.reconcile_positions()  # the account decides what is already done: a restart, a cancelled order or a refusal leaves the plan's statuses behind it
        if snapshot is None:
            return
        self.capture_premarket_candles(plan, now)
        held, working = {r["code"] for r in snapshot["held"]}, set(snapshot["working"])
        for cand in plan["candidates"]:
            if cand["status"] == planner.PRE_PLACED and cand["symbol"] not in held and cand["symbol"] not in working:
                cand["status"] = planner.PENDING
                cand["premarket_order_expired"] = True
                cand["note"] = "unfilled premarket order expired; defer this candidate to RTH market entry"
                self.journal.plan_event(now_ms, plan["session"], cand["symbol"], "pre_order_expired_deferred", cand["note"])
        codes = [c["symbol"] for c in plan["candidates"]
                 if c["status"] == planner.PENDING and not c.get("premarket_order_expired")]
        quotes = self.feed.snapshots(codes) if codes else {}
        for cand in plan["candidates"]:
            if cand["status"] != planner.PENDING or cand.get("premarket_order_expired"):
                continue
            tape_activity = self.current_tape_activity(cand["symbol"], now)
            if not tape_activity["eligible"]:
                cand["note"] = tape_activity["reason"] or "waiting for active Moomoo trade tape"
                self.journal.plan_event_if_changed(now_ms, plan["session"], cand["symbol"],
                    "gainer_waiting_tape", json.dumps(tape_activity, sort_keys=True))
                continue
            quote = quotes.get(cand["symbol"])
            pre_last = (float(quote.get("pre_price", 0) or 0) or float(quote.get("last", 0) or 0)) if quote else 0.0
            if not quote or pre_last <= 0:
                if first:
                    self.journal.plan_event(now_ms, plan["session"], cand["symbol"], "no_quote", "no pre-market quote yet; it will be bought at the open")
                continue
            gap = (pre_last / quote["prev_close"] - 1) * 100 if quote.get("prev_close") else None
            cand["note"] = f"pre-market {pre_last:.4f}" + (f", gap {gap:+.2f}%" if gap is not None else "")
            if first:
                self.journal.plan_event(now_ms, plan["session"], cand["symbol"], "pre_quote", json.dumps(quote))
            ask = quote["ask"] if quote.get("ask", 0) > 0 else pre_last
            if ask <= 0:
                continue
            refusal = self.recheck_gainer(plan, cand, quote, now)
            if refusal:
                cand["note"] = refusal
                self.journal.plan_event_if_changed(now_ms, plan["session"], symbol, "gainer_rejected", json.dumps({
                    "gate":"pre_order_recheck","reason":refusal,"price":fill,
                    "price_source":(quote or {}).get("_price_source","Moomoo_quote"),
                    "quote_time":(quote or {}).get("time"),"quote_provider_ms":(quote or {}).get("_quote_provider_ms"),
                    "book_provider_ms":(quote or {}).get("_book_provider_ms"),"bid":(quote or {}).get("bid"),
                    "ask":(quote or {}).get("ask"),"gating_evidence":cand.get("gating_evidence",{}),
                    "tape_activity":tape_activity,"position_already_held":any(r.get("code")==symbol for r in snapshot.get("held",[])),
                    "existing_position_qty":self.positions.get(symbol,{}).get("qty"),
                    "working_buy":symbol in snapshot.get("working",set())},sort_keys=True))
                continue
            ticker = cand["symbol"].removeprefix("US.")
            frame = self.store.moomoo_frame(ticker, now - timedelta(days=8))
            profile, baseline = live_quote_profile(frame, quote, now, None)
            terms = cand.get("terms") or {}
            if terms.get("volatility_source") not in ("symbol_moomoo_ohlc", "symbol_moomoo_history"):
                frame_bars = [(int(at.timestamp()*1000),float(row["Open"]),float(row["High"]),
                    float(row["Low"]),float(row["Close"]),float(row["Volume"])) for at,row in frame.iterrows()] if frame is not None and not frame.empty else []
                terms = swings.default_terms({}, current_bars=frame_bars, spread_pct=swings.quote_spread_pct(quote))
                if not terms:
                    cand["note"] = "waiting for this symbol's Moomoo candles to establish protective levels"
                    self.journal.plan_event_if_changed(now_ms, plan["session"], cand["symbol"], "gainer_waiting_risk_data", cand["note"])
                    continue
                cand["terms"] = terms
            else:
                terms = swings.apply_spread_floor(terms, swings.quote_spread_pct(quote))
                if not terms:
                    cand["note"] = "Moomoo spread is too wide for a symbol-specific protective bracket"
                    continue
                cand["terms"] = terms
            symbol_stop_pct = float(terms.get("stop_pct", 0) or 0)
            outcome = self.place_simulated_order(cand["symbol"], ask, profile, baseline, limit_price=round(ask * 1.003, 2),
                                                 max_positions=self.plan_max_positions(), notional_each=self.plan_share(plan),
                                                 symbol_stop_pct=symbol_stop_pct)
            if outcome["ordered"]:
                cand["status"] = planner.PRE_PLACED
                self.set_terms(cand["symbol"], cand["terms"], "plan", why=cand["why"])
                self.journal.plan_event(now_ms, plan["session"], cand["symbol"], "pre_order", f"limit {ask * 1.003:.2f}, {outcome.get('qty')} shares")
            else:
                self.journal.plan_event(now_ms, plan["session"], cand["symbol"], "pre_order_refused", outcome["reason"])
        planner.save_plan(plan)

    def capture_premarket_candles(self, plan: dict, now: datetime) -> None:
        """Refresh native Moomoo 1-minute candidate candles no more than once a minute."""
        now_mono = time.monotonic()
        if now_mono - getattr(self, "last_premarket_candle_capture", -1e9) < 60:
            return
        codes = [c["symbol"] for c in plan.get("candidates", []) if c.get("status") in (planner.PENDING, planner.PRE_PLACED)]
        if not codes:
            return
        self.last_premarket_candle_capture = now_mono
        available = self.feed.ensure(codes, push=False)
        captured = 0
        for code in available:
            frame = self.feed.candles(code)
            if frame is not None:
                captured += self.store.upsert_premarket_frame(code.removeprefix("US."), frame)
        self.feed.release_unused(set(codes))
        if captured:
            self.journal.plan_event(int(now.timestamp() * 1000), plan["session"], None, "premarket_candles", json.dumps({"symbols": len(available), "bars": captured, "source": "moomoo", "timeframe": "1m"}))

    def end_of_day_exits(self, plan: dict | None, now: datetime) -> bool:
        """Stop new Gainer entries at 15:00 ET; the exit worker owns liquidation."""
        local = now.astimezone(planner.NEW_YORK)
        near_close = local.time() >= datetime.strptime("15:00", "%H:%M").time()
        if time.monotonic() - getattr(self, "last_eod_check", -1e9) < 5:
            return near_close
        self.last_eod_check = time.monotonic()
        # The Codex exit worker submits and records the actual paper close.
        # This gate only prevents the entry loop from opening fresh positions.
        return near_close

    def place_plan_open(self, plan: dict, now: datetime) -> None:
        """At the open: every candidate without a position is bought at market, whatever the quote looks like. A failure is retried after the account has been read again."""
        now_ms = int(now.timestamp() * 1000)
        changed = False
        snapshot = None  # one account read per sweep (Moomoo allows 10 per 30 s); each order that follows reads it again itself
        for cand in plan["candidates"]:
            if cand["status"] in (planner.PLACED, planner.SKIPPED, planner.FAILED):
                continue
            if now_ms - cand.get("last_try_ms", 0) < self.settings.plan_retry_seconds * 1000:
                continue
            cand["last_try_ms"] = now_ms
            changed = True
            snapshot = snapshot or self.reconcile_positions()  # sync first: what the account really holds decides, never what was remembered
            if snapshot is None:
                cand["attempts"] += 1
                cand["note"] = "the account could not be read; trying again"
                continue
            symbol = cand["symbol"]
            tape_activity = self.current_tape_activity(symbol, now)
            if not tape_activity["eligible"]:
                # A still-working premarket order must not survive after the
                # candidate's real trade tape has gone quiet.
                if symbol in snapshot["working"] and cand["status"] == planner.PRE_PLACED:
                    self.cancel_symbol_orders(symbol)
                    cand["status"] = planner.PENDING
                cand["note"] = tape_activity["reason"] or "waiting for active Moomoo trade tape"
                self.journal.plan_event_if_changed(now_ms, plan["session"], symbol,
                    "gainer_waiting_tape", json.dumps(tape_activity, sort_keys=True))
                continue
            if symbol in snapshot["working"]:
                age = (now - datetime.fromtimestamp(plan.get("opened_ms", now_ms) / 1000, timezone.utc)).total_seconds()
                if cand["status"] == planner.PRE_PLACED and age > self.settings.plan_pre_order_grace_seconds:
                    self.cancel_symbol_orders(symbol)  # the pre-market order did not fill: replace it with a market order
                    cand["status"] = planner.PENDING
                    self.journal.plan_event(now_ms, plan["session"], symbol, "pre_order_cancelled", "not filled by the open; market order next")
                continue
            ticker = symbol.removeprefix("US.")
            quote = self.feed.snapshots([symbol]).get(symbol)
            frame = self.store.moomoo_frame(ticker, now - timedelta(days=8))
            terms = cand.get("terms") or {}
            if (terms.get("volatility_source") not in ("symbol_moomoo_ohlc", "symbol_moomoo_history") or
                    not all(float(terms.get(key, 0) or 0) > 0 for key in ("stop_pct", "trail_pct", "take_profit_pct"))):
                frame_bars = []
                if frame is not None and not frame.empty:
                    frame_bars = [(int(at.timestamp() * 1000), float(row["Open"]), float(row["High"]),
                        float(row["Low"]), float(row["Close"]), float(row["Volume"]))
                        for at, row in frame.iterrows()]
                terms = swings.default_terms({}, current_bars=frame_bars, spread_pct=swings.quote_spread_pct(quote))
                if not terms:
                    cand["note"] = "waiting for this symbol's Moomoo candles to establish protective levels"
                    self.journal.plan_event_if_changed(now_ms, plan["session"], symbol, "gainer_waiting_risk_data", cand["note"])
                    continue
                cand["terms"] = terms
            else:
                terms = swings.apply_spread_floor(terms, swings.quote_spread_pct(quote))
                if not terms:
                    cand["note"] = "Moomoo spread is too wide for a symbol-specific protective bracket"
                    continue
                cand["terms"] = terms
            fill = (quote["ask"] if quote and quote.get("ask", 0) > 0 else (quote or {}).get("last")) or (float(frame["Close"].iloc[-1]) if not frame.empty else 0.0)
            if fill <= 0:
                cand["attempts"] += 1
                cand["note"] = "no price yet; trying again"
                continue
            refusal = self.recheck_gainer(plan, cand, quote, now)
            if refusal:
                cand["note"] = refusal
                continue
            profile, baseline = live_quote_profile(frame, quote, now, None)
            cand["attempts"] += 1
            conviction = float(cand.get("thomasai_conviction", 1.0))
            bear_penalty = float(cand.get("thomasai_bear_penalty", 1.0))

            # Swarm adversarial sentry trap veto:
            if bear_penalty >= 2.0:
                cand["status"], cand["note"] = planner.SKIPPED, f"ThomasAi Bear Sentry flagged trap (penalty {bear_penalty:.2f})"
                self.journal.plan_event(now_ms, plan["session"], symbol, "skipped", json.dumps({
                    "gate":"bear_sentry","reason":cand["note"],"conviction":conviction,
                    "bear_penalty":bear_penalty,"candidate_reason":cand.get("why"),
                    "gating_evidence":cand.get("gating_evidence",{}),"tape_activity":tape_activity,
                    "price":fill,"quote_time":(quote or {}).get("time")},sort_keys=True))
                LOGGER.warning("Skipping %s: %s", symbol, cand["note"])
                continue

            base_share = self.plan_share(plan)
            policy = self.load_live_policy()
            rank_weights = policy.get("rank_weights", [0.50, 0.30, 0.20])
            rank_val = cand.get("rank")
            rank = int(rank_val) if rank_val is not None else 999
            if 1 <= rank <= len(rank_weights):
                cand_weight = float(rank_weights[rank - 1])
            else:
                cand_weight = float(cand.get("weight") or (1.0 / max(1, len(plan["candidates"]))))


            swarm_scale = max(0.2, min(2.5, conviction / max(0.5, bear_penalty)))
            effective_weight = cand_weight * swarm_scale
            scaled_notional = (base_share * effective_weight * len(plan["candidates"])) if base_share else None

            outcome = self.place_simulated_order(symbol, fill, profile, baseline, max_positions=self.plan_max_positions(), notional_each=scaled_notional, size_scale=swarm_scale,
                                                 symbol_stop_pct=float(terms.get("stop_pct", 0) or 0))
            if outcome["ordered"]:
                cand["status"], cand["note"] = planner.PLACED, f"limit submitted, {outcome.get('qty')} shares"
                self.set_terms(symbol, cand["terms"], "plan", why=cand["why"])
                held_before = next((r for r in snapshot.get("held", []) if r.get("code") == symbol), None)
                self.journal.plan_event(now_ms, plan["session"], symbol, "open_order", json.dumps({
                    "decision":"accepted_order_submitted","attempt":cand["attempts"],
                    "quantity":outcome.get("qty"),"effective_weight":effective_weight,"scale":swarm_scale,
                    "terms":cand["terms"],"candidate_reason":cand.get("why"),
                    "entry_gates":cand.get("gating_evidence",{}),"add_on_entry":held_before is not None,
                    "existing_position":{"qty":held_before.get("qty"),"cost":held_before.get("cost"),"value":held_before.get("value")} if held_before else None,
                    "working_orders_before_submit":sorted(snapshot.get("working",set())),
                    "limited_by":outcome.get("limited_by")},sort_keys=True))
                self.say("executor", f"Submitted {symbol} entry: {outcome.get('qty')} shares by limit (weight {effective_weight*100:.1f}%, attempt {cand['attempts']}).", topic="open", kind="note")
            elif planner.final_refusal(outcome["reason"]):
                cand["status"], cand["note"] = planner.SKIPPED, outcome["reason"]
                self.journal.plan_event(now_ms, plan["session"], symbol, "skipped", json.dumps({
                    "gate":"order_submission","reason":outcome["reason"],"price":fill,
                    "candidate_reason":cand.get("why"),"entry_gates":cand.get("gating_evidence",{}),
                    "terms":cand.get("terms"),"tape_activity":tape_activity,
                    "quote_time":(quote or {}).get("time"),"quote_provider_ms":(quote or {}).get("_quote_provider_ms"),
                    "existing_position_qty":self.positions.get(symbol,{}).get("qty")},sort_keys=True))
            elif cand["attempts"] >= self.settings.plan_retry_max:
                cand["status"], cand["note"] = planner.FAILED, outcome["reason"]
                self.journal.plan_event(now_ms, plan["session"], symbol, "failed", f"after {cand['attempts']} attempts: {outcome['reason']}")
                self.say("executor", f"Could not buy {symbol} after {cand['attempts']} attempts: {outcome['reason']}", topic="open", kind="alert")
            else:
                cand["note"] = outcome["reason"]
                self.journal.plan_event(now_ms, plan["session"], symbol, "retry", json.dumps({
                    "gate":"order_submission_retry","attempt":cand["attempts"],"reason":outcome["reason"],
                    "price":fill,"candidate_reason":cand.get("why"),"entry_gates":cand.get("gating_evidence",{}),
                    "terms":cand.get("terms"),"tape_activity":tape_activity,
                    "quote_time":(quote or {}).get("time"),"quote_provider_ms":(quote or {}).get("_quote_provider_ms"),
                    "existing_position_qty":self.positions.get(symbol,{}).get("qty")},sort_keys=True))
        if changed:
            planner.save_plan(plan)

    def say(self, sender: str, body: str, to: str = "*", topic: str = "", kind: str = "note", goal: str = "") -> None:
        """Leaves a message on the board; a board problem never stops trading."""
        try:
            messages.post(self.board, sender, body, to=to, topic=topic, kind=kind, goal=goal)
        except Exception as error:  # noqa: BLE001
            LOGGER.warning("Board post failed: %s", error)

    def cumulative_realized_pnl(self) -> float:
        """Cumulative realized PnL across all closed trades in the journal."""
        try:
            row = self.journal.db.execute("SELECT coalesce(sum((exit_price - entry) * qty), 0.0) FROM trades").fetchone()
            return float(row[0]) if row and row[0] is not None else 0.0
        except Exception:
            return 0.0

    def virtual_trading_balance(self, snapshot_equity: float | None = None) -> float:
        """Dynamic trading balance: if allocated_capital is configured, starts at allocated_capital + cumulative PnL; otherwise returns actual account equity."""
        if self.settings.allocated_capital is not None:
            pnl = self.cumulative_realized_pnl()
            return max(500.0, self.settings.allocated_capital + pnl)
        return snapshot_equity if snapshot_equity is not None else self.starting_capital

    def plan_share(self, plan: dict) -> float | None:
        """The position value each candidate gets, scaled to the dynamic virtual trading balance ($5000 + cumulative PnL if allocated)."""
        if plan.get("notional_each") is None:
            snapshot = self.account_snapshot()
            if snapshot is None:
                return None
            effective_balance = self.virtual_trading_balance(snapshot["equity"])
            effective_power = min(snapshot["power"], effective_balance * self.settings.target_leverage) if self.settings.allocated_capital is not None else snapshot["power"]
            max_funds = (min(self.settings.max_position_funds, effective_balance)
                         if self.settings.max_position_funds is not None
                         else effective_balance * self.settings.max_position_pct)
            plan["notional_each"] = planner.notional_each(effective_balance, effective_power, (self.settings.plan_size if (plan.get("selection") or {}).get("strategy") == planner.TUNED_STRATEGY else len(plan["candidates"])), max_funds, min(self.settings.target_leverage, owner_rules.MAX_LEVERAGE),
                                                          1.0 - owner_rules.MAX_POWER_USE)
            self.journal.plan_event(int(datetime.now(timezone.utc).timestamp() * 1000), plan["session"], None, "share",
                                    f"{plan['notional_each']:.2f} of position for each of {len(plan['candidates'])} candidates (balance {effective_balance:.2f}, broker buying power {snapshot['power']:.2f})")
            planner.save_plan(plan)
        return plan["notional_each"]

    def sense_gainer(self, symbol: str, now: datetime, rvol: float, quote: dict, recent_prices: list[float] | None = None) -> dict:
        """Refresh candidate-only data requests and require causal one-minute evidence."""
        # Candidate-only Moomoo ingestion.  The broad market converter is not
        # part of the live strategy path: once a mover is identified, fetch its
        # native one-minute candles here and retain them for replay/learning.
        available = self.feed.ensure([symbol], push=False)
        if symbol in available:
            frame = self.feed.candles(symbol)
            if frame is not None:
                microsense.write_minutes(self.COMMON_DB, symbol, frame)
        bars = microsense.read_minutes(self.COMMON_DB, symbol, int(now.timestamp() * 1000))
        from .decision_audit import record
        record(self.COMMON_DB, "gainer_sensing_inputs", {
            "symbol": symbol, "decision_ms": int(now.timestamp() * 1000),
            "bars": bars, "rvol": rvol, "quote": quote,
            "recent_prices": recent_prices}, {})
        if not bars:
            last = (float(quote.get("pre_price", 0) or 0) if now.astimezone(planner.NEW_YORK).time() < planner.OPEN_TIME else 0.0) or float(quote.get("last", 0) or 0)
            prices = [float(x) for x in (recent_prices or []) if float(x) > 0]
            if last > 0 and (not prices or prices[-1] != last):
                prices.append(last)
            if len(prices)<3:
                return {"reason":"insufficient changing Moomoo prices to confirm inflection",
                    "evidence_mode":"live_quote","price_samples":len(prices)}
            # These are irregular live quote samples, not fabricated one-minute
            # candles. features() uses their ordered prices; no bar timestamp is
            # attached to the result.
            bars = [(i,p,p,p,p,float(quote.get("volume",0) or 0)) for i,p in enumerate(prices[-6:])]
            sensed = microsense.features(bars, rvol, float(quote.get("bid", 0)), float(quote.get("ask", 0)))
            if sensed["price_velocity"] <= 0 or sensed["price_acceleration"] < 0:
                return {"reason": "price is decelerating", "evidence_mode": "live_quote", "features": sensed}
            try:
                conviction, bear = microsense.forward_pass(self.COMMON_DB.with_name("brain.sqlite3"), sensed)
            except (sqlite3.Error, ValueError, TypeError):
                return {"reason": "candidate connectome evidence unavailable", "evidence_mode": "live_quote"}
            return {"reason": gating.tier3_connectome(conviction, bear), "conviction": conviction, "bear": bear,
                "price_samples":len(prices[-6:]),"features": sensed, "evidence_mode": "live_quote"}
            return {"reason": "no closed one-minute bars or live quote yet", "evidence_mode": "closed_1m"}
        confirmation=microsense.confirm_candidate(bars,rvol,float(quote.get("bid",0)),float(quote.get("ask",0)),self.COMMON_DB.with_name("brain.sqlite3"))
        confirmation["bar_ms"]=bars[-1][0]
        return confirmation

    def recheck_gainer(self, plan: dict, cand: dict, quote: dict | None, now: datetime) -> str | None:
        """A discovery admission is not permission to buy later on stale evidence."""
        tape_activity = self.current_tape_activity(cand["symbol"], now)
        if not tape_activity["eligible"]:
            return tape_activity["reason"] or "waiting for active Moomoo trade tape"
        if plan.get("selection", {}).get("strategy") != "nasdaq_hottest_gainers_live":
            return None
        quote = quote or {}
        is_premarket = now.astimezone(planner.NEW_YORK).time() < planner.OPEN_TIME
        price = (float(quote.get('last', 0) or 0) if quote.get('_price_source') == 'fresh_order_book_midpoint' else
            ((float(quote.get("pre_price", 0) or 0) if is_premarket else 0.0) or float(quote.get("last", 0) or 0)))
        previous = float(quote.get("prev_close", 0))
        if previous <= 0:
            return "fresh quote and previous close required"
        rvol = float(quote.get("volume_ratio", 0))
        reason = gating.tier2_inflection(price, (price / previous - 1) * 100, rvol, is_premarket)
        if reason:
            return reason
        evidence = self.sense_gainer(cand["symbol"], now, rvol, quote)
        if evidence["reason"]:
            return evidence["reason"]
        cand.update(thomasai_conviction=evidence["conviction"], thomasai_bear_penalty=evidence["bear"], gating_evidence=evidence)
        return None

    def current_tape_activity(self, symbol: str, now: datetime) -> dict:
        """Evaluate only this symbol's current ET session prints from Moomoo's native 1m store."""
        local = now.astimezone(planner.NEW_YORK)
        session_start = local.replace(hour=4, minute=0, second=0, microsecond=0)
        now_ms = int(now.timestamp() * 1000)
        bars = microsense.read_minutes(self.COMMON_DB, symbol, now_ms)
        return candidate_tape_activity(bars, now_ms, int(session_start.timestamp() * 1000))

    def plan_max_positions(self) -> int:
        """Autonomously elastic swarm capacity: scales dynamically based on brain conviction and macro regime."""
        base_cap = max(self.settings.max_open_positions, self.settings.plan_size)
        if not getattr(self, "elastic_swarm", False):
            return base_cap
        try:
            brain_db = Path("data/brain.sqlite3")
            if not brain_db.exists():
                brain_db = planner.PROJECT_ROOT / "papercompmoomoo" / "data" / "brain.sqlite3"
            if brain_db.exists():
                with sqlite3.connect(f"file:{brain_db}?mode=ro", uri=True) as bconn:
                    bcur = bconn.cursor()
                    row = bcur.execute("SELECT avg(weight) FROM brain_synapses WHERE target_id='node:decision:rank1_execute'").fetchone()
                    conviction = float(row[0]) if row and row[0] else 1.0
                    bear_row = bcur.execute("SELECT avg(weight) FROM brain_synapses WHERE target_id='node:reflex:adversarial_bear_sentry'").fetchone()
                    bear = float(bear_row[0]) if bear_row and bear_row[0] else 1.0
                    
                    # Elastic scaling: high conviction expands capacity up to base_cap; high trap hostility contracts to 1 or 2
                    elastic_scale = conviction / max(0.6, bear)
                    return max(1, min(base_cap, int(round(base_cap * elastic_scale))))
        except Exception:
            pass
        return base_cap

    def cancel_symbol_orders(self, symbol: str) -> None:
        ret, rows = self.trade_context.order_list_query(trd_env=TrdEnv.SIMULATE, acc_id=self.acc_id)
        if ret != RET_OK:
            return
        for _, row in rows.iterrows():
            if row["code"] == symbol and str(row["order_status"]) in WORKING_ORDER_STATUSES:
                self.trade_context.modify_order(ModifyOrderOp.CANCEL, row["order_id"], 0, 0, trd_env=TrdEnv.SIMULATE, acc_id=self.acc_id)

    def last_price(self, symbol: str) -> float | None:
        quote = self.feed.snapshots([symbol]).get(symbol)
        return float(quote["last"]) if quote and quote.get("last") else None

    def plan_tick(self, now: datetime) -> None:
        """One step of the daily routine; called every few seconds with continuous autonomous sensation."""
        if not self.settings.plan_enabled:
            return
        phase = self.market_phase(now)
        plan = planner.load_plan()
        if plan and plan.get("selection") and planner.momentum_selection is planner._default_momentum_selection and not getattr(self.settings, "moocomp_static_plan", False):
            if plan.get("selection", {}).get("strategy") != "nasdaq_hottest_gainers_live":
                # A saved research selection cannot become a one-minute gainer entry.
                plan = None
        today = now.astimezone(planner.NEW_YORK).date().isoformat()
        if phase == "open":
            usable = plan if plan and plan["session"] == today and plan.get("locked") else None
            if usable is None:
                # Autonomous Intraday Bootstrap: never wait for before or after session!
                try:
                    plan = self.build_plan(now, today)
                    usable = plan
                except Exception as error:
                    LOGGER.error("Autonomous intraday plan build failed: %s", error)
            if usable:
                usable.setdefault("opened_ms", int(now.timestamp() * 1000))
            self.reconcile_positions()  # what the account holds decides (the carried positions of an earlier session included)
            flatten = self.end_of_day_exits(usable, now)  # first: the close-out and the carried non-candidates are sold before any candidate is bought
            if usable and not flatten:
                # Continuous sentient candidate maintenance:
                self.audit_and_retire_intraday_candidates(usable, now)
                self.discover_intraday_candidates(usable, now)
                self.place_plan_open(usable, now)
            return

        open_utc = planner.next_open(now)
        session = planner.session_key(open_utc)
        lead = planner.in_lead_window(now, open_utc, self.settings.plan_lead_hours)
        # Never idle or wait: if plan is absent or outdated, build it immediately!
        if plan is None or plan["session"] != session or not plan.get("locked"):
            old = plan
            try:
                plan = self.build_plan(now, session) or old
            except ValueError as error:
                LOGGER.error("No momentum plan for %s: %s", session, error)
                self.journal.plan_event(int(now.timestamp() * 1000), session, None, "plan_refused", str(error))
                plan = old
            self.drop_orders_not_in_plan(old, plan, int(now.timestamp() * 1000))
        if phase == "pre" and plan and plan.get("locked"):
            self.discover_intraday_candidates(plan, now)
        if plan and plan["session"] == session and plan.get("locked") and lead and plan.get("stage", "pre_open") in ("after_close", "pre_open") and any(c["status"] in (planner.PENDING, planner.PRE_PLACED) for c in plan["candidates"]):
            if time.monotonic() - getattr(self, "last_pre_open", -1e9) >= self.settings.plan_pre_retry_seconds:
                self.last_pre_open = time.monotonic()
                self.refresh_pre_open(plan, now)
        self.sync_moocomp(plan if plan and plan["session"] == session else None)

    def audit_and_retire_intraday_candidates(self, plan: dict, now: datetime) -> None:
        """Continuously sense and retire candidates that deteriorate during the session.

        If an un-entered candidate breaks down (adverse move, negative volume expansion, or
        bear sentry trap alert), it is decommissioned so no further capital is allocated to it.
        """
        now_ms = int(now.timestamp() * 1000)
        changed = False
        candidates = plan.get("candidates", [])
        pending_candidates = [c for c in candidates if c.get("status") in (planner.PENDING, planner.PRE_PLACED)]
        if not pending_candidates:
            return

        symbols = [c["symbol"] for c in pending_candidates]
        quotes = self.feed.snapshots(symbols)

        # Sense current Bear Sentry trap pressure from cognitive brain
        bear_weight = 1.0
        try:
            brain_db = Path("data/brain.sqlite3")
            if brain_db.exists():
                with sqlite3.connect(f"file:{brain_db}?mode=ro", uri=True) as bconn:
                    row = bconn.execute("SELECT weight FROM brain_synapses WHERE to_node='sentry:bear_regime' LIMIT 1").fetchone()
                    if row and row[0]:
                        bear_weight = float(row[0])
        except Exception:
            pass

        for cand in pending_candidates:
            symbol = cand["symbol"]
            quote = quotes.get(symbol)
            if not quote:
                continue

            prev_close = float(quote.get("prev_close") or quote.get("open") or quote.get("last") or 0.0)
            last_price = float(quote.get("last") or 0.0)
            intraday_pct = ((last_price / prev_close) - 1.0) * 100.0 if prev_close > 0 else 0.0

            should_retire = False
            retire_reason = ""

            if bear_weight >= 2.0:
                should_retire = True
                retire_reason = f"Bear Sentry trap triggered (weight {bear_weight:.2f})"
            elif intraday_pct < -1.5:
                should_retire = True
                retire_reason = f"Intraday price breakdown ({intraday_pct:+.2f}%)"

            if should_retire:
                cand["status"] = planner.SKIPPED
                cand["note"] = f"Autonomous demotion: {retire_reason}"
                self.cancel_symbol_orders(symbol)
                self.journal.plan_event(now_ms, plan["session"], symbol, "candidate_demoted", cand["note"])
                LOGGER.warning("Autonomous candidate demotion: %s retired during session: %s", symbol, retire_reason)
                self.say("executor", f"Decommissioned candidate {symbol} during session: {retire_reason}", topic="candidate demotion", kind="note")
                changed = True

        if changed:
            planner.save_plan(plan)

    def discover_intraday_candidates(self, plan: dict, now: datetime) -> None:
        """Autonomously discover and promote live hottest gainers in real-time during market hours.

        Directly queries OpenD's real-time market leaderboard (top % movers across US exchange),
        runs per-symbol walk-forward volatility calibration, verifies Bear Sentry traps,
        and promotes high-momentum breakouts into the active trading plan dynamically.
        """
        local = now.astimezone(planner.NEW_YORK)
        if local.weekday() >= 5 or not 4 <= local.hour < 16:
            return
        now_ms = int(now.timestamp() * 1000)
        # Scan at most every 10 seconds. The Moomoo rank endpoint allows up to
        # 60 calls per 30 seconds; this uses at most 3. The first external operation
        # in each discovery cycle is the leaderboard; all quote/candle requests
        # are derived from its NASDAQ candidates, never from SPY holdings.
        if now_ms - getattr(self, "last_intraday_discovery_ms", 0) < 10_000:
            return
        self.last_intraday_discovery_ms = now_ms
        leaderboard = self.feed.get_hottest_gainers(count=200)
        publish_shared_movers(leaderboard, now_ms)
        # Evaluate every Nasdaq row in Moomoo's returned top-200 list. Slicing
        # the global US ranking before applying the exchange filter silently
        # excluded Nasdaq movers ranked 51-200, even though we had already
        # requested and received them.
        movers = [mover for mover in leaderboard
                  if mover.get("security") in self.nasdaq_symbols]
        # Keep the first Moomoo leaderboard appearance separate from the
        # later gate approval so charts can show detection-to-approval delay.
        for mover in movers:
            symbol = str(mover.get("security") or "")
            if not symbol:
                continue
            seen = self.journal.db.execute(
                "SELECT 1 FROM plan_events WHERE session=? AND symbol=? AND event IN ('gainer_detected','gainer_admitted','live_gainer_discovered') LIMIT 1",
                (plan["session"], symbol),
            ).fetchone()
            if seen is None:
                self.journal.plan_event(now_ms, plan["session"], symbol, "gainer_detected", json.dumps({
                    "source": "moomoo_us_hottest_gainers",
                    "price": mover.get("price"),
                    "change_pct": float(mover.get("change_ratio", 0.0) or 0.0),
                    "rvol": float(mover.get("volume_ratio", 1.0) or 1.0),
                }, sort_keys=True))
        if not movers:
            return

        # Never admit a new gainer while the shared native 1-minute store is
        # known stale/corrupt. Existing positions are still managed by the
        # normal audit/exit path.
        try:
            candle_health = json.loads(Path("data/candle_health.json").read_text())
            quarantined = {str(x) for x in candle_health.get("quarantine_symbols", [])}
        except (OSError, ValueError):
            LOGGER.warning("Gainer discovery deferred: candle health report is unavailable")
            return

        active_positions = len(self.positions)
        working_count = len(self.working_orders)
        max_cap = self.plan_max_positions()
        history_prices = getattr(self, "mover_prices", {})
        self.mover_prices = history_prices
        policy = self.load_live_policy()
        working = set(self.working_orders)
        planned_symbols = {str(c.get("symbol")) for c in plan.get("candidates", [])}
        # A held symbol can receive a fresh add-on entry. Keep working buys and
        # candidates already promoted this session as duplicate protections.
        held = set(self.positions.keys())
        initially_held = set(held)

        discovered = []
        # Keep a bounded pushed quote/book stream for top Nasdaq movers. This
        # is the recovery path when Moomoo's bulk snapshot has an old update_time.
        live_codes=[str(m['security']) for m in movers[:12] if m.get('security')]
        self.feed.release_live_unused(set(live_codes))
        self.feed.ensure_live(live_codes)
        live_quotes = self.feed.snapshots([m["security"] for m in movers])
        stale_live_codes=[]
        for mover in movers[:12]:
            code=str(mover.get('security') or '')
            age=candidate_quote_age_ms(live_quotes.get(code,{}),now)
            if code and (age is None or age<0 or age>MAX_CANDIDATE_QUOTE_AGE_MS):
                stale_live_codes.append(code)
        if stale_live_codes:
            # Refresh stale top movers through a single quote batch and only
            # their subscribed books; per-symbol calls remain rate-limited.
            self.feed.refresh_live(stale_live_codes)
            live_quotes.update({code:(self.feed.live_snapshot(code,live_quotes.get(code,{})) or live_quotes.get(code,{}))
                for code in stale_live_codes})
        # Capture the current plan's native Moomoo candles before the evidence
        # gate runs; premarket evidence lives in its dedicated table.
        self.capture_premarket_candles(plan, now)
        pre_market = now.astimezone(planner.NEW_YORK).time() < planner.OPEN_TIME
        candle_captures = 0
        for m in movers:
            sec = m["security"]
            if sec in planned_symbols:
                continue  # already planned candidates must never be promoted/order-placed a second time
            # Add-on entries for a held symbol reuse its existing unique-symbol
            # slot; new symbols still respect the configured position cap.
            new_symbols_discovered = sum(1 for c in discovered if c["symbol"] not in initially_held)
            if sec not in held and active_positions + working_count + new_symbols_discovered >= max_cap:
                self.journal.plan_event_if_changed(now_ms, plan["session"], sec, "gainer_rejected", json.dumps({
                    "gate":"unique_symbol_capacity","reason":f"{active_positions + working_count + new_symbols_discovered} positions or working orders already (maximum {max_cap})",
                    "symbol":sec,"price":float(m.get("price") or 0),"rvol":float(m.get("volume_ratio",1.0) or 1.0),
                    "change_pct":float(m.get("change_ratio") or 0),"position_already_held":False,
                    "open_positions":active_positions,"working_orders":working_count,"maximum_positions":max_cap},sort_keys=True))
                continue
            if sec.removeprefix("US.") in quarantined or sec in quarantined:
                self.journal.plan_event_if_changed(now_ms, plan["session"], sec, "gainer_rejected",
                    json.dumps({"gate":"candle_health","reason":"candle health quarantine","symbol":sec,
                        "price":float(m.get("price") or 0),"rvol":float(m.get("volume_ratio",1.0) or 1.0),
                        "change_pct":float(m.get("change_ratio") or 0),"position_already_held":sec in held,
                        "working_buy":sec in working,"quarantine_symbols":sorted(quarantined)},sort_keys=True))
                continue
            quote = live_quotes.get(sec, {})
            quote_age_ms=candidate_quote_age_ms(quote,now)
            if quote_age_ms is None or quote_age_ms<0 or quote_age_ms>MAX_CANDIDATE_QUOTE_AGE_MS:
                # A stale bulk snapshot is not a reason to discard a mover.
                # Subscribe and actively refresh this identified mover before
                # deciding whether its provider quote and book are still stale.
                if sec in self.feed.live_subscribed:
                    self.feed.refresh_live([sec])
                quote=self.feed.live_snapshot(sec,quote) or quote
                quote_age_ms=candidate_quote_age_ms(quote,now)
            if quote_age_ms is None or quote_age_ms<0 or quote_age_ms>MAX_CANDIDATE_QUOTE_AGE_MS:
                waiting_detail={"reason":"waiting for fresh Moomoo quote and order-book push",
                    "price":float(m.get("price") or 0),"rvol":float(m.get("volume_ratio",1.0) or 1.0),
                    "change_pct":float(m.get("change_ratio") or 0),"quote_time":quote.get("time"),
                    "book_time":quote.get("book_time"),"quote_provider_ms":quote.get('_quote_provider_ms'),
                    "book_provider_ms":quote.get('_book_provider_ms'),
                    "max_quote_age_ms":MAX_CANDIDATE_QUOTE_AGE_MS}
                self.journal.plan_event_if_changed(now_ms,plan["session"],sec,"gainer_waiting_quote",
                    json.dumps(waiting_detail,sort_keys=True))
                continue
            # A recovered candidate may have a fresh two-sided book while the
            # last-sale quote remains stale. In that case use the book midpoint
            # as the current reference and never fall back to stale pre_price.
            recovered_mid = quote.get('_price_source') == 'fresh_order_book_midpoint'
            pre_quote_price = (float(quote.get("last", 0.0) or 0.0) if recovered_mid else
                (float(quote.get("pre_price", 0.0) or 0.0) if pre_market else 0.0))
            price = pre_quote_price if pre_quote_price > 0 else float(m.get("price") or 0.0)
            trail = history_prices.setdefault(sec, [])
            if price > 0 and (not trail or trail[-1] != price):
                trail.append(price)
                del trail[:-6]
            refusal = gating.tier1_hygiene(sec, price, self.nasdaq_symbols, held, working)
            failed_gate = ("working_order_conflict" if refusal == "an order for it is already working" else
                "nasdaq_universe" if refusal == "not a Nasdaq-listed symbol" else
                "price_range" if refusal and refusal.startswith("price ") else "tier1_hygiene")
            change_pct = float(m["change_ratio"])
            previous_close = float(quote.get('prev_close', 0) or 0)
            if recovered_mid and previous_close > 0:
                change_pct = (price / previous_close - 1.0) * 100.0
            vol_ratio = float(m.get("volume_ratio", 1.0) or 1.0)
            if refusal is None:
                refusal = gating.tier2_inflection(price, change_pct, vol_ratio, pre_market=pre_market, recent_prices=trail)
                if refusal:
                    failed_gate = ("rvol_minimum" if refusal.startswith("RVOL ") else
                        "positive_change" if refusal == "not a gainer" else
                        "maximum_extension" if refusal.startswith("extended ") else
                        "price_acceleration" if refusal == "price is decelerating" else "tier2_market_evidence")
            if refusal:
                self.journal.plan_event_if_changed(now_ms,plan["session"],sec,"gainer_rejected",json.dumps({
                    "gate":failed_gate,"reason":refusal,"symbol":sec,"price":price,
                    "price_source":quote.get("_price_source","Moomoo_live_quote"),
                    "rvol":vol_ratio,"minimum_rvol":gating.MIN_RVOL_PRE if pre_market else gating.MIN_RVOL,
                    "session_phase":"pre_market" if pre_market else "regular_market",
                    "change_pct":change_pct,"recent_prices":trail[-6:],
                    "quote_time":quote.get("time"),"quote_provider_ms":quote.get("_quote_provider_ms"),
                    "book_provider_ms":quote.get("_book_provider_ms"),
                    "bid":quote.get("bid"),"ask":quote.get("ask"),
                    "position_already_held":sec in held,"existing_position_qty":self.positions.get(sec,{}).get("qty"),
                    "working_buy":sec in working},sort_keys=True))
                continue

            # New live movers are not yet in the plan candle subscription set.
            # Fetch only after quote and momentum hygiene pass, then refresh at
            # most once a minute while the candidate is being evaluated.
            capture_times = getattr(self, "candidate_candle_capture_ms", {})
            self.candidate_candle_capture_ms = capture_times
            if now_ms - capture_times.get(sec, 0) >= 60_000 and candle_captures < 3:
                available = self.feed.ensure([sec])
                if sec in available:
                    frame = self.feed.candles(sec)
                    if frame is not None:
                        microsense.write_minutes(self.COMMON_DB, sec, frame)
                candle_captures += 1
                capture_times[sec] = now_ms
            sensing = self.sense_gainer(sec, now, vol_ratio, quote, recent_prices=trail)
            current_bars = microsense.read_minutes(self.COMMON_DB, sec, now_ms)
            session_start_ms = int(local.replace(hour=4, minute=0, second=0, microsecond=0).timestamp() * 1000)
            tape_activity = candidate_tape_activity(current_bars, now_ms, session_start_ms)
            sensing["tape_activity"] = tape_activity
            if not tape_activity["eligible"]:
                sensing["other_reason"] = sensing.get("reason")
                sensing["reason"] = tape_activity["reason"] or "thin traded tape"
            detail = json.dumps({"price":price,"price_source":quote.get('_price_source','Moomoo_live_quote'),
                "quote_provider_ms":quote.get('_quote_provider_ms'),"book_provider_ms":quote.get('_book_provider_ms'),
                "bid":float(quote.get("bid",0) or 0),
                "ask":float(quote.get("ask",0) or 0),"quote_time":quote.get("time"),
                "rvol":vol_ratio,"minimum_rvol":gating.MIN_RVOL_PRE if pre_market else gating.MIN_RVOL,
                "session_phase":"pre_market" if pre_market else "regular_market",
                "change_pct":change_pct,"gate":"candidate_tape_and_connectome",
                "position_already_held":sec in held,"existing_position_qty":self.positions.get(sec,{}).get("qty"),
                "working_buy":sec in working,"recent_prices":trail[-6:],**sensing},sort_keys=True)
            decision_event="gainer_rejected" if sensing["reason"] else "gainer_admitted"
            if decision_event=="gainer_rejected":
                self.journal.plan_event_if_changed(now_ms,plan["session"],sec,decision_event,detail)
            else:
                self.journal.plan_event(now_ms,plan["session"],sec,decision_event,detail)
            if sensing["reason"]:
                continue

            ticker = sec.removeprefix("US.")
            
            # Autonomous per-symbol bracket optimization from its own prior
            # Moomoo sessions, supplemented by this identified mover's candles.
            history = swings.history_for_plan(self.COMMON_DB, ticker, bar_minutes=1, before_ms=int(now.timestamp() * 1000))
            candidate_bars = microsense.read_minutes(self.COMMON_DB, sec, now_ms)
            terms = swings.default_terms(history, current_bars=candidate_bars,
                spread_pct=swings.quote_spread_pct(quote))
            if not terms:
                self.journal.plan_event_if_changed(now_ms, plan["session"], sec, "gainer_waiting_risk_data",
                    json.dumps({"reason":"symbol-specific Moomoo volatility evidence is insufficient to set protective levels",
                        "historical_bars":history.get("bars",0),"candidate_bars":len(candidate_bars)},sort_keys=True))
                continue
            
            # If Bear Sentry detects elevated market hostility, tighten stops & arm break-even
            if sensing["bear"] >= 0.75:
                bear_severity = min(1.0, max(0.0, (float(sensing["bear"]) - 0.75) / 0.25))
                risk_scale = 1.0 - 0.35 * (0.5 + 0.5 * bear_severity)
                floor = float(terms.get("minimum_pct", 0) or 0)
                terms["stop_pct"] = round(max(floor, float(terms["stop_pct"]) * risk_scale), 3)
                terms["trail_pct"] = round(max(floor, float(terms["trail_pct"]) * risk_scale), 3)
                terms["bear_risk_scale"] = round(risk_scale, 3)
            terms["break_even"] = True
            terms["hard_tp"] = False
            terms["hold_minutes"] = 180.0

            cand_score = round(change_pct / 5.0 + vol_ratio, 4)
            rank_idx = len(plan.get("candidates", []))
            rank_weights = policy.get("rank_weights", [0.50, 0.30, 0.20])
            cand_weight = rank_weights[rank_idx] if rank_idx < len(rank_weights) else (1.0 / max(1, max_cap))
            new_cand = {
                "symbol": sec,
                "why": f"Live Hottest Gainer (+{change_pct:.2f}%, vol ratio {vol_ratio:.2f})",
                "terms": terms,
                "score": cand_score,
                "rank": rank_idx + 1,
                "weight": cand_weight,
                "take_profit_pct": float(terms.get("take_profit_pct", 4.0)) / 100.0,

                "stop_loss_pct": float(terms.get("stop_pct", 1.5)) / 100.0,
                "status": planner.PENDING,
                "attempts": 0,
                "last_try_ms": 0,
                "note": f"live hottest gainer discovery +{change_pct:.2f}%",
                "thomasai_guided": True,
                "thomasai_conviction": sensing["conviction"],
                "thomasai_bear_penalty": sensing["bear"],
                "gating_evidence": {"rvol": vol_ratio, "change_pct": change_pct, **sensing},
            }
            held.add(sec)
            planned_symbols.add(sec)
            discovered.append(new_cand)
            new_symbols_discovered = sum(1 for c in discovered if c["symbol"] not in initially_held)
            if sec not in initially_held and active_positions + working_count + new_symbols_discovered >= max_cap:
                break

        if discovered:
            for cand in discovered:
                plan["candidates"].append(cand)
                self.journal.plan_event(now_ms, plan["session"], cand["symbol"], "live_gainer_discovered", cand["why"])
                LOGGER.warning("LIVE HOTTEST GAINER DISCOVERED: %s (%s)", cand["symbol"], cand["why"])
                self.say("researcher", f"Real-time market runner identified: {cand['symbol']} (+{cand['why']}). Dynamically promoted to active plan.", topic="hot runner", kind="decision")
            planner.save_plan(plan)

    @staticmethod
    def locked_plan_codes() -> set[str]:
        """The candidates of the locked plan on disk (empty when there is none): the only symbols whose pre-market orders a restart keeps."""
        plan = planner.load_plan()
        return {c["symbol"] for c in plan["candidates"]} if plan and plan.get("locked") else set()

    def drop_orders_not_in_plan(self, old: dict | None, new: dict | None, now_ms: int) -> None:
        """A plan replaced by a fresh one: working pre-market orders for symbols the new plan does not hold are cancelled (positions already filled stay managed by their stops)."""
        if not old or not new:
            return
        keep = {c["symbol"] for c in new["candidates"]}
        for cand in old["candidates"]:
            if cand["symbol"] not in keep and cand["status"] in (planner.PRE_PLACED, planner.PENDING):
                self.cancel_symbol_orders(cand["symbol"])
                self.journal.plan_event(now_ms, old["session"], cand["symbol"], "dropped", "not in the fresh plan: its pre-market order is cancelled")

    def sync_moocomp(self, plan: dict | None = None) -> None:
        """The Moomoo watchlist group holds the daily plan's candidates plus any open positions: they are kept synchronized so only valid candidates and open holdings are present."""
        if plan is None:
            plan = planner.load_plan()
        cand_codes = [c["symbol"] for c in (plan or {}).get("candidates", [])] if plan and plan.get("locked") else []
        pos_codes = list(self.positions.keys()) if hasattr(self, "positions") and self.positions else []
        codes = list(dict.fromkeys(cand_codes + pos_codes))
        if not codes or tuple(codes) == getattr(self, "last_group", None):
            return
        changed = self.feed.sync_watchlist(codes)
        if changed is not None:
            self.last_group = tuple(codes)
            if changed[0] or changed[1]:
                LOGGER.info("Moomoo watchlist: added %s, removed %s", changed[0], changed[1])

    def submit_limit_order(self, symbol: str, side, quantity: int, price: float, remark: str, adjust_limit: float = 0.0):
        """A session-scoped marketable LIMIT order to the Competition SIMULATE account."""
        if self.acc_id is None:
            return -1, "no competition account: nothing is ordered"
        if quantity <= 0 or quantity != int(quantity) or price <= 0:
            return -1, f"refusing an order for {quantity} shares at {price}"
        local = datetime.now(planner.NEW_YORK)
        if local.weekday() >= 5:
            return -1, "US SIMULATE order refused on a weekend"
        state = str(self.feed.market_state() or "").split(".")[-1].upper()
        if planner.OPEN_TIME <= local.time() < planner.CLOSE_TIME and state in planner.OPEN_STATES:
            session = Session.RTH
        elif datetime.strptime("04:00", "%H:%M").time() <= local.time() < planner.OPEN_TIME and state in planner.PRE_STATES:
            session = Session.ETH
        else:
            return -1, f"Moomoo has not confirmed a supported Competition paper session ({state or 'unknown'})"
        return self.trade_context.place_order(price=gating.tick(price), qty=int(quantity), code=symbol, order_type=OrderType.NORMAL, trd_side=side, trd_env=TrdEnv.SIMULATE,
                                              acc_id=self.acc_id, remark=remark, fill_outside_rth=False, session=session,
                                              adjust_limit=adjust_limit)

    def place_simulated_order(self, symbol: str, last_price: float, profile: dict, avg_daily_volume: float, signal_id: int | None = None,
                              rvol: float | None = None, move_pct: float | None = None, limit_price: float | None = None, max_positions: int | None = None, notional_each: float | None = None, size_scale: float = 1.0,
                              symbol_stop_pct: float | None = None) -> dict:
        """Submit a SIMULATE entry: MARKET in confirmed RTH; a bounded LIMIT in a supported extended session."""
        def refused(reason: str, level=LOGGER.warning) -> dict:
            level("Not ordering %s: %s", symbol, reason)
            return {"ordered": False, "reason": reason}

        if not may_trade(symbol, self.nasdaq_symbols):
            return refused("not a Nasdaq-listed symbol (the competition trades Nasdaq only)")
        if not (gating.MIN_PRICE <= last_price <= gating.MAX_PRICE):
            return refused("entry price outside [$2, $1000]")
        if symbol_stop_pct is None or not math.isfinite(float(symbol_stop_pct)) or float(symbol_stop_pct) <= 0:
            return refused("symbol-specific Moomoo stop distance is unavailable")
        local = datetime.now(planner.NEW_YORK)
        market_state = str(self.feed.market_state() or "").split(".")[-1].upper()
        if local.weekday() >= 5:
            return refused("US SIMULATE order refused on a weekend")
        if planner.OPEN_TIME <= local.time() < planner.CLOSE_TIME and market_state in planner.OPEN_STATES:
            execution_session = Session.RTH
        elif datetime.strptime("04:00", "%H:%M").time() <= local.time() < planner.OPEN_TIME and market_state in planner.PRE_STATES:
            execution_session = Session.ETH
        else:
            return refused(f"Moomoo has not confirmed a supported Competition paper session ({market_state or 'unknown'})")
        order_price = gating.tick(limit_price) if limit_price is not None else gating.slip_limit(gating.tick(last_price))
        # always look at the account first: what is held, what is already working, what buying power is left
        snapshot = self.reconcile_positions()
        if snapshot is None:
            return refused("the simulated account cannot be read", LOGGER.error)
        held = {r["code"] for r in snapshot["held"]}
        own_symbols = owned_symbols("gainer")
        own_working = {code for code in snapshot["working"] if shared_owner_of(code) == "gainer"}
        reason = order_gate(symbol, held, snapshot["working"], len(own_symbols | own_working), max_positions or self.settings.max_open_positions, last_price,
                            self.settings.min_price)
        if reason:
            return refused(reason)
        cash, buying_power, total_assets = snapshot["cash"], snapshot["power"], snapshot["equity"]
        for news in self.guard.update(total_assets):
            LOGGER.error("CAPITAL GUARD: %s", news)
            self.say("executor", news, to="commander", topic="capital guard", kind="alert", goal="O3")
        allowed, why = self.guard.allow_new_orders()
        if not allowed:
            return refused("capital guard: " + why)
        virtual_balance = self.virtual_trading_balance(total_assets)
        held_value = sum(max(float(r.get("value", 0)), float(r["qty"]) * float(r.get("cost", 0))) for r in snapshot["held"])
        pending = getattr(self, "pending_entry_notional", {})
        broker_pending = {}
        for row in snapshot.get("working_rows", []):
            broker_pending[row["code"]] = broker_pending.get(row["code"], 0.0) + float(row["qty"]) * float(row["price"])
        committed = sum(max(broker_pending.get(code, 0.0), pending.get(code, 0.0)) for code in snapshot["working"])
        # Tier 4: conviction-scaled notional = clamp(cash / max positions x scale, $1000, net assets x 0.25), never above 40% of the portfolio or the broker's buying power
        cap_positions = max_positions or self.settings.max_open_positions
        available = cash if cash > 0 else min(virtual_balance, buying_power)  # a margin account's cash goes negative once it borrows: the buying power is what is available
        notional = min(gating.tier4_notional(available, virtual_balance, cap_positions, size_scale), buying_power * (1.0 - self.settings.buying_power_buffer),
                       virtual_balance * self.settings.max_position_pct, max(0.0, virtual_balance * self.settings.target_leverage - held_value - committed))
        if notional_each is not None:
            notional = min(notional, max(0.0, notional_each))
        broker_max = self.broker_max_buy(symbol, order_price)
        if broker_max is None:
            return refused("the broker's maximum buy could not be read", LOGGER.error)
        # size to what is available: the money rules, the broker's own maximum and what the stock actually trades
        quantity, limited_by = size_order(order_price, notional, broker_max, profile, avg_daily_volume, virtual_balance, float(symbol_stop_pct),
                                          self.settings.risk_per_trade, self.settings.bar_participation, self.settings.daily_participation)
        if quantity <= 0:
            return refused(f"nothing available to buy (limited by {limited_by})")

        reservation, granted = reserve("gainer", symbol, quantity * order_price, buying_power)
        if reservation is None:
            return refused("shared Competition buying-power pool is reserved by other live orders")
        granted_qty = min(quantity, int(granted / order_price))
        if granted_qty <= 0:
            release(reservation, "shared pool cannot fund one whole share")
            return refused("shared Competition buying-power pool cannot fund one share")
        if granted_qty < quantity:
            quantity = granted_qty
            limited_by = f"{limited_by}, shared pool"

        LOGGER.warning(
            "Sizing %s: virtual balance $%.2f (starting $%.2f + PnL $%.2f), broker power $%.2f, max buy %d, last bar vol %.0f -> %d shares (limited by %s)",
            symbol, virtual_balance, self.starting_capital, self.cumulative_realized_pnl(), buying_power, int(broker_max), profile.get("last_bar_volume", 0.0), quantity, limited_by,
        )
        if execution_session == Session.RTH:
            ret, result = self.submit_market_order(symbol, TrdSide.BUY, quantity, planner_remark() if limit_price is not None else "papercompmoomoo momentum entry")
        else:
            # Moomoo does not support market orders in extended hours. Keep the limit bounded and let the
            # short stale-order policy below cancel it if it does not fill promptly.
            ret, result = self.submit_limit_order(symbol, TrdSide.BUY, quantity, order_price,
                                                  planner_remark() if limit_price is not None else "papercompmoomoo momentum entry")
        if ret != RET_OK:
            release(reservation, "Moomoo rejected the order")
            return refused(f"the order failed: {result}", LOGGER.error)
        # the order is only submitted: the position exists once the account shows it (the next look reconciles it from the fill), never before
        self.pending_entry_notional = {**getattr(self, "pending_entry_notional", {}), symbol: quantity * order_price}
        self.entered_symbols.add(symbol)
        self.working_orders.add(symbol)  # until the account shows the fill, watch it closely
        cached = getattr(self, "snapshot_cache", None)
        if cached:
            cached[1]["working"].add(symbol)  # the next order within seconds sees this one
        self.orders_today += 1
        try:
            broker_id = str(result.iloc[0]["order_id"])
        except (AttributeError, IndexError, KeyError):
            broker_id = ""
        if broker_id:
            bind_order(reservation, broker_id)
        self.journal.order(int(datetime.now(timezone.utc).timestamp() * 1000), symbol, "BUY", quantity, signal_id, broker_id, last_price, rvol, move_pct, limited_by)
        LOGGER.warning("SIMULATED ORDER ACCEPTED: %s", result)
        return {"ordered": True, "reason": "ordered", "qty": quantity, "limited_by": limited_by}

    def manage_position(self, symbol: str, last_price: float, move_pct: float, extended: bool = False, now: datetime | None = None, confirm: bool = False) -> None:
        from .telemetry import emit
        emit('position_evaluation', {'symbol': symbol, 'last_price': last_price,
             'move_pct': move_pct, 'position': self.positions.get(symbol),
             'extended': extended, 'confirm': confirm}, actor='gainer')
        # Position monitoring remains active, but this bot is not permitted to
        # decide a strategy exit. Codex owns HOLD/EXIT for the configured paper account.
        from .exit_ownership import legacy_strategy_exit_allowed
        if not legacy_strategy_exit_allowed(account_id("gainer")):
            emit('legacy_exit_suppressed', {'symbol': symbol, 'owner': 'codex',
                 'account_id': account_id("gainer")}, actor='gainer')
            return
        """Legacy exit path is fail-closed; the live worker owns price exits and the 15:00 ET flatten."""
        position = self.positions.get(symbol)
        if position is None or self.orders_today >= self.settings.max_orders_per_day:
            return
        entry = float(position["entry"])
        high = max(float(position["high"]), last_price)
        if high > float(position["high"]):
            position["high"] = high
            self.save_positions()
        pnl_pct = (last_price - entry) / entry * 100 if entry else 0.0
        all_terms = self.load_terms()
        terms = all_terms.get(symbol, {})  # conditions the scout set for its own entry; the strategy's numbers apply to everything else
        if symbol in all_terms:  # how far the scout's trade went for and against it, kept for the lesson
            meta = all_terms[symbol].setdefault("_meta", {})
            meta["high"], meta["low"] = max(meta.get("high", entry), last_price), min(meta.get("low", entry), last_price)
            self.save_terms(all_terms)
        stop_pct = terms.get("stop_pct", self.settings.stop_loss_pct)
        trail_pct = terms.get("trail_pct", self.settings.trailing_stop_pct)
        take_pct = terms.get("take_profit_pct", self.settings.take_profit_pct)
        hard_tp = terms.get("hard_tp", False)
        # Check for two-leg staged scale-out exit (+4% leg 1, +8% leg 2)
        staged = terms.get("staged_scale")
        if staged and isinstance(staged, (list, tuple)) and len(staged) == 2:
            t1, t2 = float(staged[0]) * 100.0, float(staged[1]) * 100.0
            if not position.get("staged_t1_hit") and pnl_pct >= t1:
                cur_qty = int(position["qty"])
                half_qty = max(1, cur_qty // 2)
                if half_qty < cur_qty:
                    position["staged_t1_hit"] = True
                    position["qty"] = cur_qty - half_qty
                    self.save_positions()
                    self.close_simulated_order(symbol, half_qty, last_price, pnl_pct, "staged take profit leg 1", extended)
                    return
            elif pnl_pct >= t2:
                self.close_simulated_order(symbol, int(position["qty"]), last_price, pnl_pct, "staged take profit leg 2", extended)
                return

        if take_pct > 0 and pnl_pct >= take_pct:
            if hard_tp:
                self.close_simulated_order(symbol, int(position["qty"]), last_price, pnl_pct, "take profit", extended)
                return
            elif not position.get("armed"):
                position["armed"] = True
                self.save_positions()

        # Dynamic live policy trailing and pyramiding:
        policy = self.load_live_policy()
        if policy and terms.get("pyramid", False) and not position.get("pyramided", False):
            pyramid_trigger = float(policy.get("pyramid_trigger_pct", 0.010)) * 100.0
            if pnl_pct >= pyramid_trigger:
                position["pyramided"] = True
                position["armed"] = True
                terms["break_even"] = True
                self.save_terms(all_terms)
                self.save_positions()
                LOGGER.warning("Pyramiding confirmed on winning runner %s (+%.2f%%): armed Break-Even floor", symbol, pnl_pct)




        # Automated Continuous Break-Even Floor Reflex: run-up required to arm break-even scales smoothly
        floor_wt = float(terms.get("floor_reflex_weight", 1.0))
        run_up_pct = (high - entry) / entry * 100 if entry else 0.0
        # High floor reflex arms break-even earlier (e.g. down to 1.2% run-up); baseline arms at 2.5%
        arm_trigger = max(1.2, 2.5 / (floor_wt ** 0.5))
        break_even_floor = (terms.get("break_even", False) or floor_wt > 1.0) and run_up_pct >= arm_trigger
        # Cognitive autonomous Bear Sentry protection:
        try:
            brain_db = Path("data/brain.sqlite3")
            if brain_db.exists():
                with sqlite3.connect(f"file:{brain_db}?mode=ro", uri=True) as bconn:
                    row = bconn.execute("SELECT weight FROM brain_synapses WHERE to_node='sentry:bear_regime' LIMIT 1").fetchone()
                    if row and row[0] and float(row[0]) >= 2.0:
                        stop_pct = min(stop_pct, 1.2)
                        if pnl_pct >= 0.5 and not position.get("armed"):
                            position["armed"] = True
                            self.save_positions()
                            LOGGER.warning("Bear Sentry protection armed break-even floor on open position %s", symbol)
        except Exception:
            pass

        # ThomasAi + Moomoo Algorithmic Microstructure Dynamic Exit & Bracket Monitor:
        try:
            from . import algo_fingerprint
            dyn = algo_fingerprint.evaluate_dynamic_exit(
                symbol=symbol,
                entry_price=entry,
                high_price=high,
                last_price=last_price,
                current_stop=float(position.get("stop") or (entry * (1.0 - stop_pct / 100.0))),
                current_target=float(position.get("target") or (entry * (1.0 + max(take_pct, 2.5) / 100.0))),
                brain_path=Path("data/brain.sqlite3"),
                common_db=self.COMMON_DB,
                base_stop_pct=float(stop_pct),
                base_trail_pct=float(trail_pct),
            )
            position["stop"] = dyn["dynamic_stop"]
            position["target"] = dyn["dynamic_target"]
            position["algo_type"] = dyn["algo_type"]
            position["conviction"] = dyn["conviction"]
            trail_pct = min(float(trail_pct), max(0.45, float(dyn["trail_pct"])))
            if dyn["should_exit"] and dyn["exit_reason"] and not dyn["exit_reason"].startswith("thomasai_protective_stop"):
                fresh = self.last_price(symbol) if confirm else last_price
                if fresh and fresh > 0:
                    last_price, pnl_pct = fresh, (fresh - entry) / entry * 100 if entry else 0.0
                    self.close_simulated_order(symbol, int(position["qty"]), last_price, pnl_pct, dyn["exit_reason"], extended)
                    return
        except Exception:
            pass

        armed = bool(position.get("armed")) or break_even_floor
        stop = pnl_pct <= -stop_pct or (float(position.get("stop") or 0) > 0 and last_price <= float(position["stop"]))
        exit_level = max(high * (1 - trail_pct / 100), float(position.get("stop") or 0))
        if armed:
            exit_level = max(exit_level, entry)
        trail = high > entry and last_price <= exit_level
        # Intraday positions are held for the session rather than dumped into midday illiquidity
        if (stop or trail) and confirm:
            # a price-based exit is decided on a second, independent Moomoo read: one bad or stale tick (a thin pre-market print, a feed glitch) must not sell a position;
            # no fresh Moomoo price means no price-based exit this time
            fresh = self.last_price(symbol)
            if fresh is None or fresh <= 0:
                return
            last_price, pnl_pct = fresh, (fresh - entry) / entry * 100 if entry else 0.0
            if take_pct > 0 and pnl_pct >= take_pct:
                if hard_tp:
                    self.close_simulated_order(symbol, int(position["qty"]), last_price, pnl_pct, "take profit", extended)
                    return
                elif not position.get("armed"):
                    position["armed"] = True
                    self.save_positions()
            stop = pnl_pct <= -stop_pct or (float(position.get("stop") or 0) > 0 and fresh <= float(position["stop"]))
            trail = high > entry and fresh <= exit_level
        if stop or trail:
            self.close_simulated_order(symbol, int(position["qty"]), last_price, pnl_pct,
                                       "stop" if stop else ("trailing take profit" if armed else "trailing stop"), extended)


    def close_simulated_order(self, symbol: str, quantity: int, last_price: float, pnl_pct: float, reason: str = "exit", extended: bool = False) -> None:
        """Retired legacy route retained only as a fail-closed call boundary."""
        from .telemetry import emit
        emit('legacy_exit_blocked', {'symbol': symbol, 'qty': quantity,
             'reason': reason, 'owner': 'codex', 'account_id': account_id("gainer")}, actor='gainer')
        LOGGER.warning("BLOCKED legacy strategy exit for %s; Codex exclusively owns exits", symbol)

    def save_positions(self) -> None:
        """Write the open positions (entry, high, opening time, dynamic stop/target) to disk, so a restart does not forget them and never exit them."""
        try:
            payload = {symbol: {"qty": int(p["qty"]), "entry": float(p["entry"]), "high": float(p["high"]), "opened": p["opened"].isoformat(), "armed": bool(p.get("armed")),
                                "stop": float(p.get("stop") or 0.0), "target": float(p.get("target") or 0.0), "algo_type": str(p.get("algo_type") or ""), "conviction": float(p.get("conviction") or 0.0)}
                       for symbol, p in self.positions.items()}
            path = Path("data/positions.json")
            path.parent.mkdir(parents=True, exist_ok=True)
            temporary = path.with_suffix(".tmp")
            temporary.write_text(json.dumps(payload, indent=2))
            from .telemetry import preserve_file
            preserve_file(path)
            temporary.replace(path)
        except (OSError, TypeError, ValueError):
            LOGGER.exception("Could not save the open positions")

    def account_snapshot(self, max_age: float = 0.0) -> dict | None:
        """The simulated account as it is right now: equity, cash, buying power, held positions and working orders. None when it cannot be read.

        Every scan and every order starts from this; the bot never trades on what it remembers.
        """
        if self.acc_id is None:
            LOGGER.error("No competition account is resolved: nothing is read or traded")
            return None
        cached = getattr(self, "snapshot_cache", None)
        if max_age > 0 and cached and time.monotonic() - cached[0] < max_age:
            return cached[1]  # the read a moment ago (orders placed since are added to its working set), so one order does not cost two reads
        wait = self.settings.accinfo_min_gap_seconds - (time.monotonic() - getattr(self, "last_accinfo", -1e9))
        if wait > 0:
            time.sleep(wait)
        self.last_accinfo = time.monotonic()
        ret, info = self.trade_context.accinfo_query(trd_env=TrdEnv.SIMULATE, acc_id=self.acc_id, currency="USD", refresh_cache=True)
        if ret != RET_OK or info.empty:
            LOGGER.error("Could not read the simulated account: %s", info)
            return None
        ret, held = self.trade_context.position_list_query(trd_env=TrdEnv.SIMULATE, acc_id=self.acc_id, refresh_cache=True)
        if ret != RET_OK:
            LOGGER.error("Could not read the simulated positions: %s", held)
            return None
        ret, orders = self.trade_context.order_list_query(trd_env=TrdEnv.SIMULATE, acc_id=self.acc_id, refresh_cache=True)
        if ret != RET_OK:
            LOGGER.error("Could not read the simulated orders: %s", orders)
            return None
        row = info.iloc[0]
        working = orders[orders["order_status"].isin(WORKING_ORDER_STATUSES) & (orders["trd_side"] == "BUY")] if len(orders) else orders
        snapshot = {
            "equity": float(row["total_assets"]), "cash": float(row["us_cash"]), "power": float(row["power"]),
            "held": [{"code": str(r["code"]), "qty": int(r["qty"]), "cost": float(r.get("cost_price", 0.0)) or float(r.get("nominal_price", 0.0)),
                      "value": float(r.get("market_val", 0.0) or 0.0)}
                     for _, r in held.iterrows() if int(r.get("qty", 0)) > 0 and str(r.get("code", "")).startswith("US.")],
            "working": {str(r["code"]) for _, r in orders.iterrows() if str(r["order_status"]) in WORKING_ORDER_STATUSES},
            "working_rows": [{"order_id": int(r["order_id"]), "code": str(r["code"]), "qty": float(r["qty"]), "price": float(r.get("price", 0.0)),
                              "remark": str(r.get("remark", "")), "status": str(r.get("order_status", "")),
                              "dealt_qty": float(r.get("dealt_qty", 0.0) or 0.0),
                              "created": datetime.strptime(str(r["create_time"])[:19], "%Y-%m-%d %H:%M:%S")}
                             for _, r in working.iterrows()],
            "terminal_rows": [{"order_id": str(r["order_id"]), "code": str(r.get("code", "")),
                               "side": str(r.get("trd_side", "")), "status": str(r["order_status"]),
                               "qty": float(r.get("qty", 0.0) or 0.0),
                               "dealt_qty": float(r.get("dealt_qty", 0.0) or 0.0)}
                              for _, r in orders.iterrows() if str(r["order_status"]) not in WORKING_ORDER_STATUSES],
        }
        try:
            saved = json.loads(Path("data/positions.json").read_text())
        except (OSError, ValueError):
            saved = {}
        owned_before = set(self.positions) | set(saved)
        orb_symbols = set()
        try:
            orb_db = sqlite3.connect(Path(__file__).resolve().parents[2] / "data" / "orb.sqlite3")
            try:
                orb_symbols = {str(r[0]) for r in orb_db.execute("SELECT symbol FROM positions WHERE status IN ('open','closing') AND qty>0")}
            finally:
                orb_db.close()
        except sqlite3.Error:
            LOGGER.exception("Could not read ORB's local position ledger for shared-account reconciliation")
        sync_shared_account([r["code"] for r in snapshot["held"]], snapshot["working_rows"],
                            {"gainer": owned_before, "orb": orb_symbols}, terminal_rows=snapshot["terminal_rows"])
        for position in snapshot["held"]:
            position["owner"] = shared_owner_of(position["code"])
        snapshot["gainer_held"] = [r for r in snapshot["held"] if r.get("owner") == "gainer"]
        snapshot["working_by_owner"] = {
            shared_owner_of(code) for code in snapshot["working"]
        }
        for order in snapshot["working_rows"]:
            order["owner"] = shared_owner_of(order["code"])
        self.snapshot_cache = (time.monotonic(), snapshot)
        self.guard.update(snapshot["equity"])
        self.write_status(
            allocated_capital=self.virtual_trading_balance(snapshot["equity"]),
            account={
                "equity": snapshot["equity"],
                "cash": snapshot["cash"],
                "power": snapshot["power"],
                "held": [r["code"] for r in snapshot["held"]],
                "working_orders": sorted(snapshot["working"]),
                "read_at": datetime.now(timezone.utc).isoformat(),
            },
        )  # always current, in or out of the session
        return snapshot

    def audit_account(self, at_startup: bool = False) -> int:
        """Look at the account and fix what is wrong: cancel this bot's stale or duplicate working orders (all it left behind, at start-up).

        Runs at every start and on the audit schedule. Returns the number of orders cancelled; -1 when the account cannot be read.
        """
        account = self.account_snapshot()
        if account is None:
            return -1
        own_orders = [row for row in account["working_rows"] if row.get("owner") == "gainer"]
        stale = orders_to_cancel(own_orders, datetime.now(timezone.utc), self.settings.stale_order_seconds, at_startup, self.locked_plan_codes())
        cancelled = 0
        for order in stale:
            ret, result = self.trade_context.modify_order(ModifyOrderOp.CANCEL, order["order_id"], qty=order["qty"], price=order["price"], trd_env=TrdEnv.SIMULATE, acc_id=self.acc_id)
            if ret == RET_OK:
                cancelled += 1
                LOGGER.warning("Cancelled %s order %s for %s (%d shares): %s", "left-over" if at_startup else "stale or duplicate", order["order_id"], order["code"],
                               int(order["qty"]), "start-up audit" if at_startup else "still working")
            else:
                LOGGER.error("Could not cancel order %s for %s: %s", order["order_id"], order["code"], result)
        LOGGER.info("Account audit%s: equity %.2f, cash %.2f, buying power %.2f, %d position(s), %d working order(s), %d cancelled",
                    " at start-up" if at_startup else "", account["equity"], account["cash"], account["power"], len(account["held"]), len(account["working_rows"]) - cancelled,
                    cancelled)
        self.write_status(audit={"at": datetime.now(timezone.utc).isoformat(), "cancelled": cancelled, "equity": account["equity"], "positions": len(account["held"])})
        return cancelled

    def run_audit(self, at_startup: bool = False, now: datetime | None = None) -> list:
        """The bot audits itself: gather the facts, evaluate the rules (``auditor.evaluate``), apply the repairs they name, write ``data/audit.json``. Returns the findings."""
        import json as _json

        now = now or datetime.now(timezone.utc)
        account = self.account_snapshot() if self.acc_id is not None else None
        own_orders = [row for row in account["working_rows"] if row.get("owner") == "gainer"] if account else []
        stale = orders_to_cancel(own_orders, now, self.settings.stale_order_seconds, at_startup, self.locked_plan_codes()) if account else []
        junk = self.store.clean_if_due(now=now) or {}
        try:
            running = _json.loads(self.status_path.read_text()).get("code_hash")
        except (OSError, ValueError):
            running = None
        facts = {"now": now, "acc_id": self.acc_id, "account": account, "tracked": dict(self.positions), "stale_orders": stale,
                 "per_position_limit": (account["equity"] * self.settings.max_position_pct * 1.05) if account else 0.0,
                 "store_junk": junk, "code_changed": bool(running) and running != code_hash(),
                 "flat": not self.positions and not (account and account["working"]), "no_working": bool(account) and not account["working"]}
        findings = evaluate(facts)
        fixed = 0
        for f in findings:
            try:
                if f.fix == "cancel_orders":
                    fixed += self.audit_account(at_startup) > 0
                elif f.fix == "adopt_positions":
                    fixed += self.reconcile_positions() is not None
                elif f.fix == "restart":
                    LOGGER.warning("Auditor: the running code is not the code on disk and nothing is held: restarting through the test gate")
                    self._write_audit(findings, fixed, now)
                    raise SystemExit(0)
            except SystemExit:
                raise
            except Exception:
                LOGGER.exception("Auditor repair %s failed", f.fix)
        self._write_audit(findings, fixed, now)
        for f in findings:
            LOGGER.log(logging.ERROR if f.level == "error" else logging.WARNING, "Audit [%s] %s%s", f.area, f.text, f" -> {f.fix}" if f.fix else "")
        return findings

    def _write_audit(self, findings: list, fixed: int, now: datetime) -> None:
        import json as _json

        doc = {"at": now.isoformat(), "errors": sum(f.level == "error" for f in findings), "warnings": sum(f.level == "warn" for f in findings), "fixed": int(fixed),
               "findings": [{"level": f.level, "area": f.area, "text": f.text, "fix": f.fix} for f in findings]}
        try:
            path = Path("data/audit.json")
            temporary = path.with_suffix(".tmp")
            temporary.write_text(_json.dumps(doc, indent=2))
            from .telemetry import preserve_file
            preserve_file(path)
            temporary.replace(path)
        except OSError:
            LOGGER.warning("Could not write data/audit.json")
        self.write_status(audit={k: doc[k] for k in ("at", "errors", "warnings", "fixed")} | {"findings": [f["text"] for f in doc["findings"]][:6]})

    def reconcile_positions(self, snapshot: dict | None = None) -> dict | None:
        """Rebuild ``positions`` from the simulated account (the truth) and the saved file (entry, high and opening time); returns the snapshot used.

        A position the account holds that the file does not know (a fill since the last look, or a restart) is tracked from its cost price, so the stop, trailing
        stop and 90-minute limit still apply. A file entry the account no longer holds is dropped.
        """
        snapshot = snapshot or self.account_snapshot(max_age=self.settings.snapshot_reuse_seconds)
        if snapshot is None:
            return None
        try:
            saved = json.loads(Path("data/positions.json").read_text())
        except (OSError, ValueError):
            saved = {}
        positions: dict[str, dict[str, float | datetime | int]] = {}
        for row in snapshot.get("gainer_held", snapshot["held"]):
            code, quantity, cost = row["code"], row["qty"], row["cost"]
            if code not in self.positions and time.monotonic() - self.exit_sent.get(code, -1e9) < EXIT_FILL_GRACE_SECONDS:
                continue  # sold a moment ago and not yet filled: adopting it again would sell it twice (a short)
            known = self.positions.get(code) or saved.get(code)
            if known and isinstance(known.get("opened"), str):
                known = {**known, "opened": datetime.fromisoformat(known["opened"])}
            if known:
                previous_qty = int(known.get("qty", quantity) or quantity)
                previous_entry = float(known.get("entry", cost) or cost)
                # Moomoo reports one weighted-average cost for the aggregated
                # symbol holding. Rebase the saved basis only when shares were
                # added; an ordinary refresh or partial exit must not rewrite
                # the original entry basis.
                entry = cost if quantity > previous_qty else previous_entry
                positions[code] = {
                    "qty": quantity,
                    "entry": entry,
                    "high": float(known["high"]),
                    "opened": known["opened"],
                    "armed": bool(known.get("armed")),
                    "stop": float(known.get("stop") or 0.0),
                    "target": float(known.get("target") or 0.0),
                    "algo_type": str(known.get("algo_type") or ""),
                    "conviction": float(known.get("conviction") or 0.0),
                }
                if quantity > previous_qty:
                    now_ms = int(datetime.now(timezone.utc).timestamp() * 1000)
                    self.journal.plan_event(now_ms, datetime.now(planner.NEW_YORK).date().isoformat(), code,
                        "position_addon_fill_reconciled", json.dumps({
                            "previous_qty":previous_qty,"current_qty":quantity,"added_qty":quantity-previous_qty,
                            "previous_average_entry":previous_entry,"broker_average_entry":cost,
                            "updated_average_entry":entry,"high_water_preserved":float(known.get("high",entry)),
                            "add_on_entry":True},sort_keys=True))
            else:
                # The broker position is authoritative for quantity and cost;
                # recover its entry order timestamp from this bot's journal so
                # the exit worker includes the position's already-recorded
                # session highs when it rebuilds symbol-specific trailing lines.
                opened = None
                try:
                    entry_order = self.journal.db.execute(
                        "SELECT ts_ms FROM orders WHERE symbol=? AND side='BUY' ORDER BY ts_ms DESC LIMIT 1",
                        (code,),
                    ).fetchone()
                    if entry_order and int(entry_order[0]) > 0:
                        opened = datetime.fromtimestamp(int(entry_order[0]) / 1000, timezone.utc)
                except (sqlite3.Error, TypeError, ValueError, OSError):
                    pass
                positions[code] = {"qty": quantity, "entry": cost, "high": cost,
                                   "opened": opened or datetime.now(timezone.utc), "stop": 0.0, "target": 0.0}
            self.entered_symbols.add(code)
        added = set(positions) - set(self.positions)
        self.positions = positions
        self.save_positions()
        if added:
            LOGGER.info("Account shows %d new position(s): %s", len(added), ", ".join(sorted(added)))
        return snapshot

    def watch_positions(self, now: float | None = None, extended: bool = False) -> None:
        """Between scans: while the bot holds something or has an order working, look at the account every ``watch_seconds`` (so a fill is tracked and its stops apply
        within seconds, not at the next scan up to 1 minutes later), then check the prices of what is held."""
        now = time.monotonic() if now is None else now
        if self.positions or self.working_orders:
            if now - getattr(self, "last_watch", -1e9) >= self.settings.watch_seconds:
                self.last_watch = now
                account = self.reconcile_positions()
                self.working_orders = set(account["working"]) if account else self.working_orders
                if account:
                    self.cancel_expired_entry_orders(account)
        else:
            self.last_watch = -1e9
        if self.positions:
            self.check_positions(True) if extended else self.check_positions()

    def cancel_expired_entry_orders(self, account: dict) -> int:
        """Reprice a stale Gainer ETH limit from a fresh ask at most twice, then cancel it."""
        now = datetime.now(timezone.utc)
        expired = orders_to_cancel(account.get("working_rows", []), now, self.settings.stale_order_seconds,
                                   at_startup=False, keep_codes=self.locked_plan_codes())
        cancelled = 0
        own = [order for order in expired
               if order.get("owner") == "gainer" and str(order.get("remark", "")).startswith("papercompmoomoo")]
        local = now.astimezone(planner.NEW_YORK)
        state = str(self.feed.market_state() or "").split(".")[-1].upper()
        can_reprice = (local.weekday() < 5 and datetime.strptime("04:00", "%H:%M").time() <= local.time() < planner.OPEN_TIME
                       and state in planner.PRE_STATES)
        quotes = self.feed.snapshots(list(dict.fromkeys(order["code"] for order in own))) if can_reprice and own else {}
        for order in expired:
            if order.get("owner") != "gainer" or not str(order.get("remark", "")).startswith("papercompmoomoo"):
                continue
            order_id = int(order["order_id"])
            quote = quotes.get(order["code"])
            attempts = self.order_reprice_attempts.get(order_id, 0)
            if can_reprice and attempts < 2 and quote and quote.get("ask", 0) > 0 and self._quote_is_fresh(quote.get("time"), now):
                new_price = gating.tick(float(quote["ask"]) * (1.0 + gating.SLIP_LIMIT))
                remaining = max(0, int(order["qty"] - order.get("dealt_qty", 0)))
                if remaining > 0 and abs(new_price - float(order["price"])) >= 0.01:
                    ret, result = self.trade_context.modify_order(ModifyOrderOp.NORMAL, order_id,
                                                                  qty=remaining, price=new_price, adjust_limit=0,
                                                                  trd_env=TrdEnv.SIMULATE, acc_id=self.acc_id)
                    if ret == RET_OK:
                        self.order_reprice_attempts[order_id] = attempts + 1
                        from .telemetry import emit
                        emit("entry_order_repriced", {"symbol": order["code"], "old_limit": float(order["price"]),
                             "new_limit": new_price, "remaining_qty": remaining, "attempt": attempts + 1,
                             "quote_time": quote.get("time")}, actor="gainer")
                        LOGGER.warning("Repriced Gainer ETH entry %s from %.4f to %.4f using fresh ask; attempt %d/2",
                                       order["code"], float(order["price"]), new_price, attempts + 1)
                        continue
                    LOGGER.error("Could not reprice Gainer ETH entry for %s; cancelling it: %s", order["code"], result)
            ret, result = self.trade_context.modify_order(ModifyOrderOp.CANCEL, order["order_id"],
                                                          qty=order["qty"], price=order["price"],
                                                          trd_env=TrdEnv.SIMULATE, acc_id=self.acc_id)
            if ret == RET_OK:
                cancelled += 1
                from .telemetry import emit
                emit("entry_order_cancelled_unfilled", {"symbol": order["code"], "order_id": order_id,
                     "age_seconds": int(order.get("_age", self.settings.stale_order_seconds)),
                     "reprice_attempts": attempts}, actor="gainer")
                LOGGER.warning("Cancelled unfilled Gainer entry for %s after %ds: order %s",
                               order["code"], int(order.get("_age", self.settings.stale_order_seconds)), order_id)
                self.order_reprice_attempts.pop(order_id, None)
            else:
                LOGGER.error("Could not cancel expired Gainer entry for %s: %s", order["code"], result)
        return cancelled

    @staticmethod
    def _quote_is_fresh(value: str | None, now: datetime, max_age_seconds: int = 15) -> bool:
        if not value:
            return False
        try:
            stamp = datetime.fromisoformat(str(value))
            if stamp.tzinfo is None:
                stamp = stamp.replace(tzinfo=planner.NEW_YORK)
            age = (now - stamp.astimezone(timezone.utc)).total_seconds()
            return -2 <= age <= max_age_seconds
        except (TypeError, ValueError):
            return False

    def check_positions(self, extended: bool = False) -> None:
        """Between scans, watch only what is held: Moomoo's live quote for each open position (one small call), so stops react within seconds.
        From pre-order through market close, position checks use Moomoo OpenD quotes only."""
        symbols = list(self.positions)
        if not symbols:
            return
        quotes = self.feed.snapshots(symbols)
        observed_at = datetime.now(timezone.utc)
        local_time = observed_at.astimezone(planner.NEW_YORK).time()
        premarket = planner.chart_window(observed_at) and local_time < planner.OPEN_TIME
        def position_price(quote: dict) -> float:
            pre_price = float(quote.get("pre_price", 0.0) or 0.0)
            last = float(quote.get("last", 0.0) or 0.0)
            if premarket and pre_price > 0:
                return pre_price
            return last if last > 0 else pre_price
        def has_live_price(quote: dict) -> bool:
            if premarket:
                return float(quote.get("pre_price", 0.0) or 0.0) > 0
            return float(quote.get("last", 0.0) or 0.0) > 0 or float(quote.get("pre_price", 0.0) or 0.0) > 0
        prices = {s: position_price(quotes[s]) for s in symbols if s in quotes and has_live_price(quotes[s])}
        for symbol, price in prices.items():
            self.manage_position(symbol, price, 0.0, extended, confirm=True)
        self.write_status(positions_live=[{"symbol": s, "qty": int(p["qty"]), "entry": p["entry"], "price": prices.get(s),
                                           "stop": round(float(p.get("stop") or 0.0), 4) or None,
                                           "target": round(float(p.get("target") or 0.0), 4) or None,
                                           "high": round(float(p.get("high") or p["entry"]), 4),
                                           "algo_type": p.get("algo_type") or None,
                                           "pnl_pct": round((prices[s] / p["entry"] - 1) * 100, 2) if s in prices and p["entry"] else None}
                                          for s, p in self.positions.items()])

    def run(self) -> None:
        """Run the momentum plan and watch its simulated positions continuously."""
        if darwin_qos.set_interactive():
            LOGGER.info("Thread prioritized to Apple Silicon Performance Cores (Darwin QOS_CLASS_USER_INTERACTIVE)")
        self.elastic_swarm = True
        cleaned = self.store.clean_if_due()
        if cleaned is not None:
            LOGGER.info("Cleaned the candle store: %s", cleaned)
        self.resolve_account()
        # Publish broker truth immediately. Baseline construction can take a
        # long time; the dashboard must never show null account values while
        # the bot is preparing its one-minute universe.
        if self.acc_id is not None:
            self.account_snapshot(max_age=0.0)
        # Publish a fresh safety audit before the potentially long baseline
        # download. Dashboards must never carry the previous process's audit
        # timestamp while this process is already running.
        self.run_audit(at_startup=True)
        # Never delay Moomoo's live gainers scan to build a broad historical
        # universe. Candidate RVOL comes directly from the Moomoo leaderboard;
        # per-candidate candles are captured only after discovery.
        missing_baselines = len(self.symbols) - self.store.baseline_count(self.baseline_cutoff())
        if missing_baselines > 0:
            self.write_status(state="scanning", baseline_source="moomoo_only", baseline_pending=missing_baselines)
        self.run_audit(at_startup=True)  # recheck after baseline construction
        # Gainer admissions use the live leaderboard RVOL and native 1m candidate bars.
        self.reconcile_positions()
        last_audit = time.monotonic()  # the start-up audit below counts as the first
        while True:
            if date.today() != self.order_day:
                self.order_day = date.today()
                self.orders_today = 0
                self.entered_symbols = {s for s in self.positions}  # a new day: symbols may be entered again, held ones stay marked
            now = datetime.now(timezone.utc)
            if time.monotonic() - last_audit >= self.settings.audit_seconds:
                last_audit = time.monotonic()
                self.run_audit(now=now)
            try:
                self.plan_tick(now)
            except Exception:  # noqa: BLE001 - the plan must never stop the scan and the exits
                LOGGER.exception("The daily plan step failed; it is tried again")

            phase = self.market_phase(now)
            if phase in ("open", "pre"):
                # MOOCOMP TRADING IS KING: zero latency, high-frequency bracket sentry, tightest loop
                if phase == "open":
                    self.watch_positions()
                elif self.positions:
                    self.watch_positions(extended=True)
                # Tight 1-second reactive polling when market or preorder is active
                active_poll = min(1.0, self.settings.poll_seconds)
                time.sleep(active_poll)
            else:
                self.write_status(state="outside_regular_session")
                if self.positions and phase == "after":
                    self.watch_positions(extended=True)
                time.sleep(self.settings.poll_seconds)

            # Research daemons run independently; the gainer never launches 1m research.


    def close(self) -> None:
        self.feed.close()
        self.trade_context.close()
        self.board.close()


def main() -> None:
    from .runtime import enter_project
    enter_project()
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    dblog.install("bot")  # every log line also goes to data/logs.sqlite3
    bot = MomentumPaperBot()
    try:
        bot.run()
    except KeyboardInterrupt:
        LOGGER.info("Stopping paper bot")
    except Exception:
        LOGGER.exception("Unhandled bot error")
    finally:
        bot.close()
        LOGGER.info("OpenD connection closed")


if __name__ == "__main__":
    main()
