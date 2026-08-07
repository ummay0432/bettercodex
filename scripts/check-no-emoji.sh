#!/usr/bin/env bash
set -euo pipefail

emoji_pattern='(?=[^\x00-\x7F])(?!\x{2714})(?:\p{Emoji}|\p{Extended_Pictographic})|\x{FE0F}|\x{20E3}'
found=0

content_status=0
git grep --untracked --exclude-standard -n -I -P -e "$emoji_pattern" -- || content_status=$?
if ((content_status == 0)); then
    found=1
elif ((content_status != 1)); then
    exit "$content_status"
fi

path_status=0
git ls-files --cached --others --exclude-standard | rg --pcre2 -e "$emoji_pattern" || path_status=$?
if ((path_status == 0)); then
    found=1
elif ((path_status != 1)); then
    exit "$path_status"
fi

if ((found != 0)); then
    echo "error: emojis are not allowed in repository contents or paths" >&2
    exit 1
fi
