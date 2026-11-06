import json
import sqlite3
import tempfile
import unittest
from pathlib import Path
from papercompmoomoo.decision_audit import preserve_versions, record

class DecisionAuditTests(unittest.TestCase):
    def test_history_keeps_both_sides_and_deletion(self):
        db = sqlite3.connect(':memory:')
        db.execute('CREATE TABLE model(id INTEGER PRIMARY KEY, weight REAL)')
        preserve_versions(db)
        db.execute('INSERT INTO model VALUES(1, .5)')
        db.execute('UPDATE model SET weight=.8')
        db.execute('DELETE FROM model')
        values = [json.loads(r[0])['weight'] for r in db.execute('SELECT row_json FROM permanent_row_history ORDER BY id')]
        self.assertEqual(values, [.5, .5, .8, .8])
        with self.assertRaises(sqlite3.IntegrityError):
            db.execute('DELETE FROM permanent_row_history')

    def test_evidence_contains_full_payload_and_is_immutable(self):
        with tempfile.TemporaryDirectory() as tmp:
            anchor = Path(tmp)/'brain.sqlite3'
            record(anchor, 'test', {'text': 'x'*10000, 'edges': [['a','b',.7]]}, {'score': .8})
            db = sqlite3.connect(anchor.with_name('decision_audit.sqlite3'))
            data = json.loads(db.execute('SELECT payload_json FROM evidence').fetchone()[0])
            self.assertEqual(len(data['inputs']['text']),10000)
            with self.assertRaises(sqlite3.IntegrityError):
                db.execute("UPDATE evidence SET kind='changed'")
            db.close()
