"""Point-in-time online exit observations and delayed, factual feedback.

This ledger does not manufacture HOLD/EXIT labels from future candles or turn
a Codex suggestion into an order. Qwen learning updates require a real training backend.
"""
import json
import sqlite3
import time
import uuid
from contextlib import closing
from pathlib import Path
from .telemetry import plain

def connect(path):
    path=Path(path);path.parent.mkdir(parents=True,exist_ok=True)
    db=sqlite3.connect(path,timeout=30)
    db.execute('PRAGMA journal_mode=WAL')
    db.executescript("""
    CREATE TABLE IF NOT EXISTS observations(
      id TEXT PRIMARY KEY, recorded_ms INTEGER NOT NULL, decision_ms INTEGER NOT NULL,
      bot TEXT NOT NULL, position_id TEXT NOT NULL, symbol TEXT NOT NULL,
      policy_id TEXT NOT NULL, state_json TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS actions(
      id TEXT PRIMARY KEY, observation_id TEXT NOT NULL, recorded_ms INTEGER NOT NULL,
      action TEXT NOT NULL CHECK(action IN ('HOLD','EXIT')), policy_id TEXT NOT NULL,
      actor TEXT NOT NULL, payload_json TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS feedback(
      id TEXT PRIMARY KEY, bot TEXT NOT NULL, position_id TEXT NOT NULL,
      observed_ms INTEGER NOT NULL, recorded_ms INTEGER NOT NULL, payload_json TEXT NOT NULL);
    """)
    for table in ('observations','actions','feedback'):
        for action in ('UPDATE','DELETE'):
            db.execute(f"CREATE TRIGGER IF NOT EXISTS {table}_no_{action.lower()} BEFORE {action} ON {table} BEGIN SELECT RAISE(ABORT,'permanent online evidence'); END")
    return db

def observe(path,bot,symbol,opened_ms,decision_ms,state,policy_id='current-bot'):
    now=time.time_ns()//1000000
    if decision_ms>now:raise ValueError('decision timestamp is in the future')
    cutoff=state.get('latest_observation_ms',decision_ms)
    if cutoff>decision_ms:raise ValueError('future observation in decision')
    bars=state.get('closed_candles',[])
    if any(b[0]+60000>decision_ms for b in bars):raise ValueError('unclosed candle in closed-candle input')
    ident=uuid.uuid4().hex;position_id=f'{symbol.removeprefix("US.")}:{int(opened_ms)}'
    with closing(connect(path)) as db,db:
        db.execute('INSERT INTO observations VALUES(?,?,?,?,?,?,?,?)',
            (ident,now,decision_ms,bot,position_id,symbol,policy_id,json.dumps(plain(state),sort_keys=True,allow_nan=False)))
    return ident

def action(path,observation_id,choice,policy_id,actor,payload=None):
    if choice not in ('HOLD','EXIT'):raise ValueError('invalid exit action')
    now=time.time_ns()//1000000
    with closing(connect(path)) as db,db:
        row=db.execute('SELECT decision_ms FROM observations WHERE id=?',(observation_id,)).fetchone()
        if not row or row[0]>now:raise ValueError('unknown or future observation')
        db.execute('INSERT INTO actions VALUES(?,?,?,?,?,?,?)',(uuid.uuid4().hex,observation_id,now,choice,policy_id,actor,json.dumps(plain(payload or {}),sort_keys=True)))

def feedback(path,bot,symbol,opened_ms,observed_ms,payload):
    now=time.time_ns()//1000000
    if observed_ms>now:raise ValueError('feedback cannot use a future outcome')
    if observed_ms<opened_ms:raise ValueError('feedback predates position')
    position_id=f'{symbol.removeprefix("US.")}:{int(opened_ms)}'
    ident=uuid.uuid5(uuid.NAMESPACE_URL,f'{bot}:{position_id}:{payload["deal_id"]}').hex if payload.get('deal_id') else uuid.uuid4().hex
    encoded=json.dumps(plain(payload),sort_keys=True,allow_nan=False)
    # Only actual observed results: do not label the unchosen action.
    with closing(connect(path)) as db,db:
        existing=db.execute('SELECT payload_json FROM feedback WHERE id=?',(ident,)).fetchone()
        if existing:
            if existing[0]!=encoded:raise ValueError('conflicting duplicate fill feedback')
            return ident
        db.execute('INSERT INTO feedback VALUES(?,?,?,?,?,?)',
            (ident,bot,position_id,observed_ms,now,encoded))
    return ident

def has_feedback(path,bot,symbol,opened_ms,deal_id):
    """Whether an immutable broker deal was already reconciled."""
    position_id=f'{symbol.removeprefix("US.")}:{int(opened_ms)}'
    ident=uuid.uuid5(uuid.NAMESPACE_URL,f'{bot}:{position_id}:{deal_id}').hex
    if not Path(path).exists():return False
    with closing(connect(path)) as db:
        return db.execute('SELECT 1 FROM feedback WHERE id=?',(ident,)).fetchone() is not None

def available_feedback(path,cutoff_ms):
    """Feedback available BEFORE an update; no retrospective backdating."""
    with closing(connect(path)) as db:
        return [dict(zip(('observation_id','bot','position_id','decision_ms','state_json','feedback_id','observed_ms','feedback_json'),r)) for r in db.execute("""
          SELECT o.id,o.bot,o.position_id,o.decision_ms,o.state_json,f.id,f.observed_ms,f.payload_json
          FROM observations o JOIN feedback f ON f.bot=o.bot AND f.position_id=o.position_id
          WHERE o.decision_ms<? AND o.recorded_ms<? AND f.observed_ms<? AND f.recorded_ms<?
          AND f.observed_ms>=o.decision_ms ORDER BY f.recorded_ms,o.decision_ms
        """,(cutoff_ms,cutoff_ms,cutoff_ms,cutoff_ms))]
