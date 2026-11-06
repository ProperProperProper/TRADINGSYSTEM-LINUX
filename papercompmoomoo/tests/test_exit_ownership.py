from papercompmoomoo.account_ids import account_id
import unittest
from unittest.mock import patch

from papercompmoomoo.exit_ownership import (
    legacy_strategy_exit_allowed, require_codex_submission, strategy_exit_owner,
)


class ExitOwnershipTests(unittest.TestCase):
    def test_codex_is_only_owner_for_both_paper_accounts(self):
        for account in (account_id("gainer"), account_id("orb")):
            self.assertEqual(strategy_exit_owner(account), 'codex')
            self.assertFalse(legacy_strategy_exit_allowed(account))

    def test_only_tagged_simulated_codex_sell_is_admitted(self):
        require_codex_submission(account_id("gainer"), 'CODEX_EXIT:decision', simulated=True)
        with self.assertRaises(PermissionError):
            require_codex_submission(account_id("gainer"), 'momentum exit', simulated=True)
        with self.assertRaises(PermissionError):
            require_codex_submission(account_id("orb"), 'CODEX_EXIT:decision', simulated=False)

    def test_unknown_account_fails_closed(self):
        with self.assertRaises(ValueError):
            strategy_exit_owner(1)

    def test_worker_provider_time_fails_closed_on_bad_values(self):
        import datetime as dt
        from zoneinfo import ZoneInfo
        from papercompmoomoo.codex_exit_worker import provider_time,extended_session,regular_session
        self.assertIsNone(provider_time(object()))
        self.assertIsNone(provider_time('not-a-time'))
        self.assertIsNotNone(provider_time('09:30:00'))
        self.assertIsNotNone(provider_time(dt.datetime(2026,11,4,9,30,tzinfo=dt.timezone.utc)))
        ny=ZoneInfo('America/New_York')
        self.assertTrue(extended_session(dt.datetime(2026,11,4,4,0,tzinfo=ny)))
        self.assertFalse(regular_session(dt.datetime(2026,11,4,4,0,tzinfo=ny)))
        self.assertTrue(regular_session(dt.datetime(2026,11,4,9,30,tzinfo=ny)))
        self.assertFalse(extended_session(dt.datetime(2026,11,4,20,0,tzinfo=ny)))


if __name__ == '__main__':
    unittest.main()
