# Adapted from Ghostty (https://github.com/ghostty-org/ghostty),
# src/shell-integration/zsh/.zshenv at 1b22f331a58a5be5ede2f1d76b7793d4130b4072.
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

# Based on (started as) a copy of Kitty's zsh integration. Kitty is
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

# This script is sourced automatically by zsh when ZDOTDIR is set to this
# directory. It therefore assumes it's running within our shell integration
# environment and should not be sourced manually (unlike tapust-integration).
#
# This file can get sourced with aliases enabled. To avoid alias expansion
# we quote everything that can be quoted. Some aliases will still break us
# though.

# Restore the original ZDOTDIR value if TAPUST_ZSH_ZDOTDIR is set.
# Otherwise, unset the ZDOTDIR that was set during shell injection.
if [[ -n "${TAPUST_ZSH_ZDOTDIR+X}" ]]; then
    'builtin' 'export' ZDOTDIR="$TAPUST_ZSH_ZDOTDIR"
    'builtin' 'unset' 'TAPUST_ZSH_ZDOTDIR'
else
    'builtin' 'unset' 'ZDOTDIR'
fi

# Use try-always to have the right error code.
{
    # Zsh treats unset ZDOTDIR as if it was HOME. We do the same.
    #
    # Source the user's .zshenv before sourcing tapust-integration because the
    # former might set fpath and other things without which tapust-integration
    # won't work.
    #
    # Use typeset in case we are in a function with warn_create_global in
    # effect. Unlikely but better safe than sorry.
    'builtin' 'typeset' _tapust_file=${ZDOTDIR-$HOME}"/.zshenv"
    # Zsh ignores unreadable rc files. We do the same.
    # Zsh ignores rc files that are directories, and so does source.
    [[ ! -r "$_tapust_file" ]] || 'builtin' 'source' '--' "$_tapust_file"
} always {
    if [[ -o 'interactive' ]]; then
        # ${(%):-%x} is the path to the current file.
        # On top of it we add :A:h to get the directory.
        'builtin' 'typeset' _tapust_file="${${(%):-%x}:A:h}"/tapust-integration
        if [[ -r "$_tapust_file" ]]; then
            'builtin' 'autoload' '-Uz' '--' "$_tapust_file"
            "${_tapust_file:t}"
            'builtin' 'unfunction' '--' "${_tapust_file:t}"
        fi
    fi
    'builtin' 'unset' '_tapust_file'
}
