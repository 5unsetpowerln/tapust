# Adapted from Ghostty (https://github.com/ghostty-org/ghostty),
# src/shell-integration/elvish/lib/ghostty-integration.elv at 1b22f331a58a5be5ede2f1d76b7793d4130b4072.
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

{
  use platform
  use str

  # Clean up XDG_DATA_DIRS by removing TAPUST_SHELL_INTEGRATION_XDG_DIR
  if (and (has-env TAPUST_SHELL_INTEGRATION_XDG_DIR) (has-env XDG_DATA_DIRS)) {
    set-env XDG_DATA_DIRS (str:replace $E:TAPUST_SHELL_INTEGRATION_XDG_DIR":" "" $E:XDG_DATA_DIRS)
    unset-env TAPUST_SHELL_INTEGRATION_XDG_DIR
  }

  # List of enabled shell integration features
  var features = [(str:split ',' $E:TAPUST_SHELL_FEATURES)]

  # State tracking for semantic prompt sequences
  # Values: 'prompt-start', 'pre-exec', 'post-exec'
  fn set-prompt-state {|new| set-env __tapust_prompt_state $new }

  fn mark-prompt-start {
    if (not-eq $E:__tapust_prompt_state 'prompt-start') {
      printf "\e]133;D;aid="$pid"\a"
    }
    set-prompt-state 'prompt-start'
    printf "\e]133;A;aid="$pid"\a"
  }

  fn mark-output-start {|_|
    set-prompt-state 'pre-exec'
    printf "\e]133;C\a"
  }

  fn mark-output-end {|cmd-info|
    set-prompt-state 'post-exec'

    var exit-status = 0

    # in case of error: retrieve exit status,
    # unless does not exist (= builtin function failure), then default to 1
    if (not-eq $nil $cmd-info[error]) {
      set exit-status = 1

      if (has-key $cmd-info[error] reason) {
        if (has-key $cmd-info[error][reason] exit-status) {
          set exit-status = $cmd-info[error][reason][exit-status]
        }
      }
    }

    printf "\e]133;D;"$exit-status";aid="$pid"\a"
  }

  # NOTE: OSC 133;B (end of prompt, start of input) cannot be reliably
  # implemented at the script level in Elvish. The prompt function's output is
  # escaped, and writing to /dev/tty has timing issues because Elvish renders
  # its prompts on a background thread. Full semantic prompt support requires a
  # native implementation: https://github.com/elves/elvish/pull/1917

  fn sudo-with-terminfo {|@args|
    var sudoedit = $false
    for arg $args {
      if (str:has-prefix $arg --) {
        if (eq $arg --edit) {
          set sudoedit = $true
          break
        }
      } elif (str:has-prefix $arg -) {
        if (str:contains (str:trim-prefix $arg -) e) {
          set sudoedit = $true
          break
        }
      } elif (not (str:contains $arg =)) {
        break
      }
    }

    if (not $sudoedit) { set args = [ --preserve-env=TERMINFO $@args ] }
    (external sudo) $@args
  }

  # SSH Integration
  #
  # Wrap `ssh` with `tapust +ssh` and translate the shell-integration
  # feature flags into command options.
  fn ssh-integration {|@args|
    var tapust = $E:TAPUST_BIN_DIR/"tapust"
    var flags = []
    if (not (has-value $features ssh-env)) {
      set flags = (conj $flags --forward-env=false)
    }
    if (not (has-value $features ssh-terminfo)) {
      set flags = (conj $flags --terminfo=false)
    }
    $tapust +ssh $@flags -- $@args
  }

  defer {
    mark-prompt-start
  }

  set edit:before-readline = (conj $edit:before-readline $mark-prompt-start~)
  set edit:after-readline  = (conj $edit:after-readline $mark-output-start~)
  set edit:after-command   = (conj $edit:after-command $mark-output-end~)

  if (str:contains $E:TAPUST_SHELL_FEATURES "cursor") {
    var cursor = "5"    # blinking bar
    if (has-value $features cursor:steady) {
      set cursor = "6"  # steady bar
    }

    fn beam  { printf "\e["$cursor" q" }
    fn reset { printf "\e[0 q" }
    set edit:before-readline = (conj $edit:before-readline $beam~)
    set edit:after-readline  = (conj $edit:after-readline {|_| reset })
  }
  if (and (has-value $features path) (has-env TAPUST_BIN_DIR)) {
    if (not (has-value $paths $E:TAPUST_BIN_DIR)) {
        set paths = [$@paths $E:TAPUST_BIN_DIR]
    }
  }
  if (and (has-value $features sudo) (not-eq "" $E:TERMINFO) (has-external sudo)) {
    edit:add-var sudo~ $sudo-with-terminfo~
  }
  if (and (str:contains $E:TAPUST_SHELL_FEATURES ssh-) (has-external ssh)) {
    edit:add-var ssh~ $ssh-integration~
  }

  # Report changes to the current directory.
  fn report-pwd { printf "\e]7;kitty-shell-cwd://%s%s\a" (platform:hostname) $pwd }
  set after-chdir = (conj $after-chdir {|_| report-pwd })
  report-pwd
}
