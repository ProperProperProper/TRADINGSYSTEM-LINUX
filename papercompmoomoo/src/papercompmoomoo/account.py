"""Which simulated account the bot trades: the paper-trading COMPETITION account, found by its type and never assumed.

OpenD lists every account of the logged-in user, including a REAL one, the ordinary simulated accounts and the competition account. A call without an account id
uses the first simulated account, which is NOT the competition one. So the bot names the account on every call. The choice is made here, from the account list, and
fails closed: exactly one simulated, US-enabled account of the wanted type, else None (the bot then trades nothing).
"""

from __future__ import annotations

KIND = "COMPETITION"
STANDARD_KIND = "STOCK_AND_OPTION"


def pick_account(accounts, kind: str = KIND) -> int | None:
    """The ``acc_id`` of the single SIMULATE account of ``sim_acc_type`` ``kind`` that is enabled for the US market; None for none or several.

    ``accounts`` is the frame ``get_acc_list()`` returns (columns ``acc_id``, ``trd_env``, ``sim_acc_type``, ``trdmarket_auth``). A REAL account is never chosen.
    Supports 'STOCK_AND_OPTION', 'STANDARD', as well as 'COMPETITION'.
    """
    if accounts is None or len(accounts) == 0:
        return None
    matches = []
    for _, row in accounts.iterrows():
        if str(row.get("trd_env")) != "SIMULATE":
            continue
        sim_type = str(row.get("sim_acc_type"))
        # Allow standard paper account (STOCK_AND_OPTION or non-competition)
        if kind == "STANDARD" or kind == "STOCK_AND_OPTION":
            if sim_type != "STOCK_AND_OPTION" and sim_type != "STOCK":
                continue
        elif sim_type != kind:
            continue
        auth = row.get("trdmarket_auth")
        if "US" not in (list(auth) if isinstance(auth, (list, tuple, set)) else [str(auth)]):
            continue
        matches.append(int(row["acc_id"]))
    return matches[0] if len(matches) == 1 else None

