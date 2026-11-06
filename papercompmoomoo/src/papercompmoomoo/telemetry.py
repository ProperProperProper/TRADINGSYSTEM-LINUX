"""Lossless, append-only application telemetry and broker-call evidence."""
from __future__ import annotations
import datetime
import hashlib
import json
import sqlite3
import time
import uuid
import os
from pathlib import Path
from contextlib import closing
from zoneinfo import ZoneInfo

DEFAULT_DB = Path(__file__).resolve().parents[2] / 'data' / 'telemetry.sqlite3'
API_BUDGET_DB = DEFAULT_DB.parent / 'moomoo_api_budget.sqlite3'

def wait_for_api_budget(method):
    """Enforce Moomoo's shared order-list ceiling across bot processes.

    Moomoo rejects order_list_query above 10 calls per 30 seconds. Reserve two
    calls for other clients and pace all wrapped contexts to 8 per rolling
    window using one SQLite transaction shared by Gainer, ORB, and exit worker.
    """
    if method.lower() != 'order_list_query':return 0
    limit=8;window_ms=30_000;waited_ms=0
    API_BUDGET_DB.parent.mkdir(parents=True,exist_ok=True)
    while True:
        now_ms=time.time_ns()//1_000_000
        with closing(sqlite3.connect(API_BUDGET_DB,timeout=30)) as db:
            db.execute('PRAGMA busy_timeout=30000')
            db.execute('PRAGMA journal_mode=WAL')
            db.execute('CREATE TABLE IF NOT EXISTS api_calls(method TEXT NOT NULL,called_ms INTEGER NOT NULL)')
            db.execute('CREATE INDEX IF NOT EXISTS api_calls_method_time ON api_calls(method,called_ms)')
            db.execute('BEGIN IMMEDIATE')
            cutoff=now_ms-window_ms
            db.execute('DELETE FROM api_calls WHERE called_ms<?',(now_ms-86_400_000,))
            recent=db.execute('SELECT called_ms FROM api_calls WHERE method=? AND called_ms>? ORDER BY called_ms',(method,cutoff)).fetchall()
            # Processes started before this guard was deployed may have made
            # broker calls absent from api_calls. Reconcile append-only broker
            # telemetry on every reservation, not just when the budget table is
            # empty, so a partial/older process cannot bypass the shared cap.
            telemetry_path=Path(os.environ.get('TRADING_TELEMETRY_DB',str(DEFAULT_DB)))
            if telemetry_path.exists():
                try:
                    with closing(sqlite3.connect(f'file:{telemetry_path}?mode=ro',uri=True,timeout=5)) as history:
                        prior=history.execute("SELECT received_ns FROM events WHERE kind='broker_request' AND json_extract(payload_json,'$.method')=? AND received_ns>=? ORDER BY received_ns",
                            (method,cutoff*1_000_000)).fetchall()
                    missing=max(0,len(prior)-len(recent))
                    # Insert the newest missing timestamps: conservative under
                    # clock/receipt skew and still naturally expire by window.
                    for (received_ns,) in prior[-missing:] if missing else ():
                        db.execute('INSERT INTO api_calls(method,called_ms) VALUES(?,?)',(method,int(received_ns)//1_000_000))
                    if missing:
                        recent=db.execute('SELECT called_ms FROM api_calls WHERE method=? AND called_ms>? ORDER BY called_ms',(method,cutoff)).fetchall()
                except sqlite3.Error:
                    # A failed history read must fail closed if our own budget
                    # is already full; callers still cannot exceed that cap.
                    pass
            if len(recent)<limit:
                db.execute('INSERT INTO api_calls(method,called_ms) VALUES(?,?)',(method,now_ms))
                db.commit()
                return waited_ms
            delay=max(25,int(recent[0][0])+window_ms-now_ms+10)
            db.commit()
        time.sleep(delay/1000)
        waited_ms+=delay

def plain(value):
    if value is None or isinstance(value, (str, bool, int)):
        return value
    if isinstance(value, float):
        import math
        return value if math.isfinite(value) else {'nonfinite': str(value)}
    if isinstance(value, dict):
        return {str(k): plain(v) for k,v in value.items()}
    if isinstance(value, (tuple,list)):
        return [plain(v) for v in value]
    if hasattr(value, 'to_dict'):
        return plain(value.to_dict(orient='records'))
    if hasattr(value, 'item'):
        return plain(value.item())
    if isinstance(value, bytes):
        return {'hex': value.hex()}
    if isinstance(value, (datetime.datetime,datetime.date)):
        return value.isoformat()
    return {'type': type(value).__name__, 'value': str(value)}

def redact(value):
    if isinstance(value,dict):
        return {k:('[REDACTED]' if any(x in str(k).lower() for x in ('password','token','secret')) else redact(v)) for k,v in value.items()}
    if isinstance(value,(list,tuple)):
        return [redact(v) for v in value]
    return value

def emit(kind, payload, *, actor='capture', correlation_id=None, path=DEFAULT_DB):
    path=Path(os.environ.get('TRADING_TELEMETRY_DB',str(path))) if Path(path)==DEFAULT_DB else Path(path)
    now=time.time_ns();session=datetime.datetime.fromtimestamp(now/1e9,ZoneInfo('America/New_York')).date().isoformat()
    body=json.dumps(redact(plain(payload)),sort_keys=True,allow_nan=False)
    Path(path).parent.mkdir(parents=True,exist_ok=True)
    with closing(sqlite3.connect(path,timeout=30)) as db, db:
        db.execute('PRAGMA busy_timeout=30000')
        db.execute('PRAGMA journal_mode=WAL')
        db.execute('CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY,event_id TEXT NOT NULL UNIQUE,session TEXT NOT NULL,received_ns INTEGER NOT NULL,monotonic_ns INTEGER NOT NULL,actor TEXT NOT NULL,kind TEXT NOT NULL,correlation_id TEXT,payload_sha256 TEXT NOT NULL,payload_json TEXT NOT NULL)')
        for action in ('UPDATE','DELETE'):
            db.execute(f"CREATE TRIGGER IF NOT EXISTS events_no_{action.lower()} BEFORE {action} ON events BEGIN SELECT RAISE(ABORT,'permanent telemetry'); END")
        event_id=uuid.uuid4().hex
        db.execute('INSERT INTO events(event_id,session,received_ns,monotonic_ns,actor,kind,correlation_id,payload_sha256,payload_json) VALUES(?,?,?,?,?,?,?,?,?)',(event_id,session,now,time.monotonic_ns(),actor,kind,correlation_id,hashlib.sha256(body.encode()).hexdigest(),body))
    return event_id

class RecordedContext:
    """Proxy preserving requests, responses, exceptions, and elapsed time."""
    def __init__(self, context, actor, path=DEFAULT_DB):
        self._context=context;self._actor=actor;self._path=path
    def __getattr__(self,name):
        method=getattr(self._context,name)
        if not callable(method) or name.startswith('_'):
            return method
        def invoke(*args,**kwargs):
            budget_wait_ms=wait_for_api_budget(name)
            protected = any(x in name.lower() for x in ('unlock','login','password'))
            if budget_wait_ms:
                emit('broker_rate_limit_wait',{'method':name,'wait_ms':budget_wait_ms,
                    'limit':8,'window_ms':30_000,'shared_across':'all paper bot contexts'},actor=self._actor,path=self._path)
            cid=emit('broker_request',{'method':name,'args':'[REDACTED]' if protected else args,
                'kwargs':'[REDACTED]' if protected else kwargs,'rate_budget_wait_ms':budget_wait_ms},actor=self._actor,path=self._path)
            start=time.monotonic_ns()
            try:
                result=method(*args,**kwargs)
            except Exception as exc:
                emit('broker_exception',{'method':name,'error':repr(exc),'duration_ns':time.monotonic_ns()-start},actor=self._actor,correlation_id=cid,path=self._path)
                raise
            emit('broker_response',{'method':name,'result':result,'duration_ns':time.monotonic_ns()-start},actor=self._actor,correlation_id=cid,path=self._path)
            try:
                ret,message=result[0],str(result[1])
                if ret!=0 and any(term in message.lower() for term in ('frequency','quota','too many requests','rate limit')):
                    emit('broker_frequency_limit_error',{'method':name,'ret':ret,'message':message[:300]},
                        actor=self._actor,correlation_id=cid,path=self._path)
            except (TypeError,IndexError,KeyError):
                pass
            return result
        return invoke
    def __setattr__(self,name,value):
        if name.startswith('_'):
            object.__setattr__(self,name,value)
        else:
            setattr(self._context,name,value)

def install_handlers(context, trade=False):
    import futu
    if not hasattr(context,'set_handler'):
        emit('capture_unavailable',{'channel':'trade_callbacks' if trade else 'quote_callbacks','reason':'context has no push handler API'})
        return
    handlers = [('TradeOrderHandlerBase','order_update'),('TradeDealHandlerBase','fill')] if trade else [('StockQuoteHandlerBase','quote'),('TickerHandlerBase','prints'),('OrderBookHandlerBase','depth')]
    for name,kind in handlers:
        base=getattr(futu,name)
        def receive(self,rsp_pb,_base=base,_kind=kind):
            ret,data=_base.on_recv_rsp(self,rsp_pb)
            emit('feed_'+_kind,{'ret':ret,'data':data})
            return ret,data
        handler=type('Capture'+name,(base,),{'on_recv_rsp':receive})()
        context.set_handler(handler)

def subscribe(context,codes):
    import futu
    complete=True
    for subtype in (futu.SubType.QUOTE,futu.SubType.TICKER,futu.SubType.ORDER_BOOK):
        ret,data=context.subscribe(codes,[subtype],subscribe_push=True,session=futu.Session.ALL)
        emit('subscription_status',{'codes':codes,'subtype':subtype,'ret':ret,'response':data,'available':ret==futu.RET_OK})
        complete = complete and ret==futu.RET_OK
    return complete

def debrief(path, session):
    with closing(sqlite3.connect(f'file:{path}?mode=ro',uri=True)) as db:
        counts=dict(db.execute('SELECT kind,count(*) FROM events WHERE session=? GROUP BY kind',(session,)))
        failures=[json.loads(x[0]) for x in db.execute("SELECT payload_json FROM events WHERE session=? AND kind IN ('subscription_status','broker_exception')",(session,))]
    ledgers={}
    start=int(datetime.datetime.fromisoformat(session).replace(tzinfo=ZoneInfo('America/New_York')).timestamp()*1000)
    for bot in ('journal','orb'):
        ledger=Path(path).parent/f'{bot}.sqlite3'
        if not ledger.exists():
            ledgers[bot]={'available':False};continue
        with closing(sqlite3.connect(f'file:{ledger}?mode=ro',uri=True)) as db:
            db.row_factory=sqlite3.Row
            trades=[dict(r) for r in db.execute('SELECT * FROM trades WHERE closed_ms>=? AND closed_ms<?',(start,start+86400000))]
            decisions=[]
            if bot=='journal':
                decisions=[dict(r) for r in db.execute('SELECT event,count(*) AS count FROM plan_events WHERE ts_ms>=? AND ts_ms<? GROUP BY event',(start,start+86400000))]
            ledgers[bot]={'available':True,'closed_trades':trades,'decision_counts':decisions,
                'gross_realized_usd':sum(r.get('pnl_usd',(r.get('exit_price',0)-r.get('entry',0))*r.get('qty',0)) for r in trades),
                'fees':'not reliably recorded in these trade tables'}
    return {'session':session,'counts':counts,'subscription_and_errors':failures,'ledgers':ledgers,
        'coverage':{k:counts.get('feed_'+k,0)>0 for k in ('quote','prints','depth')},
        'limitations':['Feed coverage is not proof of continuous coverage or executable fills.','No news subscription or clock-offset measurement configured.']}

def preserve_file(path):
    """Retain a content-addressed copy before replacing a state file."""
    path=Path(path)
    if not path.exists():
        return
    raw=path.read_bytes();digest=hashlib.sha256(raw).hexdigest()
    directory=path.parent/'permanent_file_history'/path.name
    directory.mkdir(parents=True,exist_ok=True)
    target=directory/digest
    try:
        with target.open('xb') as stream:
            stream.write(raw)
    except FileExistsError:
        if target.read_bytes()!=raw:
            raise ValueError('archived file hash mismatch')
    emit('file_version',{'path':str(path),'sha256':digest,'archive':str(target)})

def write_debrief(session, path=DEFAULT_DB):
    """Version every generated report; never replace a previous report."""
    report=debrief(path,session)
    root=Path(path).parent/'session_debriefs';root.mkdir(parents=True,exist_ok=True)
    target=root/f'{session}-{time.time_ns()}.json'
    with target.open('x') as handle:
        json.dump(report,handle,indent=2)
    return target

if __name__=='__main__':
    import argparse
    p=argparse.ArgumentParser();p.add_argument('--session',required=True);p.add_argument('--db',type=Path,default=DEFAULT_DB)
    a=p.parse_args();print(json.dumps(debrief(a.db,a.session),indent=2))
