# Adapted from Ghostty (https://github.com/ghostty-org/ghostty),
# src/shell-integration/fish/vendor_conf.d/ghostty-shell-integration.fish at 1b22f331a58a5be5ede2f1d76b7793d4130b4072.
#
# MIT License
#
# Copyright (c) 2024 Mitchell Hashimoto, Ghostty contributors
#
# Permission is hereby granted, free of charge, to any person obtaining a copy
# of this software and associated documentation files (the "Software"), to deal
# in the Software without restriction, including without limitation the rights
# to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
# copies of the Software, and to permit persons to whom the Software is
# furnished to do so, subject to the following conditions:
#
# The above copyright notice and this permission notice shall be included in all
# copies or substantial portions of the Software.
#
# THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
# IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
# FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
# AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
# LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
# OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
# SOFTWARE.

# This shell script aims to be written in a way where it can't really fail
# or all failure scenarios are handled, so that we never leave the shell in
# a weird state. If you find a way to break this, please report a bug!

function tapust_restore_xdg_data_dir -d "restore the original XDG_DATA_DIR value"
    # If we don't have our own data dir then we don't need to do anything.
    if not set -q TAPUST_SHELL_INTEGRATION_XDG_DIR
        return
    end

    # If the data dir isn't set at all then we don't need to do anything.
    if not set -q XDG_DATA_DIRS
        return
    end

    # We need to do this so that XDG_DATA_DIRS turns into an array.
    set --function --path xdg_data_dirs "$XDG_DATA_DIRS"

    # If our data dir is in the list then remove it.
    if set --function index (contains --index "$TAPUST_SHELL_INTEGRATION_XDG_DIR" $xdg_data_dirs)
        set --erase --function xdg_data_dirs[$index]
    end

    # Re-export our data dir
    if set -q xdg_data_dirs[1]
        set --global --export --unpath XDG_DATA_DIRS "$xdg_data_dirs"
    else
        set --erase --global XDG_DATA_DIRS
    end

    set --erase TAPUST_SHELL_INTEGRATION_XDG_DIR
end

function tapust_exit -d "exit the shell integration setup"
    functions -e tapust_restore_xdg_data_dir
    functions -e tapust_exit
    exit 0
end

# We always try to restore the XDG data dir
tapust_restore_xdg_data_dir

# If we aren't interactive or we've already run, don't run.
status --is-interactive || tapust_exit

# We do the full setup on the first prompt render. We do this so that other
# shell integrations that setup the prompt and modify things are able to run
# first. We want to run _last_.
function __tapust_setup --on-event fish_prompt -d "Setup tapust integration"
    functions -e __tapust_setup

    set --local features (string split , $TAPUST_SHELL_FEATURES)

    # Parse the fish version for feature detection.
    # Default to 0.0 if version is unavailable or malformed.
    set -l fish_major 0
    set -l fish_minor 0
    if set -q version[1]
        set -l fish_ver (string match -r '(\d+)\.(\d+)' -- $version[1])
        if set -q fish_ver[2]; and test -n "$fish_ver[2]"
            set fish_major "$fish_ver[2]"
        end
        if set -q fish_ver[3]; and test -n "$fish_ver[3]"
            set fish_minor "$fish_ver[3]"
        end
    end

    # Our OSC133A (prompt start) sequence. If we're using Fish >= 4.1
    # then it supports click_events so we enable that.
    set -g __tapust_prompt_start_mark "\e]133;A\a"
    if test "$fish_major" -gt 4; or test "$fish_major" -eq 4 -a "$fish_minor" -ge 1
        set -g __tapust_prompt_start_mark "\e]133;A;click_events=1\a"
    end

    if string match -q 'cursor*' -- $features
        set -l cursor 5                                   # blinking bar
        contains cursor:steady $features && set cursor 6  # steady bar

        # Change the cursor to a beam on prompt.
        function __tapust_set_cursor_beam --on-event fish_prompt -V cursor -d "Set cursor shape"
            if not functions -q fish_vi_cursor_handle
                echo -en "\e[$cursor q"
            end
        end
        function __tapust_reset_cursor --on-event fish_preexec -d "Reset cursor shape"
            if not functions -q fish_vi_cursor_handle
                echo -en "\e[0 q"
            end
        end
    end

    # Add tapust binary to PATH if the path feature is enabled
    if contains path $features; and test -n "$TAPUST_BIN_DIR"
        fish_add_path --global --path --append "$TAPUST_BIN_DIR"
    end

    # When using sudo shell integration feature, ensure $TERMINFO is set
    # and `sudo` is not already a function or alias
    if contains sudo $features; and test -n "$TERMINFO"; and test file = (type -t sudo 2> /dev/null; or echo "x")
        # Wrap `sudo` command to ensure tapust terminfo is preserved
        function sudo -d "Wrap sudo to preserve terminfo"
            set --function sudo_has_sudoedit_flags no
            for arg in $argv
                # Check if argument is '-e' or '--edit' (sudoedit flags)
                if string match -q -- -e "$arg"; or string match -q -- --edit "$arg"
                    set --function sudo_has_sudoedit_flags yes
                    break
                end
                # Check if argument is neither an option nor a key-value pair
                if not string match -r -q -- "^-" "$arg"; and not string match -r -q -- "=" "$arg"
                    break
                end
            end
            if test "$sudo_has_sudoedit_flags" = yes
                command sudo $argv
            else
                command sudo --preserve-env=TERMINFO $argv
            end
        end
    end

    # SSH Integration
    #
    # Wrap `ssh` with `tapust +ssh` and translate the shell-integration
    # feature flags into command options.
    set -l features (string split ',' -- "$TAPUST_SHELL_FEATURES")
    if contains ssh-env $features; or contains ssh-terminfo $features
        function ssh --wraps=ssh --description "SSH wrapper with tapust integration"
            set -l features (string split ',' -- "$TAPUST_SHELL_FEATURES")
            set -l flags
            contains ssh-env $features; or set -a flags --forward-env=false
            contains ssh-terminfo $features; or set -a flags --terminfo=false
            "$TAPUST_BIN_DIR/tapust" +ssh $flags -- $argv
        end
    end

    # Setup prompt marking
    function __tapust_mark_prompt_start --on-event fish_prompt --on-event fish_posterror
        # If we never got the output end event, then we need to send it now.
        if test "$__tapust_prompt_state" != prompt-start
            echo -en "\e]133;D\a"
        end

        set --global __tapust_prompt_state prompt-start
        echo -en $__tapust_prompt_start_mark
    end

    function __tapust_mark_output_start --on-event fish_preexec
        set --global __tapust_prompt_state pre-exec
        echo -en "\e]133;C\a"
    end

    function __tapust_mark_output_end --on-event fish_postexec
        set --global __tapust_prompt_state post-exec
        echo -en "\e]133;D;$status\a"
    end

    # Report pwd. This is actually built-in to fish but only for terminals
    # that match an allowlist and that isn't us.
    function __update_cwd_osc --on-variable PWD -d 'Notify capable terminals when $PWD changes'
        if status --is-command-substitution || set -q INSIDE_EMACS
            return
        end
        printf \e\]7\;file://%s%s\a $hostname (string escape --style=url $PWD)
    end

    # Enable fish to handle reflow because Ghostty clears the prompt on resize.
    set --global fish_handle_reflow 1

    # Initial calls for first prompt
    if string match -q 'cursor*' -- $features
        __tapust_set_cursor_beam
    end
    __tapust_mark_prompt_start
    __update_cwd_osc
end

tapust_exit
