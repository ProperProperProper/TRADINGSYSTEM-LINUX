import unittest

import pandas as pd

from papercompmoomoo.account import pick_account


def frame(*rows):
    return pd.DataFrame(list(rows), columns=["acc_id", "trd_env", "sim_acc_type", "trdmarket_auth"])


REAL = (1, "REAL", "N/A", ["HK", "US", "AU"])
OLD = (1002, "SIMULATE", "STOCK_AND_OPTION", ["US"])
COMP = (1001, "SIMULATE", "COMPETITION", ["US"])


class PickAccount(unittest.TestCase):
    def test_the_competition_account_is_found_among_the_real_and_ordinary_ones(self):
        self.assertEqual(pick_account(frame(REAL, OLD, COMP)), 1001)

    def test_it_is_never_the_real_account_even_if_it_claimed_the_type(self):
        liar = (7, "REAL", "COMPETITION", ["US"])
        self.assertIsNone(pick_account(frame(REAL, OLD, liar)))

    def test_none_two_or_a_wrong_market_means_no_account(self):
        self.assertIsNone(pick_account(frame(REAL, OLD)), "not registered")
        self.assertIsNone(pick_account(frame(COMP, (9, "SIMULATE", "COMPETITION", ["US"]))), "two: ambiguous")
        self.assertIsNone(pick_account(frame((5, "SIMULATE", "COMPETITION", ["HK"]))), "not enabled for the US market")
        self.assertIsNone(pick_account(None))
        self.assertIsNone(pick_account(frame()))


if __name__ == "__main__":
    unittest.main()
