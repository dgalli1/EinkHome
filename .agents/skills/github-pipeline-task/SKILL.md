---
name: github-pipeline-task
description: "Run an implementation task in a git worktree linked to the existing pbemu install, push to a branch, and iterate on the GitHub Actions pipeline until green using /usr/local/bin/github-wait-for.sh"
---

# GitHub pipeline task

Workflow for doing a code task in this repository end-to-end: isolated
worktree (sharing the pbemu install so the ~3 GB of staged firmware is
not duplicated), the change, and the push → wait → fix → push loop until
the pipeline is green.

## 1. Create the branch and worktree

```bash
# from the repo root (/home/damian/git/EinkHome)
git worktree add -b <branch> ../EinkHome-<branch> main   # or another base
cd ../EinkHome-<branch>
```

### Link the existing pbemu install (storage)

`git worktree add` creates an empty `pbemu/` directory and would need a
full submodule checkout. Instead, symlink the existing pbemu install so
the staged firmware (U633_6.8.2817, U634k3_6.10.2544 — ~3 GB), the venv
and the built shim artifacts are shared, not copied:

```bash
rm -rf pbemu                                   # remove the empty dir git created
ln -s /home/damian/git/EinkHome/pbemu pbemu    # link the existing install
git update-index --skip-worktree pbemu         # silence git's submodule-symlink check
```

Why this works: every container mount whose source is under
`<worktree>/pbemu` (the app cross-compile, `pbemu build`, the emulator
runtime) is resolved by podman on the host **before** mounting, so the
container sees the real pbemu content. The host-side tools (venv,
firmware, test harness) resolve the symlink natively. Do NOT symlink
paths *inside* the worktree that the build container mounts directly —
those resolve inside the container and break.

The SDK is NOT part of pbemu — install it in the worktree (small):

```bash
./sdk/install-sdk.sh
```

Caveats:
- Local runs are SDL-only (`EH_TEST_BACKEND=sdl`), which never touches
  the shared pbemu `.live` state — so parallel local runs share it safely.
- Never `git add pbemu`; the skip-worktree entry is excluded from
  `git status`, so commit explicit paths and check `git status` before
  committing.
- When finished: `git worktree remove ../EinkHome-<branch>` and
  `git branch -d <branch>`.

## 2. Do the task

Work normally in the worktree. Verify with scoped tests in the worktree
using the parallel local SDL build — fast, no podman/firmware needed:

```bash
EH_TEST_BACKEND=sdl pbemu/.venv/bin/python -m pytest \
  tests/test_bookshelf.py -n auto -q -k "<scope>"
```

Local verification uses ONLY the SDL backend (`EH_TEST_BACKEND=sdl`) via
pytest-xdist (`-n auto`). Never run the emulator-backed suites locally —
the real pipeline (bookshelf emulator suite + 100k scale suite, both of
which download the firmware and boot qemu) runs only on GitHub Actions.
Scoped tests only, never the full suite unprompted (see the
scoped-tests-first rule).

The SDL suite skips the storage-tier tests (downloads, reader launch,
offline boot, search keyboard, settings persistence) — those are exactly
the ones the GitHub Actions e2e run covers, so don't treat an SDL-local
green as proof of the storage tier.

## 3. Commit and push for the pipeline

```bash
git add <changed paths>            # explicit paths, never `pbemu`
git commit -m "<conventional message>"
git push -u origin <branch>
# The pipeline runs on pull requests: open one if this branch has none yet.
gh pr create --fill                # or update the existing PR for the branch
```

The GitHub Actions pipeline (`ci.yml` + reusable `e2e-suite.yml`) runs
API tests, the emulator bookshelf suite and the 100k scale suite
(emulator-backed; firmware is re-downloaded every run, so expect several
minutes), with automatic fresh-runner retries for flaky hosts. It only
runs on GitHub Actions — never locally.

## 4. Trigger the pipeline — run it in the background, don't block

The push above already starts the run. Do NOT sit and wait on it: kick
the watcher off in the background so you (or the model) can keep working
and pick up the result when it lands:

```bash
# find the run for your push (the pull_request run for the head commit)
RUN_ID=$(gh run list --commit "$(git rev-parse HEAD)" --json databaseId,event \
         --jq '.[] | select(.event=="pull_request") | .databaseId' | head -1)
nohup /usr/local/bin/github-wait-for.sh "$RUN_ID" \
  > build/pipeline-wait.log 2>&1 &
```

The run continues in the background; check it when it finishes:

```bash
tail -40 build/pipeline-wait.log   # the wait helper's exit + failed-step logs
```

Never write hand-rolled `for i in …; do sleep …; done` polling loops (a
user rule bans them). Exit codes from the helper: `0` success · `1`
failed (prints the failed step logs) · `2` cancelled · `3` timeout
(default timeout 3600 s, interval 30 s — override as positional args).
If the helper is unavailable, use `gh run watch <id>` in the background
once instead of a custom loop.

If the run failed (exit 1): read the failing job logs
(`gh run view "$RUN_ID" --log-failed`), fix the root cause, commit and
push again — each push starts a fresh run for the new HEAD. Check the
run conclusion once with
`gh api repos/dgalli1/EinkHome/actions/runs/<id> --jq '{status, conclusion}'`.

## 5. Report back

Report to the user: the branch, the PR link, the commit(s), the final
CI conclusion (with the run URL), and anything noteworthy from the
failure iterations.
