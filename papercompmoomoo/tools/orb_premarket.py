"""Research/paper-only premarket NASDAQ ranking for the ORB strategy."""
from __future__ import annotations
import json, math, sys
from datetime import datetime, time as dtime
from pathlib import Path
from zoneinfo import ZoneInfo

ROOT = Path(__file__).resolve().parents[1]
if str(ROOT / "src") not in sys.path:
    sys.path.insert(0, str(ROOT / "src"))
UNIVERSE = ROOT / "data/nasdaq.json"
OUT = ROOT / "data/orb_premarket.json"
NY = ZoneInfo("America/New_York")
OPEN = dtime(9, 30)

def _capture_candidate_candles(codes: list[str]) -> int:
    """Refresh only the selected Moomoo candidates once per scanner cycle."""
    if not codes:
        return 0
    from futu import AuType, KLType, OpenQuoteContext, RET_OK, Session, SubType
    from papercompmoomoo.microsense import write_minutes
    from papercompmoomoo.moomoo_feed import bars_from_kline
    q = OpenQuoteContext(host="127.0.0.1", port=11111)
    captured = 0
    try:
        q.subscribe(codes, [SubType.K_1M], subscribe_push=False, session=Session.ALL)
        for code in codes:
            ret, frame = q.get_cur_kline(code, num=100, ktype=KLType.K_1M, autype=AuType.NONE)
            if ret == RET_OK and frame is not None and len(frame):
                captured += write_minutes(ROOT / "data/common1m.sqlite3", code, bars_from_kline(frame, 1))
    finally:
        q.close()
    return captured

def _opend_candidates(nasdaq_set: set[str]) -> list[dict]:
    # Gainers owns the single Moomoo US leaderboard poll. ORB consumes its
    # recent local snapshot, then requests only candidate-specific candles.
    from papercompmoomoo.shared_movers import read
    out = []
    for mover in read():
        code = str(mover.get("security") or "")
        if code not in nasdaq_set:
            continue
        last = float(mover.get("price") or 0)
        gap = float(mover.get("change_ratio") or 0)
        volume = float(mover.get("volume") or 0)
        prior_close = last / (1.0 + gap / 100.0) if last > 0 and gap > -99.0 else 0.0
        if last < 2 or prior_close <= 0 or volume < 100_000 or gap < 3:
            continue
        out.append({"symbol": code, "price": last, "prior_close": prior_close,
                    "gap_pct": gap, "premarket_volume": volume,
                    "score": gap * math.log1p(volume)})
    return out

def main() -> None:
    from papercompmoomoo import darwin_qos
    darwin_qos.set_interactive()
    now = datetime.now(NY)
    if now.weekday() >= 5 or not dtime(4, 0) <= now.time() < dtime(16, 0):
        print(json.dumps({"status": "idle", "reason": "outside 04:00-16:00 ET Moomoo candidate window"}))
        return
    raw_universe = json.loads(UNIVERSE.read_text())
    nasdaq_set = {s if s.startswith("US.") else "US." + s for s in raw_universe}
    candidates = _opend_candidates(nasdaq_set)
    candidates.sort(key=lambda x: x["score"], reverse=True)
    payload = {"at": now.isoformat(), "strategy": "nasdaq_orb_premarket", "source": "shared_gainer_moomoo_movers", "top_n": 20, "candidates": candidates[:20], "scanned": len(nasdaq_set)}
    payload["captured_bars"] = _capture_candidate_candles([item["symbol"] for item in payload["candidates"]])
    # Persist the first time each ORB candidate enters the selected universe,
    # even before the regular-session execution loop starts.
    from orb_ledger import connect
    detected_ms = int(now.timestamp() * 1000)
    with connect(ROOT / "data/orb.sqlite3") as db:
        for rank, item in enumerate(payload["candidates"], 1):
            db.execute("INSERT INTO candidates(session,symbol,rank,score,gap_pct,premarket_volume,status,detected_ms) VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(session,symbol) DO UPDATE SET rank=excluded.rank,score=excluded.score,gap_pct=excluded.gap_pct,premarket_volume=excluded.premarket_volume,status=excluded.status,detected_ms=coalesce(candidates.detected_ms,excluded.detected_ms)", (now.date().isoformat(), item["symbol"].removeprefix("US."), rank, float(item.get("score", 0)), float(item.get("gap_pct", 0)), float(item.get("premarket_volume", 0)), "selected", detected_ms))
    OUT.write_text(json.dumps(payload, indent=2) + "\n")
    print(json.dumps({"scanned": len(nasdaq_set), "candidates": len(candidates), "selected": len(payload["candidates"]), "captured_bars": payload["captured_bars"], "at": payload["at"], "source": "shared_gainer_moomoo_movers"}))

if __name__ == "__main__": main()
