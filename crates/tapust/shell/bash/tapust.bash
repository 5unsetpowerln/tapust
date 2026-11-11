# tapust shell integration for bash.
#
# tapust starts an interactive bash with this file as its --rcfile, which bash
# reads after the system-wide bashrc in place of ~/.bashrc. A login shell reads
# no rc file, so tapust starts it in POSIX mode, where the only startup file is
# $ENV, and points ENV here. Either way this file reads the user's startup files
# that bash would have read, then installs the hooks (bash >= 5.1).

if [[ -o posix ]]; then
    __tapust_login=1
    set +o posix
    # Starting in POSIX mode also changed these, and leaving it does not undo them.
    [[ ${BASHOPTS@a} == *x* ]] || shopt -u inherit_errexit
    [[ ${HISTFILE@a} != *x* && $HISTFILE == ~/.sh_history ]] && HISTFILE=~/.bash_history
    [[ ${MAILCHECK@a} != *x* && $MAILCHECK == 600 ]] && MAILCHECK=60
    if [[ ${TAPUST_BASH_ENV+set} ]]; then ENV=$TAPUST_BASH_ENV; else unset ENV; fi
fi

# Defined before the user's files run, so that their aliases do not apply here.

# Runs last in PROMPT_COMMAND, after the hooks that may rewrite the prompts.
__tapust_precmd() {
    local status=$? dir=$PWD url=$PWD i c p
    local a='\[\e]133;A;cl=line;redraw=last\a\]' s='\[\e]133;A;k=s\a\]' b='\[\e]133;B\a\]'
    # PS0 is shown only when a command runs; the subscript sets __tapust_ran
    # in this shell, and the expansion is empty.
    local x='\e]133;C\a${__tapust_ran[__tapust_ran=1]-}$(__tapust_preexec)\e[0 q'
    if [[ ${__tapust_ran-} ]]; then
        builtin printf '\e]133;D;%s\a' "$status"
        unset __tapust_ran
    fi
    if [[ $url == *[!-A-Za-z0-9/._~]* ]]; then
        local LC_ALL=C
        url=
        for ((i = 0; i < ${#PWD}; i++)); do
            c=${PWD:i:1}
            case $c in
            [-A-Za-z0-9/._~]) url+=$c ;;
            *) builtin printf -v c '%%%02X' "'$c"; url+=$c ;;
            esac
        done
    fi
    if [[ ${HOME-} && ($dir == "$HOME" || $dir == "$HOME"/*) ]]; then
        dir=\~${dir#"$HOME"}
    fi
    if [[ ${TAPUST_CURSOR_BLINK-} == 1 ]]; then c=5; else c=6; fi
    builtin printf '\e]7;file://%s%s\a\e]2;%s\a\e[%s q' "$HOSTNAME" "$url" "${dir//[[:cntrl:]]/ }" "$c"
    if [[ ${PS1-} != "$a"*"$b" ]]; then p=${PS1-}; p=${p//"$a"}; PS1=$a${p//"$b"}$b; fi
    if [[ ${PS2-} != "$s"*"$b" ]]; then p=${PS2-}; p=${p//"$s"}; PS2=$s${p//"$b"}$b; fi
    if [[ ${PS0-} != "$x"* ]]; then p=${PS0-}; PS0=$x${p//"$x"}; fi
    if [[ ${PROMPT_COMMAND[-1]} != __tapust_precmd ]]; then __tapust_hook; fi
}

# Expanded by PS0 in a subshell once a command line is read.
__tapust_preexec() {
    local cmd
    cmd=$(builtin fc -ln -0 2>/dev/null)
    cmd=${cmd#$'\t'[ *]}
    if [[ $cmd ]]; then builtin printf '\e]2;%s\a' "${cmd//[[:cntrl:]]/ }"; fi
}

# Moves __tapust_precmd to the end of PROMPT_COMMAND, and never to index 0,
# which a plain assignment to PROMPT_COMMAND replaces.
__tapust_hook() {
    local -a cmds=()
    local cmd
    for cmd in "${PROMPT_COMMAND[@]}"; do
        if [[ $cmd != __tapust_precmd ]]; then cmds+=("$cmd"); fi
    done
    PROMPT_COMMAND=("${cmds[@]:-}" __tapust_precmd)
}

if [[ ${__tapust_login-} ]]; then
    unset __tapust_login TAPUST_BASH_ENV
    if [[ ${TAPUST_BASH_NOPROFILE-} ]]; then
        unset TAPUST_BASH_NOPROFILE
    else
        if [[ -e /etc/profile ]]; then . /etc/profile; fi
        if [[ -e ~/.bash_profile ]]; then
            . ~/.bash_profile
        elif [[ -e ~/.bash_login ]]; then
            . ~/.bash_login
        elif [[ -e ~/.profile ]]; then
            . ~/.profile
        fi
    fi
else
    __tapust_rc=${TAPUST_BASH_RCFILE-~/.bashrc}
    unset TAPUST_BASH_RCFILE
    # bash opens a relative --rcfile from the working directory, not from PATH.
    if [[ $__tapust_rc != */* ]]; then __tapust_rc=./$__tapust_rc; fi
    if [[ -e $__tapust_rc ]]; then . "$__tapust_rc"; fi
    unset __tapust_rc
fi

if ((BASH_VERSINFO[0] * 100 + BASH_VERSINFO[1] >= 501)); then __tapust_hook; fi
