import unittest
from papercompmoomoo.exit_execution import Quote,validate_exit,fill_metrics

class ExecutionTests(unittest.TestCase):
    def proposal(self):
        return {'action':'EXIT','created_ms':1000,'valid_until_ms':10000,'analysis_price':100}
    def test_price_changes_while_qwen_thinks_use_new_bid_not_old_price(self):
        result=validate_exit(self.proposal(),Quote(4990,98,98.02,4990),5000,True)
        self.assertTrue(result['allowed'])
        self.assertEqual(result['analysis_price'],100)
        self.assertEqual(result['decision_reference_bid'],98)
        self.assertIsNone(result['fill_price'])
    def test_old_quotes_and_expired_policies_cannot_execute(self):
        self.assertFalse(validate_exit(self.proposal(),Quote(1000,100,100.02,1000),5000,True)['allowed'])
        self.assertFalse(validate_exit(self.proposal(),Quote(10990,100,100.02,10990),11000,True)['allowed'])
    def test_old_provider_tick_with_recent_receipt_is_not_fresh(self):
        self.assertFalse(validate_exit(self.proposal(),Quote(4990,100,100.02,1000),5000,True)['allowed'])
    def test_new_price_can_invalidate_the_exit_condition(self):
        self.assertFalse(validate_exit(self.proposal(),Quote(4990,100,100.02,4990),5000,False)['allowed'])
    def test_slippage_measures_actual_fill_against_submission_quote(self):
        x=fill_metrics(98,97.95,100,5000,5080,1)
        self.assertAlmostEqual(x['sell_slippage_usd'],5)
        self.assertEqual(x['execution_latency_ms'],80)
