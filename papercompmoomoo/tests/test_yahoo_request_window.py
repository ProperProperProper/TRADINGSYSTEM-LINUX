import unittest
from datetime import datetime,timedelta,timezone
from unittest.mock import MagicMock,patch

from papercompmoomoo.candlestore import yahoo_fetch


class YahooRequestWindowTests(unittest.TestCase):
    def test_one_minute_request_is_clamped_below_eight_days(self):
        fake=MagicMock();fake.empty=True
        with patch('yfinance.download',return_value=fake) as download:
            yahoo_fetch(['AAA'],datetime.now(timezone.utc)-timedelta(days=35))
        start=download.call_args.kwargs['start']
        self.assertLessEqual(datetime.now(timezone.utc)-start,timedelta(days=7,seconds=2))


if __name__=='__main__':unittest.main()
