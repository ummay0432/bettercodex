import re


INTEGER = re.compile(r"-?(0|[1-9][0-9]*)")


def parse_value(raw: str) -> bool | int:
    if raw == "true":
        return True
    if raw == "false":
        return False
    if INTEGER.fullmatch(raw) is None:
        raise ValueError("invalid setting value")
    return int(raw)
