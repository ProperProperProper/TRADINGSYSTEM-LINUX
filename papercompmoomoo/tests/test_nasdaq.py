import unittest

import pandas as pd

from papercompmoomoo.signals import may_trade, nasdaq_listed_symbols


class NasdaqOnly(unittest.TestCase):
    def test_the_listing_gives_ordinary_nasdaq_symbols_with_moomoo_codes(self):
        listing = pd.DataFrame({"Symbol": ["AAPL", "NVDA", "ZZZZZ", "TOOLONG", "ABC.D", "TEST1"], "Test Issue": ["N", "N", "N", "N", "N", "Y"]})
        self.assertEqual(nasdaq_listed_symbols(listing), frozenset({"US.AAPL", "US.NVDA", "US.ZZZZZ"}))

    def test_only_nasdaq_symbols_may_be_traded_and_an_empty_set_trades_nothing(self):
        nasdaq = frozenset({"US.AAPL", "US.NVDA"})
        self.assertTrue(may_trade("US.NVDA", nasdaq))
        self.assertFalse(may_trade("US.JPM", nasdaq), "NYSE: data yes, orders no")
        self.assertFalse(may_trade("US.AAPL", frozenset()))


if __name__ == "__main__":
    unittest.main()
