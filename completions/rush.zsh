#compdef rush
# zsh completion for rush's own invocation flags — completing `rush <TAB>`
# at a zsh prompt, not to be confused with rush's own (much larger)
# in-shell completion engine (src/completion.rs), which completes commands
# typed *inside* rush itself.
#
# Install: place on $fpath as `_rush` (e.g. a directory already on
# $fpath, or add one with `fpath+=(/path/to/this/dir)` before `compinit`).

_rush_shopt_names() {
    local -a names
    names=(
        allexport autocd braceexpand cdspell checkwinsize cmdhist
        direxpand dirspell dotglob emacs errexit execfail expand_aliases
        extglob failglob globasciiranges globstar hashall histappend
        hostcomplete huponexit inherit_errexit lastpipe login_shell
        no_empty_cmd_completion nocaseglob nocasematch noclobber noexec
        noglob nounset nullglob patsub_replacement pipefail sourcepath vi
        xpg_echo xtrace
    )
    _describe 'shopt option' names
}

_rush() {
    _arguments -s \
        '(-c)-c[read commands from a string]:command string:' \
        '(-s)-s[read commands from standard input]' \
        '(-i)-i[force interactive mode]' \
        '(-l --login)'{-l,--login}'[start as a login shell]' \
        '(-r --restricted)'{-r,--restricted}'[start as a restricted shell]' \
        '(-n)-n[parse but do not execute (syntax check)]' \
        '--posix[accepted for compatibility]' \
        '--norc[do not source the startup file]' \
        '--rcfile[source FILE instead of the default startup file]:file:_files' \
        '--init-file[source FILE instead of the default startup file]:file:_files' \
        '-O[enable a shopt option before running]:shopt option:_rush_shopt_names' \
        '+O[disable a shopt option before running]:shopt option:_rush_shopt_names' \
        '(-)--[end option parsing]' \
        '*::script or arguments:_files'
}

_rush "$@"
