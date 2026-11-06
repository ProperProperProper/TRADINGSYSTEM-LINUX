import unittest
from datetime import datetime, timedelta, timezone

from papercompmoomoo.auditor import evaluate

NOW = datetime(2026, 10, 19, 15, 0, tzinfo=timezone.utc)


def facts(**k):
    base = {"now": NOW, "acc_id": 1001, "account": {"equity": 100_000.0, "cash": 50_000.0, "power": 100_000.0, "held": [], "working": set(), "working_rows": []},
            "tracked": {}, "stale_orders": [], "max_hold_minutes": 90, "per_position_limit": 42_000.0, "store_junk": {}, "code_changed": False,
            "flat": True, "session_open": True, "converter_age_s": 20.0}
    base.update(k)
    return base


def held(code, value=10_000.0):
    return {"code": code, "qty": 100, "cost": 10.0, "value": value}


class Rules(unittest.TestCase):
    def test_a_silent_or_missing_converter_is_reported(self):
        self.assertEqual([x.area for x in evaluate(facts(converter_age_s=None))], ["converter"])
        self.assertEqual([x.area for x in evaluate(facts(converter_age_s=900.0))], ["converter"])

    def test_a_clean_state_has_no_findings(self):
        self.assertEqual(evaluate(facts()), [])

    def test_no_account_or_an_unreadable_one_is_an_error_and_nothing_else_is_judged_about_the_account(self):
        f = evaluate(facts(acc_id=None))
        self.assertEqual([(x.level, x.area) for x in f], [("error", "account")])
        f = evaluate(facts(account=None))
        self.assertEqual([(x.level, x.area) for x in f], [("error", "account")])
        f = evaluate(facts(account=None, code_changed=True, flat=True))
        self.assertEqual({x.area for x in f}, {"account", "code"}, "code and candle checks still run without the account")

    def test_stale_orders_and_untracked_or_vanished_positions_name_their_repair(self):
        f = evaluate(facts(stale_orders=[{"code": "US.A"}, {"code": "US.A"}]))
        self.assertEqual([(x.area, x.fix) for x in f], [("orders", "cancel_orders")])
        acc = facts()["account"] | {"held": [held("US.NEW")]}
        f = evaluate(facts(account=acc))
        self.assertEqual([(x.area, x.fix) for x in f], [("positions", "adopt_positions")])
        f = evaluate(facts(tracked={"US.OLD": {"opened": NOW}}))
        self.assertEqual([(x.area, x.fix) for x in f], [("positions", "adopt_positions")])

    def test_a_position_past_its_time_limit_is_exited_only_while_the_market_is_open(self):
        acc = facts()["account"] | {"held": [held("US.A")]}
        later = NOW + timedelta(hours=1)  # 12:00 New York: the clock of a position starts at the regular open at the earliest
        old = {"US.A": {"opened": later - timedelta(minutes=120)}}
        f = evaluate(facts(now=later, account=acc, tracked=old))
        self.assertEqual([x.fix for x in f], ["exit:US.A"])
        self.assertEqual(evaluate(facts(now=later, account=acc, tracked=old, session_open=False)), [], "a closed market cannot be sold into")
        recent = {"US.A": {"opened": later - timedelta(minutes=95)}}
        self.assertEqual(evaluate(facts(now=later, account=acc, tracked=recent)), [], "ten minutes of grace beyond the limit")

    def test_exposure_and_cash_are_reported_not_guessed_at(self):
        acc = facts()["account"] | {"held": [held("US.BIG", 60_000.0)], "cash": -5.0, "power": 0.0}
        f = evaluate(facts(account=acc, tracked={"US.BIG": {"opened": NOW}}))
        self.assertEqual({(x.level, x.area) for x in f}, {("error", "exposure"), ("warn", "account")})
        self.assertTrue(all(x.fix is None for x in f))

    def test_junk_and_changed_code_are_reported_and_the_bot_restarts_only_when_flat(self):
        f = evaluate(facts(store_junk={"off_grid": 3, "no_volume": 0}))
        self.assertEqual({x.area for x in f}, {"candles"})
        self.assertIn("'off_grid': 3", f[0].text)
        self.assertNotIn("no_volume", f[0].text)
        self.assertEqual([x.fix for x in evaluate(facts(code_changed=True, flat=True))], ["restart"])
        waiting = evaluate(facts(code_changed=True, flat=False))
        self.assertEqual((waiting[0].fix, "restarting once no order is working" in waiting[0].text), (None, True))
        holding = evaluate(facts(code_changed=True, flat=False, no_working=True))
        self.assertEqual(holding[0].fix, "restart", "open positions do not block a restart: they are adopted again")


class OverdueClock(unittest.TestCase):
    def _tracked(self, opened):
        return {"US.AAA": {"qty": 1, "entry": 10.0, "high": 10.0, "opened": opened}}

    def _acc(self):
        return {"equity": 100_000.0, "cash": 50_000.0, "power": 100_000.0, "held": [{"code": "US.AAA", "qty": 1, "cost": 10.0, "value": 10.0}], "working": set(), "working_rows": []}

    def test_a_position_bought_pre_market_is_not_overdue_until_the_hold_time_after_the_regular_open(self):
        from zoneinfo import ZoneInfo
        ny = ZoneInfo("America/New_York")
        opened = datetime(2026, 10, 21, 7, 30, tzinfo=ny).astimezone(timezone.utc)
        early = evaluate(facts(now=datetime(2026, 10, 21, 9, 52, tzinfo=ny).astimezone(timezone.utc), tracked=self._tracked(opened), account=self._acc()))
        self.assertEqual([f.fix for f in early if f.fix and f.fix.startswith("exit:")], [], "22 minutes into the session, not 142")
        late = evaluate(facts(now=datetime(2026, 10, 21, 11, 25, tzinfo=ny).astimezone(timezone.utc), tracked=self._tracked(opened), account=self._acc()))
        self.assertEqual([f.fix for f in late if f.fix and f.fix.startswith("exit:")], ["exit:US.AAA"], "11 minutes after the open")

    def test_the_position_s_own_hold_time_is_the_limit_when_it_has_one(self):
        from zoneinfo import ZoneInfo
        ny = ZoneInfo("America/New_York")
        opened = datetime(2026, 10, 21, 9, 31, tzinfo=ny).astimezone(timezone.utc)
        now = datetime(2026, 10, 21, 11, 25, tzinfo=ny).astimezone(timezone.utc)  # 114 minutes held
        self.assertEqual([f.fix for f in evaluate(facts(now=now, tracked=self._tracked(opened), account=self._acc())) if f.fix and f.fix.startswith("exit:")], ["exit:US.AAA"], "the strategy's 90 minutes")
        self.assertEqual([f.fix for f in evaluate(facts(now=now, tracked=self._tracked(opened), account=self._acc(), hold_by_symbol={"US.AAA": 150.0})) if f.fix and f.fix.startswith("exit:")], [], "its own 150 minutes")


if __name__ == "__main__":
    unittest.main()
