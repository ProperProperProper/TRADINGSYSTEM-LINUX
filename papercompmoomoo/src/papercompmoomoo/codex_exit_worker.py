"""Real-time SIMULATE-only Codex exit worker; Qwen is reserved for learning."""
from __future__ import annotations
import datetime as dt
import json
import math
import os
import sqlite3
import sys
import time
from contextlib import closing
from pathlib import Path
from zoneinfo import ZoneInfo
from . import online_exit
from .live_market_brief import build_market_brief,read_moomoo_closed_minutes
from .exit_execution import Quote,check_quote
from .paper_exit_controller import PaperExitController,PAPER_ACCOUNTS,SIGNALS,validate_analysis
from .exit_ownership import CODEX_EXIT_REMARK,require_codex_submission
from .journal import Journal
from .shared_account import adopt as adopt_shared_owner, owner_of as shared_owner_of
from .codex_exit_decision import decide
from .algo_fingerprint import evaluate_dynamic_exit
from .telemetry import RecordedContext,emit,plain

DATA=Path(__file__).resolve().parents[2]/'data'
MODE=DATA/'codex_exit_mode.json'
LEDGER=DATA/'online_exit.sqlite3'
NY=ZoneInfo('America/New_York')
WORKER_RSS_RESTART_BYTES=3*1024**3
# Both strategy contexts share one simulated account. Reuse the broker position
# snapshot across worker cycles to stay under OpenD's position-query frequency limit.
POSITIONS_CACHE_TTL_SECONDS=5.0
DEFERRED_EXITS=DATA/'deferred_exits.sqlite3'
MAX_EXIT_PROVIDER_AGE_MS=30_000
PRICE_LINE_EXIT_SIGNALS={'protective_stop','profit_target','profit_target_100','profit_retrace_failure','midpoint_failure','liquidity_trap'}

def positive_executable_bid(quote):
    """Return a finite, positive bid; zero/NaN placeholders must never cross exit lines."""
    try:
        bid=float(getattr(quote,'bid',0) or 0)
    except (TypeError,ValueError):
        return None
    return bid if math.isfinite(bid) and bid>0 else None

def valid_deferred_trigger(data):
    """Existing submitted exits may be latched by order id; price triggers require a real bid."""
    if data.get('signal') not in PRICE_LINE_EXIT_SIGNALS or data.get('source_order_id'):
        return True
    try:bid=float(data.get('trigger_bid'))
    except (TypeError,ValueError):return False
    return math.isfinite(bid) and bid>0

def provider_quote_is_fresh(quote,now_ms,max_age_ms=MAX_EXIT_PROVIDER_AGE_MS):
    provider_ms=getattr(quote,'provider_ms',None)
    if provider_ms is None:
        return 0<=now_ms-getattr(quote,'received_ms',0)<=1_000
    return 0<=now_ms-provider_ms<=max_age_ms

def market_quote_is_fresh(market_quote,now_ms,max_age_ms=MAX_EXIT_PROVIDER_AGE_MS):
    if not market_quote:return False
    try:
        provider_ms=int(market_quote.get('provider_ms') or 0)
        received_ms=int(market_quote.get('received_ms') or 0)
    except (TypeError,ValueError):return False
    return provider_ms>0 and 0<=now_ms-provider_ms<=max_age_ms and 0<=now_ms-received_ms<=max_age_ms

def record_latched_exit(bot,position,signal,reason,payload=None):
    """Persist a price trigger so it can be retried until the broker position closes."""
    from .telemetry import plain
    if not valid_deferred_trigger({'signal':signal,**(payload or {})}):
        emit('codex_exit_invalid_trigger_not_latched',{'bot':bot,'symbol':position.get('symbol'),
            'opened_ms':position.get('opened_ms'),'signal':signal,'reason':reason,
            'trigger_bid':(payload or {}).get('trigger_bid'),
            'detail':'price-line and liquidity exits require a finite positive executable bid'},actor=bot)
        return {'bot':bot,'symbol':position.get('symbol'),'opened_ms':position.get('opened_ms'),
            'signal':signal,'reason':reason,'latched':False}
    data={'bot':bot,'symbol':position['symbol'],'opened_ms':int(position['opened_ms']),
        'signal':signal if signal in SIGNALS else 'protective_stop',
        'reason':reason,'triggered_ms':time.time_ns()//1_000_000,**(payload or {})}
    encoded=json.dumps(plain(data),sort_keys=True,separators=(',',':'),allow_nan=False)
    DEFERRED_EXITS.parent.mkdir(parents=True,exist_ok=True)
    inserted=False
    with closing(sqlite3.connect(DEFERRED_EXITS,timeout=10)) as db,db:
        db.execute('PRAGMA busy_timeout=10000')
        db.executescript("""
        CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY,bot TEXT NOT NULL,symbol TEXT NOT NULL,opened_ms INTEGER NOT NULL,signal TEXT NOT NULL,triggered_ms INTEGER NOT NULL,payload_json TEXT NOT NULL,UNIQUE(bot,symbol,opened_ms,signal));
        CREATE TRIGGER IF NOT EXISTS deferred_exit_events_no_update BEFORE UPDATE ON events BEGIN SELECT RAISE(ABORT,'immutable deferred exit evidence'); END;
        CREATE TRIGGER IF NOT EXISTS deferred_exit_events_no_delete BEFORE DELETE ON events BEGIN SELECT RAISE(ABORT,'immutable deferred exit evidence'); END;
        """)
        cur=db.execute('INSERT OR IGNORE INTO events(bot,symbol,opened_ms,signal,triggered_ms,payload_json) VALUES(?,?,?,?,?,?)',
            (bot,data['symbol'],data['opened_ms'],data['signal'],data['triggered_ms'],encoded))
        inserted=cur.rowcount>0
    if inserted:emit('codex_exit_trigger_latched',data,actor=bot)
    return data

def pending_deferred_exits():
    if not DEFERRED_EXITS.exists():return {}
    with closing(sqlite3.connect(f'file:{DEFERRED_EXITS}?mode=ro',uri=True,timeout=2)) as db:
        db.execute('PRAGMA busy_timeout=2000')
        try:rows=db.execute('SELECT bot,symbol,opened_ms,payload_json FROM events ORDER BY triggered_ms,id').fetchall()
        except sqlite3.Error:return {}
    pending={}
    for bot,symbol,opened,payload in rows:
        pending.setdefault((bot,symbol,int(opened)),json.loads(payload))
    return pending

def resident_bytes():
    try:
        pages=int(Path('/proc/self/statm').read_text().split()[1])
        return pages*os.sysconf('SC_PAGE_SIZE')
    except (OSError,ValueError,IndexError):
        return 0

def restart_if_memory_high():
    rss=resident_bytes()
    if rss<WORKER_RSS_RESTART_BYTES:return
    emit('codex_exit_worker_memory_restart',{'rss_bytes':rss,
        'threshold_bytes':WORKER_RSS_RESTART_BYTES,
        'reason':'worker RSS threshold reached; reinitialize broker contexts and release process memory'},
        actor='codex_exit_worker')
    os.execv(sys.executable,[sys.executable,'-m','papercompmoomoo.codex_exit_worker'])

def regular_session(local):return local.weekday()<5 and dt.time(9,30)<=local.time()<dt.time(16)
def extended_session(local):return local.weekday()<5 and dt.time(4)<=local.time()<dt.time(20)
def session_flatten_due(local):return local.weekday()<5 and local.time()>=dt.time(15,0)

def orb_setup_context(symbol,opened_ms,price=None,entry_price=None):
    """Recover this position's own causal ORB setup; never borrow another candidate's levels."""
    ticker=symbol.removeprefix('US.')
    opened_local=dt.datetime.fromtimestamp(opened_ms/1000,NY)
    session=opened_local.date().isoformat()
    result={'strategy':'opening_range_breakout','candidate_symbol':ticker,'position_opened_ms':opened_ms,
        'entry_side':'LONG','setup_record_status':'missing'}
    try:
        with closing(sqlite3.connect(f'file:{DATA/"orb.sqlite3"}?mode=ro',uri=True,timeout=3)) as db:
            db.execute('PRAGMA busy_timeout=3000')
            row=db.execute("SELECT ts_ms,side,orh,orl,vwap,breakout_volume,risk,decision,reason,payload_json "
                "FROM signal_observations WHERE symbol=? AND session=? AND ts_ms<=? "
                "ORDER BY ts_ms DESC LIMIT 1",(ticker,session,opened_ms)).fetchone()
        if row:
            ts,side,orh,orl,entry_vwap,breakout_volume,risk,decision,reason,payload=row
            meta=json.loads(payload or '{}')
            result.update({'entry_signal_ts_ms':ts,'entry_side':side,'opening_range_high':orh,
                'opening_range_low':orl,'entry_signal_vwap':entry_vwap,
                'entry_breakout_volume':breakout_volume,'setup_risk':risk,
                'entry_price':entry_price,
                'opening_bars':meta.get('opening_bars'),'entry_decision':decision,
                'entry_reason':reason,'setup_record_status':'matched'})
            if risk and entry_price and entry_price>0:
                result['setup_risk_pct_of_entry']=risk/entry_price*100
            if orh and orl and orl>0:
                result['opening_range_width_pct']=(orh/orl-1)*100
                if price and price>0:
                    result['price_vs_opening_range_high_pct']=(price/orh-1)*100
                    result['price_vs_opening_range_low_pct']=(price/orl-1)*100
                    if entry_vwap and entry_vwap>0:
                        result['price_vs_entry_signal_vwap_pct']=(price/entry_vwap-1)*100
    except (sqlite3.Error,OSError,ValueError,TypeError) as exc:
        result['setup_record_status']='unavailable'
        result['setup_record_error']=type(exc).__name__
    return result

def exclusive_mode():
    if not MODE.exists():return False
    try:
        state=json.loads(MODE.read_text())
        return (state.get('exclusive') is True and state.get('paper_only') is True
                and state.get('decision_agent')=='codex'
                and state.get('accounts')==PAPER_ACCOUNTS)
    except (OSError,ValueError,TypeError):return False

def provider_time(value):
    if not value:return None
    try:
        if hasattr(value,'to_pydatetime'):value=value.to_pydatetime()
        if isinstance(value,dt.datetime):
            date=value
            if date.tzinfo is None:date=date.replace(tzinfo=NY)
            return int(date.timestamp()*1000)
        text=str(value)
        if len(text)<=15:
            # OpenD can report a time-only server receipt field. Resolve its
            # trading date against the local date, including the midnight
            # boundary, then let freshness validation reject old values.
            local=dt.datetime.now(NY)
            date=dt.datetime.fromisoformat(local.date().isoformat()+' '+text)
            candidates=[date.replace(tzinfo=NY),
                (date-dt.timedelta(days=1)).replace(tzinfo=NY),
                (date+dt.timedelta(days=1)).replace(tzinfo=NY)]
            return min((int(x.timestamp()*1000) for x in candidates),
                       key=lambda ms:abs(ms-time.time_ns()//1000000))
        date=dt.datetime.fromisoformat(text)
        if date.tzinfo is None:date=date.replace(tzinfo=NY)
        return int(date.timestamp()*1000)
    except (TypeError,ValueError):return None

class Broker:
    def __init__(self):
        from futu import OpenQuoteContext,OpenSecTradeContext,TrdMarket,TrdEnv,RET_OK
        self.env=TrdEnv.SIMULATE;self.ok=RET_OK
        self.q=RecordedContext(OpenQuoteContext(host='127.0.0.1',port=11111),'codex_quote')
        self.t=RecordedContext(OpenSecTradeContext(host='127.0.0.1',port=11111,filter_trdmarket=TrdMarket.US),'codex_trade')
        ret,rows=self.t.get_acc_list()
        if ret!=self.ok:raise RuntimeError('paper account list unavailable')
        available={int(r['acc_id']) for _,r in rows.iterrows()
            if str(r.get('trd_env'))=='SIMULATE' and str(r.get('sim_acc_type'))=='COMPETITION'
            and 'US' in list(r.get('trdmarket_auth') or [])}
        if not set(PAPER_ACCOUNTS.values())<=available:raise RuntimeError('both configured US Competition SIMULATE accounts must be verified')
        self.subscribed=set();self.decision_id='';self.known={};self.push_event_ms={}
        self.latest_quotes={};self.push_quotes={}
        self.last_exit_trade_snapshot_ms={}
        self.positions_cache={}
        from futu import OrderBookHandlerBase,StockQuoteHandlerBase
        owner=self
        def receive(self,rsp_pb):
            ret,data=StockQuoteHandlerBase.on_recv_rsp(self,rsp_pb)
            if ret==owner.ok and data is not None and not data.empty:
                received=time.time_ns()//1000000
                for _,row in data.iterrows():
                    code=str(row.get('code',''))
                    stamp=f"{row.get('data_date','')} {row.get('data_time','')}".strip()
                    provider=provider_time(stamp)
                    quote={key:plain(row.get(key)) for key in ('last_price','prev_close_price','pre_price','after_price','overnight_price','volume','pre_volume','after_volume','overnight_volume')}
                    owner.market_quotes[code]={'received_ms':received,'provider_ms':provider,'values':quote}
                    if owner.should_record_push('codex_quote_push',code,received):
                        emit('codex_quote_push',{'symbol':code,'received_ms':received,
                            'provider_ms':provider,'data_date':plain(row.get('data_date')),
                            'data_time':plain(row.get('data_time')),'values':quote},actor='codex_quote')
            return ret,data
        handler=type('QwenLiveQuoteHandler',(StockQuoteHandlerBase,),{'on_recv_rsp':receive})()
        self.q.set_handler(handler)
        def receive_book(self,rsp_pb):
            ret,book=OrderBookHandlerBase.on_recv_rsp(self,rsp_pb)
            if ret==owner.ok and isinstance(book,dict):
                code=str(book.get('code',''))
                bids=book.get('Bid') or [];asks=book.get('Ask') or []
                bid=float(bids[0][0]) if bids else 0.0;ask=float(asks[0][0]) if asks else 0.0
                bid_ms=provider_time(book.get('svr_recv_time_bid'));ask_ms=provider_time(book.get('svr_recv_time_ask'))
                if rsp_pb.HasField('s2c'):
                    body=rsp_pb.s2c
                    bid_ts=float(body.svrRecvTimeBidTimestamp) if body.HasField('svrRecvTimeBidTimestamp') else 0.0
                    ask_ts=float(body.svrRecvTimeAskTimestamp) if body.HasField('svrRecvTimeAskTimestamp') else 0.0
                    if bid_ts>0:bid_ms=int(bid_ts*1000 if bid_ts<10_000_000_000 else bid_ts)
                    if ask_ts>0:ask_ms=int(ask_ts*1000 if ask_ts<10_000_000_000 else ask_ts)
                received=time.time_ns()//1000000
                old=owner.push_quotes.get(code)
                if bid>0 and ask>=bid and (old is None or received>old.received_ms):
                    owner.push_quotes[code]=Quote(received,bid,ask,bid_ms,True)
                if owner.should_record_push('codex_order_book_push',code,received):
                    emit('codex_order_book_push',{'symbol':code,'received_ms':received,
                        'bid_provider_ms':bid_ms,'ask_provider_ms':ask_ms,'bid':bid,'ask':ask,
                        'svr_recv_time_bid':plain(book.get('svr_recv_time_bid')),
                        'svr_recv_time_ask':plain(book.get('svr_recv_time_ask'))},actor='codex_quote')
            return ret,book
        book_handler=type('QwenLiveOrderBookHandler',(OrderBookHandlerBase,),{'on_recv_rsp':receive_book})()
        self.q.set_handler(book_handler)
        self.market_quotes={}

    def should_record_push(self,kind,symbol,received_ms):
        """Persist sampled feed heartbeats; decision/exit snapshots stay complete."""
        key=(kind,symbol);previous=self.push_event_ms.get(key,0)
        if received_ms-previous<5_000:return False
        self.push_event_ms[key]=received_ms
        return True

    def opened_ms(self,bot,symbol):
        if bot=='gainer':
            path=DATA/'positions.json'
            if not path.exists():return None
            row=json.loads(path.read_text()).get(symbol)
            return provider_time(row.get('opened')) if row else None
        with closing(sqlite3.connect(f'file:{DATA/"orb.sqlite3"}?mode=ro',uri=True)) as db:
            row=db.execute("SELECT opened_ms FROM positions WHERE symbol=? AND status IN ('open','closing')",(symbol.removeprefix('US.'),)).fetchone()
            return int(row[0]) if row else None

    def gainer_risk_context(self,symbol,entry,opened_ms=None):
        """Load the Gainers bot's own persisted stop and high-water parameters."""
        ticker=symbol.removeprefix('US.')
        try:positions=json.loads((DATA/'positions.json').read_text())
        except (OSError,ValueError):positions={}
        row=positions.get(symbol) or positions.get('US.'+ticker) or positions.get(ticker) or {}
        # Moomoo adjusts cost_price for the remaining lot after a partial sale
        # to fold realized P&L into the displayed basis. It is not the trade's
        # original entry and must never move the stop/target levels.
        entry=float(row.get('entry') or entry or 0.0)
        try:terms=json.loads((DATA/'scout_terms.json').read_text())
        except (OSError,ValueError):terms={}
        plan=terms.get(symbol) or terms.get('US.'+ticker) or terms.get(ticker) or {}
        meta=plan.get('_meta') or {}
        trusted_symbol_terms=plan.get('volatility_source') in ('symbol_moomoo_ohlc','symbol_moomoo_history')
        symbol_terms={}
        if not trusted_symbol_terms or not all(float(plan.get(key,0) or 0)>0 for key in ('stop_pct','trail_pct','take_profit_pct')):
            try:
                from .swings import symbol_volatility_terms
                with closing(sqlite3.connect(f'file:{DATA/"common1m.sqlite3"}?mode=ro',uri=True,timeout=1)) as db:
                    rows=db.execute("SELECT ts,open,high,low,close,volume FROM bars WHERE symbol IN (?,?) AND source='moomoo' AND volume>0 AND ts<? ORDER BY ts DESC LIMIT 120",
                        (ticker,'US.'+ticker,int(opened_ms or time.time()*1000))).fetchall()
                symbol_terms=symbol_volatility_terms(list(reversed(rows)))
            except (sqlite3.Error,OSError,TypeError,ValueError):
                symbol_terms={}
        candle_high=0.0
        if opened_ms is not None:
            try:
                with closing(sqlite3.connect(f'file:{DATA/"common1m.sqlite3"}?mode=ro',uri=True,timeout=1)) as db:
                    candle_high=float(db.execute("SELECT coalesce(max(high),0) FROM bars WHERE symbol IN (?,?) AND source='moomoo' AND ts>=?",
                        (ticker,'US.'+ticker,int(opened_ms))).fetchone()[0] or 0)
            except (sqlite3.Error,OSError,TypeError,ValueError):pass
        high=max(float(entry or 0),float(row.get('high') or 0),float(meta.get('high') or 0),candle_high)
        stop_pct=float((plan.get('stop_pct') if trusted_symbol_terms else 0) or symbol_terms.get('stop_pct') or 0.0)
        trail_pct=float((plan.get('trail_pct') if trusted_symbol_terms else 0) or symbol_terms.get('trail_pct') or 0.0)
        take_pct=float((plan.get('take_profit_pct') if trusted_symbol_terms else 0) or symbol_terms.get('take_profit_pct') or 0.0)
        initial_stop=float(entry)*(1.0-stop_pct/100.0) if entry else 0.0
        trail_stop=high*(1.0-trail_pct/100.0) if high>0 else 0.0
        stored_stop=float(row.get('stop') or 0.0)
        protective_stop=max(initial_stop,trail_stop,stored_stop)
        # If an old/incomplete record has no symbol-derived stop, protect the
        # position at its own entry price instead of inventing a generic %.
        if protective_stop<=0 and entry>0:
            protective_stop=entry
        initial_target=float(entry)*(1.0+take_pct/100.0) if entry and take_pct>0 else 0.0
        reward_distance=max(0.0,initial_target-float(entry or 0))
        # The 100% line starts at the entry target, then ratchets behind a
        # sustained high by one original reward distance so a downward cross
        # can execute while still protecting the full initial move.
        target_price=max(float(row.get('target') or 0.0),initial_target,high-reward_distance if reward_distance>0 else 0.0)
        # Ratchet every line upward: 70% follows the current full target.
        target_70=max(float(row.get('target_70') or 0.0),float(entry)+(target_price-float(entry))*0.70) if entry else 0.0
        profit_midpoint=max(float(row.get('profit_midpoint') or 0.0),
            (target_70+protective_stop)/2.0 if protective_stop>0 and target_70>0 else 0.0)
        profit_retrace=max(float(row.get('profit_retrace') or 0.0),
            (target_70+profit_midpoint)/2.0 if target_70>profit_midpoint>0 else 0.0)
        return {'entry':entry,'configured_stop_pct':stop_pct,'configured_trail_pct':trail_pct,
            'persisted_stop':stored_stop or None,'high_water_price':high,
            'initial_stop_price':initial_stop or None,'trailing_stop_price':trail_stop or None,
            'break_even_armed':False,'protective_stop':protective_stop or None,
            'stop_price':protective_stop or None,'target_price':target_price or None,
            'initial_target_price':initial_target or None,
            'target_70_price':target_70 or None,'profit_retrace_price':profit_retrace or None,
            'profit_midpoint_price':profit_midpoint or None,
            'protective_stop_source':('max(symbol-calibrated initial stop, persisted stop, symbol-volatility trail)' if stop_pct or trail_pct else
                ('persisted stop' if stored_stop>0 else 'entry-price safety floor; symbol volatility evidence unavailable')),
            'risk_terms_source':(plan.get('volatility_source') if trusted_symbol_terms else None) or symbol_terms.get('volatility_source') or ('persisted_position_levels' if stored_stop>0 else 'entry_price_safety_floor'),
            'risk_terms_sample_count':symbol_terms.get('volatility_bar_count',0),
            'risk_terms_incomplete':not bool(stop_pct and trail_pct and take_pct)}

    def positions(self,bot):
        account=PAPER_ACCOUNTS[bot]
        cached=self.positions_cache.get(account)
        if cached and time.monotonic()-cached[0]<POSITIONS_CACHE_TTL_SECONDS:
            rows=cached[1]
        else:
            ret,rows=self.t.position_list_query(trd_env=self.env,acc_id=account,refresh_cache=True)
            if ret!=self.ok:raise RuntimeError('paper positions unreadable')
            self.positions_cache[account]=(time.monotonic(),rows)
        out=[]
        for _,r in rows.iterrows():
            symbol=str(r['code']);qty=int(float(r.get('qty',0) or 0))
            if qty<=0:continue
            owner=shared_owner_of(symbol)
            if owner is None and self.opened_ms(bot,symbol) is not None:
                adopt_shared_owner(bot,{symbol});owner=bot
            if owner!=bot:continue
            opened=self.opened_ms(bot,symbol)
            if opened is None:
                emit('codex_position_unavailable',{'bot':bot,'symbol':symbol,'reason':'entry identity missing'});continue
            broker_cost=float(r.get('cost_price',0) or 0)
            entry=broker_cost
            if bot=='gainer':
                # Use Gainer's persisted original fill basis. The broker's
                # remaining-lot cost can change after partial exits.
                try:
                    saved=json.loads((DATA/'positions.json').read_text())
                    persisted=saved.get(symbol) or saved.get(symbol.removeprefix('US.')) or {}
                    entry=float(persisted.get('entry') or broker_cost)
                except (OSError,ValueError,TypeError):
                    pass
            item={'symbol':symbol,'qty':qty,'available_qty':int(float(r.get('can_sell_qty',qty) or 0)),
                'entry':entry,'opened_ms':opened}
            if bot=='gainer':item.update(self.gainer_risk_context(symbol,entry,opened))
            elif bot=='orb':
                try:
                    with closing(sqlite3.connect(f'file:{DATA/"orb.sqlite3"}?mode=ro',uri=True,timeout=1)) as db:
                        risk=db.execute("SELECT stop_price,target_price,high_price,entry_price,initial_target_price FROM positions WHERE symbol=? AND status IN ('open','closing')",(symbol.removeprefix('US.'),)).fetchone()
                    if risk:
                        stop,target,high,ledger_entry,initial_target=map(float,risk)
                        entry=ledger_entry or entry
                        initial_target=initial_target or target
                        reward=max(0.0,initial_target-entry)
                        target=max(initial_target,high-reward) if initial_target>entry else target
                        target70=entry+(target-entry)*0.70 if target>entry else 0.0
                        midpoint=(target70+stop)/2 if target70>0 and stop>0 else 0.0
                        item['entry']=entry
                        item.update({'stop_price':stop,'target_price':target or None,
                            'target_70_price':target70 or None,
                            'initial_target_price':initial_target or None,
                            'high_water_price':max(entry,high),'profit_midpoint_price':midpoint or None,
                            'profit_retrace_price':((target70+midpoint)/2 if target70>midpoint>0 else None)})
                except (sqlite3.Error,OSError,TypeError,ValueError):pass
            item=ratchet_position_lines(bot,item)
            self.known[(account,symbol)]=item;out.append(item)
        return out

    def position(self,account,symbol):
        owner=shared_owner_of(symbol)
        bot=owner if owner in PAPER_ACCOUNTS else next(k for k,v in PAPER_ACCOUNTS.items() if v==account)
        return next((p for p in self.positions(bot) if p['symbol']==symbol),None)

    def position_for_bot(self,bot,account,symbol):
        if PAPER_ACCOUNTS.get(bot)!=account:raise ValueError('position account/strategy mismatch')
        return next((p for p in self.positions(bot) if p['symbol']==symbol),None)

    def subscribe_live(self,symbol):
        from futu import Session,SubType
        if symbol in self.subscribed:return
        ret,response=self.q.subscribe([symbol],[SubType.QUOTE,SubType.ORDER_BOOK],subscribe_push=True,session=Session.ALL)
        emit('codex_quote_subscription',{'symbol':symbol,'ret':ret,'response':response},actor='codex_quote')
        if ret!=self.ok:raise RuntimeError(f'live quote/order-book subscription refused for {symbol}')
        self.subscribed.add(symbol)

    def quote(self,symbol,force_refresh=False):
        self.subscribe_live(symbol)
        pushed=self.push_quotes.get(symbol)
        now=time.time_ns()//1000000
        if not force_refresh and pushed is not None and now-pushed.received_ms<=1_000:
            quote=Quote(pushed.received_ms,pushed.bid,pushed.ask,pushed.provider_ms,True)
            self.latest_quotes[symbol]=quote
            emit('codex_price_snapshot',{'symbol':symbol,'received_ms':quote.received_ms,
                'provider_ms':quote.provider_ms,'source':'fresh_order_book_push',
                'bid':quote.bid,'ask':quote.ask})
            return quote
        ret,book=self.q.get_order_book(symbol,num=1)
        book=book if ret==self.ok and isinstance(book,dict) else {}
        bid=float(book['Bid'][0][0]) if book.get('Bid') else 0.0
        ask=float(book['Ask'][0][0]) if book.get('Ask') else 0.0
        stamp_values={k:plain(book.get(k)) for k in ('svr_recv_time_bid','svr_recv_time_ask')}
        bid_ms=provider_time(book.get('svr_recv_time_bid'))
        ask_ms=provider_time(book.get('svr_recv_time_ask'))
        source='order_book'
        pushed=self.push_quotes.get(symbol)
        received=time.time_ns()//1000000
        push_fresh=bool(not force_refresh and pushed and received-pushed.received_ms<=1_000)
        if push_fresh and pushed.provider_ms is not None:
            # Prefer Moomoo's live quote push over its cached order book when
            # the push carries the newer provider timestamp.
            if bid_ms is None or pushed.provider_ms>bid_ms:
                bid,ask,bid_ms,ask_ms=pushed.bid,pushed.ask,pushed.provider_ms,pushed.provider_ms
                source='order_book_push'
        elif push_fresh and pushed is not None and bid_ms is None:
            # US order-book pushes normally omit exchange timestamps. Their
            # recent callback time is the available live freshness evidence.
            bid,ask,ask_ms=pushed.bid,pushed.ask,None
            source='order_book_push'
        now_ms=time.time_ns()//1_000_000
        stale_book=source=='order_book' and (bid_ms is None or now_ms-bid_ms>MAX_EXIT_PROVIDER_AGE_MS)
        stale_push=bid_ms is not None and now_ms-bid_ms>MAX_EXIT_PROVIDER_AGE_MS
        if stale_book or stale_push:
            snapshot_ret,snapshot=self.q.get_market_snapshot([symbol])
            if snapshot_ret==self.ok and snapshot is not None and not snapshot.empty:
                row=snapshot.iloc[0]
                stamp=plain(row.get('update_time'))
                snapshot_ms=provider_time(stamp)
                if snapshot_ms is None:
                    stamp=f"{plain(row.get('data_date')) or ''} {plain(row.get('data_time')) or ''}".strip()
                    snapshot_ms=provider_time(stamp)
                snapshot_bid=float(row.get('bid_price',0) or 0)
                snapshot_ask=float(row.get('ask_price',0) or 0)
                if (snapshot_ms is not None and 0<=now_ms-snapshot_ms<=MAX_EXIT_PROVIDER_AGE_MS
                        and 0<snapshot_bid<=snapshot_ask):
                    bid,ask,bid_ms,ask_ms=snapshot_bid,snapshot_ask,snapshot_ms,snapshot_ms
                    source='fresh_market_snapshot_fallback'
                    stamp_values={'update_time':stamp}
                else:
                    emit('codex_stale_order_book_fallback_unavailable',{'symbol':symbol,
                        'order_book_provider_ms':bid_ms,'snapshot_provider_ms':snapshot_ms,
                        'snapshot_bid':snapshot_bid,'snapshot_ask':snapshot_ask,
                        'max_provider_age_ms':MAX_EXIT_PROVIDER_AGE_MS},actor='codex_quote')
                    if stale_book:bid=ask=0.0
            else:
                emit('codex_stale_order_book_fallback_unavailable',{'symbol':symbol,
                    'order_book_provider_ms':bid_ms,'snapshot_result':str(snapshot)[:160],
                    'max_provider_age_ms':MAX_EXIT_PROVIDER_AGE_MS},actor='codex_quote')
                if stale_book:bid=ask=0.0
        # A failed/empty book can still be served by a live push. Validation
        # below rejects missing, invalid or genuinely stale provider data.
        # Sell execution depends on the bid. Retain ask freshness in evidence,
        # while the execution gate uses the timestamp for the executable bid.
        provider=bid_ms
        quote=Quote(received,bid,ask,provider,True)
        self.latest_quotes[symbol]=quote
        emit('codex_price_snapshot',{'symbol':symbol,'received_ms':received,'provider_ms':provider,
             'bid_provider_ms':bid_ms,'ask_provider_ms':ask_ms,'source':source,
             'provider_fields':stamp_values,'bid':bid,'ask':ask,'book':book})
        return quote

    def exit_trade_quote(self,symbol):
        """Refresh last-trade confirmation only while a price-line cross is pending."""
        now=time.time_ns()//1_000_000
        cached=self.market_quotes.get(symbol)
        # One on-demand snapshot per symbol per second keeps confirmation
        # responsive without turning the normal monitoring loop into polling.
        if now-self.last_exit_trade_snapshot_ms.get(symbol,0)<1_000:return cached
        self.last_exit_trade_snapshot_ms[symbol]=now
        ret,snapshot=self.q.get_market_snapshot([symbol])
        if ret!=self.ok or snapshot is None or snapshot.empty:return cached
        row=snapshot.iloc[0]
        stamp=plain(row.get('update_time'))
        provider=provider_time(stamp)
        if provider is None:
            stamp=f"{plain(row.get('data_date')) or ''} {plain(row.get('data_time')) or ''}".strip()
            provider=provider_time(stamp)
        received=time.time_ns()//1_000_000
        try:last=float(row.get('last_price') or 0)
        except (TypeError,ValueError):last=0.0
        if (provider is None or last<=0 or not math.isfinite(last)
                or not 0<=received-provider<=MAX_EXIT_PROVIDER_AGE_MS):return cached
        refreshed={'received_ms':received,'provider_ms':provider,'values':{'last_price':last},'source':'market_snapshot'}
        if not cached or provider>int(cached.get('provider_ms') or 0):
            self.market_quotes[symbol]=refreshed
            emit('codex_exit_trade_price_snapshot',{'symbol':symbol,'received_ms':received,
                'provider_ms':provider,'last_price':last,'source':'fresh_market_snapshot'},actor='codex_quote')
            return refreshed
        return cached

    def fresh_quote(self,symbol):
        """Force an OpenD book read for an exit check; timestamp receipt locally."""
        return self.quote(symbol,force_refresh=True)

    def subscribe_market_context(self):
        for symbol in ('US.SPY','US.QQQ'):
            try:self.subscribe_live(symbol)
            except Exception as exc:emit('codex_market_stream_unavailable',{'symbol':symbol,'error':repr(exc)},actor='codex_quote')

    def sync_position_subscriptions(self,positions_by_bot):
        """Keep streaming subscriptions and push caches bounded to live holdings."""
        from futu import SubType
        wanted={'US.SPY','US.QQQ'}
        wanted.update(p['symbol'] for positions in positions_by_bot.values() for p in positions)
        stale=self.subscribed-wanted
        if not stale:return
        codes=sorted(stale)
        ret,result=self.q.unsubscribe(codes,[SubType.QUOTE,SubType.ORDER_BOOK])
        emit('codex_quote_unsubscribe',{'symbols':codes,'ret':ret,'response':result,
            'reason':'symbols no longer held by either paper bot'},actor='codex_quote')
        if ret==self.ok:
            self.subscribed.difference_update(stale)
            for symbol in stale:
                self.latest_quotes.pop(symbol,None)
                self.push_quotes.pop(symbol,None)
                self.market_quotes.pop(symbol,None)
            self.push_event_ms={key:stamp for key,stamp in self.push_event_ms.items() if key[1] not in stale}

    def market_context(self):
        context={symbol:dict(quote) for symbol,quote in self.market_quotes.items()
                 if symbol in ('US.SPY','US.QQQ')}
        now=time.time_ns()//1000000
        for symbol in ('US.SPY','US.QQQ'):
            pushed=self.push_quotes.get(symbol)
            if pushed is not None:
                age=max(0,now-pushed.received_ms)
                context.setdefault(symbol,{})['live_order_book']={
                    'source':'Moomoo order-book push','bid':pushed.bid,'ask':pushed.ask,
                    'received_ms':pushed.received_ms,'provider_ms':pushed.provider_ms,
                    'local_receipt_age_ms':age,
                    'status':'fresh push' if age<=1000 else 'stale push'}
        return context

    def working_sell(self,account,symbol):
        ret,rows=self.t.order_list_query(code=symbol,trd_env=self.env,acc_id=account,refresh_cache=True)
        if ret!=self.ok:raise RuntimeError('cannot verify pending exits')
        active={'SUBMITTING','SUBMITTED','FILLED_PART','WAITING_SUBMIT','CANCELLING_PART','CANCELLING_ALL'}
        return any(str(r.get('trd_side','')).endswith('SELL') and str(r.get('order_status','')).split('.')[-1] in active for _,r in rows.iterrows())

    def maintain_exit_orders(self,positions_by_bot,local):
        """Keep ETH exit limits marketable; convert any leftover ETH order to RTH market at the open."""
        from futu import ModifyOrderOp
        from . import gating
        active={'SUBMITTING','SUBMITTED','FILLED_PART','WAITING_SUBMIT'}
        positions={(bot,p['symbol']):p for bot,items in positions_by_bot.items() for p in items}
        rth=regular_session(local)
        for account in set(PAPER_ACCOUNTS.values()):
            ret,rows=self.t.order_list_query(trd_env=self.env,acc_id=account,refresh_cache=True)
            if ret!=self.ok:raise RuntimeError('cannot maintain Competition exit orders')
            for _,row in rows.iterrows():
                symbol=str(row.get('code',''));remark=str(row.get('remark',''))
                status=str(row.get('order_status','')).split('.')[-1]
                if not remark.startswith(CODEX_EXIT_REMARK) or not str(row.get('trd_side','')).upper().endswith('SELL') or status not in active:
                    continue
                bot=next((b for b in PAPER_ACCOUNTS if PAPER_ACCOUNTS[b]==account and (b,symbol) in positions),None)
                if bot is None:continue
                position=positions[(bot,symbol)]
                order_session=str(row.get('session','')).split('.')[-1].upper()
                if rth and order_session=='ETH':
                    intent=record_latched_exit(bot,position,'protective_stop',
                        'replace residual ETH exit with regular-session market exit',
                        {'source_order_id':str(row.get('order_id')),'source_order_status':status,
                         'source_order_price':float(row.get('price',0) or 0)})
                    cancel_ret,result=self.t.modify_order(ModifyOrderOp.CANCEL,row['order_id'],
                        qty=float(row.get('qty',0) or 0),price=float(row.get('price',0) or 0),
                        trd_env=self.env,acc_id=account)
                    emit('eth_exit_cancel_for_rth_market',{'bot':bot,'symbol':symbol,
                        'order_id':str(row.get('order_id')),'cancel_accepted':cancel_ret==self.ok,
                        'result':str(result),'deferred_signal':intent['signal']},actor=bot)
                    continue
                if rth or order_session not in ('ETH','N/A',''):
                    continue
                try:current_quote=self.quote(symbol,force_refresh=True)
                except Exception as exc:
                    emit('eth_exit_reprice_quote_unavailable',{'bot':bot,'symbol':symbol,'reason':repr(exc)},actor=bot)
                    continue
                current_ms=time.time_ns()//1_000_000
                if not provider_quote_is_fresh(current_quote,current_ms) or current_quote.bid<=0:
                    emit('eth_exit_reprice_quote_stale',{'bot':bot,'symbol':symbol,
                        'provider_ms':current_quote.provider_ms,'received_ms':current_quote.received_ms,
                        'max_provider_age_ms':MAX_EXIT_PROVIDER_AGE_MS},actor=bot)
                    continue
                current=float(row.get('price',0) or 0)
                new_price=gating.tick(current_quote.bid)
                if current>0 and new_price!=current:
                    change_ret,result=self.t.modify_order(ModifyOrderOp.NORMAL,row['order_id'],
                        qty=float(row.get('qty',0) or 0),price=new_price,
                        trd_env=self.env,acc_id=account)
                    emit('eth_exit_limit_repriced',{'bot':bot,'symbol':symbol,
                        'order_id':str(row.get('order_id')),'old_limit':current,'new_limit':new_price,
                        'executable_bid':current_quote.bid,'modify_accepted':change_ret==self.ok,
                        'result':str(result),'reason':'keep the triggered exit at the latest fresh executable bid'},actor=bot)

    def submit_exit(self,account,symbol,qty,trd_env):
        from futu import TrdSide,OrderType,Session
        if trd_env!='SIMULATE' or account not in PAPER_ACCOUNTS.values():raise ValueError('paper-only exit route')
        remark=CODEX_EXIT_REMARK+self.decision_id[:24]
        require_codex_submission(account,remark,simulated=True)
        local=dt.datetime.now(NY)
        if not (regular_session(local) or extended_session(local)):
            raise RuntimeError('Moomoo Competition paper orders are unavailable outside weekday 04:00-20:00 ET')
        state_ret,states=self.q.get_global_state()
        if state_ret!=self.ok or not isinstance(states,dict):
            raise RuntimeError('Moomoo market state unavailable; refusing an unplaceable paper exit')
        market_state=str(states.get('market_us','')).split('.')[-1].upper()
        regular_states={'MORNING','AFTERNOON','TRADE_AT_LAST'}
        extended_states={'PRE_MARKET_BEGIN','PRE_MARKET_END','AFTER_HOURS_BEGIN'}
        if regular_session(local) and market_state not in regular_states:
            raise RuntimeError(f'Moomoo US market is not accepting regular-session orders ({market_state or "unknown"})')
        if not regular_session(local) and market_state not in extended_states:
            raise RuntimeError(f'Moomoo US market is not accepting extended-hours orders ({market_state or "unknown"})')
        if regular_session(local):
            price=0;order_type=OrderType.MARKET;session=Session.RTH;outside=False;execution='regular_market'
        else:
            quote=self.latest_quotes.get(symbol)
            if quote is None or quote.bid<=0:raise RuntimeError('no fresh executable bid for Competition ETH limit exit')
            # Moomoo only accepts limit orders in ETH. Price at the current
            # executable bid; never add a discretionary discount below it.
            from . import gating
            price=gating.tick(quote.bid);order_type=OrderType.NORMAL;session=Session.ETH;outside=False
            execution='premarket_marketable_limit' if local.time()<dt.time(9,30) else 'afterhours_marketable_limit'
        ret,result=self.t.place_order(price=price,qty=qty,code=symbol,order_type=order_type,
            trd_side=TrdSide.SELL,trd_env=self.env,acc_id=account,remark=remark,
            fill_outside_rth=outside,session=session)
        if ret!=self.ok:raise RuntimeError('Codex-approved paper exit rejected: '+str(result))
        return {'order_id':str(result.iloc[0]['order_id']),'execution':execution,
                'limit_price':price if order_type==OrderType.NORMAL else None,'fill_outside_rth':outside}

    def filled_exit_orders(self,account):
        """Read broker-confirmed cumulative fills from simulated order records.

        Moomoo's SIMULATE environment does not expose deal history on all
        installations. Its order list still reports dealt_qty/average price;
        accepted or working orders are deliberately excluded.
        """
        ret,rows=self.t.order_list_query(trd_env=self.env,acc_id=account,refresh_cache=True)
        if ret!=self.ok:
            raise RuntimeError(f'broker order/fill history unavailable (ret={ret!r}, response={str(rows)[:240]})')
        terminal={'FILLED_ALL','CANCELLED_PART','CANCELLED_ALL'}
        for _,r in rows.iterrows():
            if not str(r.get('trd_side','')).endswith('SELL'):continue
            remark=str(r.get('remark',''))
            if not remark.startswith(CODEX_EXIT_REMARK):continue
            status=str(r.get('order_status','')).split('.')[-1]
            qty=int(float(r.get('dealt_qty',0) or 0))
            price=float(r.get('dealt_avg_price',0) or 0)
            if status not in terminal or qty<=0 or price<=0:continue
            stamp=provider_time(r.get('updated_time')) or provider_time(r.get('create_time'))
            if stamp is None:continue
            yield {'deal_id':f"order:{r['order_id']}",'order_id':str(r['order_id']),
                'price':price,'qty':qty,'filled_ms':stamp,'fees_usd':None,
                'order_status':status,'remark':remark}


def record_gainer_closed_trade(submission,fill,observed_ms):
    """Write the confirmed Competition exit to its dashboard trade journal."""
    if submission.get('bot')!='gainer':return False
    entry=float(submission.get('entry_price') or 0)
    reason='broker_confirmed_exit'
    try:
        with closing(sqlite3.connect(f'file:{LEDGER}?mode=ro',uri=True,timeout=3)) as db:
            row=db.execute('SELECT payload_json FROM actions WHERE observation_id=? ORDER BY recorded_ms DESC LIMIT 1',
                (submission['observation_id'],)).fetchone()
            observation=db.execute('SELECT state_json FROM observations WHERE id=?',
                (submission['observation_id'],)).fetchone()
        if row:
            analysis=json.loads(row[0]);answer=analysis.get('answer') or {}
            reason=str(answer.get('signal') or reason)
            detail=str(analysis.get('final_reason') or '')
            if detail:reason=f'{reason}: {detail}'
            post_position=analysis.get('post_inference_position') or {}
            entry=entry or float(post_position.get('entry') or 0)
        if entry<=0 and observation:
            state=json.loads(observation[0]);position=state.get('position') or {}
            entry=float(position.get('entry_price') or position.get('entry') or 0)
    except (sqlite3.Error,OSError,ValueError,TypeError):
        pass
    if entry<=0:raise ValueError('entry price missing from exit submission and immutable decision evidence')
    evidence={'source':'Moomoo SIMULATE order_list_query','actual_broker_fill':True,
        'account_id':submission.get('account_id'),'bot':'gainer','order_id':fill['order_id'],
        'observation_id':submission['observation_id'],'symbol':submission['symbol'],
        'order_status':fill['order_status'],'dealt_qty':fill['qty'],
        'dealt_avg_price':fill['price'],'broker_updated_ms':fill['filled_ms'],
        'observed_ms':observed_ms,'opened_ms':submission['opened_ms'],
        'entry_price':entry,'submitted_ms':submission['submitted_ms']}
    journal=Journal(DATA/'journal.sqlite3')
    try:
        return journal.broker_exit(order_id=fill['order_id'],recorded_ms=observed_ms,
            symbol=submission['symbol'],opened_ms=int(submission['opened_ms']),
            closed_ms=int(fill['filled_ms']),qty=int(fill['qty']),entry=entry,
            exit_price=float(fill['price']),order_status=fill['order_status'],
            evidence=evidence,reason=reason)
    finally:
        journal.close()

def publish_chart_exit_levels(positions_by_bot):
    """Expose the worker's computed levels to the read-only dashboards."""
    rows={}
    for bot,positions in positions_by_bot.items():
        rows[bot]=[{
            'symbol':p['symbol'],'qty':p.get('qty'),'entry':p.get('entry'),
            'opened_ms':p.get('opened_ms'),'high_water_price':p.get('high_water_price'),
            'stop_price':p.get('protective_stop') or p.get('stop_price'),
            'target_price':p.get('target_price'),'initial_target_price':p.get('initial_target_price'),
            'target_70_price':p.get('target_70_price'),
            'profit_retrace_price':p.get('profit_retrace_price'),
            'profit_midpoint_price':p.get('profit_midpoint_price'),
        } for p in positions]
    payload={'updated_ms':time.time_ns()//1000000,**rows}
    path=DATA/'exit_levels.json';temporary=path.with_suffix('.json.tmp')
    temporary.write_text(json.dumps(payload,separators=(',',':'))+'\n')
    temporary.replace(path)

def ratchet_position_lines(bot,position):
    """Persist each exit line as an upward-only ratchet for this exact position.

    Invariant for both bots: every line (red stop, midpoint, intermediate,
    70%, and 100%) is max(current calculation, previously persisted value).
    No caller may publish or trigger on a lower recalculated value.
    """
    symbol=str(position['symbol']);opened=int(position['opened_ms']);entry=float(position.get('entry') or 0)
    if entry<=0:return position
    path=DATA/'online_exit.sqlite3';path.parent.mkdir(parents=True,exist_ok=True)
    with closing(sqlite3.connect(path,timeout=10)) as db:
        db.execute('PRAGMA busy_timeout=10000')
        db.executescript("""
        CREATE TABLE IF NOT EXISTS exit_line_ratchets(bot TEXT NOT NULL,symbol TEXT NOT NULL,opened_ms INTEGER NOT NULL,high_water REAL NOT NULL,stop REAL NOT NULL,target_100 REAL NOT NULL,target_70 REAL NOT NULL,intermediate REAL NOT NULL,midpoint REAL NOT NULL,updated_ms INTEGER NOT NULL,PRIMARY KEY(bot,symbol,opened_ms));
        CREATE TABLE IF NOT EXISTS exit_line_ratchet_events(id INTEGER PRIMARY KEY AUTOINCREMENT,bot TEXT NOT NULL,symbol TEXT NOT NULL,opened_ms INTEGER NOT NULL,updated_ms INTEGER NOT NULL,payload_json TEXT NOT NULL);
        CREATE TRIGGER IF NOT EXISTS exit_line_ratchet_events_no_update BEFORE UPDATE ON exit_line_ratchet_events BEGIN SELECT RAISE(ABORT,'permanent exit-line evidence'); END;
        CREATE TRIGGER IF NOT EXISTS exit_line_ratchet_events_no_delete BEFORE DELETE ON exit_line_ratchet_events BEGIN SELECT RAISE(ABORT,'permanent exit-line evidence'); END;
        """)
        db.execute('BEGIN IMMEDIATE')
        old=db.execute('SELECT high_water,stop,target_100,target_70,intermediate,midpoint FROM exit_line_ratchets WHERE bot=? AND symbol=? AND opened_ms=?',(bot,symbol,opened)).fetchone()
        get=lambda *keys: next((float(position[k]) for k in keys if position.get(k) is not None and float(position[k])>0),0.0)
        high=max(get('high_water_price','high_price','high_water'),old[0] if old else 0.0,entry)
        stop=max(get('protective_stop','stop_price'),old[1] if old else 0.0)
        target100=max(get('target_price'),old[2] if old else 0.0)
        target70=max(get('target_70_price'),entry+(target100-entry)*.70 if target100>entry else 0.0,old[3] if old else 0.0)
        midpoint=max((target70+stop)/2 if target70>0 and stop>0 else 0.0,old[5] if old else 0.0)
        intermediate=max((target70+midpoint)/2 if target70>midpoint>0 else 0.0,old[4] if old else 0.0)
        now=time.time_ns()//1_000_000
        values=(high,stop,target100,target70,intermediate,midpoint)
        changed=old is None or any(abs(float(a)-float(b))>1e-9 for a,b in zip(values,old))
        db.execute('INSERT INTO exit_line_ratchets VALUES(?,?,?,?,?,?,?,?,?,?) ON CONFLICT(bot,symbol,opened_ms) DO UPDATE SET high_water=max(exit_line_ratchets.high_water,excluded.high_water),stop=max(exit_line_ratchets.stop,excluded.stop),target_100=max(exit_line_ratchets.target_100,excluded.target_100),target_70=max(exit_line_ratchets.target_70,excluded.target_70),intermediate=max(exit_line_ratchets.intermediate,excluded.intermediate),midpoint=max(exit_line_ratchets.midpoint,excluded.midpoint),updated_ms=excluded.updated_ms',(bot,symbol,opened,*values,now))
        if changed:
            db.execute('INSERT INTO exit_line_ratchet_events(bot,symbol,opened_ms,updated_ms,payload_json) VALUES(?,?,?,?,?)',(bot,symbol,opened,now,json.dumps(dict(zip(('high_water','stop','target_100','target_70','intermediate','midpoint'),values)),sort_keys=True,separators=(',',':'))))
        db.commit()
    position.update({'high_water_price':high,'target_price':target100 or None,'target_70_price':target70 or None,
        'profit_retrace_price':intermediate or None,'profit_midpoint_price':midpoint or None})
    if bot=='gainer':position['protective_stop']=stop or None
    else:position['stop_price']=stop or None
    return position


def recheck(signal,symbol,quote=None,position=None,broker=None,bot='gainer'):
    checked_ms=time.time_ns()//1000000
    if signal=='session_flatten':
        due=session_flatten_due(dt.datetime.fromtimestamp(checked_ms/1000,NY))
        return {'condition':due,'evidence':{'checked_ms':checked_ms,'signal':signal,'condition':due,'reason':'mandatory one-hour-before-close liquidation' if due else 'before 15:00 ET flatten time'}}
    bars,missing=read_moomoo_closed_minutes(DATA/'common1m.sqlite3',symbol,checked_ms)
    context=broker.market_context() if broker else {}
    setup=(orb_setup_context(symbol,(position or {}).get('opened_ms'),bars[-1][4],(position or {}).get('entry'))
           if bot=='orb' and bars and (position or {}).get('opened_ms') else {'strategy':'intraday_gainer'})
    brief=build_market_brief(symbol,position,quote,bars,context,checked_ms,missing,strategy_context=setup)
    evidence={'checked_ms':checked_ms,'signal':signal,'live_market_brief':brief}
    if signal in ('protective_stop','profit_target','profit_target_100','profit_retrace_failure','midpoint_failure','liquidity_trap','session_flatten'):
        if signal=='liquidity_trap':
            trap=liquidity_trap_overlay(symbol,position or {},quote,bars)
            condition=bool(trap)
            evidence.update({'condition':condition,'liquidity_trap':trap,
                'reason':trap.get('exit_reason') if trap else 'liquidity trap no longer confirmed'})
            return {'condition':condition,'evidence':evidence}
        if signal=='profit_target':
            target=float((position or {}).get('target_70_price') or 0)
            trade_confirmed=market_trade_below_line(broker.market_quotes.get(symbol) if broker else None,target,checked_ms)
            hit=line_crossed_down(position,'target_70_price',bars,quote) and trade_confirmed
            evidence.update({'condition':hit,'executable_bid':getattr(quote,'bid',None),'target_70_price':target or None,
                'trade_price_confirmation':trade_confirmed,
                'reason':'fresh traded price and executable bid confirmed the downward 70% cross' if hit else '70% line cross lacks current traded-price confirmation'})
            return {'condition':hit,'evidence':evidence}
        if signal=='profit_target_100':
            target=float((position or {}).get('target_price') or 0)
            market_quote=broker.exit_trade_quote(symbol) if broker else None
            # The worker enters this recheck only after latching a fresh upward
            # touch. Keep that trigger latched while refreshing the executable
            # bid; the trade may have moved below the line during order checks.
            trade_confirmed=market_quote_is_fresh(market_quote,checked_ms)
            trade_price=float(((market_quote or {}).get('values') or {}).get('last_price') or 0)
            trade_confirmed=trade_confirmed and math.isfinite(trade_price) and trade_price>0
            hit=positive_executable_bid(quote) is not None and trade_confirmed
            evidence.update({'condition':hit,'executable_bid':getattr(quote,'bid',None),'target_100_price':target or None,
                'trade_price':trade_price or None,'trigger_latched':True,
                'trade_price_confirmation':trade_confirmed,
                'reason':'previously confirmed upward target touch remains latched; current executable quote is fresh' if hit else 'latched 100% target touch lacks a fresh executable quote'})
            return {'condition':hit,'evidence':evidence}
        if signal=='profit_retrace_failure':
            level=(position or {}).get('profit_retrace_price')
            trade_confirmed=market_trade_below_line(broker.market_quotes.get(symbol) if broker else None,level,checked_ms)
            hit=line_crossed_down(position,'profit_retrace_price',bars,quote) and trade_confirmed
            evidence.update({'condition':hit,'executable_bid':getattr(quote,'bid',None),'profit_retrace_price':(position or {}).get('profit_retrace_price'),
                'trade_price_confirmation':trade_confirmed,
                'reason':'fresh traded price and executable bid confirmed the downward intermediate cross' if hit else 'intermediate cross lacks current traded-price confirmation'})
            return {'condition':hit,'evidence':evidence}
        if signal=='midpoint_failure':
            level=(position or {}).get('profit_midpoint_price')
            trade_confirmed=market_trade_below_line(broker.market_quotes.get(symbol) if broker else None,level,checked_ms)
            condition=line_crossed_down(position,'profit_midpoint_price',bars,quote) and trade_confirmed
            evidence.update({'condition':condition,'profit_midpoint_price':(position or {}).get('profit_midpoint_price'),
                'latest_closed_candle':bars[-1][0:6] if bars else None,
                'executable_bid':getattr(quote,'bid',None),
                'trade_price_confirmation':trade_confirmed,
                'reason':'fresh traded price and executable bid confirmed the downward midpoint cross' if condition else 'midpoint cross lacks current traded-price confirmation'})
            return {'condition':condition,'evidence':evidence}
        stop=(position or {}).get('protective_stop') or (position or {}).get('stop_price')
        bid=getattr(quote,'bid',None)
        trade_confirmed=market_trade_below_line(broker.market_quotes.get(symbol) if broker else None,stop,checked_ms)
        hit=bool(stop and bid is not None and math.isfinite(float(bid)) and float(bid)>0 and float(bid)<=float(stop) and trade_confirmed)
        evidence.update({'condition':hit,'executable_bid':bid,'protective_stop':stop,
            'trade_price_confirmation':trade_confirmed,
            'position_risk_context':{k:(position or {}).get(k) for k in (
                'entry','configured_stop_pct','configured_trail_pct','persisted_stop',
                'initial_stop_price','trailing_stop_price','high_water_price','break_even_armed')},
            'reason':'fresh traded price and executable bid confirmed the unchanged position stop' if hit else 'stop lacks current traded-price confirmation'})
        return {'condition':hit,'evidence':evidence}
    if len(bars)<5:return {'condition':False,'evidence':dict(evidence,reason=missing or 'fewer than five current-session closed Moomoo candles')}
    recent=bars[-5:];last=recent[-1]
    den=sum(float(b[5]) for b in bars)
    vwap=sum(float(b[4])*float(b[5]) for b in bars)/den if den else None
    evidence['session_vwap']=vwap
    closes=[float(b[4]) for b in bars]
    volume_ratio=None
    if len(bars)>=6:
        prior=sum(float(b[5]) for b in bars[-6:-3])/3
        recent_volume=sum(float(b[5]) for b in bars[-3:])/3
        volume_ratio=recent_volume/prior if prior>0 else None
    down_volume=sum(float(b[5]) for b in bars[-3:] if float(b[4])<float(b[1]))
    total_recent_volume=sum(float(b[5]) for b in bars[-3:])
    down_share=down_volume/total_recent_volume if total_recent_volume>0 else None
    span=float(last[2])-float(last[3])
    close_location=(float(last[4])-float(last[3]))/span if span>0 else None
    prior_lows=[float(b[3]) for b in recent[:-1]]
    below_vwap_count=sum(1 for b in bars[-3:] if vwap is not None and float(b[4])<vwap)
    ret3=(closes[-1]/closes[-4]-1) if len(closes)>=4 and closes[-4] else None
    or_high=setup.get('opening_range_high') if bot=='orb' else None
    or_low=setup.get('opening_range_low') if bot=='orb' else None
    closes_below_orh=sum(1 for b in bars[-3:] if or_high is not None and float(b[4])<float(or_high))
    evidence.update({'closed_candle_price':float(last[4]),'volume_ratio_recent3_vs_prior3':volume_ratio,
        'down_candle_volume_share_last3':down_share,'last_candle_close_location':close_location,
        'closes_below_session_vwap_last3':below_vwap_count,'return_last3':ret3,
        'prior_four_bar_low':min(prior_lows) if prior_lows else None,
        'orb_setup':setup,'closes_below_opening_range_high_last3':closes_below_orh,
        'opening_range_low_breached':bool(or_low is not None and float(last[4])<float(or_low)),
        'fresh_quote_available_for_execution':check_quote(quote,time.time_ns()//1000000) is None if quote else False})
    if bot=='orb' and setup.get('entry_breakout_volume'):
        mean_recent=sum(float(b[5]) for b in bars[-3:])/3
        evidence['recent_mean_volume_vs_entry_breakout_multiple']=mean_recent/float(setup['entry_breakout_volume'])
        entry_vwap=setup.get('entry_signal_vwap')
        evidence['current_session_vwap_vs_entry_signal_vwap_pct']=(
            (vwap/entry_vwap-1)*100 if vwap is not None and entry_vwap else None)
    if signal=='structure_failure':
        if bot=='orb' and or_high is not None:
            # Confirm a failed long breakout: at least two of the last three
            # closes have fallen back under that candidate's own opening high,
            # with VWAP loss or a breach of the opening range low.
            condition=closes_below_orh>=2 and (bool(vwap is not None and float(last[4])<vwap)
                or bool(or_low is not None and float(last[4])<float(or_low)))
        else:condition=float(last[4])<min(prior_lows)
    elif signal=='vwap_failure':
        weak_reclaim=(vwap is not None and below_vwap_count>=2 and float(last[4])<vwap)
        if bot=='orb':
            weak_reclaim=weak_reclaim and float(last[4])<float(bars[-2][4]) and (down_share is None or down_share>=0.45)
        downside_confirm=(ret3 is not None and ret3<0 and down_share is not None and down_share>=0.35)
        bid=positive_executable_bid(quote)
        entry_risk=(bot=='gainer' and bid is not None and (position or {}).get('entry',0)>0 and
            bid<=float(position['entry'])*0.995)
        condition=bool(weak_reclaim or downside_confirm or entry_risk)
    elif signal=='distribution':condition=(float(last[4])<float(last[1]) and close_location is not None and close_location<=0.30
        and down_share is not None and down_share>=0.60 and (volume_ratio is None or volume_ratio>=1.0))
    if signal=='distribution' and bot=='orb' and vwap is not None:
        condition=condition and float(last[4])<vwap
    elif signal=='momentum_decay':
        current_bid=positive_executable_bid(quote)
        entry=float((position or {}).get('entry') or 0)
        peak=float((position or {}).get('high_water_price') or 0)
        pnl_below_entry=bool(bot=='gainer' and current_bid and entry and current_bid<entry)
        peak_giveback=bool(bot=='gainer' and current_bid and peak>entry>0 and
            (peak-entry)/entry*100>=0.75 and (peak-current_bid)/entry*100>=0.5)
        condition=(ret3 is not None and ret3<=-0.15 and (volume_ratio is None or volume_ratio>=0.75)) or pnl_below_entry or peak_giveback
        if bot=='orb' and vwap is not None:
            condition=condition and float(last[4])<vwap and (down_share is None or down_share>=0.45)
    elif signal not in ('structure_failure','vwap_failure','distribution'):
        condition=False
    return {'condition':bool(condition),'evidence':evidence}

def ask(bot,state,observation_id=None):
    answer,metadata=decide(bot,state)
    emit('codex_exit_decision',{'bot':bot,'brief':state,'decision':answer,'backend':metadata},
        actor=bot,correlation_id=observation_id)
    return validate_analysis(answer),metadata

def gainer_codex_review_reasons(state):
    """Only spend a model call when risk, trend, or profit giveback needs judgment."""
    pos=state.get('position') or {}; ctx=state.get('strategy_context') or {}
    candles=state.get('current_session_candles') or {}; tech=candles.get('derived_structure') or {}
    returns=candles.get('recent_close_returns') or {}
    pnl=pos.get('unrealized_at_executable_bid_pct')
    close_vwap=tech.get('last_close_vs_session_vwap_pct')
    ret3=returns.get('3_bar_close_return_pct')
    down=tech.get('recent_3bar_down_volume_share')
    high=ctx.get('high_water_price');entry=pos.get('entry_price')
    bid=(state.get('executable_book') or {}).get('bid')
    reasons=[]
    if pnl is not None and pnl<=-0.25:reasons.append('loss approaching protective stop')
    if close_vwap is not None and close_vwap<0:reasons.append('price below session VWAP')
    if ret3 is not None and ret3<=-0.25:reasons.append('negative three-bar momentum')
    if down is not None and down>=0.60:reasons.append('dominant down-candle volume')
    if high and entry and bid and high>entry and (high-entry)/entry*100>=0.75 and (high-bid)/entry*100>=0.5:
        reasons.append('meaningful giveback from position high')
    return reasons

def gainer_position_decision_context(position,quote):
    entry=float(position.get('entry') or 0);bid=positive_executable_bid(quote) or 0.0
    high=float(position.get('high_water_price') or entry);stop=float(position.get('protective_stop') or 0)
    return {'strategy':'intraday_gainer','configured_stop_pct':position.get('configured_stop_pct'),
        'configured_trail_pct':position.get('configured_trail_pct'),'persisted_stop':position.get('persisted_stop'),
        'high_water_price':high,'initial_stop_price':position.get('initial_stop_price'),
        'trailing_stop_price':position.get('trailing_stop_price'),'break_even_armed':position.get('break_even_armed'),
        'protective_stop':stop or None,'protective_stop_source':position.get('protective_stop_source'),
        'target_price':position.get('target_price'),'target_70_price':position.get('target_70_price'),
        'profit_midpoint_price':position.get('profit_midpoint_price'),
        'executable_bid':bid or None,
        'open_pnl_at_bid_pct':(bid/entry-1)*100 if bid and entry else None,
        'peak_open_pnl_pct':(high/entry-1)*100 if entry else None,
        'peak_to_current_giveback_pct_of_entry':(high-bid)/entry*100 if bid and entry and high>bid else 0.0,
        'distance_above_protective_stop_pct':(bid/stop-1)*100 if bid and stop else None,
        'unrealized_loss_at_bid_usd':max(0.0,(entry-bid)*int(position.get('qty') or 0)) if bid and entry else None}

def profit_midpoint_crossed(position,bars,quote=None):
    """Exit when executable bid is below midpoint after this position traded above it."""
    midpoint=float(position.get('profit_midpoint_price') or 0)
    bid=positive_executable_bid(quote)
    if midpoint<=0 or bid is None or bid>=midpoint:return False
    opened=int(position.get('opened_ms') or 0)
    traded_above=float(position.get('high_water_price') or position.get('high_price') or 0)>=midpoint
    traded_above=traded_above or any(int(b[0])>=opened and float(b[2])>=midpoint for b in bars)
    return traded_above

def line_crossed_down(position,line_key,bars,quote=None):
    """True only after the position traded above a ratcheting line and bid fell below it."""
    level=float((position or {}).get(line_key) or 0)
    bid=positive_executable_bid(quote)
    if level<=0 or bid is None or bid>=level:return False
    opened=int((position or {}).get('opened_ms') or 0)
    high=float((position or {}).get('high_water_price') or (position or {}).get('high_price') or 0)
    return high>=level or any(int(b[0])>=opened and float(b[2])>=level for b in bars)

def market_trade_at_or_above_line(market_quote,level,now_ms=None):
    """A fresh Moomoo last sale touching or exceeding the 100% target is an exit."""
    if not market_quote or level is None:return False
    try:
        price=float((market_quote.get('values') or {}).get('last_price') or 0)
        provider_ms=int(market_quote.get('provider_ms') or 0)
    except (TypeError,ValueError):return False
    now_ms=now_ms or time.time_ns()//1_000_000
    received_ms=int(market_quote.get('received_ms') or 0)
    return (math.isfinite(price) and price>0 and provider_ms>0
        and 0<=now_ms-provider_ms<=MAX_EXIT_PROVIDER_AGE_MS
        and 0<=now_ms-received_ms<=MAX_EXIT_PROVIDER_AGE_MS
        and price>=float(level))

def target_100_touched_up(position,quote,market_quote,now_ms=None):
    """Trigger at once when a fresh traded price touches the 100% line and a bid is executable."""
    level=float((position or {}).get('target_price') or 0)
    return (level>0 and positive_executable_bid(quote) is not None
        and market_trade_at_or_above_line(market_quote,level,now_ms))

def has_pending_line_cross(position,quote,bot):
    stop_key='protective_stop' if bot=='gainer' else 'stop_price'
    return any(line_crossed_down(position,key,[],quote) for key in (
        stop_key,'target_70_price','profit_retrace_price','profit_midpoint_price'))

def confirmed_line_cross(position,line_key,quote,bot,cross_samples,market_quote=None):
    """Require bid and traded price below a line on two distinct fresh updates.

    A low book bid alone is not a price cross (CHTR exposed this with a $124.20
    bid while the last trade was $125.00). Repeated timestamps do not count.
    A rebound above the line or rising second traded price resets confirmation.
    This uses subscribed Moomoo data and does not wait for a one-minute candle.
    """
    opened=int((position or {}).get('opened_ms') or 0)
    symbol=str((position or {}).get('symbol') or '')
    key=(bot,symbol,opened,line_key)
    bid=positive_executable_bid(quote)
    market_quote=market_quote or {}
    values=market_quote.get('values') or {}
    price=float(values.get('last_price') or 0)
    provider_ms=int(market_quote.get('provider_ms') or 0)
    now_ms=time.time_ns()//1_000_000
    level=float((position or {}).get(line_key) or 0)
    market_fresh=market_quote_is_fresh(market_quote,now_ms)
    if (bid is None or not market_fresh or not math.isfinite(price) or price<=0
            or level<=0 or bid>=level or price>=level
            or not line_crossed_down(position,line_key,[],quote)):
        cross_samples.pop(key,None)
        return False,None
    sample=cross_samples.get(key)
    if (sample is None or provider_ms<=sample['provider_ms']
            or provider_ms-sample['provider_ms']>30_000 or price>sample['price']):
        cross_samples[key]={'provider_ms':provider_ms,'bid':bid,'price':price}
        return False,None
    if provider_ms-sample['provider_ms']<500:
        return False,None
    evidence={'line':line_key,'line_price':level,'first_bid':sample['bid'],'first_trade_price':sample['price'],
        'first_provider_ms':sample['provider_ms'],'confirming_bid':bid,
        'confirming_trade_price':price,'confirming_provider_ms':provider_ms,'distinct_updates':2,
        'confirmation':'fresh traded price and executable bid stayed below line on a second update without rebounding'}
    cross_samples.pop(key,None)
    return True,evidence

def market_trade_below_line(market_quote,level,now_ms=None):
    """A fresh Moomoo last trade must still confirm a line at submission time."""
    if not market_quote or level is None:return False
    values=market_quote.get('values') or {}
    try:price=float(values.get('last_price') or 0);provider_ms=int(market_quote.get('provider_ms') or 0)
    except (TypeError,ValueError):return False
    now_ms=now_ms or time.time_ns()//1_000_000
    received_ms=int(market_quote.get('received_ms') or 0)
    return (math.isfinite(price) and price>0 and provider_ms>0
        and 0<=now_ms-provider_ms<=MAX_EXIT_PROVIDER_AGE_MS
        and 0<=now_ms-received_ms<=MAX_EXIT_PROVIDER_AGE_MS and price<float(level))

def liquidity_trap_overlay(symbol,position,quote,bars):
    """Return the audited ThomasAi liquidity-trap result, and only that exit type.

    Other dynamic-exit outcomes are deliberately ignored so the existing
    high-water/TP/midpoint/stop strategy remains authoritative.
    """
    if quote is None or len(bars)<5:return None
    bid=float(getattr(quote,'bid',0) or 0);ask=float(getattr(quote,'ask',0) or 0)
    entry=float(position.get('entry') or position.get('entry_price') or 0)
    high=float(position.get('high_water_price') or position.get('high_price') or entry)
    stop=float(position.get('protective_stop') or position.get('stop_price') or 0)
    target=float(position.get('target_price') or position.get('target_70_price') or 0)
    if bid<=0 or entry<=0:return None
    result=evaluate_dynamic_exit(symbol,entry,high,bid,stop,target,bars=bars,
        moomoo_telemetry={'bid':bid,'ask':ask,'source':'codex_exit_worker_moomoo'})
    reason=str(result.get('exit_reason') or '')
    return result if result.get('should_exit') and reason.startswith('thomasai_liquidity_trap_exit') else None

def main():
    import argparse
    import fcntl
    p=argparse.ArgumentParser();p.parse_args()
    os.chdir(Path(__file__).resolve().parents[2])
    lock_path=DATA/'codex_exit_worker.lock';lock_path.parent.mkdir(parents=True,exist_ok=True)
    worker_lock=lock_path.open('a+')
    try:fcntl.flock(worker_lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
    except BlockingIOError:raise RuntimeError('another Codex exit worker already owns execution')
    if not exclusive_mode():
        raise RuntimeError('Codex exit ownership is not explicitly installed; refusing to poll or submit exits')
    broker=Broker()
    controller=PaperExitController(broker,LEDGER)
    broker.subscribe_market_context()
    last_model_bar={}
    risk_guard_retry_ms={}
    cross_confirmation_samples={}
    invalid_deferred_reported=set()
    last_fill_reconcile_ms={}
    last_exit_order_maintenance=0
    while True:
        try:
            local=dt.datetime.now(NY)
            if not extended_session(local):
                emit('codex_exit_idle',{'reason':'outside US extended session'});time.sleep(30);continue
            # Persistent submission records survive worker restarts.
            from . import telemetry
            if telemetry.DEFAULT_DB.exists():
                with closing(sqlite3.connect(f'file:{telemetry.DEFAULT_DB}?mode=ro',uri=True)) as db:
                    submissions=[json.loads(r[0]) for r in db.execute("SELECT payload_json FROM events WHERE kind IN ('codex_exit_submitted','qwen_exit_submitted') AND session=?",(local.date().isoformat(),))]
                for account in set(PAPER_ACCOUNTS.values()):
                    relevant={s['order_id']:s for s in submissions if s['account_id']==account and s.get('bot') in PAPER_ACCOUNTS}
                    now_mono=time.monotonic()*1000
                    if relevant and now_mono-last_fill_reconcile_ms.get(account,0)>=15_000:
                        last_fill_reconcile_ms[account]=now_mono
                        try:
                            for fill in broker.filled_exit_orders(account):
                                submission=relevant.get(fill['order_id'])
                                if not submission:continue
                                observed_ms=time.time_ns()//1000000
                                if submission.get('bot')=='gainer':record_gainer_closed_trade(submission,fill,observed_ms)
                                controller.confirm_fill(submission,fill)
                        except Exception as exc:
                            # Fill/journal reconciliation must never suspend live
                            # position protection or Codex exit reviews.
                            emit('codex_fill_reconciliation_deferred',{'account_id':account,
                                'reason':repr(exc),'pending_order_ids':list(relevant)},actor='shared_paper_account')
            positions_by_bot={bot:broker.positions(bot) for bot in PAPER_ACCOUNTS}
            maintenance_ms=time.monotonic()*1000
            if (extended_session(local) and any(positions_by_bot.values())
                    and maintenance_ms-last_exit_order_maintenance>=15_000):
                broker.maintain_exit_orders(positions_by_bot,local)
                last_exit_order_maintenance=maintenance_ms
            publish_chart_exit_levels(positions_by_bot)
            broker.sync_position_subscriptions(positions_by_bot)
            restart_if_memory_high()
            # Subscribe every holding before the deterministic risk pass so the
            # protective stop can be checked from the existing quote stream.
            # This pre-pass runs before any potentially slow Codex invocation.
            for bot in PAPER_ACCOUNTS:
                for position in positions_by_bot[bot]:
                    try:broker.subscribe_live(position['symbol'])
                    except Exception as exc:emit('codex_quote_subscription_error',{'bot':bot,'symbol':position['symbol'],'reason':repr(exc)})
            time.sleep(.15)
            now_guard=time.time_ns()//1000000
            deferred=pending_deferred_exits()
            deferred_attempted=set()
            for bot in PAPER_ACCOUNTS:
                for position in positions_by_bot[bot]:
                    symbol=position['symbol'];pushed=broker.push_quotes.get(symbol)
                    session_due=session_flatten_due(dt.datetime.fromtimestamp(now_guard/1000,NY))
                    if pushed is not None and not provider_quote_is_fresh(pushed,now_guard):
                        emit('codex_exit_push_quote_stale',{'bot':bot,'symbol':symbol,
                            'provider_ms':pushed.provider_ms,'received_ms':pushed.received_ms,
                            'max_provider_age_ms':MAX_EXIT_PROVIDER_AGE_MS},actor=bot)
                        pushed=None
                    if pushed is not None and not session_due and positive_executable_bid(pushed) is None:
                        emit('codex_exit_invalid_bid_ignored',{'bot':bot,'symbol':symbol,
                            'bid':getattr(pushed,'bid',None),'received_ms':pushed.received_ms,
                            'reason':'zero or non-finite bid cannot trigger a price-line exit'},actor=bot)
                        pushed=None
                    deferred_intent=deferred.get((bot,symbol,int(position['opened_ms'])))
                    if deferred_intent and not valid_deferred_trigger(deferred_intent):
                        deferred_key=(bot,symbol,int(position['opened_ms']))
                        if deferred_key not in invalid_deferred_reported:
                            emit('codex_exit_invalid_deferred_trigger_ignored',{
                                'bot':bot,'symbol':symbol,'opened_ms':position['opened_ms'],
                                'signal':deferred_intent.get('signal'),'trigger_bid':deferred_intent.get('trigger_bid'),
                                'reason':'legacy deferred price trigger has no finite positive bid'},actor=bot)
                            invalid_deferred_reported.add(deferred_key)
                        deferred_intent=None
                    if deferred_intent and extended_session(dt.datetime.now(NY)):
                        key=(bot,symbol,int(position['opened_ms']))
                        if now_guard-risk_guard_retry_ms.get(key,0)<10_000:
                            continue
                        risk_guard_retry_ms[key]=now_guard
                        if key not in deferred_attempted:
                            deferred_attempted.add(key)
                            try:
                                quote=broker.quote(symbol,force_refresh=True)
                                state=build_market_brief(symbol,position,quote,[],broker.market_context(),now_guard,
                                    'deferred premarket trigger; current-session candles are supplemental',strategy_context={
                                        'deferred_premarket_exit':deferred_intent,'protective_stop':position.get('protective_stop') or position.get('stop_price'),
                                        'target_70_price':position.get('target_70_price'),'target_100_price':position.get('target_price')})
                                observation=online_exit.observe(LEDGER,bot,symbol,position['opened_ms'],now_guard,state,policy_id='deferred-premarket-exit-v1')
                                broker.decision_id=observation
                                answer={'action':'EXIT','signal':deferred_intent['signal'],'p_up':0.0,'horizon_seconds':300,
                                    'reason':'Execute the latched exit using the currently supported Competition session: '+str(deferred_intent.get('reason','price-line exit trigger'))}
                                metadata={'backend':'latched_outside_rth_exit_execution','model_call_skipped':True,
                                    'original_trigger':deferred_intent,'executed_at_ms':now_guard}
                                forced_check=lambda exit_signal,code,current_quote,current_position=None: {'condition':True,
                                    'evidence':{'latched_premarket_trigger':deferred_intent,'execution_quote_bid':getattr(current_quote,'bid',None),
                                        'execution_quote_ask':getattr(current_quote,'ask',None),'rth_execution':True}}
                                result=controller.execute(bot,symbol,position['opened_ms'],observation,now_guard,answer,forced_check,
                                    decision_metadata=metadata)
                                emit('codex_live_decision_result',dict(result,bot=bot,symbol=symbol,
                                    opened_ms=position['opened_ms'],observation_id=observation,decision_backend=metadata),actor=bot,correlation_id=observation)
                            except Exception as exc:
                                emit('codex_deferred_exit_attempt_error',{'bot':bot,'symbol':symbol,'reason':repr(exc),
                                    'deferred_trigger':deferred_intent},actor=bot)
                        continue
                    stop_level=position.get('protective_stop') if bot=='gainer' else position.get('stop_price')
                    stop_key='protective_stop' if bot=='gainer' else 'stop_price'
                    market_quote=broker.market_quotes.get(symbol)
                    target100_level=float(position.get('target_price') or 0)
                    pending_target100=market_trade_at_or_above_line(market_quote,target100_level,now_guard)
                    target100_trigger_quote=market_quote
                    if has_pending_line_cross(position,pushed,bot) or pending_target100:
                        market_quote=broker.exit_trade_quote(symbol)
                    target=float(position.get('target_70_price') or 0)
                    stop_hit,stop_confirmation=confirmed_line_cross(position,stop_key,pushed,bot,cross_confirmation_samples,market_quote)
                    target100_hit=(pending_target100
                        or target_100_touched_up(position,pushed,market_quote,now_guard))
                    target100_evidence_quote=target100_trigger_quote if pending_target100 else market_quote
                    target100_confirmation=({'line':'target_price','line_price':target100_level,
                        'trade_price':(target100_evidence_quote.get('values') or {}).get('last_price') if target100_evidence_quote else None,
                        'provider_ms':target100_evidence_quote.get('provider_ms') if target100_evidence_quote else None,
                        'confirmation':'fresh Moomoo trade touched or crossed up through 100% target'} if target100_hit else None)
                    target_hit,target70_confirmation=confirmed_line_cross(position,'target_70_price',pushed,bot,cross_confirmation_samples,market_quote)
                    retrace_hit,retrace_confirmation=confirmed_line_cross(position,'profit_retrace_price',pushed,bot,cross_confirmation_samples,market_quote)
                    midpoint_hit,midpoint_confirmation=confirmed_line_cross(position,'profit_midpoint_price',pushed,bot,cross_confirmation_samples,market_quote)
                    signal=('session_flatten' if session_due else ('protective_stop' if stop_hit else ('profit_target_100' if target100_hit else ('profit_target' if target_hit else ('profit_retrace_failure' if retrace_hit else ('midpoint_failure' if midpoint_hit else None))))))
                    if not signal or pushed is None or (signal!='session_flatten' and now_guard-pushed.received_ms>1_000):
                        continue
                    risk_key=(bot,symbol)
                    if now_guard-risk_guard_retry_ms.get(risk_key,0)<10_000:continue
                    risk_guard_retry_ms[risk_key]=now_guard
                    setup=(gainer_position_decision_context(position,pushed) if bot=='gainer' else {'strategy':'opening_range_breakout','deterministic_levels':True,'target_70_price':target,'protective_stop':stop_level,'profit_midpoint_price':position.get('profit_midpoint_price')})
                    bars,missing=read_moomoo_closed_minutes(DATA/'common1m.sqlite3',symbol,now_guard)
                    state=build_market_brief(symbol,position,pushed,bars,broker.market_context(),now_guard,missing,strategy_context=setup)
                    observation=online_exit.observe(LEDGER,bot,symbol,position['opened_ms'],now_guard,state,policy_id='deterministic-price-exit-v1')
                    reason=(f"Mandatory 15:00 ET session flatten; executable bid ${pushed.bid:.4f}." if signal=='session_flatten' else
                        (f"Executable bid ${pushed.bid:.4f} crossed down through stop ${float(stop_level):.4f}." if signal=='protective_stop' else
                        (f"Fresh Moomoo trade touched or crossed up through 100% line ${float(position.get('target_price') or 0):.4f}; exit at the current executable price." if signal=='profit_target_100' else
                        (f"Executable bid ${pushed.bid:.4f} crossed down through 70% line ${target:.4f}." if signal=='profit_target' else
                        (f"Executable bid ${pushed.bid:.4f} crossed down through intermediate line ${float(position.get('profit_retrace_price') or 0):.4f}." if signal=='profit_retrace_failure' else f"Executable bid ${pushed.bid:.4f} crossed down through midpoint ${float(position.get('profit_midpoint_price') or 0):.4f}.")))))
                    answer={'action':'EXIT','signal':signal,'p_up':0.0,'horizon_seconds':300,'reason':reason}
                    confirmation=({'protective_stop':stop_confirmation,'profit_target_100':target100_confirmation,
                        'profit_target':target70_confirmation,'profit_retrace_failure':retrace_confirmation,
                        'midpoint_failure':midpoint_confirmation}.get(signal))
                    metadata={'backend':'deterministic_price_level_or_session_flatten','model_call_skipped':True,
                        'risk_context':setup,'trigger_bid':pushed.bid,'triggered_ms':now_guard,
                        'cross_confirmation':confirmation}
                    emit('deterministic_exit_triggered',{'bot':bot,'symbol':symbol,'account_id':PAPER_ACCOUNTS[bot],
                        'signal':signal,'position':position,'bid':pushed.bid,'protective_stop':stop_level,
                        'target_100_price':position.get('target_price'),'target_70_price':target,
                        'profit_retrace_price':position.get('profit_retrace_price'),
                        'profit_midpoint_price':position.get('profit_midpoint_price'),'reason':answer['reason'],
                        'cross_confirmation':confirmation},actor=bot,correlation_id=observation)
                    if signal=='profit_target_100' or not regular_session(dt.datetime.now(NY)):
                        record_latched_exit(bot,position,signal,answer['reason'],{
                            'trigger_bid':pushed.bid,'triggered_ms':now_guard,'stop_price':stop_level,
                            'target_100_price':position.get('target_price'),'target_70_price':target,
                            'intermediate_price':position.get('profit_retrace_price'),'midpoint_price':position.get('profit_midpoint_price')})
                    broker.decision_id=observation
                    check=lambda exit_signal,code,current_quote,current_position=None: recheck(exit_signal,code,current_quote,current_position,broker,bot)
                    try:
                        result=controller.execute(bot,symbol,position['opened_ms'],observation,now_guard,
                            answer,check,decision_metadata=metadata)
                        emit('codex_live_decision_result',dict(result,bot=bot,symbol=symbol,
                            opened_ms=position['opened_ms'],observation_id=observation,
                            decision_backend=metadata),actor=bot,correlation_id=observation)
                    except Exception as exc:
                        emit('codex_decision_deferred',{'bot':bot,'symbol':symbol,'reason':repr(exc),
                            'risk_guard':metadata},correlation_id=observation)
            codex_call_used=False
            for bot in PAPER_ACCOUNTS:
                for position in positions_by_bot[bot]:
                    symbol=position['symbol'];now=time.time_ns()//1000000
                    try:quote=broker.quote(symbol,force_refresh=True)
                    except Exception as exc:
                        quote=None
                        emit('codex_quote_unavailable',{'bot':bot,'symbol':symbol,'reason':repr(exc)})
                    session_due=session_flatten_due(dt.datetime.fromtimestamp(now/1000,NY))
                    if not session_due and (quote is None or not provider_quote_is_fresh(quote,now)
                                            or positive_executable_bid(quote) is None):
                        emit('codex_exit_fresh_quote_required',{'bot':bot,'symbol':symbol,
                            'provider_ms':getattr(quote,'provider_ms',None),
                            'received_ms':getattr(quote,'received_ms',None),
                            'max_provider_age_ms':MAX_EXIT_PROVIDER_AGE_MS,
                            'bid':getattr(quote,'bid',None),
                            'reason':'no fresh finite positive executable bid for a new price-line trigger'},actor=bot)
                        continue
                    bars,missing=read_moomoo_closed_minutes(DATA/'common1m.sqlite3',symbol,now)
                    setup=(orb_setup_context(symbol,position['opened_ms'],bars[-1][4] if bars else None,position.get('entry'))
                           if bot=='orb' else gainer_position_decision_context(position,quote))
                    state=build_market_brief(symbol,position,quote,bars,broker.market_context(),now,missing,
                        strategy_context=setup)
                    # Enforce all ratcheting chart levels from the executable bid.
                    stop_level=position.get('protective_stop') if bot=='gainer' else position.get('stop_price')
                    stop_key='protective_stop' if bot=='gainer' else 'stop_price'
                    market_quote=broker.market_quotes.get(symbol)
                    target100_level=float(position.get('target_price') or 0)
                    pending_target100=market_trade_at_or_above_line(market_quote,target100_level,now)
                    target100_trigger_quote=market_quote
                    if has_pending_line_cross(position,quote,bot) or pending_target100:
                        market_quote=broker.exit_trade_quote(symbol)
                    stop_hit,stop_confirmation=confirmed_line_cross(position,stop_key,quote,bot,cross_confirmation_samples,market_quote)
                    target70=float(position.get('target_70_price') or 0)
                    target100_break=(pending_target100
                        or target_100_touched_up(position,quote,market_quote,now))
                    target100_evidence_quote=target100_trigger_quote if pending_target100 else market_quote
                    target100_confirmation=({'line':'target_price','line_price':target100_level,
                        'trade_price':(target100_evidence_quote.get('values') or {}).get('last_price') if target100_evidence_quote else None,
                        'provider_ms':target100_evidence_quote.get('provider_ms') if target100_evidence_quote else None,
                        'confirmation':'fresh Moomoo trade touched or crossed up through 100% target'} if target100_break else None)
                    target70_break,target70_confirmation=confirmed_line_cross(position,'target_70_price',quote,bot,cross_confirmation_samples,market_quote)
                    retrace_break,retrace_confirmation=confirmed_line_cross(position,'profit_retrace_price',quote,bot,cross_confirmation_samples,market_quote)
                    midpoint_break,midpoint_confirmation=confirmed_line_cross(position,'profit_midpoint_price',quote,bot,cross_confirmation_samples,market_quote)
                    trap=liquidity_trap_overlay(symbol,position,quote,bars) if bot in ('gainer','orb') else None
                    automatic_signal=('session_flatten' if session_due else ('protective_stop' if stop_hit else
                        ('profit_target_100' if target100_break else ('profit_target' if target70_break else ('profit_retrace_failure' if retrace_break else ('midpoint_failure' if midpoint_break else
                        ('liquidity_trap' if trap else None)))))))
                    if bot in ('gainer','orb') and automatic_signal:
                        risk_key=(bot,symbol)
                        if now-risk_guard_retry_ms.get(risk_key,0)<10_000:continue
                        risk_guard_retry_ms[risk_key]=now
                        confirmation=({'protective_stop':stop_confirmation,'profit_target_100':target100_confirmation,
                            'profit_target':target70_confirmation,'profit_retrace_failure':retrace_confirmation,
                            'midpoint_failure':midpoint_confirmation}.get(automatic_signal))
                        observation=online_exit.observe(LEDGER,bot,symbol,position['opened_ms'],now,state,policy_id='deterministic-price-exit-v1')
                        if automatic_signal=='protective_stop':
                            reason=f"Executable bid ${quote.bid:.4f} crossed protective stop ${float(stop_level):.4f}."
                            backend='deterministic_position_stop';event_kind='protective_stop_triggered'
                        elif automatic_signal=='profit_target':
                            reason=f"Executable bid ${quote.bid:.4f} crossed down through the ratcheting 70% line ${target70:.4f}."
                            backend='deterministic_70pct_take_profit';event_kind='profit_target_70_touched'
                        elif automatic_signal=='profit_target_100':
                            reason=f"Fresh Moomoo trade touched or crossed up through the ratcheting 100% line ${float(position.get('target_price') or 0):.4f}; exit at the current executable price."
                            backend='deterministic_100pct_target';event_kind='profit_target_100_touched_or_crossed_up'
                        elif automatic_signal=='profit_retrace_failure':
                            reason=f"Executable bid ${quote.bid:.4f} crossed down through the intermediate line ${float(position.get('profit_retrace_price') or 0):.4f}."
                            backend='deterministic_profit_retrace';event_kind='profit_retrace_crossed_down'
                        elif automatic_signal=='midpoint_failure':
                            reason=f"Executable bid ${quote.bid:.4f} crossed below the profit midpoint ${position['profit_midpoint_price']:.4f}."
                            backend='deterministic_profit_midpoint';event_kind='profit_midpoint_crossed_down'
                        elif automatic_signal=='liquidity_trap':
                            reason=trap['exit_reason']
                            backend='thomasai_liquidity_trap_exit';event_kind='thomasai_liquidity_trap_exit'
                        else:
                            reason=f"Mandatory 15:00 ET session flatten; executable bid ${quote.bid:.4f}."
                            backend='deterministic_session_flatten';event_kind='session_flatten_triggered'
                        answer={'action':'EXIT','signal':automatic_signal,'p_up':0.0,'horizon_seconds':300,'reason':reason}
                        metadata={'backend':backend,'model_call_skipped':True,'risk_context':setup,
                            'trigger_bid':quote.bid,'triggered_ms':now,'target_100_price':position.get('target_price'),
                            'target_70_price':target70 or None,'profit_retrace_price':position.get('profit_retrace_price'),
                            'protective_stop':stop_level,'profit_midpoint_price':position.get('profit_midpoint_price'),
                            'liquidity_trap':trap,'cross_confirmation':confirmation}
                        emit(event_kind,{'bot':bot,'symbol':symbol,'account_id':PAPER_ACCOUNTS[bot],
                            'position':position,'bid':quote.bid,'protective_stop':stop_level,
                            'target_100_price':position.get('target_price'),'target_70_price':target70 or None,
                            'profit_retrace_price':position.get('profit_retrace_price'),'profit_midpoint_price':position.get('profit_midpoint_price'),
                            'reason':reason,'cross_confirmation':confirmation},actor=bot,correlation_id=observation)
                        if automatic_signal=='profit_target_100' or not regular_session(dt.datetime.now(NY)):
                            record_latched_exit(bot,position,automatic_signal,reason,{
                                'trigger_bid':quote.bid,'triggered_ms':now,'protective_stop':stop_level,
                                'target_100_price':position.get('target_price'),'target_70_price':target70 or None,
                                'intermediate_price':position.get('profit_retrace_price'),'midpoint_price':position.get('profit_midpoint_price')})
                        broker.decision_id=observation
                        check=lambda exit_signal,code,current_quote,current_position=None: recheck(
                            exit_signal,code,current_quote,current_position,broker,bot)
                        result=controller.execute(bot,symbol,position['opened_ms'],observation,now,answer,check,
                            decision_metadata=metadata)
                        emit('codex_live_decision_result',dict(result,bot=bot,symbol=symbol,
                            opened_ms=position['opened_ms'],observation_id=observation,
                            decision_backend=metadata),actor=bot,correlation_id=observation)
                        continue
                    if bot in ('gainer','orb'):
                        emit('discretionary_review_skipped',{'bot':bot,'symbol':symbol,
                            'reason':'100% target touch/up-cross; down-crosses of 70%, intermediate, midpoint, and stop lines; audited ThomasAi liquidity traps; or the 15:00 ET session flatten'},actor=bot)
                        continue
                    if len(bars)<5:
                        emit('codex_decision_deferred',{'bot':bot,'symbol':symbol,
                            'reason':missing or 'fewer than five current-session closed Moomoo candles'})
                        continue
                    if bot=='gainer':
                        bar_ms=int(bars[-1][0])
                        if last_model_bar.get(symbol)==bar_ms:continue
                        reasons=gainer_codex_review_reasons(state)
                        if not reasons:
                            last_model_bar[symbol]=bar_ms
                            emit('codex_exit_review_skipped',{'bot':bot,'symbol':symbol,
                                'bar_ms':bar_ms,'reason':'no current risk, trend deterioration, or material profit giveback'},
                                actor=bot)
                            continue
                        if codex_call_used:continue
                        last_model_bar[symbol]=bar_ms
                        codex_call_used=True
                        state['review_trigger_reasons']=reasons
                    observation=online_exit.observe(LEDGER,bot,symbol,position['opened_ms'],now,state,policy_id='codex-live-exit-v1')
                    try:
                        answer,metadata=ask(bot,state,observation);broker.decision_id=observation
                        check=lambda signal,code,current_quote,current_position=None: recheck(
                            signal,code,current_quote,current_position,broker,bot)
                        # Let the controller use its own post-inference clock;
                        # passing the pre-inference `now` would freeze quote
                        # freshness and the model's decision horizon.
                        result=controller.execute(bot,symbol,position['opened_ms'],observation,now,answer,check,
                            decision_metadata=metadata)
                        emit('codex_live_decision_result',dict(result,bot=bot,symbol=symbol,
                            opened_ms=position['opened_ms'],observation_id=observation,
                            decision_backend=metadata),actor=bot,correlation_id=observation)
                    except Exception as exc:emit('codex_decision_deferred',{'bot':bot,'symbol':symbol,
                        'reason':repr(exc),'decision_call_evidence':getattr(exc,'evidence',{})},correlation_id=observation)
            time.sleep(2)
        except Exception as exc:
            emit('codex_exit_worker_error',{'error':repr(exc)});time.sleep(5)

if __name__=='__main__':main()
