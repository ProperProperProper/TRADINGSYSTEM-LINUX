"""Codex decision -> fresh paper exit -> broker-confirmed feedback.

Broker access is injected so causal/execution checks can be verified offline.
This controller never treats accepted orders as fills.
"""
import math
import datetime as dt
from zoneinfo import ZoneInfo
import time
from .exit_execution import check_quote, fill_metrics
from . import online_exit
from .telemetry import emit
from .account_ids import account_id

PAPER_ACCOUNTS={'gainer':account_id('gainer'),'orb':account_id('orb')}
SIGNALS={'vwap_failure','structure_failure','distribution','momentum_decay','protective_stop','profit_target','profit_target_100','profit_retrace_failure','midpoint_failure','liquidity_trap','session_flatten'}

def validate_analysis(answer):
    if not isinstance(answer,dict) or answer.get('action') not in ('HOLD','EXIT'):raise ValueError('invalid action')
    if answer.get('signal') not in SIGNALS:raise ValueError('unsupported analytical signal')
    p=answer.get('p_up')
    if isinstance(p,bool) or not isinstance(p,(int,float)) or not math.isfinite(p) or not 0<=p<=1:raise ValueError('invalid probability')
    horizon=answer.get('horizon_seconds')
    if isinstance(horizon,bool) or not isinstance(horizon,int) or not 10<=horizon<=300:raise ValueError('undefined probability horizon')
    if not isinstance(answer.get('reason'),str) or not answer['reason'].strip():raise ValueError('missing reasoning')
    return answer

class PaperExitController:
    """Broker must provide fresh quotes/positions, pending sells and fill records.

    The decision worker calls this AFTER Codex responds. Existing deterministic
    risk protections run independently while inference is in progress.
    """
    def __init__(self,broker,ledger_path,actor='codex-live-exit'):
        self.broker=broker;self.ledger_path=ledger_path;self.actor=actor

    def _quote(self,symbol,force=False):
        if force:
            reader=getattr(self.broker,'fresh_quote',None)
            if reader is not None:return reader(symbol)
        return self.broker.quote(symbol)

    @staticmethod
    def _quote_rejection(quote,now_ms,automatic_exit=False):
        if not automatic_exit:return check_quote(quote,now_ms)
        # A forced OpenD book read is made immediately before the risk exit.
        if quote is None:return 'missing quote'
        if not all(math.isfinite(x) and x>0 for x in (quote.bid,quote.ask)):return 'invalid bid/ask'
        if quote.bid>quote.ask:return 'crossed quote'
        if quote.received_ms>now_ms:return 'future receipt timestamp'
        provider_ms=getattr(quote,'provider_ms',None)
        if provider_ms is not None:
            age=now_ms-provider_ms
            if age<0:return 'provider clock ahead; offset unresolved'
            if age>30_000:return f'stale executable quote ({age}ms old)'
        elif now_ms-quote.received_ms>1_000:
            return 'stale executable quote with no provider timestamp'
        return None

    def execute(self,bot,symbol,opened_ms,observation_id,analysis_started_ms,answer,recheck,now_ms=None,decision_metadata=None):
        supplied_clock=now_ms
        answer=validate_analysis(answer);now_ms=now_ms if now_ms is not None else time.time_ns()//1000000
        account=PAPER_ACCOUNTS.get(bot)
        if account is None:raise ValueError('unknown simulated account')
        # Codex's output is provisional until fresh broker and candle data have
        # been read. Persist only the final, post-inference action.
        bot_position=getattr(self.broker,'position_for_bot',None)
        position=bot_position(bot,account,symbol) if bot_position else self.broker.position(account,symbol)
        if not position or position.get('opened_ms')!=opened_ms:
            emit('codex_exit_revalidation_failed',{'symbol':symbol,'reason':'position changed during inference'},actor=bot,correlation_id=observation_id)
            return {'status':'rejected','reason':'position changed during analysis'}
        # Read the executable book after the slower position refresh so a
        # fresh quote does not age out while the broker reconciles holdings.
        automatic_exit=answer['signal'] in ('protective_stop','profit_target','profit_target_100','profit_retrace_failure','midpoint_failure','liquidity_trap','session_flatten')
        try:quote=self._quote(symbol,force=automatic_exit)
        except Exception:quote=None
        check_ms=time.time_ns()//1000000 if supplied_clock is None else supplied_clock
        quote_rejection=self._quote_rejection(quote,check_ms,automatic_exit)
        backend=(decision_metadata or {}).get('backend','')
        local=dt.datetime.fromtimestamp(check_ms/1000,ZoneInfo('America/New_York'))
        regular_market_exit=(automatic_exit and local.weekday()<5
            and dt.time(9,30)<=local.time()<dt.time(16,0)
            and (answer['signal']=='session_flatten' or backend=='latched_outside_rth_exit_execution'))
        if regular_market_exit:
            # A RTH market order has no limit price to derive from a stale quote.
            # Broker account/market-state validation still occurs at submission.
            quote_rejection=None

        def current_condition(current_quote,current_position):
            result=recheck(answer['signal'],symbol,current_quote,current_position)
            if isinstance(result,dict):
                return bool(result.get('condition')),result.get('evidence',result)
            return bool(result),{'signal':answer['signal'],'condition':bool(result)}

        condition,evidence=current_condition(quote,position)
        proposal_expired=check_ms-analysis_started_ms>answer['horizon_seconds']*1000
        common_analysis={'observation_id':observation_id,'bot':bot,'symbol':symbol,
            'position_id':f'{symbol.removeprefix("US.")}:{int(opened_ms)}','analysis_started_ms':analysis_started_ms,
            'model_completed_ms':now_ms,'answer':answer,'probability_status':'uncalibrated_model_estimate',
            'decision_metadata':decision_metadata or {},
            'probability_definition':'future price above the latest closed-candle price at horizon; uncalibrated and not a guaranteed trade-profit probability',
            'post_inference_refresh_ms':check_ms,'post_inference_quote':({'bid':quote.bid,'ask':quote.ask,
                'received_ms':quote.received_ms,'provider_ms':quote.provider_ms} if quote else None),
            'post_inference_position':{'opened_ms':position.get('opened_ms'),'qty':position.get('qty'),
                'available_qty':position.get('available_qty'),'entry':position.get('entry')},
            'post_inference_market_evidence':evidence}

        def finalize(action,reason,final_quote,final_evidence,final_ms):
            analysis=dict(common_analysis,finalized_ms=final_ms,proposed_action=answer['action'],
                final_action=action,final_reason=reason,final_quote=({'bid':final_quote.bid,'ask':final_quote.ask,
                    'received_ms':final_quote.received_ms,'provider_ms':final_quote.provider_ms} if final_quote else None),
                final_market_evidence=final_evidence)
            emit('codex_exit_analysis',analysis,actor=bot,correlation_id=observation_id)
            backend=(decision_metadata or {}).get('backend','codex')
            online_exit.action(self.ledger_path,observation_id,action,'codex-live-exit-v1',
                f'{self.actor}:{backend}',analysis)
            emit('codex_exit_action_updated',{'symbol':symbol,'proposed_action':answer['action'],
                'final_action':action,'reason':reason,'bid':final_quote.bid if final_quote else None,'ask':final_quote.ask if final_quote else None,
                'updated_ms':final_ms},actor=bot,correlation_id=observation_id)
            return analysis

        # Candle evidence supports analysis without a book; executable quotes
        # are required only for submitting a simulated EXIT.
        if answer['action']=='HOLD':
            analysis=finalize('HOLD','Codex HOLD confirmed against post-inference candles',quote,evidence,check_ms)
            return {'status':'hold','analysis':analysis,'updated_action':'HOLD'}
        if proposal_expired:
            analysis=finalize('HOLD','Codex EXIT horizon elapsed during inference',quote,evidence,check_ms)
            return {'status':'hold','analysis':analysis,'updated_action':'HOLD'}
        if not condition:
            analysis=finalize('HOLD','Codex EXIT condition no longer holds on refreshed data',quote,evidence,check_ms)
            return {'status':'hold','analysis':analysis,'updated_action':'HOLD'}
        if quote_rejection:
            analysis=finalize('HOLD','EXIT withheld because no fresh executable quote is available: '+quote_rejection,quote,evidence,check_ms)
            return {'status':'hold','analysis':analysis,'updated_action':'HOLD'}
        qty=position.get('available_qty',0)
        if not isinstance(qty,int) or qty<=0:
            analysis=finalize('HOLD','no available whole shares after refresh',quote,evidence,check_ms)
            return {'status':'hold','analysis':analysis,'updated_action':'HOLD'}
        if self.broker.working_sell(account,symbol):
            analysis=finalize('HOLD','sell already pending after refresh',quote,evidence,check_ms)
            return {'status':'hold','analysis':analysis,'updated_action':'HOLD'}
        # The adapter must serialize/reconcile sells with the bots' other routes.
        # Position/order reads can themselves be slow. Check again directly
        # before submission rather than reusing a quote from before those calls.
        position=bot_position(bot,account,symbol) if bot_position else self.broker.position(account,symbol)
        if not position or position.get('opened_ms')!=opened_ms:
            analysis=finalize('HOLD','position changed before order submission',quote,evidence,check_ms)
            return {'status':'rejected','reason':'position changed before order submission','analysis':analysis}
        qty=position.get('available_qty',0)
        if not isinstance(qty,int) or qty<=0:
            analysis=finalize('HOLD','no available whole shares after final refresh',quote,evidence,check_ms)
            return {'status':'hold','analysis':analysis,'updated_action':'HOLD'}
        try:quote=self._quote(symbol,force=automatic_exit)
        except Exception:quote=None
        check_ms=time.time_ns()//1000000 if supplied_clock is None else supplied_clock
        rejection=self._quote_rejection(quote,check_ms,automatic_exit)
        if rejection:
            analysis=finalize('HOLD','EXIT withheld because no fresh executable quote is available: '+rejection,quote,evidence,check_ms)
            return {'status':'hold','analysis':analysis,'updated_action':'HOLD'}
        condition,evidence=current_condition(quote,position)
        if not condition or check_ms-analysis_started_ms>answer['horizon_seconds']*1000:
            reason='condition changed before submission' if not condition else 'Codex EXIT horizon elapsed before submission'
            analysis=finalize('HOLD',reason,quote,evidence,check_ms)
            return {'status':'hold','analysis':analysis,'updated_action':'HOLD'}
        origin=(decision_metadata or {}).get('backend','codex')
        automatic_reason={
            'protective_stop':'Protective stop crossed',
            'profit_target':'70% target crossed down',
            'profit_target_100':'100% target touched or crossed up',
            'profit_retrace_failure':'Intermediate profit line crossed down',
            'midpoint_failure':'Profit midpoint crossed down',
            'liquidity_trap':'Liquidity trap confirmed',
            'session_flatten':'Scheduled session flatten',
        }.get(answer['signal'],origin)
        reason=(f'{automatic_reason}; condition confirmed on a fresh pre-submission quote'
                if automatic_exit else 'Codex EXIT signal remains valid on fresh pre-submission data')
        analysis=finalize('EXIT',reason,quote,evidence,check_ms)
        try:
            order=self.broker.submit_exit(account,symbol,qty,trd_env='SIMULATE')
        except Exception as exc:
            # Preserve the attempted decision, then append the actual outcome:
            # no EXIT was executed if the paper venue/session rejected it.
            failure={'submission_error':repr(exc),'bot':bot,'symbol':symbol,
                'account_id':account,'opened_ms':opened_ms,'decision_id':observation_id,
                'checked_ms':time.time_ns()//1_000_000}
            emit('codex_exit_submission_refused',failure,actor=bot,correlation_id=observation_id)
            corrected=finalize('HOLD','Paper exit was not submitted: '+str(exc),quote,
                dict(evidence,submission_refused=repr(exc)),failure['checked_ms'])
            return {'status':'rejected','reason':str(exc),'analysis':corrected}
        result={'status':'submitted','order_id':order['order_id'],'account_id':account,'bot':bot,
            'symbol':symbol,'opened_ms':opened_ms,'observation_id':observation_id,'quantity':qty,
            'entry_price':position.get('entry'),'position_quantity':position.get('qty'),
            'submitted_ms':check_ms,'submission_bid':quote.bid,'submission_ask':quote.ask,
            'fill_price':None,'realized_pnl':None}
        result.update({k:v for k,v in order.items() if k!='order_id'})
        emit('codex_exit_submitted',result,actor=bot,correlation_id=observation_id)
        return result

    def confirm_fill(self,submission,fill):
        """One actual broker deal, including partial fills, keyed by deal_id."""
        if submission.get('status')!='submitted':raise ValueError('not a submitted exit')
        if not fill.get('deal_id') or fill.get('order_id')!=submission['order_id']:raise ValueError('fill identity mismatch')
        if online_exit.has_feedback(self.ledger_path,submission['bot'],submission['symbol'],
                                    submission['opened_ms'],fill['deal_id']):
            return {'status':'already_reconciled','deal_id':fill['deal_id'],'order_id':fill['order_id']}
        qty=fill.get('qty')
        if not isinstance(qty,int) or not 0<qty<=submission['quantity']:raise ValueError('invalid fill quantity')
        metrics=fill_metrics(submission['submission_bid'],fill['price'],qty,
            submission['submitted_ms'],fill['filled_ms'],fill.get('fees_usd'))
        payload=dict(metrics,deal_id=fill['deal_id'],order_id=fill['order_id'],
            observation_id=submission['observation_id'],actual_broker_fill=True,
            remaining_quantity_unknown=True,
            unchosen_action_outcome='unknown; not a training label')
        online_exit.feedback(self.ledger_path,submission['bot'],submission['symbol'],
            submission['opened_ms'],fill['filled_ms'],payload)
        emit('codex_exit_actual_fill',payload,actor=submission['bot'],correlation_id=submission['observation_id'])
        return payload
