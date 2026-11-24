#!/bin/sh
[ "$1" = --remote=false ] || exit 97
case "$2" in
ps) [ "$3" = --all ] && [ "$4" = --filter ] && [ "$5" = label=io.maestro.service=demo ] && [ "$6" = --format ] && [ "$7" = json ] || exit 98; cat -- "${0%/*}/rows.json" ;;
container) [ "$3" = exists ] && [ "$4" = -- ] && [ -n "$5" ] || exit 99; exit "$(cat -- "${0%/*}/exists-code")" ;;
*) exit 96 ;;
esac
