"""One bounded ORB paper-execution pass. Only the configured OpenD SIMULATE account."""
from __future__ import annotations
import json, math, sqlite3, sys, time
from datetime import datetime, time as dtime
from pathlib import Path
from zoneinfo import ZoneInfo
from futu import OpenSecTradeContext, OpenQuoteContext, RET_OK, TrdEnv, TrdMarket, TrdSide, OrderType, KLType, AuType, SubType, Session
from orb_ledger import connect, ACCOUNT_ID, ACCOUNT_KIND

ROOT = Path(__file__).resolve().parents[1]; CANDIDATES = ROOT / "data/orb_premarket.json"; BARS = ROOT / "data/common1m.sqlite3"; CANDLE_HEALTH = ROOT / "data/candle_health.json"; ACCOUNT_OUT = ROOT / "data/orb_account.json"
if str(ROOT / "src") not in sys.path:
    sys.path.insert(0, str(ROOT / "src"))
from papercompmoomoo import algo_fingerprint, darwin_qos, microsense, swings
from papercompmoomoo.signals import WORKING_ORDER_STATUSES, candidate_tape_activity
from papercompmoomoo.shared_account import bind_order, release, reserve, sync as sync_shared_account
from papercompmoomoo.exit_ownership import legacy_strategy_exit_allowed
NY = ZoneInfo("America/New_York"); OPEN = dtime(9,30); CUTOFF = dtime(10,30); CLOSE = dtime(16,0)

def account(ctx):
    ret, df = ctx.get_acc_list()
    if ret != RET_OK: raise RuntimeError("OpenD account list unavailable")
    rows = df[(df["trd_env"] == "SIMULATE") & (df["acc_id"] == ACCOUNT_ID) & (df["sim_acc_type"] == "COMPETITION")]
    if len(rows) != 1 or "US" not in list(rows.iloc[0]["trdmarket_auth"]): raise RuntimeError("configured shared Competition US paper account not uniquely available")
    ret, info = ctx.accinfo_query(trd_env=TrdEnv.SIMULATE, acc_id=ACCOUNT_ID, currency="USD", refresh_cache=True)
    if ret != RET_OK: raise RuntimeError("ORB account balance unavailable")
    row = info.iloc[0]; return float(row["total_assets"]), float(row["us_cash"]), float(row["power"])

def session_window(now: datetime) -> tuple[int, int]:
    """Return today's New York 09:30 and 10:30 boundaries in epoch ms."""
    start = datetime.combine(now.date(), OPEN, NY)
    end = datetime.combine(now.date(), CUTOFF, NY)
    return int(start.timestamp()*1000), int(end.timestamp()*1000)

def quarantined_symbols() -> set[str]:
    try: return {str(x).removeprefix("US.") for x in json.loads(CANDLE_HEALTH.read_text()).get("quarantine_symbols", [])}
    except (OSError, ValueError): return set()

def main() -> None:
    darwin_qos.set_interactive()
    now = datetime.now(NY)
    if now.weekday() >= 5 or not OPEN <= now.time() <= CLOSE:
        print(json.dumps({"status":"idle","reason":"outside US regular session"})); return
    in_entry_window = OPEN <= now.time() <= CUTOFF and now.time() < dtime(15,0)
    quarantined = quarantined_symbols()
    need_refresh = not CANDIDATES.exists() or (time.time() - CANDIDATES.stat().st_mtime >= 60)
    selected = json.loads(CANDIDATES.read_text()).get("candidates", [])[:20] if CANDIDATES.exists() else []
    if in_entry_window and (need_refresh or not selected):
        import orb_premarket
        orb_premarket.main()
        selected = json.loads(CANDIDATES.read_text()).get("candidates", [])[:20] if CANDIDATES.exists() else []
    trade = OpenSecTradeContext(host="127.0.0.1", port=11111, filter_trdmarket=TrdMarket.US)
    quote = OpenQuoteContext(host="127.0.0.1", port=11111)
    from papercompmoomoo.telemetry import RecordedContext, emit
    trade = RecordedContext(trade, 'orb_trade')
    quote = RecordedContext(quote, 'orb_quote')
    emit('candidate_universe', {'selected': selected, 'account_id': ACCOUNT_ID}, actor='orb')
    from papercompmoomoo.telemetry import install_handlers
    install_handlers(trade, trade=True)
    try:
        market_ret, market_info = quote.get_global_state()
        market_state = str(market_info.get("market_us", "")).split(".")[-1].upper() if market_ret == RET_OK and isinstance(market_info, dict) else ""
        # ORB entries are RTH-only. Do not trust wall-clock time alone on
        # holidays, delayed opens, or when OpenD cannot confirm the session.
        if market_state != "MORNING":
            in_entry_window = False
        equity, cash, power = account(trade); db = connect(); now_ms = int(time.time()*1000)
        session = now.date().isoformat()
        for rank, item in enumerate(selected, 1):
            db.execute("INSERT INTO candidates(session,symbol,rank,score,gap_pct,premarket_volume,status,detected_ms) VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(session,symbol) DO UPDATE SET rank=excluded.rank,score=excluded.score,gap_pct=excluded.gap_pct,premarket_volume=excluded.premarket_volume,status=excluded.status,detected_ms=coalesce(candidates.detected_ms,excluded.detected_ms)", (session, item["symbol"].removeprefix("US."), rank, float(item.get("score", 0)), float(item.get("gap_pct", 0)), float(item.get("premarket_volume", 0)), "selected", now_ms))
        db.commit()
        # Reconcile broker truth before considering new entries.
        ret_o, broker_orders = trade.order_list_query(trd_env=TrdEnv.SIMULATE, acc_id=ACCOUNT_ID)
        ret_p, broker_positions = trade.position_list_query(trd_env=TrdEnv.SIMULATE, acc_id=ACCOUNT_ID)
        if ret_o != RET_OK or ret_p != RET_OK:
            raise RuntimeError("shared Competition positions/orders unavailable; ORB will not trade on partial account state")
        working_rows = [{"order_id": str(o.get("order_id")), "code": str(o.get("code")), "qty": float(o.get("qty", 0) or 0),
                         "price": float(o.get("price", 0) or 0), "side": str(o.get("trd_side", "BUY")),
                         "remark": str(o.get("remark", ""))}
                        for _, o in broker_orders.iterrows() if str(o.get("order_status", "")) in WORKING_ORDER_STATUSES and str(o.get("trd_side", "BUY")).upper().endswith("BUY")]
        terminal_rows = [{"order_id": str(o.get("order_id")), "status": str(o.get("order_status", "terminal"))}
                         for _, o in broker_orders.iterrows() if str(o.get("order_status", "")) not in WORKING_ORDER_STATUSES]
        own_ledger = {str(r[0]) for r in db.execute("SELECT symbol FROM positions WHERE status IN ('open','closing') AND qty>0")}
        gainer_ledger = set()
        try:
            saved_gainer = json.loads((ROOT / "data/positions.json").read_text())
            gainer_ledger = {str(s).removeprefix("US.") for s in saved_gainer}
        except (OSError, ValueError, sqlite3.Error):
            pass
        held_symbols = [str(p.get("code")) for _, p in broker_positions.iterrows() if int(float(p.get("qty", 0) or 0)) > 0]
        sync_shared_account(held_symbols, working_rows, {"orb": own_ledger, "gainer": gainer_ledger}, terminal_rows=terminal_rows)
        from papercompmoomoo.shared_account import owner_of
        broker_held = [p for _, p in broker_positions.iterrows() if int(float(p.get("qty", 0) or 0)) > 0 and owner_of(str(p.get("code", ""))) == "orb"]
        if ret_o == RET_OK:
            for _, order in broker_orders.iterrows():
                oid = str(order.get("order_id")); status = str(order.get("order_status", "unknown"))
                db.execute("UPDATE orders SET status=?,updated_ms=? WHERE order_id=?", (status, now_ms, oid))
                # A filled entry becomes a managed ledger position.  Keep the
                # strategy's stop/target from the submitted order attached to it.
                filled = float(order.get("dealt_qty", 0) or 0)
                normalized_status = status.lower()
                is_filled = normalized_status in {"filled", "filled_all", "partial_filled", "partially_filled"} or "filled" in normalized_status
                if is_filled and filled > 0:
                    planned = db.execute("SELECT symbol,limit_price,stop_price,target_price,created_ms,processed_qty FROM orders WHERE order_id=?", (oid,)).fetchone()
                    side = str(order.get("trd_side", "BUY")).upper()
                    if planned and side.endswith("BUY"):
                        symbol, limit_price, stop_price, target_price, opened_ms, processed_qty = planned
                        delta = max(0, int(filled) - int(processed_qty or 0))
                        if delta <= 0: continue
                        entry = float(order.get("dealt_avg_price", 0) or limit_price)
                        trail_row=db.execute("SELECT trail_pct FROM orders WHERE order_id=?",(oid,)).fetchone()
                        db.execute("INSERT OR REPLACE INTO positions(symbol,qty,entry_price,stop_price,target_price,high_price,opened_ms,status,initial_target_price,trail_pct) VALUES(?,?,?,?,?,?,?,?,?,?)", (symbol, int(filled), entry, float(stop_price or 0), float(target_price or 0), entry, int(opened_ms), "open", float(target_price or 0), float(trail_row[0]) if trail_row else 0.0))
                        db.execute("UPDATE orders SET processed_qty=? WHERE order_id=?", (int(filled), oid))
                    elif planned and side.endswith("SELL"):
                        symbol, _, _, _, _, processed_qty = planned
                        delta = max(0, int(filled) - int(processed_qty or 0))
                        if delta <= 0: continue
                        position = db.execute("SELECT qty,entry_price,opened_ms FROM positions WHERE symbol=? AND status IN ('open','closing')", (symbol,)).fetchone()
                        if position:
                            qty, entry, opened_ms = position
                            exit_price = float(order.get("dealt_avg_price", 0) or 0)
                            if exit_price > 0:
                                close_qty = min(delta, int(qty))
                                db.execute("INSERT INTO trades(symbol,qty,entry_price,exit_price,pnl_usd,reason,opened_ms,closed_ms) VALUES(?,?,?,?,?,?,?,?)", (symbol, close_qty, entry, exit_price, (exit_price-entry)*close_qty, "broker_exit", opened_ms, now_ms))
                                db.execute("UPDATE positions SET qty=MAX(0,qty-?),status=CASE WHEN qty<=? THEN 'closed' ELSE 'open' END WHERE symbol=?", (close_qty, close_qty, symbol))
                                db.execute("UPDATE orders SET processed_qty=? WHERE order_id=?", (int(filled), oid))
        if ret_p == RET_OK:
            live = set()
            for pos in broker_held:
                symbol = str(pos.get("code")); ticker = symbol.removeprefix("US."); qty = int(float(pos.get("qty", 0) or 0));
                if qty <= 0: continue
                live.add(symbol)
                existing = db.execute("SELECT entry_price,stop_price,target_price,opened_ms FROM positions WHERE symbol=?", (ticker,)).fetchone()
                if existing is None:
                    entry = float(pos.get("cost_price", pos.get("平均成本价", 0)) or 0)
                    db.execute("INSERT OR REPLACE INTO positions(symbol,qty,entry_price,stop_price,target_price,high_price,opened_ms,status,initial_target_price,trail_pct) VALUES(?,?,?,?,?,?,?,?,?,?)", (ticker,qty,entry,0,0,entry,now_ms,"open",0,0.0))
                else:
                    db.execute("UPDATE positions SET qty=?,status='open' WHERE symbol=?", (qty,ticker))
            for symbol, qty in db.execute("SELECT symbol,qty FROM positions WHERE status='open'").fetchall():
                if symbol not in {s.removeprefix("US.") for s in live}:
                    db.execute("UPDATE positions SET status='closed',qty=0 WHERE symbol=?", (symbol,))
        db.commit()  # release ledger write lock before market reads and telemetry
        # Position decisions belong to the deterministic price-line worker.
        exits = []
        open_rows = db.execute("SELECT symbol,qty,entry_price,stop_price,target_price,high_price FROM positions WHERE status='open'").fetchall()
        # Persist native Moomoo candles only for today's ORB candidates and
        # current holdings.  This replaces the all-market converter.
        candle_codes=list(dict.fromkeys([f"US.{r[0].removeprefix('US.')}" for r in open_rows if int(r[1] or 0)>0]+[str(i["symbol"]) for i in selected]))[:25]
        if candle_codes:
            quote.subscribe(candle_codes,[SubType.K_1M],subscribe_push=False,session=Session.ALL)
            from papercompmoomoo.moomoo_feed import bars_from_kline
            from papercompmoomoo.microsense import write_minutes
            for code in candle_codes:
                ret_k,kline=quote.get_cur_kline(code,num=100,ktype=KLType.K_1M,autype=AuType.NONE)
                if ret_k==RET_OK and kline is not None and len(kline):
                    write_minutes(BARS,code,bars_from_kline(kline,1))
        for symbol, qty, entry, stop, target, high_px in open_rows:
            if qty <= 0: continue
            ticker=symbol.removeprefix('US.')
            with sqlite3.connect(BARS) as market:
                bars=market.execute("SELECT ts,open,high,low,close,volume FROM bars WHERE symbol=? AND ts>=? AND source='moomoo' ORDER BY ts",(ticker,int(db.execute("SELECT opened_ms FROM positions WHERE symbol=?",(symbol,)).fetchone()[0]))).fetchall()
            if not bars: continue
            high=max(float(high_px or entry),*(float(b[2] or 0) for b in bars),float(b[4] or 0))
            meta=db.execute("SELECT initial_target_price,trail_pct FROM positions WHERE symbol=?",(symbol,)).fetchone()
            initial_target=float(meta[0] or target or 0); old_trail=float(meta[1] or 0)
            symbol_terms=swings.symbol_volatility_terms(bars)
            if not symbol_terms: continue
            trail_pct=max(old_trail,float(symbol_terms["trail_pct"]))
            risk_distance=max(0.0,initial_target-float(entry))
            new_stop=max(float(stop or 0),high*(1-trail_pct/100))
            # Keep the full-target exit line reachable on a pullback: after
            # price advances by one original reward distance, trail the line
            # one reward distance behind the high. Never ratchet it down.
            new_target=max(float(target or 0),initial_target,high-risk_distance) if risk_distance>0 else float(target or 0)
            if high>float(high_px or 0) or new_stop>float(stop or 0) or new_target>float(target or 0) or abs(trail_pct-old_trail)>1e-9:
                db.execute("UPDATE positions SET high_price=?,stop_price=?,target_price=?,trail_pct=? WHERE symbol=?",(high,new_stop,new_target,trail_pct,symbol))
                emit('orb_dynamic_levels_updated',{'symbol':ticker,'entry':entry,'old_high':high_px,'high_water_price':high,'old_stop':stop,'new_stop':new_stop,'old_target':target,'new_target':new_target,'initial_target':initial_target,'old_trail_pct':old_trail,'trail_pct':trail_pct,'trail_source':symbol_terms['volatility_source'],'trail_sample_count':symbol_terms['volatility_bar_count'],'target_70_price':float(entry)+(new_target-float(entry))*.70,'profit_midpoint_price':(float(entry)+(new_target-float(entry))*.70+new_stop)/2},actor='orb')
            if not legacy_strategy_exit_allowed(ACCOUNT_ID):
                emit('legacy_exit_suppressed', {'symbol': symbol, 'qty': int(qty), 'owner': 'deterministic_price_levels', 'account_id': ACCOUNT_ID}, actor='orb')
        db.commit()
        db.execute("INSERT INTO account_snapshots VALUES(?,?,?,?,?,?)", (now_ms,ACCOUNT_ID,ACCOUNT_KIND,equity,cash,power)); db.commit()
        ACCOUNT_OUT.write_text(json.dumps({"account_id": ACCOUNT_ID, "account_kind": ACCOUNT_KIND, "equity": equity, "cash": cash, "buying_power": power, "updated_ms": now_ms}, indent=2) + "\n")
        if not in_entry_window or not selected:
            print(json.dumps({"status":"ok","account_id":ACCOUNT_ID,"equity":equity,"mode":"thomasai_position_monitor","open_positions":len(open_rows),"exits_submitted":exits}))
            return
        opened = {r[0] for r in db.execute("SELECT symbol FROM positions WHERE status='open'")} | {r[0] for r in db.execute("SELECT symbol FROM orders WHERE session=? AND side='BUY' AND status NOT LIKE 'rejected%' AND status NOT IN ('CANCELLED_ALL','FAILED','DISABLED','DELETED')", (session,))}
        session_start, session_end = session_window(now)
        for item in selected:
            symbol = item["symbol"]; ticker = symbol.removeprefix("US.")
            if ticker in opened: continue
            if ticker in quarantined: continue
            rows = sqlite3.connect(BARS).execute("SELECT ts,open,high,low,close,volume FROM bars WHERE symbol=? AND source='moomoo' AND ts>=? AND ts<=? ORDER BY ts", (ticker, session_start, session_end)).fetchall()
            if len(rows) < 6: continue
            opening = rows[:5]; latest = rows[-1]
            orh=max(r[2] for r in opening); orl=min(r[3] for r in opening); entry=round(orh+0.01, 2); stop=round((orh+orl)/2, 2); risk=entry-stop
            tape_activity=candidate_tape_activity(rows,now_ms,session_start)
            opening_trade_bars=sum(1 for r in opening if float(r[5] or 0)>0)
            opening_trade_minimum=max(2,int(math.ceil(len(opening)*.60)))
            opening_vwap = sum(r[4]*r[5] for r in opening) / max(sum(r[5] for r in opening), 1.0)
            breakout_volume = max(r[5] for r in rows[-2:]) > 0
            confirmation = microsense.confirm_candidate(rows, 1.0, float(latest[4]), float(latest[4]), ROOT / "data/brain.sqlite3")
            sensed = confirmation["features"]
            reason = "eligible"
            if not tape_activity["eligible"]: reason = tape_activity["reason"] or "thin traded tape"
            elif opening_trade_bars < opening_trade_minimum: reason = f"opening-range tape thin: {opening_trade_bars}/{len(opening)} bars traded"
            elif risk <= 0: reason = "non_positive_risk"
            elif orh/orl-1 > .10: reason = "opening_range_too_wide"
            elif latest[4] <= orh: reason = "no_range_breakout"
            elif latest[4] <= opening_vwap: reason = "below_vwap"
            elif not breakout_volume: reason = "no_volume_confirmation"
            elif confirmation.get("reason"): reason = confirmation["reason"]
            db.execute("INSERT INTO signal_observations(ts_ms,session,symbol,side,last_price,orh,orl,vwap,breakout_volume,risk,decision,reason,payload_json) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)", (now_ms, now.date().isoformat(), ticker, "LONG", float(latest[4]), float(orh), float(orl), float(opening_vwap), float(latest[5]), float(risk), "candidate" if reason == "eligible" else "rejected", reason, json.dumps({"opening_bars": len(opening), "opening_trade_bars":opening_trade_bars,"opening_trade_minimum":opening_trade_minimum,"tape_activity":tape_activity,"latest_ts": latest[0], "latest_volume": latest[5], "kalman": sensed})))
            if reason != "eligible": continue
            qty=min(int((equity*.01)/risk), int((equity/20)/entry), int(power/entry))
            if qty < 1: continue
            snap=quote.get_market_snapshot([symbol]);
            if snap[0] != RET_OK or snap[1].empty or float(snap[1].iloc[0].get("last_price",0)) < orh: continue
            srow = snap[1].iloc[0]
            live_px = float(srow.get("ask_price", 0) or srow.get("last_price", entry))
            limit_px = round(max(entry, live_px) * 1.003, 2)
            init_eval = algo_fingerprint.evaluate_dynamic_exit(
                symbol=symbol,
                entry_price=max(entry, live_px),
                high_price=max(entry, live_px),
                last_price=live_px,
                current_stop=stop,
                current_target=round(max(entry, live_px) + 1.5 * risk, 2),
                bars=rows,
                brain_path=ROOT / "data/brain.sqlite3",
                common_db=BARS,
            )
            stop_px = float(init_eval["dynamic_stop"])
            target_px = float(init_eval["dynamic_target"])
            symbol_terms = swings.symbol_volatility_terms(rows,
                spread_pct=swings.quote_spread_pct({
                    "bid":float(srow.get("bid_price",0) or 0),
                    "ask":float(srow.get("ask_price",0) or 0)}))
            if not symbol_terms:
                emit('orb_entry_deferred', {'symbol':ticker,'reason':'insufficient symbol-specific Moomoo volatility bars for trailing stop'},actor='orb')
                continue
            trail_pct = float(symbol_terms['trail_pct'])
            reservation, grant = reserve("orb", ticker, qty * limit_px, power)
            if reservation is None:
                continue
            qty = min(qty, int(grant / limit_px))
            if qty < 1:
                release(reservation, "shared pool cannot fund one whole share")
                continue
            ret, result=trade.place_order(price=limit_px, qty=qty, code=symbol, order_type=OrderType.NORMAL,
                trd_side=TrdSide.BUY, trd_env=TrdEnv.SIMULATE, acc_id=ACCOUNT_ID,
                remark="ORB Competition paper", fill_outside_rth=False, session=Session.RTH)
            if ret != RET_OK:
                release(reservation, "Moomoo rejected the order")
            oid=str(result.iloc[0].get("order_id")) if ret==RET_OK else f"rejected-{time.time_ns()}"
            db.execute("INSERT OR REPLACE INTO orders(order_id,session,symbol,side,qty,limit_price,stop_price,target_price,status,created_ms,updated_ms,broker_id,trail_pct) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)", (oid, now.date().isoformat(), ticker, "BUY", qty, limit_px, stop_px, target_px, "submitted" if ret==RET_OK else "rejected", int(time.time()*1000), int(time.time()*1000), oid if ret==RET_OK else None, trail_pct)); db.commit()
            emit('orb_symbol_risk_levels_initialized',{'symbol':ticker,'entry_reference':max(entry,live_px),
                'opening_range_stop':stop_px,'initial_target':target_px,'trail_pct':trail_pct,
                'trail_source':symbol_terms['volatility_source'],'trail_sample_count':symbol_terms['volatility_bar_count'],
                'moomoo_bar_ts':rows[-1][0],'order_id':oid,'accepted':ret==RET_OK},actor='orb')
            if ret == RET_OK:
                bind_order(reservation, oid)
                db.execute("UPDATE candidates SET approved_ms=coalesce(approved_ms,?) WHERE session=? AND symbol=?", (int(time.time()*1000), session, ticker))
                db.commit()
                opened.add(ticker)
        db.commit()
        print(json.dumps({"status":"ok","account_id":ACCOUNT_ID,"equity":equity,"selected":len(selected),"exits_submitted":exits}))
    finally: quote.close(); trade.close()

if __name__ == "__main__": main()
