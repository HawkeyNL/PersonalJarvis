#!/usr/bin/env bash
# Small, dependency-free presentation helpers for root-operated deployment
# scripts. Output remains plain and pipe-friendly unless stdout is a colour TTY.
if [[ -t 1 && -z ${NO_COLOR:-} && ${TERM:-dumb} != dumb ]]; then
    _jarvis_ui_blue=$'\033[36m'; _jarvis_ui_green=$'\033[32m'
    _jarvis_ui_yellow=$'\033[33m'; _jarvis_ui_red=$'\033[31m'; _jarvis_ui_dim=$'\033[2m'; _jarvis_ui_reset=$'\033[0m'
else
    _jarvis_ui_blue=''; _jarvis_ui_green=''; _jarvis_ui_yellow=''; _jarvis_ui_red=''; _jarvis_ui_dim=''; _jarvis_ui_reset=''
fi
ui_heading() { printf '\n%s%s%s\n' "$_jarvis_ui_blue" "$*" "$_jarvis_ui_reset"; }
ui_step() { printf '\n%s%s%s\n' "$_jarvis_ui_blue" "$*" "$_jarvis_ui_reset"; }
ui_success() { printf '%s✓%s %s\n' "$_jarvis_ui_green" "$_jarvis_ui_reset" "$*"; }
ui_warning() { printf '%s!%s %s\n' "$_jarvis_ui_yellow" "$_jarvis_ui_reset" "$*"; }
ui_error() { printf '%sERROR:%s %s\n' "$_jarvis_ui_red" "$_jarvis_ui_reset" "$*" >&2; }
ui_detail() { printf '%s%s%s\n' "$_jarvis_ui_dim" "$*" "$_jarvis_ui_reset"; }
ui_run() {
    local label=$1 output status
    shift
    if [[ ${JARVIS_VERBOSE:-0} == 1 ]]; then
        "$@"; ui_success "$label"; return
    fi
    output=$(mktemp)
    if "$@" >"$output" 2>&1; then
        rm -f -- "$output"; ui_success "$label"
    else
        status=$?
        ui_error "$label failed; diagnostics follow"
        cat "$output" >&2
        rm -f -- "$output"
        return "$status"
    fi
}

# Security-sensitive provisioning helpers intentionally require a controlling
# terminal so one-time secrets can never be redirected into captured output.
# Keep their stdin/stdout attached to the owner terminal in normal pretty mode.
ui_run_tty() {
    local label=$1
    shift
    [[ -t 0 && -t 1 && -r /dev/tty && -w /dev/tty ]] || {
        ui_error "$label requires an interactive terminal"
        return 1
    }
    ui_detail "$label …"
    if "$@"; then
        ui_success "$label"
    else
        local status=$?
        ui_error "$label failed; inspect the message above"
        return "$status"
    fi
}
