# This file is private to one wtm shell; no user profile is modified.
[[ -o interactive ]] || return
zmodload zsh/net/socket || return
zmodload zsh/parameter || return
zsocket @SOCKET@ 2>/dev/null || return
typeset -g _wtm_control_fd=$REPLY
typeset -gi _wtm_prompt_epoch=0
print -r -u $_wtm_control_fd -- '{"kind":"shellIntegration","token":"@TOKEN@"}'

_wtm_prompt_ready() {
    emulate -L zsh
    [[ -n $_wtm_control_fd ]] || return 0
    (( ++_wtm_prompt_epoch ))
    print -r -u $_wtm_control_fd -- "prompt $_wtm_prompt_epoch ${#jobstates}"
}

_wtm_prompt_busy() {
    emulate -L zsh
    [[ -n $_wtm_control_fd ]] || return 0
    print -r -u $_wtm_control_fd -- "busy $_wtm_prompt_epoch"
}

_wtm_control_widget() {
    emulate -L zsh
    if [[ -n $2 ]]; then
        zle -F $1
        return
    fi
    local action epoch capability
    # A partial or stale control message must never freeze the line editor.
    if ! read -r -t 0.1 -u $1 action epoch capability; then
        # EOF remains readable forever; leaving its handler installed spins ZLE.
        zle -F $1
        exec {_wtm_control_fd}>&-
        unset _wtm_control_fd
        return
    fi
    [[ $action == run && ${#capability} == 32 && $capability != *[^0-9a-f]* ]] || return
    if [[ $epoch != $_wtm_prompt_epoch || $CONTEXT != start || -n $BUFFER || -n $PREBUFFER || ${#jobstates} != 0 || $PENDING != 0 || $KEYS_QUEUED_COUNT != 0 ]]; then
        print -r -u $_wtm_control_fd -- "refused $capability"
        return
    fi
    zle -I
    # Only a one-use capability crosses argv; the helper obtains the immutable
    # script privately. It closes the inherited control fd before running it.
    # ZLE's callback stdin is not the terminal. Reopen this shell's controlling
    # tty explicitly so interactive child scripts can read and own its foreground.
    @HELPER@ --shell-run @SOCKET@ "$capability" "$_wtm_control_fd" </dev/tty
    _wtm_prompt_ready
    zle redisplay
}

zle -N _wtm_control_widget
zle -F -w $_wtm_control_fd _wtm_control_widget
autoload -Uz add-zle-hook-widget
add-zle-hook-widget line-init _wtm_prompt_ready
add-zle-hook-widget line-finish _wtm_prompt_busy
