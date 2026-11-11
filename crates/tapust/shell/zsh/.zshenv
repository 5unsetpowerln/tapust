# tapust shell integration for zsh.
#
# tapust points ZDOTDIR at this directory, so zsh reads this file in place of
# the user's .zshenv. It restores ZDOTDIR and reads the user's .zshenv, and zsh
# then reads the user's other startup files from the restored ZDOTDIR.

if (( ${+TAPUST_ZSH_ZDOTDIR} )); then
    ZDOTDIR=$TAPUST_ZSH_ZDOTDIR
    unset TAPUST_ZSH_ZDOTDIR
else
    unset ZDOTDIR
fi

# Defined before the user's files run, so that their aliases do not apply here.
if [[ -o interactive ]] && zmodload zsh/sched; then
    __tapust_precmd() {
        local ret=$?
        emulate -L zsh
        local dir=$PWD url shape=6
        if (( ${+__tapust_ran} )); then
            printf '\e]133;D;%s\a' $ret
            unset __tapust_ran
        fi
        () {
            setopt localoptions extendedglob no_multibyte
            url=${PWD//(#m)[^A-Za-z0-9\/._~-]/%${(l:2::0:)$(( [##16] #MATCH ))}}
        }
        if [[ -n $HOME && ( $dir == $HOME || $dir == $HOME/* ) ]]; then
            dir=\~${dir#$HOME}
        fi
        if [[ $TAPUST_CURSOR_BLINK == 1 ]]; then shape=5; fi
        printf '\e]7;file://%s%s\a\e]2;%s\a\e[%s q' $HOST $url ${dir//[[:cntrl:]]/ } $shape
        # Runs after every precmd hook, which may rewrite the prompts.
        sched +0 __tapust_mark
    }

    __tapust_mark() {
        emulate -L zsh
        local a=$'%{\e]133;A;cl=m;redraw=1\a%}' s=$'%{\e]133;A;k=s\a%}' b=$'%{\e]133;B\a%}'
        if [[ $PS1 != ${a}*${b} ]]; then PS1=$a${${PS1//$a}//$b}$b; fi
        if [[ $PS2 != ${s}*${b} ]]; then PS2=$s${${PS2//$s}//$b}$b; fi
    }

    __tapust_preexec() {
        emulate -L zsh
        typeset -g __tapust_ran=1
        # $1 is empty when history is off.
        local cmd=${1:-$2}
        printf '\e]133;C\a\e]2;%s\a\e[0 q' ${cmd//[[:cntrl:]]/ }
    }

    # The hooks are added once the startup files have run, so that they cannot
    # replace them, and the first prompt is handled after their precmd hooks.
    sched +0 'precmd_functions+=(__tapust_precmd); preexec_functions+=(__tapust_preexec); __tapust_precmd'
fi

if [[ -r "${ZDOTDIR:-$HOME}/.zshenv" ]]; then
    source "${ZDOTDIR:-$HOME}/.zshenv"
fi
