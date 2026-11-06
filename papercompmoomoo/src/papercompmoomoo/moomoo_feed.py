"""Moomoo's own live data for the few symbols the bot may trade, fetched sparingly so Moomoo's limits are never reached.

* Snapshots (``get_market_snapshot``): last price, bid, ask, previous close, today's volume; no subscription, one call for up to 400 codes, 0.35 s. Used for the live
  price and percent of the watch list, for the held positions, and as the check just before an order.
* Live 1-minute candles (``get_cur_kline``): needs a subscription (100 units for the whole account). Only identified candidates and open positions are subscribed,
  read as needed, and released after Moomoo's one-minute minimum. Live decisions read native Moomoo bars only; legacy bars from another source are excluded.
* Historical bulk imports are not part of live strategy ingestion; request native candles only for identified movers and held positions.

Moomoo labels a 1-minute bar by its END time. Preserve the provider label when recording evidence and account for the interval end when displaying it in charts.
"""

from __future__ import annotations

import logging
import time
import math
from datetime import datetime
from futu import AuType, KLType, ModifyUserSecurityOp, RET_OK, Session, SubType
from zoneinfo import ZoneInfo

LOGGER = logging.getLogger(__name__)
NEW_YORK = "America/New_York"
MIN_HOLD_SECONDS = 65  # Moomoo refuses to unsubscribe within a minute of subscribing
WATCHLIST_GROUP = "MooComp"  # the owner's custom watchlist group in the Moomoo app; only this group is ever changed, and it is never created
WATCHLIST_MAX = 100
RESERVE = 10  # subscription units always left free for other users of the account
LIVE_QUOTE_LIMIT = 12
LIVE_QUOTE_MAX_AGE_MS = 30_000
NY = ZoneInfo(NEW_YORK)

def provider_time_ms(value):
    if not value:return None
    try:
        stamp=value if isinstance(value,datetime) else datetime.fromisoformat(str(value))
        if stamp.tzinfo is None:stamp=stamp.replace(tzinfo=NY)
        return int(stamp.timestamp()*1000)
    except (TypeError,ValueError,OverflowError):return None

def finite_number(value):
    try:
        number=float(value)
        return number if math.isfinite(number) else 0.0
    except (TypeError,ValueError):return 0.0


def bars_from_kline(kline, minutes: int = 15):
    """Native OHLCV in New York time. Legacy 1m conversion keeps its old label
    convention; the gainer preserves the 1m time_key and waits a full minute
    before using that row, so a forming candle cannot be admitted early.
    """
    import pandas as pd

    if kline is None or len(kline) == 0:
        return pd.DataFrame(columns=["Open", "High", "Low", "Close", "Volume"])
    ends = pd.to_datetime(kline["time_key"]).dt.tz_localize(NEW_YORK, ambiguous="NaT", nonexistent="NaT")
    frame = pd.DataFrame({"Open": kline["open"].to_numpy(), "High": kline["high"].to_numpy(), "Low": kline["low"].to_numpy(),
                          "Close": kline["close"].to_numpy(), "Volume": kline["volume"].to_numpy()}, index=ends if minutes == 1 else ends - pd.Timedelta(minutes=minutes))
    frame.index.name = "Datetime"
    return frame[frame.index.notna()]


class MoomooFeed:
    def __init__(self, quote_context, max_subscriptions: int = 25, minutes: int = 1) -> None:
        if minutes != 1:
            raise ValueError("only native 1-minute candles are supported")
        self.minutes = minutes
        if minutes != 1:
            raise ValueError("the active gainer system accepts native 1-minute candles only")
        self.k_type = KLType.K_1M
        self.sub_type = SubType.K_1M
        from .telemetry import RecordedContext
        self.q = RecordedContext(quote_context, 'quote_client')
        self.max_subscriptions = max_subscriptions
        self.subscribed: dict[str, float] = {}  # code -> monotonic time it was subscribed
        self.live_subscribed: dict[str,float]={}
        self.live_quote_push: dict[str,dict]={}
        self.live_book_push: dict[str,dict]={}
        self.push_recorded_ms: dict[str,int]={}
        self.last_live_refresh_ms: dict[str,int]={}
        self._install_live_handlers()

    def _install_live_handlers(self):
        """Keep candidate quote and executable book data flowing without polling."""
        from futu import OrderBookHandlerBase,StockQuoteHandlerBase
        from .telemetry import emit
        owner=self
        def receive_quote(self,rsp_pb):
            ret,data=StockQuoteHandlerBase.on_recv_rsp(self,rsp_pb)
            if ret==RET_OK and data is not None and not data.empty:
                received=int(time.time()*1000)
                for _,row in data.iterrows():
                    code=str(row.get('code',''))
                    date=str(row.get('data_date','') or '')
                    clock=str(row.get('data_time','') or '')
                    stamp=f'{date} {clock}'.strip()
                    provider=provider_time_ms(stamp)
                    quote={'last':finite_number(row.get('last_price')),
                        'pre_price':finite_number(row.get('pre_price')),
                        'prev_close':finite_number(row.get('prev_close_price')),
                        'volume':finite_number(row.get('volume')),'time':stamp,
                        '_provider_ms':provider,'_received_ms':received}
                    if code and provider is not None and quote['last']>0:
                        old=owner.live_quote_push.get(code)
                        if old is None or provider>=int(old.get('_provider_ms') or 0):
                            owner.live_quote_push[code]=quote
                            if received-owner.push_recorded_ms.get(code,0)>=5_000:
                                owner.push_recorded_ms[code]=received
                                emit('candidate_live_quote_push',{'symbol':code,'received_ms':received,
                                    'provider_ms':provider,'last':quote['last'],'pre_price':quote['pre_price'],
                                    'volume':quote['volume'],'provider_time':stamp},actor='quote_client')
            return ret,data
        def receive_book(self,rsp_pb):
            ret,book=OrderBookHandlerBase.on_recv_rsp(self,rsp_pb)
            if ret==RET_OK and isinstance(book,dict):
                code=str(book.get('code',''))
                bids=book.get('Bid') or [];asks=book.get('Ask') or []
                bid=finite_number(bids[0][0]) if bids else 0.0
                ask=finite_number(asks[0][0]) if asks else 0.0
                bid_time=str(book.get('svr_recv_time_bid') or '')
                ask_time=str(book.get('svr_recv_time_ask') or '')
                bid_ms=provider_time_ms(bid_time);ask_ms=provider_time_ms(ask_time)
                if rsp_pb.HasField('s2c'):
                    body=rsp_pb.s2c
                    if body.HasField('svrRecvTimeBidTimestamp'):
                        v=float(body.svrRecvTimeBidTimestamp);bid_ms=int(v*1000 if v<10_000_000_000 else v)
                    if body.HasField('svrRecvTimeAskTimestamp'):
                        v=float(body.svrRecvTimeAskTimestamp);ask_ms=int(v*1000 if v<10_000_000_000 else v)
                received=int(time.time()*1000)
                if code and 0<bid<=ask and bid_ms is not None and ask_ms is not None:
                    old=owner.live_book_push.get(code)
                    if old is None or min(bid_ms,ask_ms)>=min(int(old.get('bid_provider_ms') or 0),int(old.get('ask_provider_ms') or 0)):
                        owner.live_book_push[code]={'bid':bid,'ask':ask,'bid_provider_ms':bid_ms,
                            'ask_provider_ms':ask_ms,'received_ms':received}
            return ret,book
        self.q.set_handler(type('CandidateQuoteHandler',(StockQuoteHandlerBase,),{'on_recv_rsp':receive_quote})())
        self.q.set_handler(type('CandidateOrderBookHandler',(OrderBookHandlerBase,),{'on_recv_rsp':receive_book})())

    # ---- subscriptions ----------------------------------------------------------------------------------------------------------------------------
    def ensure(self, codes: list[str], push: bool = False) -> set[str]:
        """Subscribe the codes not yet subscribed (within both caps) and return the codes whose candles can be read. ``push`` also makes OpenD push every candle update to this
        connection's handler for identified candidates."""
        wanted = [c for c in dict.fromkeys(codes) if c not in self.subscribed]
        if not wanted:
            return set(c for c in codes if c in self.subscribed)
        room = self.max_subscriptions - len(self.subscribed)
        ret, sub = self.q.query_subscription()
        if ret == RET_OK:
            room = min(room, int(sub.get("remain", 0)) - RESERVE)
        wanted = wanted[: max(0, room)]
        if wanted:
            ret, msg = self.q.subscribe(wanted, [self.sub_type], subscribe_push=push, session=Session.ALL)
            if ret == RET_OK:
                now = time.monotonic()
                self.subscribed.update({c: now for c in wanted})
            else:
                LOGGER.warning("Moomoo batch subscription failed (%s); subscribing individually", str(msg)[:120])
                now = time.monotonic()
                for c in wanted:
                    r_one, _ = self.q.subscribe([c], [self.sub_type], subscribe_push=push, session=Session.ALL)
                    if r_one == RET_OK:
                        self.subscribed[c] = now
        return set(c for c in codes if c in self.subscribed)

    def ensure_live(self,codes: list[str]) -> set[str]:
        """Subscribe a small set of identified movers to pushed quote and book data."""
        wanted=[c for c in dict.fromkeys(codes) if c not in self.live_subscribed]
        if not wanted:return set(self.live_subscribed)
        wanted=wanted[:max(0,LIVE_QUOTE_LIMIT-len(self.live_subscribed))]
        if not wanted:return set(self.live_subscribed)
        ret,sub=self.q.query_subscription()
        if ret!=RET_OK:
            LOGGER.warning('Moomoo subscription quota unreadable; live candidate quotes deferred: %s',str(sub)[:120])
            return set(self.live_subscribed)
        room=max(0,int(sub.get('remain',0))-RESERVE)
        batch=[];units=0
        for code in wanted:
            need=2
            if units+need>room:break
            batch.append(code);units+=need
        if not batch:return set(self.live_subscribed)
        ret,msg=self.q.subscribe(batch,[SubType.QUOTE,SubType.ORDER_BOOK],subscribe_push=True,session=Session.ALL)
        if ret!=RET_OK:
            LOGGER.warning('Moomoo live candidate subscription failed: %s',str(msg)[:160])
            return set(self.live_subscribed)
        now=time.monotonic()
        self.live_subscribed.update({code:now for code in batch})
        from .telemetry import emit
        emit('candidate_live_subscription',{'symbols':batch,'subtypes':['QUOTE','ORDER_BOOK'],
            'remaining_quota_before':int(sub.get('remain',0)),'quota_reserve':RESERVE},actor='quote_client')
        return set(self.live_subscribed)

    def release_live_unused(self,keep: set[str]) -> int:
        stale=[code for code,started in self.live_subscribed.items()
            if code not in keep and time.monotonic()-started>=MIN_HOLD_SECONDS]
        if not stale:return 0
        ret,msg=self.q.unsubscribe(stale,[SubType.QUOTE,SubType.ORDER_BOOK])
        if ret==RET_OK:
            for code in stale:
                self.live_subscribed.pop(code,None)
                self.live_quote_push.pop(code,None)
                self.live_book_push.pop(code,None)
            return len(stale)
        LOGGER.warning('Moomoo live candidate unsubscribe failed: %s',str(msg)[:120])
        return 0

    def refresh_live(self,codes: list[str],min_gap_ms: int=30_000) -> int:
        """Actively refresh stale subscribed candidates through Moomoo's live quote/book APIs."""
        now=int(time.time()*1000)
        eligible=[c for c in dict.fromkeys(codes) if c in self.live_subscribed
            and now-self.last_live_refresh_ms.get(c,0)>=min_gap_ms]
        if not eligible:return 0
        for code in eligible:self.last_live_refresh_ms[code]=now
        ret,rows=self.q.get_stock_quote(eligible)
        refreshed=0
        if ret==RET_OK and rows is not None and not rows.empty:
            for _,row in rows.iterrows():
                code=str(row.get('code',''));stamp=f"{row.get('data_date','')} {row.get('data_time','')}".strip()
                provider=provider_time_ms(stamp);last=finite_number(row.get('last_price'))
                if not code or provider is None or last<=0:continue
                quote={'last':last,'pre_price':finite_number(row.get('pre_price')),
                    'prev_close':finite_number(row.get('prev_close_price')),'volume':finite_number(row.get('volume')),
                    'time':stamp,'_provider_ms':provider,'_received_ms':int(time.time()*1000)}
                old=self.live_quote_push.get(code)
                if old is None or provider>=int(old.get('_provider_ms') or 0):
                    self.live_quote_push[code]=quote;refreshed+=1
        # get_order_book is per symbol; only stale, subscribed movers are read,
        # and each symbol is rate-limited above. Push callbacks remain primary.
        for code in eligible:
            ret,book=self.q.get_order_book(code,num=1)
            if ret!=RET_OK or not isinstance(book,dict):continue
            bids=book.get('Bid') or [];asks=book.get('Ask') or []
            bid=finite_number(bids[0][0]) if bids else 0.0
            ask=finite_number(asks[0][0]) if asks else 0.0
            bid_ms=provider_time_ms(book.get('svr_recv_time_bid'))
            ask_ms=provider_time_ms(book.get('svr_recv_time_ask'))
            if 0<bid<=ask and bid_ms is not None and ask_ms is not None:
                old=self.live_book_push.get(code)
                if old is None or min(bid_ms,ask_ms)>=min(int(old.get('bid_provider_ms') or 0),int(old.get('ask_provider_ms') or 0)):
                    self.live_book_push[code]={'bid':bid,'ask':ask,'bid_provider_ms':bid_ms,
                        'ask_provider_ms':ask_ms,'received_ms':int(time.time()*1000)}
        # The last-sale and book endpoints can both lag for a mover. Ask for a
        # provider-stamped batch market snapshot only for those same identified
        # stale symbols; never fan this recovery out to the full universe.
        now=int(time.time()*1000)
        fallback=[]
        for code in eligible:
            quote=self.live_quote_push.get(code) or {}
            book=self.live_book_push.get(code) or {}
            qms=int(quote.get('_provider_ms') or 0)
            bms=min(int(book.get('bid_provider_ms') or 0),int(book.get('ask_provider_ms') or 0))
            if not (0<=now-qms<=LIVE_QUOTE_MAX_AGE_MS and 0<bms and 0<=now-bms<=LIVE_QUOTE_MAX_AGE_MS):
                fallback.append(code)
        if fallback:
            ret,snap=self.q.get_market_snapshot(fallback)
            if ret==RET_OK and snap is not None and not snap.empty:
                from .telemetry import emit
                received=int(time.time()*1000)
                for _,row in snap.iterrows():
                    code=str(row.get('code',''));stamp=str(row.get('update_time','') or '')
                    provider=provider_time_ms(stamp)
                    last=finite_number(row.get('last_price'));bid=finite_number(row.get('bid_price'));ask=finite_number(row.get('ask_price'))
                    if code not in fallback or provider is None or not 0<=received-provider<=LIVE_QUOTE_MAX_AGE_MS:
                        continue
                    if not (last>0 and 0<bid<=ask):continue
                    quote={'last':last,'pre_price':finite_number(row.get('pre_price')),
                        'prev_close':finite_number(row.get('prev_close_price')),'volume':finite_number(row.get('volume')),
                        'bid':bid,'ask':ask,'time':stamp,'_provider_ms':provider,
                        '_received_ms':received,'_price_source':'fresh_market_snapshot'}
                    old=self.live_quote_push.get(code)
                    if old is None or provider>=int(old.get('_provider_ms') or 0):
                        self.live_quote_push[code]=quote
                        self.live_book_push[code]={'bid':bid,'ask':ask,'bid_provider_ms':provider,
                            'ask_provider_ms':provider,'received_ms':received,'source':'market_snapshot_bbo'}
                        refreshed+=1
                        emit('candidate_market_snapshot_recovery',{'symbol':code,'provider_ms':provider,
                            'received_ms':received,'last':last,'bid':bid,'ask':ask,'provider_time':stamp},actor='quote_client')
        return refreshed

    def live_snapshot(self,code: str,base: dict | None=None) -> dict | None:
        """Merge recent quote/book pushes into the bulk snapshot, checking provider ages."""
        result=dict(base or {})
        now=int(time.time()*1000)
        quote=self.live_quote_push.get(code)
        if quote:
            provider=int(quote.get('_provider_ms') or 0)
            if 0<=now-provider<=LIVE_QUOTE_MAX_AGE_MS:
                if not result.get('book_time'):result['book_time']=result.get('time')
                result.update({k:quote[k] for k in ('last','pre_price','prev_close','volume','time')})
                if quote.get('_price_source'):
                    result['_price_source']=quote['_price_source']
                result['_quote_provider_ms']=provider
                result['_quote_received_ms']=quote.get('_received_ms')
        book=self.live_book_push.get(code)
        if book:
            bid_ms=int(book.get('bid_provider_ms') or 0);ask_ms=int(book.get('ask_provider_ms') or 0)
            if 0<=now-bid_ms<=LIVE_QUOTE_MAX_AGE_MS and 0<=now-ask_ms<=LIVE_QUOTE_MAX_AGE_MS:
                result.update({'bid':book['bid'],'ask':book['ask'],
                    'book_time':datetime.fromtimestamp(min(bid_ms,ask_ms)/1000,NY).isoformat(),
                    '_book_provider_ms':min(bid_ms,ask_ms),'_book_received_ms':book.get('received_ms')})
                # A current two-sided book is usable recovery evidence when
                # Moomoo's last-sale field is stale. Use its midpoint as the
                # live reference while retaining the separately timestamped
                # bid/ask for execution and audit. Never relabel the old print.
                quote_provider=int((quote or {}).get('_provider_ms') or 0)
                if now-quote_provider>LIVE_QUOTE_MAX_AGE_MS:
                    result['last']=(float(book['bid'])+float(book['ask']))/2
                    result['time']=result['book_time']
                    result['_price_source']='fresh_order_book_midpoint'
                    result['_quote_provider_ms']=None
                    result['_quote_received_ms']=None
        return result or None

    def release_unused(self, keep: set[str]) -> int:
        """Unsubscribe codes nobody needs now, once they are older than Moomoo's one-minute minimum. Returns how many were released."""
        now = time.monotonic()
        stale = [c for c, t in self.subscribed.items() if c not in keep and now - t >= MIN_HOLD_SECONDS]
        if stale:
            ret, msg = self.q.unsubscribe(stale, [self.sub_type])
            if ret == RET_OK:
                for c in stale:
                    self.subscribed.pop(c, None)
                return len(stale)
            LOGGER.warning("Moomoo unsubscribe failed: %s", str(msg)[:120])
        return 0

    # ---- data ------------------------------------------------------------------------------------------------------------------------------------
    def candles(self, code: str):
        """Native bars at this feed's configured cadence, with raw prices and Moomoo volume, or None when unavailable."""
        ret, kline = self.q.get_cur_kline(code, num=100, ktype=self.k_type, autype=AuType.NONE)
        if ret != RET_OK:
            LOGGER.warning("Moomoo candles for %s unavailable: %s", code, str(kline)[:100])
            return None
        frame = bars_from_kline(kline, self.minutes)
        return frame if len(frame) else None

    def snapshots(self, codes: list[str]) -> dict[str, dict]:
        """Live quotes: ``{code: {last, bid, ask, prev_close, volume, volume_ratio, time}}`` (codes Moomoo does not answer for are missing)."""
        out: dict[str, dict] = {}
        for i in range(0, len(codes), 100):
            ret, snap = self.q.get_market_snapshot(codes[i:i + 100])
            if ret != RET_OK:
                LOGGER.warning("Moomoo snapshot failed: %s", str(snap)[:100])
                continue
            for _, r in snap.iterrows():
                pre_price = r.get("pre_price", 0.0)
                pre_price_val = float(pre_price) if pre_price == pre_price and pre_price is not None else 0.0
                out[str(r["code"])] = {"last": float(r["last_price"]), "pre_price": pre_price_val,
                                       "bid": float(r.get("bid_price", 0.0) or 0.0), "ask": float(r.get("ask_price", 0.0) or 0.0),
                                       "prev_close": float(r.get("prev_close_price", 0.0) or 0.0), "volume": float(r.get("volume", 0.0) or 0.0),
                                       "volume_ratio": float(r.get("volume_ratio", 0.0) or 0.0), "time": str(r.get("update_time", "")),
                                       "book_time": str(r.get("update_time", "")),
                                       "market_cap": float(r.get("total_market_val", 0.0) or 0.0)}
        return {code:(self.live_snapshot(code,quote) or quote) for code,quote in out.items()}

    def sync_watchlist(self, codes: list[str], group: str = WATCHLIST_GROUP, limit: int = WATCHLIST_MAX) -> tuple[list[str], list[str]] | None:
        """Make the Moomoo watchlist group hold exactly ``codes`` (the first ``limit``): add the new ones, remove the ones no longer candidates.
        Returns ``(added, removed)``, or None when the group cannot be read (it is not created) or a change fails; never raises, the trading loop must not depend on it."""
        try:
            ret, current = self.q.get_user_security(group)
            if ret != RET_OK:
                LOGGER.warning("Moomoo watchlist %s unreadable: %s", group, str(current)[:100])
                return None
            have = {str(c) for c in current["code"]}
            want = list(dict.fromkeys(codes))[:limit]
            add = [c for c in want if c not in have]
            drop = sorted(have - set(want))
            for op, batch in ((ModifyUserSecurityOp.ADD, add), (ModifyUserSecurityOp.DEL, drop)):
                if batch:
                    ret, msg = self.q.modify_user_security(group, op, batch)
                    if ret != RET_OK:
                        LOGGER.warning("Moomoo watchlist %s %s failed: %s", group, op, str(msg)[:100])
                        return None
            return add, drop
        except Exception as error:  # noqa: BLE001 - a watchlist problem must never stop a scan
            LOGGER.warning("Moomoo watchlist sync failed: %s", error)
            return None

    def get_hottest_gainers(self, count: int = 50) -> list[dict]:
        """Fetch live top percentage movers across the US market from OpenD.
        Returns clean records filtered for minimum price and liquidity.
        """
        from futu import Market
        try:
            ret, res = self.q.get_top_movers_rank(market=Market.US, count=count)
            if ret != RET_OK or not isinstance(res, tuple) or len(res) < 2:
                return []
            df = res[1]
            if df is None or df.empty:
                return []
            records = []
            for _, r in df.iterrows():
                sec = str(r.get("security", ""))
                price = float(r.get("cur_price", 0.0) or 0.0)
                change = float(r.get("change_ratio", 0.0) or 0.0)
                vol = float(r.get("volume", 0.0) or 0.0)
                turnover = float(r.get("turnover", 0.0) or 0.0)
                vol_ratio = float(r.get("volume_ratio", 0.0) or 0.0)
                if price >= 2.0 and vol >= 20_000:  # Avoid ultra-thin sub-penny traps
                    records.append({
                        "security": sec,
                        "price": price,
                        "change_ratio": change,
                        "volume": vol,
                        "turnover": turnover,
                        "volume_ratio": vol_ratio,
                    })
            return records
        except Exception as error:  # noqa: BLE001
            LOGGER.warning("Failed to fetch top movers rank from OpenD: %s", error)
            return []

    def market_state(self, key: str = "market_us") -> str | None:
        """OpenD's state of the US market (``MORNING`` in the regular session, ``PRE_MARKET_BEGIN``, ``AFTER_HOURS_BEGIN``, ``CLOSED``...), or None when it cannot be read."""
        try:
            ret, state = self.q.get_global_state()
        except Exception as error:  # noqa: BLE001 - an unreadable state means the clock decides
            LOGGER.warning("Moomoo market state failed: %s", error)
            return None
        if ret != RET_OK or not isinstance(state, dict):
            LOGGER.warning("Moomoo market state unreadable: %s", str(state)[:100])
            return None
        value = state.get(key)
        return None if value is None else str(value)

    def close(self) -> None:
        try:
            if self.subscribed:
                self.q.unsubscribe_all()
        except Exception:  # shutting down: nothing more to do
            pass
        self.q.close()
