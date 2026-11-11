# tapust shell integration for fish, found through the XDG_DATA_DIRS entry
# tapust added. fish reports the prompt, commands and directory itself, so this
# only sets the cursor shape.

if set -q TAPUST_XDG_DATA_DIRS
    set -gx XDG_DATA_DIRS $TAPUST_XDG_DATA_DIRS
    set -e TAPUST_XDG_DATA_DIRS
else
    set -e XDG_DATA_DIRS
end

if status is-interactive
    function __tapust_prompt --on-event fish_prompt
        if test "$TAPUST_CURSOR_BLINK" = 1
            printf '\e[5 q'
        else
            printf '\e[6 q'
        end
    end

    function __tapust_preexec --on-event fish_preexec
        printf '\e[0 q'
    end
end
