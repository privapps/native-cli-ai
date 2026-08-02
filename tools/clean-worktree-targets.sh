#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: clean-worktree-targets.sh [OPTIONS] [WORKTREES_DIR]

Find direct child worktrees and remove their Rust target directories.
The default mode is a dry run. Use --apply to delete the caches.

Options:
  -a, --apply     Remove target directories instead of only listing them.
  -y, --yes       Do not prompt before applying the cleanup.
  -h, --help      Show this help.

Examples:
  tools/clean-worktree-targets.sh
  tools/clean-worktree-targets.sh --apply
  tools/clean-worktree-targets.sh --apply --yes /path/to/.nca/worktrees
EOF
}

error() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

apply_cleanup=false
assume_yes=false
worktrees_dir=''

while (($# > 0)); do
    case "$1" in
        -a|--apply)
            apply_cleanup=true
            ;;
        -y|--yes)
            assume_yes=true
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        --)
            shift
            if (($# > 0)); then
                if (($# > 1)); then
                    error "expected one worktrees directory"
                fi
                worktrees_dir=$1
            fi
            break
            ;;
        -* )
            error "unknown option: $1"
            ;;
        *)
            if [[ -n "$worktrees_dir" ]]; then
                error "expected one worktrees directory"
            fi
            worktrees_dir=$1
            ;;
    esac
    shift
done

if [[ -z "$worktrees_dir" ]]; then
    repo_root=$(git rev-parse --show-toplevel 2>/dev/null) || \
        error 'not inside a Git repository; pass WORKTREES_DIR explicitly'
    worktrees_dir="$repo_root/.nca/worktrees"
fi

[[ -d "$worktrees_dir" ]] || error "directory does not exist: $worktrees_dir"
worktrees_dir=$(cd -P "$worktrees_dir" && pwd)

targets=()
total_bytes=0

for entry in "$worktrees_dir"/*; do
    [[ -d "$entry" && ! -L "$entry" ]] || continue

    target="$entry/target"
    [[ -d "$target" && ! -L "$target" ]] || continue

    size_kib=$(du -sk "$target" | awk '{print $1}')
    targets+=("$target")
    total_bytes=$((total_bytes + size_kib * 1024))
    printf '%s\t%s\n' "$(du -sh "$target" | awk '{print $1}')" "$target"
done

if ((${#targets[@]} == 0)); then
    printf 'No Rust target directories found under %s\n' "$worktrees_dir"
    exit 0
fi

if ! $apply_cleanup; then
    printf 'Dry run: %d target director%s found (about %s total).\n' \
        "${#targets[@]}" \
        "$([[ ${#targets[@]} == 1 ]] && printf 'y' || printf 'ies')" \
        "$(awk -v bytes="$total_bytes" 'BEGIN { printf "%.1f GiB", bytes / 1024 / 1024 / 1024 }')"
    printf 'Run with --apply to remove them.\n'
    exit 0
fi

if ! $assume_yes; then
    printf 'Remove these %d target director%s? [y/N] ' \
        "${#targets[@]}" \
        "$([[ ${#targets[@]} == 1 ]] && printf 'y' || printf 'ies')"
    read -r reply
    [[ "$reply" == [yY] || "$reply" == [yY][eE][sS] ]] || {
        printf 'Cancelled.\n'
        exit 0
    }
fi

for target in "${targets[@]}"; do
    rm -rf "$target"
done

printf 'Removed %d target director%s (about %s).\n' \
    "${#targets[@]}" \
    "$([[ ${#targets[@]} == 1 ]] && printf 'y' || printf 'ies')" \
    "$(awk -v bytes="$total_bytes" 'BEGIN { printf "%.1f GiB", bytes / 1024 / 1024 / 1024 }')"
