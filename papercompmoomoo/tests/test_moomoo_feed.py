import unittest
from datetime import timezone

import pandas as pd

from papercompmoomoo.moomoo_feed import MIN_HOLD_SECONDS, MoomooFeed, bars_from_kline
from papercompmoomoo.signals import entry_gate

UTC = timezone.utc


class FakeQuote:
    def __init__(self, remain=100):
        self.remain, self.subscribed, self.calls = remain, [], []

    def query_subscription(self):
        return 0, {"remain": self.remain - len(self.subscribed)}

    def subscribe(self, codes, types, subscribe_push=False, session=None):
        self.calls.append(("subscribe", list(codes), session))
        self.subscribed += list(codes)
        return 0, None

    def unsubscribe(self, codes, types):
        self.calls.append(("unsubscribe", list(codes)))
        self.subscribed = [c for c in self.subscribed if c not in codes]
        return 0, None

    def unsubscribe_all(self):
        self.subscribed = []

    def get_cur_kline(self, code, num, ktype, autype=None):
        frame = pd.DataFrame({"time_key": ["2026-10-19 10:45:00", "2026-10-19 11:00:00"], "open": [10.0, 10.1], "high": [10.2, 10.3], "low": [9.9, 10.0],
                              "close": [10.1, 10.25], "volume": [500.0, 700.0]})
        return 0, frame

    def get_market_snapshot(self, codes):
        return 0, pd.DataFrame([{"code": c, "last_price": 10.5, "bid_price": 10.49, "ask_price": 10.51, "prev_close_price": 10.0, "volume": 1e6, "volume_ratio": 2.0,
                                 "update_time": "2026-10-19 11:20:00"} for c in codes])

    def close(self):
        pass


class Feed(unittest.TestCase):
    def test_moomoo_labels_a_bar_by_its_end_and_the_feed_converts_to_the_start(self):
        _, kline = FakeQuote().get_cur_kline("US.A", 10, None)
        frame = bars_from_kline(kline)
        self.assertEqual([t.strftime("%H:%M") for t in frame.index], ["10:30", "10:45"])
        self.assertEqual(str(frame.index.tz), "America/New_York")
        self.assertEqual(list(frame["Volume"]), [500.0, 700.0])

    def test_subscriptions_stay_within_both_caps_and_are_released_only_after_the_minimum_hold(self):
        quote = FakeQuote(remain=14)  # 14 left, 10 always kept in reserve: room for 4
        feed = MoomooFeed(quote, max_subscriptions=25)
        usable = feed.ensure(["US.A", "US.B", "US.C", "US.D", "US.E", "US.F"])
        self.assertEqual(usable, {"US.A", "US.B", "US.C", "US.D"})
        self.assertEqual(feed.release_unused(set()), 0, "inside Moomoo's one-minute minimum nothing is unsubscribed")
        for code in feed.subscribed:
            feed.subscribed[code] -= MIN_HOLD_SECONDS + 1
        self.assertEqual(feed.release_unused({"US.A"}), 3)
        self.assertEqual(sorted(feed.subscribed), ["US.A"])
        self.assertEqual(feed.ensure(["US.A"]), {"US.A"})
        self.assertEqual([c for c in quote.calls if c[0] == "subscribe"].__len__(), 1, "an already subscribed code is not subscribed again")

    def test_snapshots_give_live_price_bid_ask_and_previous_close_in_one_call(self):
        quotes = MoomooFeed(FakeQuote()).snapshots(["US.A", "US.B"])
        self.assertEqual(quotes["US.A"]["last"], 10.5)
        self.assertEqual((quotes["US.B"]["bid"], quotes["US.B"]["ask"], quotes["US.B"]["prev_close"]), (10.49, 10.51, 10.0))



class EntryGate(unittest.TestCase):
    def test_the_entry_gate_refuses_a_chase_a_wide_spread_and_a_missing_quote(self):
        live = {"last": 10.5, "bid": 10.49, "ask": 10.51}
        self.assertIsNone(entry_gate(10.42, live, 1.0, 1.5))
        self.assertIn("from the signal", entry_gate(10.0, live, 1.0, 1.5))
        self.assertIn("spread", entry_gate(10.5, {"last": 10.5, "bid": 10.3, "ask": 10.6}, 1.0, 1.5))
        self.assertIn("no live", entry_gate(10.0, None, 1.0, 1.5))
        self.assertIn("no live", entry_gate(10.0, {"last": 0.0}, 1.0, 1.5))



class FakeWatchlist:
    def __init__(self, held, ret=0):
        self.held, self.ret, self.calls = list(held), ret, []

    def get_user_security(self, group):
        self.calls.append(("get", group))
        return (self.ret, pd.DataFrame({"code": self.held})) if self.ret == 0 else (self.ret, "no such group")

    def modify_user_security(self, group, op, codes):
        self.calls.append((str(op), group, list(codes)))
        return 0, "success"


class WatchlistSyncTests(unittest.TestCase):
    def test_new_candidates_are_added_and_dropped_ones_removed_in_the_owners_group_only(self):
        q = FakeWatchlist(["US.OLD", "US.KEEP"])
        self.assertEqual(MoomooFeed(q).sync_watchlist(["US.KEEP", "US.NEW"]), (["US.NEW"], ["US.OLD"]))
        self.assertEqual({c[1] for c in q.calls}, {"MooComp"})
        self.assertEqual([c[2] for c in q.calls if len(c) == 3], [["US.NEW"], ["US.OLD"]])

    def test_nothing_is_sent_when_the_group_already_matches_and_the_cap_applies(self):
        q = FakeWatchlist(["US.A"])
        self.assertEqual(MoomooFeed(q).sync_watchlist(["US.A"]), ([], []))
        self.assertEqual(len(q.calls), 1, "only the read")
        q = FakeWatchlist([])
        added, _ = MoomooFeed(q).sync_watchlist(["US.A", "US.B", "US.C"], limit=2)
        self.assertEqual(added, ["US.A", "US.B"])

    def test_a_missing_group_is_not_created_and_nothing_raises(self):
        q = FakeWatchlist([], ret=-1)
        self.assertIsNone(MoomooFeed(q).sync_watchlist(["US.A"]))
        self.assertEqual(q.calls, [("get", "MooComp")])


if __name__ == "__main__":
    unittest.main()
