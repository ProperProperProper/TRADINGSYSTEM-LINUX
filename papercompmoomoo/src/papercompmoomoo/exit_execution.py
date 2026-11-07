"""Pure execution freshness checks. No model HTTP calls or order submission."""
from __future__ import annotations
import math
from dataclasses import dataclass

@dataclass(frozen=True)
class Quote:
    received_ms: int
    bid: float
    ask: float
    provider_ms: int | None = None
    live_data: bool = False

def check_quote(quote,now_ms,max_age_ms=1000):
    if quote is None:return 'missing quote'
    if not all(math.isfinite(x) and x>0 for x in (quote.bid,quote.ask)):return 'invalid bid/ask'
    if quote.bid>quote.ask:return 'crossed quote'
    if quote.received_ms>now_ms:return 'future receipt timestamp'
    if now_ms-quote.received_ms>max_age_ms:return 'stale received quote'
    # A live Moomoo order-book push/read is the current executable book. US
    # server timestamps can be absent or describe when that unchanged book was
    # last updated; use the local age of the received live book for execution.
    # Provider time remains captured for evidence, and is required for quotes
    # that are not explicitly marked as live broker data.
    if quote.live_data:return None
    if quote.provider_ms is None:return 'provider timestamp unavailable'
    if quote.provider_ms>now_ms:return 'provider clock ahead; offset unresolved'
    if now_ms-quote.provider_ms>max_age_ms:return 'stale provider quote'
    return None

def validate_exit(proposal,quote,now_ms,condition_still_true,*,max_quote_age_ms=1000):
    """Freshly re-evaluate a bounded policy; never fill at its old price.

    This is a research/pre-integration guard, not an installed order router.
    Missing clock alignment fails closed for AI-driven execution.
    Existing deterministic protections are independent of this optional gate.
    """
    rejection=check_quote(quote,now_ms,max_quote_age_ms)
    if rejection:return {'allowed':False,'reason':rejection}
    if proposal.get('action')!='EXIT':return {'allowed':False,'reason':'no exit intent'}
    if proposal.get('created_ms',now_ms)>now_ms:return {'allowed':False,'reason':'future proposal'}
    if now_ms>proposal.get('valid_until_ms',-1):return {'allowed':False,'reason':'expired policy'}
    if not condition_still_true:return {'allowed':False,'reason':'exit condition no longer holds'}
    return {'allowed':True,'decision_reference_bid':quote.bid,'decision_reference_ask':quote.ask,
        'quote_received_ms':quote.received_ms,'checked_ms':now_ms,
        'analysis_price':proposal.get('analysis_price'),'fill_price':None,
        'reason':'current quote and current condition validated; actual fill still unknown'}

def fill_metrics(submission_bid,fill_price,qty,submitted_ms,filled_ms,fees_usd=None):
    if not all(math.isfinite(x) and x>0 for x in (submission_bid,fill_price,qty)):
        raise ValueError('invalid measured fill')
    if filled_ms<submitted_ms:raise ValueError('fill predates submission')
    return {'submission_reference_bid':submission_bid,'actual_fill_price':fill_price,
        'quantity':qty,'execution_latency_ms':filled_ms-submitted_ms,
        'sell_slippage_usd':qty*(submission_bid-fill_price),
        'sell_slippage_bps':10000*(submission_bid-fill_price)/submission_bid,
        'fees_usd':fees_usd}
