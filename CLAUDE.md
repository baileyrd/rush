# Working on rush

## PR workflow

When landing a batch of changes from a feature/working branch: open the PR
and merge it (squash) without asking for confirmation first. Don't pause to
check — just do it, then continue.

After merging, restart the working branch from the new `main` tip
(`git fetch origin main && git checkout -B <branch> origin/main && git push
--force-with-lease -u origin <branch>`) so it doesn't accumulate
already-merged history.
