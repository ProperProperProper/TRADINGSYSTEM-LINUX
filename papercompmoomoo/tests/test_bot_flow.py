from papercompmoomoo.account_ids import account_id
"""The bot's data flow end to end with no network and no broker: a fake download, a dummy trade connection, a temporary folder."""
import json
import os
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pandas as pd

from papercompmoomoo import bot as botmod
from papercompmoomoo import board as messages
from papercompmoomoo.bot import MomentumPaperBot, Settings


PROFILE = {"last_bar_volume": 5_000_000.0, "typical_bar_volume": 5_000_000.0, "median_range": 0.004}


def sessions(tickers, days=32, volume=1000.0):
    """Weekday one-minute session bars ending yesterday, stacked like a yfinance multi-ticker frame."""
    end = datetime.now(timezone.utc).replace(hour=0, minute=0, second=0, microsecond=0)
    frames = []
    for offset in range(days, 0, -1):
        day = end - timedelta(days=offset)
        if day.weekday() >= 5:
            continue
        index = pd.date_range(f"{day:%Y-%m-%d} 09:30", periods=26, freq="1min", tz="America/New_York")  # a regular session, in New York time whatever the date
        frames.append(pd.DataFrame({"Open": 10.0, "High": 11.0, "Low": 9.0, "Close": 10.5, "Volume": volume}, index=index))
    one = pd.concat(frames)
    one.index.name = "Datetime"
    return pd.concat({t: one for t in tickers}, axis=1)


class FakeQuote:
    """Moomoo's quote side: live candles and snapshots set by each test."""
    kline: dict = {}
    quotes: dict = {}

    def __init__(self, *a, **k):
        self.subscribed = []

    def query_subscription(self):
        return botmod.RET_OK, {"remain": 100 - len(self.subscribed)}

    def subscribe(self, codes, types, subscribe_push=False, session=None):
        self.subscribed += [c for c in codes if c not in self.subscribed]
        return botmod.RET_OK, None

    def unsubscribe(self, codes, types):
        self.subscribed = [c for c in self.subscribed if c not in codes]
        return botmod.RET_OK, None

    def unsubscribe_all(self):
        self.subscribed = []

    def get_cur_kline(self, code, num, ktype, autype=None):
        if code not in FakeQuote.kline or code not in self.subscribed:
            return -1, "no candles"
        return botmod.RET_OK, FakeQuote.kline[code]

    def get_market_snapshot(self, codes):
        rows = [{"code": c, **FakeQuote.quotes[c]} for c in codes if c in FakeQuote.quotes]
        return (botmod.RET_OK, pd.DataFrame(rows)) if rows else (-1, "none")

    def close(self):
        pass


class Dummy:
    holdings = [{"code": "US.AAA", "qty": 100, "cost_price": 10.0, "nominal_price": 10.2}]
    working_orders = []
    readable = True

    def __init__(self, *a, **k):
        self.orders = []

    def order_list_query(self, **k):
        if not Dummy.readable:
            return -1, "no connection"
        return botmod.RET_OK, pd.DataFrame(Dummy.working_orders, columns=["order_id", "code", "trd_side", "order_status", "qty", "price", "create_time", "remark"])

    def position_list_query(self, **k):
        if not Dummy.readable:
            return -1, "no connection"
        return botmod.RET_OK, pd.DataFrame(Dummy.holdings, columns=["code", "qty", "cost_price", "nominal_price"])

    def accinfo_query(self, **k):
        if not Dummy.readable:
            return -1, "no connection"
        return botmod.RET_OK, pd.DataFrame([{"us_cash": -5.0, "power": 100000.0, "total_assets": 50000.0}])

    def place_order(self, **k):
        self.orders.append(k)
        return botmod.RET_OK, pd.DataFrame([{"order_id": "1"}])

    max_buy = 4886.0
    accounts = None  # set in setUp

    def get_acc_list(self):
        if Dummy.accounts is None:
            return -1, "no list"
        return botmod.RET_OK, Dummy.accounts

    def acctradinginfo_query(self, **k):
        if Dummy.max_buy is None:
            return -1, "cannot read"
        return botmod.RET_OK, pd.DataFrame([{"max_cash_and_margin_buy": Dummy.max_buy, "max_cash_buy": Dummy.max_buy / 2}])

    def modify_order(self, op, order_id, qty=0, price=0, **k):
        self.cancelled = getattr(self, "cancelled", []) + [order_id]
        return botmod.RET_OK, pd.DataFrame([{"order_id": order_id}])

    def close(self):
        pass


class Flow(unittest.TestCase):
    def setUp(self):
        self.old = os.getcwd()
        self.dir = tempfile.mkdtemp()
        os.chdir(self.dir)
        self.addCleanup(os.chdir, self.old)
        self.fetches = []
        Dummy.accounts = pd.DataFrame([
            {"acc_id": 111, "trd_env": "REAL", "sim_acc_type": "N/A", "trdmarket_auth": ["HK", "US", "AU"]},
            {"acc_id": account_id("orb"), "trd_env": "SIMULATE", "sim_acc_type": "STOCK_AND_OPTION", "trdmarket_auth": ["US"]},
            {"acc_id": account_id("gainer"), "trd_env": "SIMULATE", "sim_acc_type": "COMPETITION", "trdmarket_auth": ["US"]},
        ])
        Dummy.max_buy = 4886.0
        Dummy.holdings, Dummy.working_orders, Dummy.readable = [{"code": "US.AAA", "qty": 100, "cost_price": 10.0, "nominal_price": 10.2}], [], True
        self.saved = (botmod.OpenSecTradeContext, botmod.yahoo_fetch, MomentumPaperBot.load_exchange_universe, botmod.time.sleep, botmod.OpenQuoteContext)
        botmod.OpenQuoteContext = FakeQuote
        FakeQuote.kline, FakeQuote.quotes = {}, {}

        def fake_fetch(chunk, start):
            self.fetches.append((list(chunk), start))
            return sessions(chunk)

        def universe(bot):
            bot.nasdaq_symbols = frozenset({"US.AAA"})
            return ("US.AAA", "US.BBB")

        botmod.OpenSecTradeContext = Dummy
        botmod.yahoo_fetch = fake_fetch
        MomentumPaperBot.load_exchange_universe = universe
        botmod.time.sleep = lambda s: None
        self.addCleanup(self.restore)
        self.bot = MomentumPaperBot(Settings(history_db="data/c1m.sqlite3", live_shortlist_size=5, accinfo_min_gap_seconds=0.0, snapshot_reuse_seconds=0.0))
        self.addCleanup(self.bot.store.close)
        self.addCleanup(self.bot.journal.close)
        self.assertTrue(self.bot.resolve_account())

    def restore(self):
        botmod.OpenSecTradeContext, botmod.yahoo_fetch, MomentumPaperBot.load_exchange_universe, botmod.time.sleep, botmod.OpenQuoteContext = self.saved

    def test_baselines_come_from_disk_after_one_backfill_and_a_second_start_downloads_nothing(self):
        self.bot.ensure_baselines()
        self.assertEqual(len(self.fetches), 1, "two symbols are one batch")
        self.assertEqual(self.bot.store.baseline_count(self.bot.baseline_cutoff()), 2)
        average = self.bot.store.db.execute("SELECT avg_volume FROM baselines WHERE symbol='AAA'").fetchone()[0]
        self.assertAlmostEqual(average, 26 * 1000.0)
        before = len(self.fetches)
        self.bot.ensure_baselines()
        self.assertEqual(len(self.fetches), before, "baselines are on disk: nothing is downloaded again")

    def test_positions_are_rebuilt_from_the_account_and_survive_in_a_file(self):
        self.bot.reconcile_positions()
        self.assertEqual(list(self.bot.positions), ["US.AAA"])
        self.assertEqual(self.bot.positions["US.AAA"]["entry"], 10.0)
        self.assertTrue(Path("data/positions.json").exists())
        self.bot.positions.clear()
        self.bot.reconcile_positions()
        self.assertIn("US.AAA", self.bot.positions, "a restart does not forget what the account holds")

    def test_a_position_sold_a_moment_ago_is_not_adopted_again_while_the_fill_lags(self):
        self.bot.reconcile_positions()
        self.bot.close_simulated_order("US.AAA", 100, 10.0, 0.0)
        self.bot.reconcile_positions()  # the account still shows the 100 shares: the sell has not filled yet
        self.assertNotIn("US.AAA", self.bot.positions, "adopting it again would sell it twice and leave a short")
        self.bot.exit_sent["US.AAA"] -= 500
        self.bot.reconcile_positions()
        self.assertIn("US.AAA", self.bot.positions, "after the grace a real holding is tracked again")

    def test_only_nasdaq_symbols_are_ordered_and_sizing_works_on_a_negative_cash_account(self):
        Dummy.holdings = []
        self.bot.place_simulated_order("US.BBB", 10.0, PROFILE, 50_000_000.0)  # NYSE-style: not in the Nasdaq set
        self.assertEqual(self.bot.trade_context.orders, [])
        self.bot.place_simulated_order("US.AAA", 10.0, PROFILE, 50_000_000.0)
        self.assertEqual(len(self.bot.trade_context.orders), 1)
        self.assertEqual(self.bot.trade_context.orders[0]["qty"], 331, "Tier 4: available (buying power, the cash is negative) / 15 positions = 3,333 of value at $10")
        # the position appears when the account shows it, not when the order is submitted
        self.assertNotIn("US.AAA", self.bot.positions)

    def test_the_account_is_checked_first_no_duplicate_no_second_order_while_one_is_working_no_penny_stocks(self):
        Dummy.holdings = [{"code": "US.AAA", "qty": 100, "cost_price": 10.0, "nominal_price": 10.2}]
        self.bot.place_simulated_order("US.AAA", 10.0, PROFILE, 50_000_000.0)
        self.assertEqual(self.bot.trade_context.orders, [], "already held")
        Dummy.holdings = []
        Dummy.working_orders = [{"order_id": 9, "code": "US.AAA", "trd_side": "BUY", "order_status": "SUBMITTED", "qty": 100, "price": 0.0, "create_time": datetime.now().strftime("%Y-%m-%d %H:%M:%S"), "remark": "placed by hand"}]
        self.bot.place_simulated_order("US.AAA", 10.0, PROFILE, 50_000_000.0)
        self.assertEqual(self.bot.trade_context.orders, [], "a buy for it is already working")
        Dummy.working_orders = []
        self.bot.place_simulated_order("US.AAA", 0.005, PROFILE, 50_000_000.0)
        self.assertEqual(self.bot.trade_context.orders, [], "below the minimum price (0.01)")
        Dummy.working_orders = [{"order_id": 9, "code": "US.AAA", "trd_side": "BUY", "order_status": "CANCELLED_ALL", "qty": 100, "price": 0.0, "create_time": datetime.now().strftime("%Y-%m-%d %H:%M:%S"), "remark": "x"}]
        self.bot.place_simulated_order("US.AAA", 10.0, PROFILE, 50_000_000.0)
        self.assertEqual(len(self.bot.trade_context.orders), 1, "a cancelled order does not block")

    def test_every_broker_call_names_the_competition_account_and_nothing_is_traded_without_it(self):
        Dummy.holdings = []
        self.assertEqual(self.bot.acc_id, account_id("gainer"), "the COMPETITION account, not the first simulated one and never the REAL one")
        self.bot.place_simulated_order("US.AAA", 10.0, PROFILE, 50_000_000.0)
        self.assertEqual(self.bot.trade_context.orders[-1]["acc_id"], account_id("gainer"))
        calls = []
        d = self.bot.trade_context
        for name in ("accinfo_query", "position_list_query", "order_list_query", "acctradinginfo_query"):
            original = getattr(d, name)
            setattr(d, name, lambda *a, _o=original, _n=name, **k: calls.append((_n, k.get("acc_id"))) or _o(*a, **k))
        self.bot.audit_account()
        self.bot.broker_max_buy("US.AAA", 10.0)
        self.assertTrue(calls and all(acc == account_id("gainer") for _, acc in calls), calls)
        # no competition account (not registered / two of them / the list unreadable): nothing is read or ordered
        for accounts in (Dummy.accounts[Dummy.accounts["sim_acc_type"] != "COMPETITION"], None):
            Dummy.accounts = accounts
            self.assertFalse(self.bot.resolve_account())
            n = len(self.bot.trade_context.orders)
            self.bot.place_simulated_order("US.AAA", 10.0, PROFILE, 50_000_000.0)
            self.assertEqual(len(self.bot.trade_context.orders), n)
            self.assertIsNone(self.bot.account_snapshot())

    def test_entries_are_slip_protected_limits_and_exits_market_orders_to_the_simulated_account(self):
        from futu import OrderType, TrdEnv, TrdSide

        Dummy.holdings = []
        self.bot.place_simulated_order("US.AAA", 10.0, PROFILE, 50_000_000.0)
        self.bot.close_simulated_order("US.AAA", 50, 10.0, 1.0)
        self.assertEqual(len(self.bot.trade_context.orders), 2)
        buy, sell = self.bot.trade_context.orders
        self.assertEqual((buy["order_type"], buy["trd_env"], buy["price"]), (OrderType.NORMAL, TrdEnv.SIMULATE, 10.0))
        self.assertEqual(buy["adjust_limit"], 0.005, "native adjustment is bounded to 0.5%; sizing reserves its worst cent price")
        self.assertEqual((sell["order_type"], sell["trd_env"], sell["price"]), (OrderType.MARKET, TrdEnv.SIMULATE, 0))
        self.assertEqual([o["trd_side"] for o in self.bot.trade_context.orders], [TrdSide.BUY, TrdSide.SELL])
        self.assertEqual(self.bot.submit_market_order("US.AAA", TrdSide.BUY, 0, "x")[0], -1, "zero or fractional shares are refused")
        self.assertEqual(len(self.bot.trade_context.orders), 2)

    def test_the_audit_cancels_the_bots_own_left_over_orders_at_start_up_but_never_someone_elses(self):
        created = datetime.now() .strftime("%Y-%m-%d %H:%M:%S")
        Dummy.working_orders = [
            {"order_id": 1, "code": "US.ACTU", "trd_side": "BUY", "order_status": "SUBMITTED", "qty": 640760, "price": 0.0, "create_time": created, "remark": "papercompmoomoo momentum entry"},
            {"order_id": 2, "code": "US.ACTU", "trd_side": "BUY", "order_status": "SUBMITTED", "qty": 640760, "price": 0.0, "create_time": created, "remark": "papercompmoomoo momentum entry"},
            {"order_id": 3, "code": "US.MSFT", "trd_side": "BUY", "order_status": "SUBMITTED", "qty": 10, "price": 0.0, "create_time": created, "remark": "placed by hand"},
        ]
        self.assertEqual(self.bot.audit_account(at_startup=True), 2)
        self.assertEqual(sorted(self.bot.trade_context.cancelled), [1, 2])
        self.bot.trade_context.cancelled = []
        self.assertEqual(self.bot.audit_account(at_startup=False), 1, "in the session only the duplicate goes; a fresh single order stays")
        Dummy.working_orders = Dummy.working_orders[:1] + Dummy.working_orders[2:]
        self.bot.trade_context.cancelled = []
        self.assertEqual(self.bot.audit_account(at_startup=False), 0)

    def test_the_size_is_what_is_available_the_broker_maximum_and_what_the_stock_trades(self):
        Dummy.holdings = []
        Dummy.max_buy = 200.0
        self.bot.place_simulated_order("US.AAA", 10.0, PROFILE, 50_000_000.0)
        self.assertEqual(self.bot.trade_context.orders[-1]["qty"], 200, "the broker's maximum")
        Dummy.max_buy = 4886.0
        self.bot.place_simulated_order("US.AAA", 10.0, {**PROFILE, "last_bar_volume": 3_000.0, "typical_bar_volume": 3_000.0}, 50_000_000.0)
        self.assertEqual(self.bot.trade_context.orders[-1]["qty"], 300, "10% of the last bar's volume")
        self.bot.place_simulated_order("US.AAA", 10.0, PROFILE, 7_500.0)
        self.assertEqual(self.bot.trade_context.orders[-1]["qty"], 150, "2% of the average daily volume")
        n = len(self.bot.trade_context.orders)
        self.bot.place_simulated_order("US.AAA", 10.0, {**PROFILE, "last_bar_volume": 0.0, "typical_bar_volume": 0.0}, 50_000_000.0)
        Dummy.max_buy = None
        self.bot.place_simulated_order("US.AAA", 10.0, PROFILE, 50_000_000.0)
        self.assertEqual(len(self.bot.trade_context.orders), n, "no volume or no broker answer: nothing is bought")

    def test_orders_and_trades_are_journaled_with_the_signal_behind_them(self):
        Dummy.holdings = []
        sid = self.bot.journal.signal(1, "US.AAA", 1, 10.0, 6.0, 4.0, PROFILE, 5e7, True, "candidate")
        outcome = self.bot.place_simulated_order("US.AAA", 10.0, PROFILE, 50_000_000.0, signal_id=sid, rvol=6.0, move_pct=4.0)
        self.assertTrue(outcome["ordered"])
        order = self.bot.journal.db.execute("SELECT side, qty, signal_id, rvol FROM orders").fetchone()
        self.assertEqual((order[0], order[2], order[3]), ("BUY", sid, 6.0))
        self.bot.positions["US.AAA"] = {"qty": order[1], "entry": 10.0, "high": 10.0, "opened": datetime.now(timezone.utc)}
        self.bot.close_simulated_order("US.AAA", order[1], 10.4, 4.0, "trailing stop")
        trade = self.bot.journal.db.execute("SELECT symbol, pnl_pct, reason, signal_id, rvol FROM trades").fetchone()
        self.assertEqual(trade, ("US.AAA", 4.0, "trailing stop", sid, 6.0))
        refused = self.bot.place_simulated_order("US.BBB", 10.0, PROFILE, 50_000_000.0)
        self.assertFalse(refused["ordered"])
        self.assertIn("Nasdaq", refused["reason"])

    def test_after_an_order_the_account_is_watched_so_the_fill_is_tracked_within_seconds_not_at_the_next_scan(self):
        Dummy.holdings = []
        self.bot.check_positions = lambda: self.checked.append(dict(self.bot.positions))
        self.checked = []
        self.bot.watch_positions(now=100.0)  # nothing held, nothing working: no account queries, no price checks
        self.assertEqual(self.checked, [])
        self.bot.place_simulated_order("US.AAA", 10.0, PROFILE, 50_000_000.0)
        self.assertEqual(self.bot.working_orders, {"US.AAA"})
        Dummy.holdings = [{"code": "US.AAA", "qty": 1250, "cost_price": 10.2, "nominal_price": 10.2}]  # the order filled
        self.bot.watch_positions(now=101.0)
        self.assertIn("US.AAA", self.bot.positions, "the fill is tracked at once")
        self.assertEqual(len(self.checked), 1, "and its price is checked in the same pass, so a stop can fire")
        self.bot.working_orders = set()
        queries = []
        original = self.bot.reconcile_positions
        self.bot.reconcile_positions = lambda *a, **k: queries.append(1) or original(*a, **k)
        self.bot.watch_positions(now=105.0)
        self.assertEqual(queries, [], "inside watch_seconds the account is not read again")
        self.bot.watch_positions(now=125.0)
        self.assertEqual(len(queries), 1)

    NOW = datetime(2026, 10, 19, 15, 21, tzinfo=timezone.utc)  # 11:21 New York: bars through the 11:00 start are closed

    def _seed_scan(self, ask=10.46):
        """Yahoo has three prior sessions and a stale, partial today; Moomoo has today's final bars. AAA really surged, BBB only moved, CCC has no Moomoo candles."""
        Dummy.holdings = []
        Dummy.max_buy = 1e9
        today_close = [10.0 + 0.06 * (k + 1) for k in range(8)]
        frames = []
        for day in ("2026-10-14", "2026-10-15", "2026-10-16"):
            index = pd.date_range(f"{day} 13:30", periods=26, freq="1min", tz="UTC").tz_convert("America/New_York")
            frames.append(pd.DataFrame({"Open": 10.0, "High": 10.1, "Low": 9.9, "Close": 10.0, "Volume": 1000.0}, index=index))
        idx = pd.date_range("2026-10-19 13:30", periods=8, freq="1min", tz="UTC").tz_convert("America/New_York")
        yahoo_volume = [1000.0] * 5 + [30.0, 30.0, 30.0]  # settled early on, partial at the end
        today = pd.DataFrame({"Open": [c - 0.05 for c in today_close], "High": [c + 0.02 for c in today_close], "Low": [c - 0.07 for c in today_close],
                              "Close": today_close, "Volume": yahoo_volume}, index=idx)
        history = pd.concat(frames + [today])
        for ticker in ("AAA", "BBB", "CCC"):
            self.bot.store.upsert_frame(ticker, history)
            self.bot.store.set_baseline(ticker, 26000.0, "2026-10-19")
        self.bot.nasdaq_symbols = frozenset({"US.AAA", "US.BBB", "US.CCC"})
        self.bot.symbols = ("US.AAA", "US.BBB", "US.CCC")

        def moomoo_bars(volumes):  # Moomoo labels bars by their END time
            ends = [(t + pd.Timedelta(minutes=1)).tz_localize(None).strftime("%Y-%m-%d %H:%M:%S") for t in idx]
            return pd.DataFrame({"time_key": ends, "open": today["Open"].to_numpy(), "high": today["High"].to_numpy(), "low": today["Low"].to_numpy(),
                                 "close": today_close, "volume": volumes})
        # Moomoo's volume is 80% of Yahoo's on the settled bars; AAA then surges on the last bars, BBB does not
        FakeQuote.kline = {"US.AAA": moomoo_bars([800.0] * 5 + [14000.0] * 3), "US.BBB": moomoo_bars([800.0] * 8)}  # CCC: none
        quote = {"last_price": 10.45, "bid_price": 10.44, "ask_price": ask, "prev_close_price": 10.0, "volume": 1e5, "volume_ratio": 3.0, "update_time": "2026-10-19 11:21:00"}
        FakeQuote.quotes = {"US.AAA": quote, "US.BBB": quote, "US.CCC": quote}
        # the fetch inside the scan returns nothing new: the store already holds what Yahoo has
        botmod.yahoo_fetch = lambda chunk, start: None

    def test_open_positions_are_priced_from_moomoo_quotes_and_stopped_out_on_them(self):
        Dummy.holdings = [{"code": "US.AAA", "qty": 100, "cost_price": 10.0, "nominal_price": 10.0}]
        self.bot.reconcile_positions()
        FakeQuote.quotes = {"US.AAA": {"last_price": 9.7, "bid_price": 9.69, "ask_price": 9.71, "prev_close_price": 10.0, "volume": 1, "volume_ratio": 1, "update_time": "t"}}
        self.bot.check_positions()
        sells = [o for o in self.bot.trade_context.orders if o["trd_side"] == botmod.TrdSide.SELL]
        self.assertEqual(len(sells), 1, "-3% on the live quote is below the 2% stop")
        self.assertNotIn("US.AAA", self.bot.positions)

    def test_the_take_profit_level_arms_a_trailing_exit_it_does_not_sell_the_winner(self):
        from dataclasses import replace

        self.bot.settings = replace(self.bot.settings, take_profit_pct=1.5, trailing_stop_pct=1.0)
        self.bot.positions["US.AAA"] = {"qty": 100, "entry": 10.0, "high": 10.0, "opened": datetime.now(timezone.utc)}
        self.bot.manage_position("US.AAA", 10.1, 1.0)  # +1.0%: not yet armed
        self.assertEqual((self.bot.trade_context.orders, bool(self.bot.positions["US.AAA"].get("armed"))), ([], False))
        self.bot.manage_position("US.AAA", 10.16, 1.6)  # +1.6%: the level is reached, the position is armed, nothing is sold
        self.assertEqual((self.bot.trade_context.orders, self.bot.positions["US.AAA"]["armed"]), ([], True))
        self.bot.manage_position("US.AAA", 10.4, 4.0)  # the winner runs
        self.assertEqual(self.bot.trade_context.orders, [])
        self.bot.manage_position("US.AAA", 10.29, 2.9)  # 1.06% off the 10.40 high: the trailing exit
        self.assertEqual([o["trd_side"] for o in self.bot.trade_context.orders], [botmod.TrdSide.SELL])
        self.assertEqual(self.bot.journal.db.execute("SELECT reason FROM trades").fetchone()[0], "trailing take profit")

    def test_the_bots_own_auditor_repairs_a_stale_order_an_untracked_position_and_an_overdue_one_and_writes_its_report(self):
        import json

        created = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
        Dummy.working_orders = [
            {"order_id": 1, "code": "US.ZZZ", "trd_side": "BUY", "order_status": "SUBMITTED", "qty": 10, "price": 0.0, "create_time": created, "remark": "papercompmoomoo momentum entry"},
            {"order_id": 2, "code": "US.ZZZ", "trd_side": "BUY", "order_status": "SUBMITTED", "qty": 10, "price": 0.0, "create_time": created, "remark": "papercompmoomoo momentum entry"},
        ]
        Dummy.holdings = [{"code": "US.AAA", "qty": 100, "cost_price": 10.0, "nominal_price": 10.0}]  # held by the account, unknown to the bot
        now = datetime.now(timezone.utc)
        findings = self.bot.run_audit(now=now)
        self.assertEqual({f.fix for f in findings if f.fix}, {"cancel_orders", "adopt_positions"})
        self.assertEqual(self.bot.trade_context.cancelled, [2], "the duplicate goes, the first order stays")
        self.assertIn("US.AAA", self.bot.positions, "the unknown position is adopted")
        report = json.loads(Path("data/audit.json").read_text())
        self.assertEqual(report["errors"], 0)
        self.assertGreaterEqual(report["warnings"], 2)
        self.assertGreaterEqual(report["fixed"], 2)
        # the adopted position, held far beyond its limit while the market is open, is exited at Moomoo's live price
        now = datetime(2026, 10, 21, 15, 30, tzinfo=timezone.utc)  # a fixed mid-session time (11:30 New York): the hold clock starts at the regular open, so the real clock must not decide
        self.bot.positions["US.AAA"]["opened"] = now - timedelta(hours=2)
        FakeQuote.quotes = {"US.AAA": {"last_price": 10.2, "bid_price": 10.19, "ask_price": 10.21, "prev_close_price": 10.0, "volume": 1, "volume_ratio": 1, "update_time": "t"}}
        self.bot.phase_cache = (float("inf"), "open")
        self.bot.run_audit(now=now)
        sells = [o for o in self.bot.trade_context.orders if o["trd_side"] == botmod.TrdSide.SELL]
        self.assertEqual(len(sells), 1)
        self.assertEqual(self.bot.journal.db.execute("SELECT reason FROM trades").fetchone()[0], "audit: overdue")

    def test_the_auditor_restarts_the_bot_through_the_gate_when_its_code_changed_unless_an_order_is_working(self):
        Dummy.holdings = []
        self.bot.write_status(code_hash="old-hash-that-no-longer-matches-the-files")
        with self.assertRaises(SystemExit):
            self.bot.run_audit()
        Dummy.holdings = [{"code": "US.AAA", "qty": 100, "cost_price": 10.0, "nominal_price": 10.0}]
        self.bot.reconcile_positions()
        Dummy.working_orders = [{"order_id": 9, "code": "US.BBB", "trd_side": "BUY", "order_status": "SUBMITTED", "qty": 10, "price": 5.0, "create_time": datetime.now().strftime("%Y-%m-%d %H:%M:%S"), "remark": "placed by hand"}]
        findings = self.bot.run_audit()  # an order is working: no restart, it waits
        self.assertTrue(any(f.area == "code" and f.fix is None for f in findings))
        Dummy.working_orders = []
        with self.assertRaises(SystemExit):  # holding a position but no order working: restarts (the position is adopted again)
            self.bot.run_audit()

    def test_a_fill_since_the_last_look_is_picked_up_and_tracked_from_its_cost(self):
        Dummy.holdings = []
        self.bot.reconcile_positions()
        self.assertEqual(self.bot.positions, {})
        Dummy.holdings = [{"code": "US.AAA", "qty": 300, "cost_price": 9.5, "nominal_price": 9.6}]
        self.bot.reconcile_positions()
        self.assertEqual((self.bot.positions["US.AAA"]["entry"], self.bot.positions["US.AAA"]["qty"]), (9.5, 300))



    def test_a_blocked_message_left_by_the_supervisor_is_cleared_once_the_bot_runs(self):
        self.bot.write_status(state="tests_failed", error="tests failed: the bot was not started")
        self.assertIn("error", json.loads(Path("data/status.json").read_text()))
        self.bot.write_status(state="scanning")
        self.assertNotIn("error", json.loads(Path("data/status.json").read_text()))

    def _plan(self, terms=None):
        from papercompmoomoo import planner
        return planner.new_plan("2026-10-21", [{"symbol": "US.AAA", "why": "closed at its high on volume", "terms": terms or {"stop_pct": 1.4, "trail_pct": 2.2, "take_profit_pct": 6.0, "hold_minutes": 100.0}, "score": 5.0}], 1, "after_close")

    def _open_setup(self):
        from dataclasses import replace
        Dummy.holdings = []
        self.bot.ensure_baselines()
        self.bot.settings = replace(self.bot.settings, plan_retry_seconds=0)
        FakeQuote.quotes = {"US.AAA": {"last_price": 10.0, "bid_price": 9.99, "ask_price": 10.01, "prev_close_price": 9.5, "volume": 1000.0, "volume_ratio": 1.0, "update_time": "x"}}

    def test_gainer_entry_rechecks_current_evidence_instead_of_trusting_discovery(self):
        self._open_setup()
        plan = self._plan()
        plan["selection"] = {"strategy": "nasdaq_hottest_gainers_live"}
        FakeQuote.quotes["US.AAA"]["volume_ratio"] = 2.0
        self.bot.place_plan_open(plan, datetime.now(timezone.utc))
        self.assertEqual(self.bot.trade_context.orders, [])
        self.assertIn("one-minute", plan["candidates"][0]["note"])

    def test_a_saved_research_selection_is_replaced_before_gainer_execution(self):
        from unittest.mock import Mock
        from papercompmoomoo import planner
        self._open_setup()
        old = self._plan()
        old["selection"] = {"strategy": planner.MOMENTUM_STRATEGY}
        planner.save_plan(old)
        fresh = planner.new_plan(old["session"], [], 1, "after_close", {"strategy": "nasdaq_hottest_gainers_live"})
        self.bot.build_plan = Mock(return_value=fresh)
        self.bot.market_phase = lambda now: "open"
        self.bot.plan_tick(datetime(2026, 10, 21, 15, 0, tzinfo=timezone.utc))
        self.bot.build_plan.assert_called_once()
        self.assertEqual(self.bot.trade_context.orders, [])

    def test_at_the_open_a_candidate_is_bought_at_market_with_its_own_conditions_once_and_never_twice(self):
        from papercompmoomoo import planner
        self._open_setup()
        plan = self._plan()
        now = datetime.now(timezone.utc)
        self.bot.place_plan_open(plan, now)
        self.assertEqual(plan["candidates"][0]["status"], planner.PLACED)
        self.assertEqual(len(self.bot.trade_context.orders), 1)
        order = self.bot.trade_context.orders[0]
        self.assertEqual(str(order["order_type"]).split(".")[-1], "NORMAL")
        self.assertEqual(self.bot.load_terms()["US.AAA"]["stop_pct"], 1.4, "the conditions assessed for this symbol are kept for its exits")
        # the account now shows the position: a second pass buys nothing
        Dummy.holdings = [{"code": "US.AAA", "qty": 100, "cost_price": 10.0, "nominal_price": 10.0}]
        plan["candidates"][0]["status"] = planner.PENDING
        self.bot.place_plan_open(plan, now + timedelta(seconds=30))
        self.assertEqual(len(self.bot.trade_context.orders), 1, "one position per candidate, decided from the account")
        self.assertEqual(plan["candidates"][0]["status"], planner.PLACED)
        kinds = [r[0] for r in self.bot.journal.db.execute("SELECT event FROM plan_events ORDER BY id")]
        self.assertIn("open_order", kinds)
        self.assertIn("held", kinds)

    def test_a_failed_order_is_retried_after_the_account_is_read_again_and_gives_up_after_the_retry_limit(self):
        from papercompmoomoo import planner
        self._open_setup()
        plan = self._plan()
        real = self.bot.trade_context.place_order
        calls = {"n": 0}

        def flaky(**k):
            calls["n"] += 1
            return (-1, "timeout") if calls["n"] < 3 else real(**k)

        self.bot.trade_context.place_order = flaky
        now = datetime.now(timezone.utc)
        for i in range(4):
            self.bot.place_plan_open(plan, now + timedelta(seconds=30 * i))
        cand = plan["candidates"][0]
        self.assertEqual((cand["status"], cand["attempts"]), (planner.PLACED, 3), "two failures, then the third attempt went through")
        self.assertEqual(len(self.bot.trade_context.orders), 1)
        # a broker that never accepts: marked failed after the limit, nothing is ordered
        plan2 = self._plan()
        self.bot.trade_context.place_order = lambda **k: (-1, "timeout")
        for i in range(10):
            self.bot.place_plan_open(plan2, now + timedelta(seconds=300 + 30 * i))
        self.assertEqual(plan2["candidates"][0]["status"], planner.FAILED)
        self.assertEqual(plan2["candidates"][0]["attempts"], self.bot.settings.plan_retry_max)

    def test_a_final_refusal_is_not_retried_and_an_unreadable_account_is_retried(self):
        from papercompmoomoo import planner
        self._open_setup()
        plan = self._plan()
        Dummy.readable = False
        self.bot.place_plan_open(plan, datetime.now(timezone.utc))
        self.assertEqual((plan["candidates"][0]["status"], plan["candidates"][0]["attempts"]), (planner.PENDING, 1), "the account could not be read: nothing is traded blind, it is tried again")
        Dummy.readable = True
        self.bot.nasdaq_symbols = frozenset()  # not a Nasdaq symbol any more: final
        self.bot.place_plan_open(plan, datetime.now(timezone.utc) + timedelta(seconds=30))
        self.assertEqual(plan["candidates"][0]["status"], planner.SKIPPED)
        self.assertEqual(self.bot.trade_context.orders, [])

    def test_two_hours_before_the_open_a_pre_market_limit_order_is_placed_and_marked(self):
        from papercompmoomoo import planner
        self._open_setup()
        plan = self._plan()
        self.bot.refresh_pre_open(plan, datetime.now(timezone.utc))
        cand = plan["candidates"][0]
        self.assertEqual(cand["status"], planner.PRE_PLACED)
        order = self.bot.trade_context.orders[0]
        self.assertEqual(str(order["order_type"]).split(".")[-1], "NORMAL")
        self.assertTrue(order["fill_outside_rth"])
        self.assertTrue(order["remark"].startswith("papercompmoomoo pre-open plan"))
        self.assertAlmostEqual(order["price"], 10.01 * 1.003, places=2)
        self.assertIn("gap", cand["note"])

    def test_every_candidate_gets_an_equal_share_that_fits_the_buying_power_so_none_is_refused_for_it(self):
        from papercompmoomoo import planner
        # equity 100,000, buying power 175,000, 15 candidates, funds cap 10,000, leverage 2, buffer 1%
        each = planner.notional_each(100_000.0, 175_000.0, 15, 10_000.0, 2.0, 0.01)
        self.assertAlmostEqual(each, 175_000.0 * 0.99 / 15, places=6, msg="the buying power is the limit here, not the equity share (13,333)")
        self.assertLessEqual(each * 15, 175_000.0, "the shares add up to no more than the buying power")
        self.assertAlmostEqual(planner.notional_each(100_000.0, 400_000.0, 15, 10_000.0, 2.0, 0.01), 100_000.0 * 2 / 15, places=6, msg="plenty of buying power: the equal share of equity at the leverage")
        self.assertEqual(planner.notional_each(100_000.0, 400_000.0, 4, 10_000.0, 2.0, 0.01), 20_000.0, "capped at the funds cap at the leverage")
        self.assertEqual(planner.notional_each(0.0, 1.0, 5, 10_000.0, 2.0, 0.01), 0.0)

    def test_the_share_is_worked_out_once_stored_in_the_plan_and_every_order_is_held_to_it(self):
        from papercompmoomoo import planner
        self._open_setup()
        plan = self._plan()
        plan["candidates"] = plan["candidates"] * 10  # ten candidates (the same symbol listed ten times is enough for the arithmetic)
        share = self.bot.plan_share(plan)
        self.assertAlmostEqual(share, min(50_000.0 * 2 / 10, 100_000.0 * 0.80 / 10), places=6, msg="equity 50,000, buying power 100,000 in the fake account; at most 80% of the buying power is used")
        self.assertEqual(plan["notional_each"], share)
        Dummy.holdings = []
        self.bot.place_simulated_order("US.AAA", 10.0, PROFILE, 50_000_000.0, max_positions=15, notional_each=share)
        self.assertEqual(self.bot.trade_context.orders[-1]["qty"], int(min(share, 50_000 / 15) // 10.05), "held to its share or the Tier 4 notional, whichever is smaller")
        self.assertIn("share", [r[0] for r in self.bot.journal.db.execute("SELECT event FROM plan_events")])

    def test_the_momentum_plan_is_announced_on_the_board_without_an_agent_changing_its_candidates(self):
        from papercompmoomoo import board as bd, planner
        import papercompmoomoo.swings as sw
        real_selection, real_history = planner.momentum_selection, sw.history_for_plan
        self.addCleanup(setattr, planner, "momentum_selection", real_selection)
        self.addCleanup(setattr, sw, "history_for_plan", real_history)
        planner.momentum_selection = lambda session, size, tradable: {"schema": 1, "strategy": planner.MOMENTUM_STRATEGY, "data_cutoff_day": 20748,
            "data_cutoff": "2026-10-22", "lookback_sessions": 20, "candidates": [{"symbol": "US.AAA", "score": 0.2, "observations": 20, "rank": 1}]}
        sw.history_for_plan = lambda db, symbol: {"median_bar_range_pct": 0.8, "median_give_back_from_high_pct": 2.0, "p75_run_up_from_open_pct": 5.0}
        self.bot.build_plan(datetime.now(timezone.utc), "2026-10-23")
        board = bd.recent(self.bot.board, 20)
        kinds = [(m["sender"], m["recipient"], m["kind"]) for m in board]
        self.assertIn(("planner", "*", "decision"), kinds, "the plan is announced")
        self.assertIn("Momentum plan", next(m["body"] for m in board if m["sender"] == "planner" and m["kind"] == "decision"))

    def test_paper_mode_records_loss_limit_but_allows_new_simulated_orders_and_exits(self):
        self._open_setup()
        Dummy.holdings = []
        self.assertTrue(self.bot.place_simulated_order("US.AAA", 10.0, PROFILE, 50_000_000.0)["ordered"], "inside the limits: trading as usual")
        # the account's equity falls 3% below the day's start
        self.bot.guard.state["day_start_equity"] = 50_000.0 / 0.97
        Dummy.holdings = []
        before = len(self.bot.trade_context.orders)
        out = self.bot.place_simulated_order("US.AAA", 10.0, PROFILE, 50_000_000.0)
        self.assertTrue(out["ordered"], "paper testing continues without loss halts")
        self.assertEqual(len(self.bot.trade_context.orders), before + 1, "the simulated order was sent")
        # an exit still works: the breaker can only reduce risk
        Dummy.holdings = [{"code": "US.AAA", "qty": 100, "cost_price": 10.0, "nominal_price": 9.0}]
        self.bot.reconcile_positions()
        self.bot.manage_position("US.AAA", 9.0, 0.0)
        self.assertNotIn("US.AAA", self.bot.positions, "the stop still sells")

    def test_the_hard_rules_are_pinned_on_the_board_once_and_are_in_every_agents_instructions(self):
        from papercompmoomoo import agents, owner_rules
        pinned = [m for m in messages.recent(self.bot.board, 50) if m["sender"] == "owner" and m["topic"] == "hard rules"]
        self.assertEqual(len(pinned), 1)
        self.assertFalse(owner_rules.pin_hard_rules(self.bot.board), "not pinned twice")
        for role in agents.ROLES:
            self.assertIn("HARD RULES from the owner", agents.system_prompt(role))

    def test_the_cap_is_ten_thousand_of_funds_at_two_times_leverage_and_up_to_fifteen_positions(self):
        self.assertEqual((self.bot.settings.target_leverage, self.bot.settings.max_position_funds, self.bot.settings.plan_size), (2.0, 10_000.0, 15))
        self._open_setup()
        self.bot.place_simulated_order("US.AAA", 10.0, PROFILE, 50_000_000.0, max_positions=15)
        self.assertEqual(self.bot.trade_context.orders[0]["qty"], 331, "Tier 4 notional: equity / 15 positions")
        self.assertLessEqual(self.bot.trade_context.orders[0]["qty"] * 10.0, 20_000.0)
        self.assertEqual(self.bot.plan_max_positions(), 15)

    def test_after_the_close_the_plan_is_built_from_the_pool_with_the_agents_picks_and_without_it(self):
        from papercompmoomoo import planner
        real = planner.momentum_selection
        import papercompmoomoo.swings as sw
        real_history = sw.history_for_plan
        self.addCleanup(setattr, planner, "momentum_selection", real)
        self.addCleanup(setattr, sw, "history_for_plan", real_history)
        planner.momentum_selection = lambda session, size, tradable: {"schema": 1, "strategy": planner.MOMENTUM_STRATEGY, "data_cutoff_day": 20748,
            "data_cutoff": "2026-10-22", "lookback_sessions": 20, "candidates": [{"symbol": "US.AAA", "score": 0.2, "observations": 20, "rank": 1}]}
        sw.history_for_plan = lambda db, symbol: {"median_bar_range_pct": 0.8, "median_give_back_from_high_pct": 2.0, "p75_run_up_from_open_pct": 5.0}
        plan = self.bot.build_plan(datetime.now(timezone.utc), "2026-10-23")
        self.assertEqual([c["symbol"] for c in plan["candidates"]], ["US.AAA"])
        self.assertEqual(plan["selection"]["strategy"], planner.MOMENTUM_STRATEGY)
        self.assertIn("trail_pct", plan["candidates"][0]["terms"])
        self.assertEqual(planner.load_plan()["session"], "2026-10-23")
        self.assertIn("plan_built", [r[0] for r in self.bot.journal.db.execute("SELECT event FROM plan_events")])

    def test_plan_tick_follows_opend_a_candidate_is_bought_when_it_says_open_not_before(self):
        from papercompmoomoo import planner
        self._open_setup()
        plan = self._plan()
        now = datetime(2026, 10, 21, 10, 0, tzinfo=planner.NEW_YORK).astimezone(timezone.utc)
        today = now.astimezone(planner.NEW_YORK).date().isoformat()
        plan["session"] = today
        planner.save_plan(plan)
        self.bot.build_plan = lambda *a, **k: None  # whatever the clock says, this test is about the open, not about building the next plan
        self.bot.refresh_pre_open = lambda *a, **k: None  # nor about the pre-open order, which the lead window before the real open places by design
        self.bot.market_phase = lambda now: "closed"
        self.bot.plan_tick(now)
        self.assertEqual(self.bot.trade_context.orders, [], "the market is not open")
        self.bot.market_phase = lambda now: "open"
        self.bot.plan_tick(now)
        self.assertEqual(len(self.bot.trade_context.orders), 1)
        self.assertEqual(planner.load_plan()["candidates"][0]["status"], planner.PLACED)

    def test_a_restart_orders_again_a_pre_market_candidate_whose_order_is_gone_and_never_one_that_is_held(self):
        from papercompmoomoo import planner
        self._open_setup()
        plan = self._plan()
        plan["stage"], plan["candidates"][0]["status"] = "pre_open", planner.PRE_PLACED
        self.bot.refresh_pre_open(plan, datetime.now(timezone.utc))
        self.assertEqual(len(self.bot.trade_context.orders), 1, "no position and no working order: ordered again")
        self.assertEqual(plan["candidates"][0]["status"], planner.PRE_PLACED)
        self.bot.trade_context.orders.clear()
        Dummy.holdings = [{"code": "US.AAA", "qty": 100, "cost_price": 10.0, "nominal_price": 10.0, "market_val": 1000.0}]
        plan["candidates"][0]["status"] = planner.PENDING
        self.bot.refresh_pre_open(plan, datetime.now(timezone.utc))
        self.assertEqual(self.bot.trade_context.orders, [], "held: nothing more is bought")
        self.assertEqual(plan["candidates"][0]["status"], planner.PLACED)

    def test_outside_the_session_a_position_is_stopped_with_a_limit_order_but_not_timed_out(self):
        from futu import OrderType, TrdSide
        self._open_setup()
        self.bot.positions["US.AAA"] = {"qty": 50, "entry": 10.0, "high": 10.0, "opened": datetime.now(timezone.utc) - timedelta(hours=6)}
        self.bot.manage_position("US.AAA", 10.1, 0.0, True)
        self.assertEqual(self.bot.trade_context.orders, [], "the 90-minute limit waits for the session")
        self.bot.manage_position("US.AAA", 9.0, 0.0, True)
        order = self.bot.trade_context.orders[-1]
        self.assertEqual((order["order_type"], order["trd_side"], order["price"], order["fill_outside_rth"]), (OrderType.NORMAL, TrdSide.SELL, 8.91, True))

    def test_the_plans_candidates_are_fixed_for_the_session_and_the_watchlist_group_follows_only_them(self):
        from papercompmoomoo import planner
        self._open_setup()
        plan = self._plan()
        now = datetime(2026, 10, 21, 12, 30, tzinfo=timezone.utc)  # 08:30 New York, the pre-market, before the 09:30 open
        plan["session"] = planner.session_key(planner.next_open(now))
        plan["stage"], plan["candidates"][0]["status"] = "pre_open", planner.PLACED
        planner.save_plan(plan)
        built = []
        self.bot.build_plan = lambda *a, **k: built.append(a)
        self.bot.market_phase = lambda now: "pre"
        self.bot.watchlist = [{"symbol": "US.NEWSCAN"}]
        synced = []
        self.bot.feed.sync_watchlist = lambda codes, *a, **k: synced.append(list(codes)) or ([], [])
        self.bot.plan_tick(now)
        self.assertEqual(built, [], "the plan is not rebuilt once it exists for the session")
        self.assertEqual(synced, [["US.AAA"]], "only the plan's candidates, not a later scan's")
        self.assertEqual([c["symbol"] for c in planner.load_plan()["candidates"]], ["US.AAA"])

    def test_the_next_plan_is_frozen_after_close_without_touching_quotes_or_orders(self):
        from papercompmoomoo import planner
        now = datetime(2026, 10, 21, 23, 0, tzinfo=planner.NEW_YORK).astimezone(timezone.utc)
        old = self._plan()
        planner.save_plan(old)
        session = planner.session_key(planner.next_open(now))
        fresh = planner.new_plan(session, [{"symbol": "US.AAA", "terms": {}, "score": 1.0}], 2, "after_close")
        built, refreshed = [], []
        self.bot.build_plan = lambda *args, **kwargs: built.append(args) or fresh
        self.bot.refresh_pre_open = lambda *args, **kwargs: refreshed.append(args)
        self.bot.market_phase = lambda current: "closed"
        self.bot.feed.sync_watchlist = lambda codes, *args, **kwargs: ([], [])
        self.bot.plan_tick(now)
        self.assertEqual(len(built), 1)
        self.assertEqual(built[0][1], session)
        self.assertEqual(refreshed, [], "quotes and premarket orders wait for the two-hour lead window")

    def test_an_unlocked_plan_is_replaced_once_in_the_lead_window_by_a_fresh_locked_one_and_its_dropped_orders_are_cancelled(self):
        from papercompmoomoo import planner
        self._open_setup()
        now = datetime(2026, 10, 21, 12, 30, tzinfo=timezone.utc)  # 08:30 New York, inside the lead window
        old = self._plan()
        old["session"], old["locked"] = planner.session_key(planner.next_open(now)), False
        old["candidates"][0]["symbol"], old["candidates"][0]["status"] = "US.OLD", planner.PRE_PLACED
        planner.save_plan(old)
        fresh = planner.new_plan(old["session"], [{"symbol": "US.AAA", "terms": {}, "score": 1.0}], 2, "after_close")
        built, cancelled = [], []
        self.bot.build_plan = lambda *a, **k: built.append(a) or fresh
        self.bot.cancel_symbol_orders = lambda symbol: cancelled.append(symbol)
        self.bot.market_phase = lambda now: "pre"
        self.bot.refresh_pre_open = lambda *a, **k: None
        self.bot.plan_tick(now)
        self.assertEqual(len(built), 1)
        self.assertEqual(cancelled, ["US.OLD"], "the old candidate's working order goes")
        planner.save_plan(fresh)
        self.bot.plan_tick(now)
        self.assertEqual(len(built), 1, "a locked plan is never rebuilt, a restart included")

    def test_a_restart_keeps_pre_market_orders_only_for_the_locked_plans_candidates(self):
        from papercompmoomoo import planner
        from papercompmoomoo.signals import PRE_OPEN_REMARK
        created = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
        planner.save_plan(planner.new_plan("2026-10-21", [{"symbol": "US.KEEP", "terms": {}}], 1, "pre_open"))
        Dummy.working_orders = [
            {"order_id": 1, "code": "US.KEEP", "trd_side": "BUY", "order_status": "SUBMITTED", "qty": 10, "price": 5.0, "create_time": created, "remark": PRE_OPEN_REMARK},
            {"order_id": 2, "code": "US.GONE", "trd_side": "BUY", "order_status": "SUBMITTED", "qty": 10, "price": 5.0, "create_time": created, "remark": PRE_OPEN_REMARK},
        ]
        self.assertEqual(self.bot.audit_account(at_startup=True), 1)
        self.assertEqual(self.bot.trade_context.cancelled, [2])

    def test_a_pre_market_order_unfilled_30_seconds_after_the_open_is_cancelled_and_replaced_by_a_market_order(self):
        from papercompmoomoo import planner
        from papercompmoomoo.signals import PRE_OPEN_REMARK
        self._open_setup()
        plan = self._plan()
        plan["candidates"][0]["status"] = planner.PRE_PLACED
        now = datetime.now(timezone.utc)
        now_ms = int(now.timestamp() * 1000)
        Dummy.working_orders = [{"order_id": 7, "code": "US.AAA", "trd_side": "BUY", "order_status": "SUBMITTED", "qty": 10, "price": 10.0,
                                 "create_time": now.strftime("%Y-%m-%d %H:%M:%S"), "remark": PRE_OPEN_REMARK}]
        plan["opened_ms"] = now_ms - 20_000
        self.bot.place_plan_open(plan, now)
        self.assertEqual((getattr(self.bot.trade_context, "cancelled", []), plan["candidates"][0]["status"]), ([], planner.PRE_PLACED), "20 s after the open it still waits")
        plan["opened_ms"], plan["candidates"][0]["last_try_ms"] = now_ms - 35_000, 0
        self.bot.place_plan_open(plan, now)
        self.assertEqual(self.bot.trade_context.cancelled, [7], "35 s after the open the unfilled order is cancelled")
        self.assertEqual(plan["candidates"][0]["status"], planner.PENDING, "and the market order follows on the next sweep")

    def test_the_trailing_stop_follows_the_high_and_sells_when_the_price_falls_the_trail_from_it_and_the_take_profit_level_arms_a_trailing_exit(self):
        self._open_setup()
        self.bot.set_terms("US.AAA", {"stop_pct": 2.0, "trail_pct": 3.0, "take_profit_pct": 0.0, "hold_minutes": 999.0}, "plan")
        self.bot.positions["US.AAA"] = {"qty": 50, "entry": 10.0, "high": 10.0, "opened": datetime.now(timezone.utc)}
        for price in (10.5, 11.0, 10.75):  # up to a high of 11.00, then a 2.3% pull-back: inside the 3% trail
            self.bot.manage_position("US.AAA", price, 0.0)
        self.assertEqual((self.bot.trade_context.orders, self.bot.positions["US.AAA"]["high"]), ([], 11.0), "the high follows the price and no exit yet")
        self.bot.manage_position("US.AAA", 10.65, 0.0)  # 3.2% below the 11.00 high
        self.assertEqual(len(self.bot.trade_context.orders), 1, "the trailing stop sells")
        self.assertEqual(self.bot.journal.db.execute("SELECT reason FROM trades").fetchone()[0], "trailing stop")
        self.assertNotIn("US.AAA", self.bot.positions)
        self.bot.set_terms("US.AAA", {"stop_pct": 2.0, "trail_pct": 3.0, "take_profit_pct": 5.0, "hold_minutes": 999.0}, "plan")
        self.bot.positions["US.AAA"] = {"qty": 50, "entry": 10.0, "high": 10.0, "opened": datetime.now(timezone.utc)}
        orders = len(self.bot.trade_context.orders)
        self.bot.manage_position("US.AAA", 10.6, 0.0)  # +6%: past the 5% take-profit level
        self.assertEqual(len(self.bot.trade_context.orders), orders, "reaching the take-profit level does not sell: it arms the trailing exit")
        self.assertTrue(self.bot.positions["US.AAA"]["armed"])
        self.bot.manage_position("US.AAA", 11.5, 0.0)  # the winner runs
        self.assertEqual(len(self.bot.trade_context.orders), orders)
        self.bot.manage_position("US.AAA", 11.1, 0.0)  # 3.5% off the 11.50 high
        self.assertEqual(self.bot.journal.db.execute("SELECT reason FROM trades ORDER BY id DESC").fetchone()[0], "trailing take profit")

    def test_a_position_bought_pre_market_starts_its_hold_clock_at_the_regular_open(self):
        from datetime import timedelta
        from papercompmoomoo import planner
        self._open_setup()
        self.bot.set_terms("US.AAA", {"stop_pct": 5.0, "trail_pct": 5.0, "take_profit_pct": 0.0, "hold_minutes": 90.0}, "plan")
        opened = datetime(2026, 10, 21, 7, 30, tzinfo=planner.NEW_YORK).astimezone(timezone.utc)  # bought pre-market, two hours before the open
        self.bot.positions["US.AAA"] = {"qty": 50, "entry": 10.0, "high": 10.0, "opened": opened}
        self.bot.manage_position("US.AAA", 10.0, 0.0, now=datetime(2026, 10, 21, 9, 31, tzinfo=planner.NEW_YORK).astimezone(timezone.utc))
        self.assertEqual(self.bot.trade_context.orders, [], "one minute into the session it is not 'held 121 minutes'")
        self.bot.manage_position("US.AAA", 10.0, 0.0, now=datetime(2026, 10, 21, 11, 5, tzinfo=planner.NEW_YORK).astimezone(timezone.utc))
        self.assertEqual(len(self.bot.trade_context.orders), 1, "95 minutes after the open the time limit applies")
        self.assertEqual(self.bot.journal.db.execute("SELECT reason FROM trades").fetchone()[0], "time limit")

    def test_a_plan_position_closed_early_by_a_time_rule_is_bought_again_within_30_seconds_and_only_then(self):
        from papercompmoomoo import planner
        self._open_setup()
        ny = planner.NEW_YORK
        now = datetime(2026, 10, 21, 9, 31, 0, tzinfo=ny).astimezone(timezone.utc)
        ms = lambda d: int(d.timestamp() * 1000)

        def candidate(reason, closed, opened=None):
            plan = self._plan()
            plan["candidates"][0].update(status=planner.PLACED)
            plan["candidates"][0]["terms"]["hold_minutes"] = 90.0
            self.bot.journal.db.execute("DELETE FROM trades")
            self.bot.journal.trade("US.AAA", ms(opened or now - timedelta(hours=2)), ms(closed), 100, 10.0, 10.0, 0.0, reason)
            self.bot.last_reentry_check = -1e9
            self.bot.reenter_premature_exits(plan, now)
            return plan["candidates"][0]

        from datetime import timedelta
        self.assertEqual(candidate("time limit", now - timedelta(seconds=10))["status"], planner.PENDING, "closed 10 s ago by the clock, 1 min after the open")
        self.assertEqual(candidate("audit: overdue", now - timedelta(seconds=10))["status"], planner.PENDING)
        self.assertEqual(candidate("time limit", now - timedelta(seconds=45))["status"], planner.PLACED, "an exit older than 30 s is never bought back")
        for legit in ("stop", "trailing stop", "take profit"):
            self.assertEqual(candidate(legit, now - timedelta(seconds=5))["status"], planner.PLACED, f"a {legit} is the plan working")
        late = datetime(2026, 10, 21, 11, 10, 0, tzinfo=ny).astimezone(timezone.utc)  # 100 minutes after the open: its 90-minute hold really ran
        plan = self._plan()
        plan["candidates"][0].update(status=planner.PLACED)
        plan["candidates"][0]["terms"]["hold_minutes"] = 90.0
        self.bot.journal.db.execute("DELETE FROM trades")
        self.bot.journal.trade("US.AAA", ms(now), ms(late - timedelta(seconds=5)), 100, 10.0, 10.0, 0.0, "time limit")
        self.bot.last_reentry_check = -1e9
        self.bot.reenter_premature_exits(plan, late)
        self.assertEqual(plan["candidates"][0]["status"], planner.PLACED, "a legitimate time limit")
        again = candidate("time limit", now - timedelta(seconds=10))
        again["status"] = planner.PLACED
        self.bot.last_reentry_check = -1e9
        plan2 = {"session": "2026-10-21", "candidates": [again]}
        self.bot.reenter_premature_exits(plan2, now)
        self.assertEqual(again["status"], planner.PLACED, "once per candidate per session")

    def test_a_price_based_exit_needs_a_second_moomoo_read_that_agrees(self):
        self._open_setup()
        self.bot.set_terms("US.AAA", {"stop_pct": 2.0, "trail_pct": 5.0, "take_profit_pct": 0.0, "hold_minutes": 999.0}, "plan")
        self.bot.positions["US.AAA"] = {"qty": 50, "entry": 10.0, "high": 10.0, "opened": datetime.now(timezone.utc)}
        FakeQuote.quotes = {"US.AAA": {"last_price": 10.0, "bid_price": 9.99, "ask_price": 10.01, "prev_close_price": 10.0, "volume": 1, "volume_ratio": 1, "update_time": "t"}}
        self.bot.manage_position("US.AAA", 9.5, 0.0, confirm=True)  # a -5% tick that Moomoo's own quote does not show
        self.assertEqual(self.bot.trade_context.orders, [], "one bad tick does not sell")
        FakeQuote.quotes = {}
        self.bot.manage_position("US.AAA", 9.5, 0.0, confirm=True)
        self.assertEqual(self.bot.trade_context.orders, [], "no fresh price: no price-based exit")
        FakeQuote.quotes = {"US.AAA": {"last_price": 9.6, "bid_price": 9.59, "ask_price": 9.61, "prev_close_price": 10.0, "volume": 1, "volume_ratio": 1, "update_time": "t"}}
        self.bot.manage_position("US.AAA", 9.5, 0.0, confirm=True)
        self.assertEqual(len(self.bot.trade_context.orders), 1, "both reads below the 2% stop: sold")
        self.assertEqual(self.bot.journal.db.execute("SELECT exit_price FROM trades").fetchone()[0], 9.6, "at the fresh price")

    def test_an_armed_position_never_trails_below_its_entry(self):
        self._open_setup()
        self.bot.set_terms("US.AAA", {"stop_pct": 3.0, "trail_pct": 8.0, "take_profit_pct": 5.0, "hold_minutes": 999.0}, "plan")
        self.bot.positions["US.AAA"] = {"qty": 50, "entry": 10.0, "high": 10.0, "opened": datetime.now(timezone.utc)}
        self.bot.manage_position("US.AAA", 10.6, 0.0)  # +6%: armed, high 10.60
        self.bot.manage_position("US.AAA", 10.01, 0.0)  # 5.6% off the high: inside the 8% trail, still above the entry
        self.assertEqual(self.bot.trade_context.orders, [])
        self.bot.manage_position("US.AAA", 9.99, 0.0)  # back through the entry: the 8% trail (9.75) would not have sold yet
        self.assertEqual(len(self.bot.trade_context.orders), 1, "the armed position is sold at the entry, not 2.5% below it")
        self.assertEqual(self.bot.journal.db.execute("SELECT reason FROM trades").fetchone()[0], "trailing take profit")
        self.bot.set_terms("US.AAA", {"stop_pct": 3.0, "trail_pct": 8.0, "take_profit_pct": 0.0, "hold_minutes": 999.0}, "plan")
        self.bot.positions["US.AAA"] = {"qty": 50, "entry": 10.0, "high": 10.0, "opened": datetime.now(timezone.utc)}
        self.bot.manage_position("US.AAA", 10.6, 0.0)
        self.bot.manage_position("US.AAA", 9.99, 0.0)  # never armed (no take-profit level): the ordinary 8% trail applies
        self.assertEqual(len(self.bot.trade_context.orders), 1, "an unarmed position keeps its ordinary trailing stop")

    def test_everything_is_sold_at_market_ten_minutes_before_the_close_and_carried_non_candidates_at_the_open(self):
        from datetime import timedelta
        from papercompmoomoo import planner
        self._open_setup()
        ny = planner.NEW_YORK
        at = lambda h, m: datetime(2026, 10, 21, h, m, tzinfo=ny).astimezone(timezone.utc)
        plan = self._plan()  # candidate: US.AAA
        plan["candidates"][0]["status"] = planner.PLACED
        self.bot.positions["US.AAA"] = {"qty": 50, "entry": 10.0, "high": 10.0, "opened": at(9, 31) - timedelta(days=1)}  # carried, but a candidate again today
        self.bot.positions["US.OLD"] = {"qty": 20, "entry": 5.0, "high": 5.0, "opened": at(9, 31) - timedelta(days=1)}  # carried, no longer a candidate
        self.bot.positions["US.NEW"] = {"qty": 10, "entry": 7.0, "high": 7.0, "opened": at(9, 40)}  # opened today, not in the plan: not carried
        self.bot.last_eod_check = -1e9
        self.assertFalse(self.bot.end_of_day_exits(plan, at(9, 30)))
        self.assertEqual(sorted(self.bot.positions), ["US.AAA", "US.NEW"], "the carried non-candidate is sold at the open")
        self.assertEqual(self.bot.journal.db.execute("SELECT symbol, reason FROM trades").fetchall(), [("US.OLD", "not a candidate")])
        self.bot.last_eod_check = -1e9
        self.assertFalse(self.bot.end_of_day_exits(plan, at(15, 49)))
        self.assertEqual(sorted(self.bot.positions), ["US.AAA", "US.NEW"], "nothing before 15:50")
        self.bot.last_eod_check = -1e9
        self.assertTrue(self.bot.end_of_day_exits(plan, at(15, 50)))
        self.assertEqual(self.bot.positions, {}, "everything still open is sold at market ten minutes before the close")
        reasons = sorted(r[0] for r in self.bot.journal.db.execute("SELECT reason FROM trades"))
        self.assertEqual(reasons, ["not a candidate", "session close", "session close"])
        self.assertTrue(all(o["order_type"] == botmod.OrderType.MARKET for o in self.bot.trade_context.orders if o["trd_side"] == botmod.TrdSide.SELL))

    def test_a_new_process_clears_what_the_old_one_left_in_the_status_file(self):
        self.bot.write_status(account={"held": ["US.OLD"]}, watchlist=[{"symbol": "US.OLD"}], last_candidates=[1])
        self.bot.clear_stale_status()
        status = json.loads(self.bot.status_path.read_text())
        self.assertFalse({"account", "watchlist", "last_candidates"} & set(status))


if __name__ == "__main__":
    unittest.main()
