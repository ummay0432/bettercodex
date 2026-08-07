"""Ledger aggregation."""


def summarize(lines: list[str]) -> str:
    totals: dict[str, int] = {}
    for raw in lines:
        if not raw.strip():
            continue
        account, kind, amount = raw.strip().split(",")
        if kind not in {"charge", "refund"}:
            raise ValueError("unknown ledger kind")
        cents = int(float(amount) * 100)
        totals[account] = totals.get(account, 0) + cents
    return "\n".join(f"{account}={cents}" for account, cents in totals.items())
