"""Apply textual updates to settings."""


def _parse_value(raw: str) -> bool | int:
    if raw in {"true", "false"}:
        return raw == "true"
    return int(raw)


def apply_updates(current: dict[str, bool | int], lines: list[str]) -> dict[str, bool | int]:
    for line in lines:
        if not line:
            continue
        key, raw = line.split("=", 1)
        current[key] = _parse_value(raw)
    return current
