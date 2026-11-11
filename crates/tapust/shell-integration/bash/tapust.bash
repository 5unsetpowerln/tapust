# Adapted from Ghostty (https://github.com/ghostty-org/ghostty),
# src/shell-integration/bash/ghostty.bash at 1b22f331a58a5be5ede2f1d76b7793d4130b4072.
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

# Parts of this script are based on Kitty's bash integration. Kitty is
# distributed under GPLv3, so this file is also distributed under GPLv3.
# The license header is reproduced below:
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program.  If not, see <http://www.gnu.org/licenses/>.

# We need to be in interactive mode to proceed.
if [[ "$-" != *i* ]]; then builtin return; fi

# When automatic shell integration is active, we were started in POSIX
# mode and need to manually recreate the bash startup sequence.
if [ -n "$TAPUST_BASH_INJECT" ]; then
  # Store a temporary copy of our startup flags and unset these global
  # environment variables so we can safely handle reentrancy.
  builtin declare __tapust_bash_flags="$TAPUST_BASH_INJECT"
  builtin unset ENV TAPUST_BASH_INJECT

  # Restore an existing ENV that was replaced by the shell integration code.
  if [[ -n "$TAPUST_BASH_ENV" ]]; then
    builtin export ENV=$TAPUST_BASH_ENV
    builtin unset TAPUST_BASH_ENV
  fi

  # Restore bash's default 'posix' behavior. Also reset 'inherit_errexit',
  # which doesn't happen as part of the 'posix' reset.
  builtin set +o posix
  builtin shopt -u inherit_errexit 2>/dev/null

  # Unexport HISTFILE if it was set by the shell integration code.
  if [[ -n "$TAPUST_BASH_UNEXPORT_HISTFILE" ]]; then
    builtin export -n HISTFILE
    builtin unset TAPUST_BASH_UNEXPORT_HISTFILE
  fi

  # Manually source the startup files. See INVOCATION in bash(1) and
  # run_startup_files() in shell.c in the Bash source code.
  if builtin shopt -q login_shell; then
    if [[ $__tapust_bash_flags != *"--noprofile"* ]]; then
      [ -r /etc/profile ] && builtin source "/etc/profile"
      for __tapust_rcfile in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
        [ -r "$__tapust_rcfile" ] && {
          builtin source "$__tapust_rcfile"
          break
        }
      done
    fi
  else
    if [[ $__tapust_bash_flags != *"--norc"* ]]; then
      # The location of the system bashrc is determined at bash build
      # time via -DSYS_BASHRC and can therefore vary across distros:
      #  Arch, Debian, Ubuntu use /etc/bash.bashrc
      #  Fedora uses /etc/bashrc sourced from ~/.bashrc instead of SYS_BASHRC
      #  Void Linux uses /etc/bash/bashrc
      #  Nixos uses /etc/bashrc
      for __tapust_rcfile in /etc/bash.bashrc /etc/bash/bashrc /etc/bashrc; do
        [ -r "$__tapust_rcfile" ] && {
          builtin source "$__tapust_rcfile"
          break
        }
      done
      if [[ -z "$TAPUST_BASH_RCFILE" ]]; then TAPUST_BASH_RCFILE="$HOME/.bashrc"; fi
      [ -r "$TAPUST_BASH_RCFILE" ] && builtin source "$TAPUST_BASH_RCFILE"
    fi
  fi

  builtin unset __tapust_rcfile
  builtin unset __tapust_bash_flags
  builtin unset TAPUST_BASH_RCFILE
fi

# Add tapust binary to PATH if the path feature is enabled
if [[ "$TAPUST_SHELL_FEATURES" == *"path"* && -n "$TAPUST_BIN_DIR" ]]; then
  if [[ ":$PATH:" != *":$TAPUST_BIN_DIR:"* ]]; then
    export PATH="$PATH:$TAPUST_BIN_DIR"
  fi
fi

# Sudo
if [[ "$TAPUST_SHELL_FEATURES" == *"sudo"* && -n "$TERMINFO" ]]; then
  # Wrap `sudo` command to ensure tapust terminfo is preserved.
  #
  # This approach supports wrapping a `sudo` alias, but the alias definition
  # must come _after_ this function is defined. Otherwise, the alias expansion
  # will take precedence over this function, and it won't be wrapped.
  function sudo {
    builtin local sudo_has_sudoedit_flags="no"
    for arg in "$@"; do
      # Check if argument is '-e' or '--edit' (sudoedit flags)
      if [[ "$arg" == "-e" || $arg == "--edit" ]]; then
        sudo_has_sudoedit_flags="yes"
        builtin break
      fi
      # Check if argument is neither an option nor a key-value pair
      if [[ "$arg" != -* && "$arg" != *=* ]]; then
        builtin break
      fi
    done
    if [[ "$sudo_has_sudoedit_flags" == "yes" ]]; then
      builtin command sudo "$@"
    else
      builtin command sudo --preserve-env=TERMINFO "$@"
    fi
  }
fi

# SSH Integration
#
# Wrap `ssh` with `tapust +ssh` and translate the shell-integration
# feature flags into command options.
if [[ "$TAPUST_SHELL_FEATURES" == *ssh-* ]]; then
  function ssh() {
    builtin local -a flags
    flags=()
    [[ "$TAPUST_SHELL_FEATURES" != *ssh-env* ]] && flags+=(--forward-env=false)
    [[ "$TAPUST_SHELL_FEATURES" != *ssh-terminfo* ]] && flags+=(--terminfo=false)
    "$TAPUST_BIN_DIR/tapust" +ssh "${flags[@]}" -- "$@"
  }
fi

# This is set to 1 when we're executing a command so that we don't
# send prompt marks multiple times.
_tapust_executing=""
_tapust_last_reported_cwd=""

function __tapust_precmd() {
  local ret="${1:-$?}"
  if test "$_tapust_executing" != "0"; then
    _TAPUST_SAVE_PS1="$PS1"
    _TAPUST_SAVE_PS2="$PS2"

    # Use 133;P (not 133;A) inside PS1 to avoid fresh-line behavior on
    # readline redraws (e.g., vi mode switches, Ctrl-L). The initial
    # 133;A with fresh-line is emitted once via printf below.
    PS1='\[\e]133;P;k=i\a\]'$PS1'\[\e]133;B\a\]'
    PS2='\[\e]133;P;k=s\a\]'$PS2'\[\e]133;B\a\]'

    # Bash doesn't redraw the leading lines in a multiline prompt so we mark
    # the start of each line (after each newline) as a secondary prompt. This
    # correctly handles multiline prompts by setting the first to primary and
    # the subsequent lines to secondary.
    #
    # We only replace the \n prompt escape, not literal newlines ($'\n'),
    # because literal newlines may appear inside $(...) command substitutions
    # where inserting escape sequences would break shell syntax.
    if [[ "$PS1" == *"\n"* ]]; then
      PS1="${PS1//\\n/\\n$'\\[\\e]133;P;k=s\\a\\]'}"
    fi

    # Cursor
    if [[ "$TAPUST_SHELL_FEATURES" == *"cursor"* ]]; then
      builtin local cursor=5  # blinking bar
      [[ "$TAPUST_SHELL_FEATURES" == *"cursor:steady"* ]] && cursor=6  # steady bar

      [[ "$PS1" != *"\[\e[${cursor} q\]"* ]] && PS1=$PS1"\[\e[${cursor} q\]"
      [[ "$PS0" != *'\[\e[0 q\]'* ]] && PS0=$PS0'\[\e[0 q\]' # reset
    fi

    # Title (working directory)
    if [[ "$TAPUST_SHELL_FEATURES" == *"title"* ]]; then
      PS1=$PS1'\[\e]2;\w\a\]'
    fi
  fi

  if test "$_tapust_executing" != ""; then
    # End of current command. Report its status.
    builtin printf "\e]133;D;%s;aid=%s\a" "$ret" "$BASHPID"
  fi

  # Fresh line and start of prompt. When ble.sh is active, emit 133;P instead
  # of 133;A because ble.sh maintains its own cursor position tracking. 133;A's
  # cursor movement (CR+LF when not at column 0) is invisible to ble.sh and
  # desyncs its position state, causing display artifacts like duplicate
  # prompts. See: https://github.com/akinomyoga/ble.sh/issues/684
  if [[ -n "${BLE_VERSION-}" ]]; then
    builtin printf "\e]133;P;k=i\a"
  else
    builtin printf "\e]133;A;redraw=last;cl=line;aid=%s\a" "$BASHPID"
  fi

  # unfortunately bash provides no hooks to detect cwd changes
  # in particular this means cwd reporting will not happen for a
  # command like cd /test && cat. PS0 is evaluated before cd is run.
  if [[ "$_tapust_last_reported_cwd" != "$PWD" ]]; then
    _tapust_last_reported_cwd="$PWD"
    # Remove control bytes from the hostname and directory before sending
    # them. A directory name that contains a raw ESC or BEL byte could
    # otherwise end our OSC 7 early and start a sequence of its own, such
    # as an OSC 52 clipboard write. Backslashes are safe here because
    # printf only expands escapes in the format string, not in %s values.
    builtin printf "\e]7;kitty-shell-cwd://%s%s\a" "${HOSTNAME//[[:cntrl:]]/}" "${PWD//[[:cntrl:]]/}"
  fi

  _tapust_executing=0
}

function __tapust_preexec() {
  builtin local cmd="$1"

  PS1="$_TAPUST_SAVE_PS1"
  PS2="$_TAPUST_SAVE_PS2"

  # Title (current command)
  if [[ -n $cmd && "$TAPUST_SHELL_FEATURES" == *"title"* ]]; then
    builtin printf "\e]2;%s\a" "${cmd//[[:cntrl:]]/}"
  fi

  # End of input, start of output.
  builtin printf "\e]133;C;\a"
  _tapust_executing=1
}

if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
  __tapust_preexec_hook() {
    builtin local cmd
    cmd=$(LC_ALL=C HISTTIMEFORMAT='' builtin history 1)
    cmd="${cmd#*[[:digit:]][* ] }"  # remove leading history number
    [[ -n "$cmd" ]] && __tapust_preexec "$cmd"
  }

  # Bash 5.1+ restores the command status for each PROMPT_COMMAND array entry.
  # Scalar values need to save and restore it before existing commands run.
  __tapust_restore_status() {
    builtin return "$1"
  }

  __tapust_hook() {
    builtin local ret="${__tapust_status:-$?}"
    builtin unset __tapust_status
    __tapust_precmd "$ret"

    # Append preexec hook to PS0 if not already present.
    # Use function substitution in 5.3+, otherwise command substitution.
    if [[ "$PS0" != *"__tapust_preexec_hook"* ]]; then
      if (( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 3) )); then
        # shellcheck disable=SC2016
        PS0+='${ __tapust_preexec_hook; }'
      else
        # shellcheck disable=SC2016
        PS0+='$(__tapust_preexec_hook >/dev/tty)'
      fi
    fi
  }

  # Append our hook to PROMPT_COMMAND, preserving its existing type. Array
  # entries receive their command's status directly, while scalar values
  # need a leading status capture before existing commands run.
  #
  # The 2>/dev/null suppresses "command not found" in subshells that inherit
  # PROMPT_COMMAND without the function definitions. This also silences any
  # errors from inside our hooks, but those are all terminal escape sequences
  # and non-actionable.
  # shellcheck disable=SC2128,SC2178,SC2179
  if [[ "${PROMPT_COMMAND[*]:-}" != *"__tapust_hook 2>/dev/null"* ]]; then
    if [[ -z "${PROMPT_COMMAND[*]}" ]]; then
      if (( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 1) )); then
        PROMPT_COMMAND=("__tapust_hook 2>/dev/null")
      else
        PROMPT_COMMAND="__tapust_hook 2>/dev/null"
      fi
    elif [[ $(builtin declare -p PROMPT_COMMAND 2>/dev/null) == "declare -a"* ]]; then
      PROMPT_COMMAND+=("__tapust_hook 2>/dev/null")
    else
      PROMPT_COMMAND='__tapust_status=$?;if builtin declare -F __tapust_restore_status >/dev/null;then __tapust_restore_status "$__tapust_status";else (exit "$__tapust_status");fi;'"${PROMPT_COMMAND}"
      [[ "${PROMPT_COMMAND}" =~ (\;[[:space:]]*|$'\n')$ ]] || PROMPT_COMMAND+=";"
      PROMPT_COMMAND+="__tapust_hook 2>/dev/null"
    fi
  fi
else
  builtin source "$(dirname -- "${BASH_SOURCE[0]}")/bash-preexec.sh"
  preexec_functions+=(__tapust_preexec)
  precmd_functions+=(__tapust_precmd)
fi
