# tapust shell integration for nushell, found through the XDG_DATA_DIRS entry
# tapust added. nushell reports the prompt, commands and directory itself, so
# this only sets the cursor shape.

if 'TAPUST_XDG_DATA_DIRS' in $env {
    $env.XDG_DATA_DIRS = $env.TAPUST_XDG_DATA_DIRS
    hide-env TAPUST_XDG_DATA_DIRS
} else {
    hide-env XDG_DATA_DIRS
}

$env.config.hooks.pre_prompt = $env.config.hooks.pre_prompt? | default [] | append {||
    print -n (if $env.TAPUST_CURSOR_BLINK? == '1' { "\e[5 q" } else { "\e[6 q" })
}
$env.config.hooks.pre_execution = $env.config.hooks.pre_execution? | default [] | append {||
    print -n "\e[0 q"
}
