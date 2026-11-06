"""Validate the shared native 1-minute candle store for both bots."""
import json, sqlite3, time
from datetime import date, datetime, timedelta, time as dtime
from pathlib import Path
from zoneinfo import ZoneInfo
ROOT=Path(__file__).resolve().parents[1]; DB=ROOT/"data/common1m.sqlite3"; OUT=ROOT/"data/candle_health.json"; NY=ZoneInfo("America/New_York")
def nth_weekday(year, month, weekday, n):
    day=date(year,month,1); return day+timedelta(days=(weekday-day.weekday())%7+7*(n-1))
def last_weekday(year, month, weekday):
    day=date(year,month+1,1)-timedelta(days=1); return day-timedelta(days=(day.weekday()-weekday)%7)
def observed(day):
    return day-timedelta(days=1) if day.weekday()==5 else day+timedelta(days=1) if day.weekday()==6 else day
def us_holiday(day):
    y=day.year
    fixed={observed(date(y,1,1)),nth_weekday(y,1,0,3),nth_weekday(y,2,0,3),last_weekday(y,5,0),observed(date(y,6,19)),observed(date(y,7,4)),nth_weekday(y,9,0,1),nth_weekday(y,11,3,4),observed(date(y,12,25))}
    # Good Friday is two days before Easter; compact Gregorian calculation.
    a=y%19; b=y//100; c=y%100; d=b//4; e=b%4; f=(b+8)//25; g=(b-f+1)//3; h=(19*a+b-d-g+15)%30; i=c//4; k=c%4; l=(32+2*e+2*i-h-k)%7; m=(a+11*h+22*l)//451; month=(h+l-7*m+114)//31; easter=date(y,month,(h+l-7*m+114)%31+1)
    fixed.add(easter-timedelta(days=2))
    return day in fixed
def close_time(day):
    return dtime(13,0) if (day.month,day.day) in ((11,27),(12,24)) and day.weekday()<5 else dtime(16,0)
def phase(now):
    if now.weekday()>=5 or us_holiday(now.date()): return "closed"
    close=close_time(now.date())
    if dtime(4,0)<=now.time()<dtime(9,30): return "premarket"
    if dtime(9,30)<=now.time()<close: return "regular"
    if close<=now.time()<dtime(20,0): return "after_hours"
    return "closed"
def main():
    now_ms=int(time.time()*1000); now=datetime.now(NY); market_phase=phase(now); regular=market_phase=="regular"; chart_session=market_phase in ("premarket","regular")
    result={"status":"ok","checked_ms":now_ms,"market_phase":market_phase,"expected_idle":not chart_session,"chart_session":chart_session,"regular_session":regular,"db":str(DB),"symbols":0,"latest_bar_ms":None,"stale_symbols":0,"stale_symbol_list":[],"gap_symbols":0,"gap_symbol_list":[],"invalid_rows":0,"invalid_symbol_list":[],"quarantine_symbols":[],"message":"candle store healthy"}
    if not DB.exists(): result.update(status="error",message="common1m.sqlite3 is missing"); OUT.write_text(json.dumps(result,indent=2)+"\n"); print(json.dumps(result)); return
    with sqlite3.connect(DB) as db:
        result["symbols"]=db.execute("select count(distinct symbol) from bars").fetchone()[0]
        result["latest_bar_ms"]=db.execute("select max(ts) from bars").fetchone()[0]
        invalid_rows=db.execute("select symbol,count(*) from bars where ts<=0 or open<=0 or high<=0 or low<=0 or close<=0 or high<low or volume<0 group by symbol").fetchall()
        result["invalid_rows"]=sum(x[1] for x in invalid_rows); result["invalid_symbol_list"]=[x[0] for x in invalid_rows]
        active=[s for s, in db.execute("select symbol from hot where reason='bought_chart' and requested_ms>=?",(now_ms-600_000,))]
        result["active_chart_symbols"]=len(active)
        recent=db.execute("select symbol,max(ts) from bars group by symbol").fetchall()
        latest={s:ts for s,ts in recent}
        if chart_session:
            result["stale_symbol_list"]=[s for s in active if latest.get(s) is None or now_ms-int(latest[s])>180_000]; result["stale_symbols"]=len(result["stale_symbol_list"])
        for symbol,_ in recent:
            ts=[r[0] for r in db.execute("select ts from bars where symbol=? order by ts desc limit 30",(symbol,)).fetchall()]
            # Overnight/weekend closures are expected. Only flag gaps inside
            # the same UTC trading-day bucket, where a 1-minute stream should
            # be contiguous (allowing one missing minute for feed rollover).
            def regular_bar(value):
                local=datetime.fromtimestamp(value/1000, NY)
                return local.weekday()<5 and dtime(9,30)<=local.time()<=dtime(16,0)
            if regular and any(a-b>120_000 and regular_bar(a) and regular_bar(b) and datetime.fromtimestamp(a/1000,NY).date()==now.date() for a,b in zip(ts,ts[1:])): result["gap_symbols"]+=1; result["gap_symbol_list"].append(symbol)
    result["quarantine_symbols"]=sorted(set(result["stale_symbol_list"]+result["gap_symbol_list"]+result["invalid_symbol_list"]))
    problems=[]
    if result["invalid_rows"]: problems.append(f"{result['invalid_rows']} invalid rows")
    if chart_session and result["stale_symbols"]: problems.append(f"{result['stale_symbols']} stale chart symbols")
    if result["gap_symbols"]: problems.append(f"{result['gap_symbols']} symbols have candle gaps")
    if problems:
        result.update(status="degraded",message="; ".join(problems)+"; quarantined symbols continue to be skipped individually")
    OUT.parent.mkdir(exist_ok=True); OUT.write_text(json.dumps(result,indent=2)+"\n"); print(json.dumps(result))
if __name__=="__main__": main()
