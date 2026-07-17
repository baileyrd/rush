# bash completion for rush's own invocation flags — completing `rush <TAB>`
# at a bash prompt, not to be confused with rush's own (much larger)
# in-shell completion engine (src/completion.rs), which completes commands
# typed *inside* rush itself.
#
# Install: source this file, or drop it in bash-completion's own
# completions directory (commonly /etc/bash_completion.d or
# $(pkg-config --variable=completionsdir bash-completion)).

_rush_shopt_names() {
    printf '%s\n' allexport autocd braceexpand cdspell checkwinsize cmdhist \
        direxpand dirspell dotglob emacs errexit execfail expand_aliases \
        extglob failglob globasciiranges globstar hashall histappend \
        hostcomplete huponexit inherit_errexit lastpipe login_shell \
        no_empty_cmd_completion nocaseglob nocasematch noclobber noexec \
        noglob nounset nullglob patsub_replacement pipefail sourcepath vi \
        xpg_echo xtrace
}

_rush() {
    local cur prev
    COMPREPLY=()
    cur="${COMP_WORDS[COMP_CWORD]}"
    prev="${COMP_WORDS[COMP_CWORD - 1]}"

    case "$prev" in
        --rcfile | --init-file)
            COMPREPLY=($(compgen -f -- "$cur"))
            return 0
            ;;
        -O | +O)
            COMPREPLY=($(compgen -W "$(_rush_shopt_names)" -- "$cur"))
            return 0
            ;;
        -c)
            # A command string, not a filename — nothing sensible to offer.
            return 0
            ;;
    esac

    if [[ $cur == -* ]]; then
        COMPREPLY=($(compgen -W "-c -s -i -l -r -n -O +O --login --restricted \
            --posix --norc --rcfile --init-file --" -- "$cur"))
        return 0
    fi

    # Otherwise: a script path (rush's first non-option argument).
    COMPREPLY=($(compgen -f -- "$cur"))
}

complete -F _rush rush
