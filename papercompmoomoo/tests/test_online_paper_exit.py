from papercompmoomoo.account_ids import account_id
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
from papercompmoomoo import online_exit
from papercompmoomoo.exit_execution import Quote
from papercompmoomoo.paper_exit_controller import PaperExitController

class OnlinePaperExitTests(unittest.TestCase):
    def test_feedback_not_visible_before_it_is_observed_and_recorded(self):
        with tempfile.TemporaryDirectory() as tmp:
            p=Path(tmp)/'online.db'
            with patch('time.time_ns',return_value=1000*1000000):
                online_exit.observe(p,'orb','US.TEST',500,1000,{'latest_observation_ms':1000})
            with patch('time.time_ns',return_value=3000*1000000):
                online_exit.feedback(p,'orb','US.TEST',500,2000,{'deal_id':'deal','price':99})
                online_exit.feedback(p,'orb','US.TEST',500,2000,{'deal_id':'deal','price':99})
            self.assertEqual(online_exit.available_feedback(p,2500),[])
            self.assertEqual(len(online_exit.available_feedback(p,3001)),1)
            with patch('time.time_ns',return_value=3000*1000000):
                with self.assertRaises(ValueError):online_exit.feedback(p,'orb','US.TEST',500,4000,{})

    def test_reprices_after_analysis_and_never_invents_a_fill(self):
        class Broker:
            orders=[]
            def quote(self,symbol):return Quote(4990,98,98.02,4990)
            def position(self,account,symbol):return {'opened_ms':500,'available_qty':2}
            def working_sell(self,account,symbol):return False
            def submit_exit(self,account,symbol,qty,**kwargs):
                self.orders.append((account,symbol,qty,kwargs));return {'order_id':'order'}
        with tempfile.TemporaryDirectory() as tmp:
            p=Path(tmp)/'online.db';broker=Broker();controller=PaperExitController(broker,p)
            with patch('time.time_ns',return_value=1000*1000000):
                obs=online_exit.observe(p,'orb','US.TEST',500,1000,{'price':100})
            answer={'action':'EXIT','signal':'structure_failure','p_up':.2,'horizon_seconds':60,'reason':'observed breakdown'}
            with patch('papercompmoomoo.paper_exit_controller.emit'),patch('time.time_ns',return_value=5000*1000000):
                submission=controller.execute('orb','US.TEST',500,obs,1000,answer,lambda *a:True,now_ms=5000)
            self.assertEqual(submission['submission_bid'],98)
            self.assertIsNone(submission['fill_price'])
            self.assertEqual(broker.orders[0][0],account_id("orb"))
            self.assertEqual(broker.orders[0][3]['trd_env'],'SIMULATE')
            with patch('papercompmoomoo.paper_exit_controller.emit'),patch('time.time_ns',return_value=6000*1000000):
                feedback=controller.confirm_fill(submission,{'deal_id':'d1','order_id':'order','price':97.9,'qty':1,'filled_ms':5100})
                duplicate=controller.confirm_fill(submission,{'deal_id':'d1','order_id':'order','price':97.9,'qty':1,'filled_ms':5100})
            self.assertAlmostEqual(feedback['sell_slippage_usd'],.1)
            self.assertEqual(feedback['quantity'],1)
            self.assertEqual(duplicate['status'],'already_reconciled')

    def test_future_state_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp,patch('time.time_ns',return_value=1000*1000000):
            with self.assertRaises(ValueError):
                online_exit.observe(Path(tmp)/'db','orb','US.TEST',500,1000,{'latest_observation_ms':1001})
