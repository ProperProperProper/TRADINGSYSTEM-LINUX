import json
import sqlite3
import tempfile
import unittest
from pathlib import Path
from papercompmoomoo.telemetry import RecordedContext, emit, debrief

class TelemetryTests(unittest.TestCase):
    def test_broker_request_response_linkage_and_failure(self):
        class Context:
            def order(self, qty): return (0, {'qty':qty,'order_id':'abc'})
            def fail(self): raise ValueError('broker unavailable')
        with tempfile.TemporaryDirectory() as tmp:
            path=Path(tmp)/'events.db';proxy=RecordedContext(Context(),'test',path)
            self.assertEqual(proxy.order(qty=2),(0,{'qty':2,'order_id':'abc'}))
            with self.assertRaises(ValueError):proxy.fail()
            with sqlite3.connect(path) as db:
                rows=db.execute('SELECT event_id,kind,correlation_id FROM events ORDER BY id').fetchall()
                self.assertEqual(rows[1][2],rows[0][0])
                self.assertEqual(rows[3][1],'broker_exception')
                with self.assertRaises(sqlite3.IntegrityError):db.execute('DELETE FROM events')

    def test_full_payload_and_coverage(self):
        with tempfile.TemporaryDirectory() as tmp:
            path=Path(tmp)/'events.db';emit('feed_quote',{'symbol':'US.X','text':'x'*20000},path=path)
            with sqlite3.connect(path) as db:
                session,payload=db.execute('SELECT session,payload_json FROM events').fetchone()
                self.assertEqual(len(json.loads(payload)['text']),20000)
            self.assertTrue(debrief(path,session)['coverage']['quote'])
            self.assertFalse(debrief(path,session)['coverage']['depth'])
