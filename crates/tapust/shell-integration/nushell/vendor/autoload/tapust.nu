# Adapted from Ghostty (https://github.com/ghostty-org/ghostty),
# src/shell-integration/nushell/vendor/autoload/ghostty.nu at 1b22f331a58a5be5ede2f1d76b7793d4130b4072.
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

# tapust shell integration
export module tapust {
  def has_feature [feature: string] {
    $feature in ($env.TAPUST_SHELL_FEATURES | default "" | split row ',')
  }

  # Wrap `ssh` with `tapust +ssh` and translate the shell-integration
  # feature flags into command options.
  @complete external
  export def --wrapped ssh [...args] {
    if not ((has_feature "ssh-env") or (has_feature "ssh-terminfo")) {
      ^ssh ...$args
      return
    }

    let tapust = ($env.TAPUST_BIN_DIR? | default "") | path join "tapust"
    mut flags = []
    if not (has_feature "ssh-env") {
      $flags = ($flags ++ ["--forward-env=false"])
    }
    if not (has_feature "ssh-terminfo") {
      $flags = ($flags ++ ["--terminfo=false"])
    }
    ^$tapust "+ssh" ...$flags "--" ...$args
  }

  # Wrap `sudo` to preserve tapust's TERMINFO environment variable
  @complete external
  export def --wrapped sudo [...args] {
    mut sudo_args = $args

    if (has_feature "sudo") {
      # Extract just the sudo options (before the command)
      let sudo_options = (
        $args | take until {|arg|
          not (($arg | str starts-with "-") or ($arg | str contains "="))
        }
      )

      # Prepend TERMINFO preservation flag if not using sudoedit
      if (not ("-e" in $sudo_options or "--edit" in $sudo_options)) {
        $sudo_args = ($args | prepend "--preserve-env=TERMINFO")
      }
    }

    ^sudo ...$sudo_args
  }
}

# Clean up XDG_DATA_DIRS by removing TAPUST_SHELL_INTEGRATION_XDG_DIR
if 'TAPUST_SHELL_INTEGRATION_XDG_DIR' in $env {
  if 'XDG_DATA_DIRS' in $env {
    $env.XDG_DATA_DIRS = ($env.XDG_DATA_DIRS | str replace $"($env.TAPUST_SHELL_INTEGRATION_XDG_DIR):" "")
  }
  hide-env TAPUST_SHELL_INTEGRATION_XDG_DIR
}
