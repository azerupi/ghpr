# ghpr

Find the GitHub pull requests that need your attention, and decide which to
handle first.

`ghpr` opens an interactive table of pull requests. For each one it works out
**whose turn it is** — yours, the author's, or other reviewers' — and how long
it has been waiting, then ranks them so the ones waiting on you come first.

```
 ghpr   for-me  mine  backend                                  [ ] views · ? help
 21 of 23 PRs · 7 your turn · 2 new · 2 snoozed/muted hidden · sort: score
  TURN      REPO               #    TITLE                     AUTHOR  CI REVIEW   WAIT
★ me        acme/api        1234    Fix token refresh race    alice   ✓  changes  3d
  me        acme/web         889    Migrate settings page     bob     ✗  approved 1d
  reviewers acme/api        1201    Add tracing to job runner me      ✓  required 5h
```

It only reads from GitHub. Reviewing, approving, and checking out branches
happen in the browser or with `gh`; `o` and `y` get you there quickly.

## Install

```sh
cargo install --path .
```

`ghpr` uses your GitHub CLI login (`gh auth login`), or a token in `GH_TOKEN` /
`GITHUB_TOKEN`. Only github.com is supported. For separate work and personal
accounts, see [Setup](#setup) and [Profiles](#profiles).

## Setup

The first time you run `ghpr` without a config file, it offers a short guided
setup. It lists the accounts gh is logged in to and makes a profile for each,
then asks where each profile applies (directories, a repo's git `user.email`),
whose team review requests to include, which view to open with, and which
accounts are bots. Every question previews its effect: where each profile
would apply, your teams with how many PRs wait on each, a live sample of the
highlighted view. Nothing is written until you confirm; *Skip* writes a short
starter config instead.

`ghpr config init` runs setup again any time (`--config` picks another file).
It starts from your current values, changes only the keys it asks about
(comments, views, and everything else stay as they are), and backs the file up
first, next to it (`config.toml.bak-<time>`). Seen/snoozed/muted state moves
along when a profile is added or renamed.

When an update adds new setup questions, `ghpr` offers setup again and lists
what's new; *Don't ask again* waits for the next update. Setup only ever comes
up in a terminal: `list`, `count`, `paths`, `--config`, and `--offline` never
prompt.

## Whose turn is it?

The turn comes from the PR's latest activity by a person (bots are ignored):

| Turn | Meaning |
|---|---|
| `me` | You're requested and haven't reviewed; or you reviewed and the author responded; or someone @mentioned you since you last acted. On your own PRs: a reviewer responded, CI fails, there are conflicts, it's approved but behind its base, or it's about to be removed from the merge queue. |
| `team` | Requested from one of your configured teams, not you directly. |
| `merge` | Approved, green, and mergeable: nothing branch protection requires is missing. |
| `queued` | In the merge queue; nothing to do but wait. |
| `author` | A reviewer acted last, or it's approved but behind its base; waiting on the author. |
| `reviewers` | The author acted last; waiting on someone other than you. |
| `blocked` | Draft, failing CI, merge conflicts, or a `wip` / `blocked` / `do-not-merge` label. |

Whether it's mergeable comes from GitHub's merge state, which knows about
branch protection: approved PRs that still need a second approval or a
required check aren't shown as `merge` (they take the turn their latest
activity gives), and ones that must be brought up to date go back to the
author. In repos with a merge queue, the queue tests the
merged result, so there only approval, CI, and conflicts count. The detail
pane spells out the merge state, or the PR's place in the queue and GitHub's
estimate of the time left.

GitHub removes you from the requested reviewers once you review, so the
`for-me` view also searches PRs you've reviewed and mentions of you, keeping
those only when it's your turn again.

## Views

| View | Shows |
|---|---|
| `for-me` (default) | Review requests for you (and your teams), your own PRs, and PRs you reviewed or were mentioned in when it's your turn. Sorted by score. |
| `mine` | Your open PRs. |
| `repo` | Open PRs in the current checkout's GitHub repo, or the ones given with `-R`. In a fork, also its upstream (see below). |

In a fork, PRs mostly go to the upstream repo, so `repo` searches it along
with the fork. The upstream is found the way `gh` finds it: the remote
`gh repo set-default` marked, else a remote named `upstream` (`gh repo fork`
sets both up), then `github`, then `origin`. If your fork's checkout has
neither, `git remote add upstream <url>` or `gh repo set-default` fixes it.

Add your own in the config. Each view is a tab: `[` / `]` or Alt-1..9 switch.

## Filtering

Press `/` and type; the table updates as you go. **Tab** completes keys and
values (with how many PRs each would match); Enter keeps the filter, Esc undoes it.

```
turn:me -draft waiting:>2d repo:acme/* label:"needs review" ci:fail,pending
```

- Terms are ANDed, `-` negates, `a,b` means either.
- Keys: `turn`, `repo`, `org`, `author`, `reviewer`, `team`, `label`, `ci`,
  `merge` (`clean`, `unstable`, `behind`, `blocked`, `conflict`, `unknown`, `queued`),
  `review`, `mine` (`unreviewed`, `stale`, `approved`, `changes`, `commented`),
  `last`, `waiting`, `updated`, `age`, `size`, `files`, `threads`, `base`, `head`.
- Flags: `draft`, `conflict`, `bot`, `mentioned`, `new`, `snoozed`, `muted`.
- Times: `>2d`, `<3h`, `1d..1w`, or dates like `2026-09-01..2026-09-20`.
- Sizes: `<100`, `100..400`, or `xs`, `s`, `m`, `l`, `xl`.
- Plain words search titles, branches, repos, and authors; `#123` finds a number.
- `@me` is you.

One-key toggles: `m` my turn, `n` new, `d` hide drafts, `b` hide bots, `c` hide
failing CI, `Z` snoozed. `f` opens a **facets** panel with counts per turn,
repo, author, label, CI, and review: Space includes a value, `x` excludes it.

## Sorting and the score

`s` opens the sort prompt (`turn,waiting,-size`); `S` reverses; `1`–`9` sort by
that column. Each key sorts in its natural direction (`score`: highest first,
`waiting`: longest first, `size`: smallest first, ...) and `-` reverses it.

The default `score` adds up weights like "your turn +100", "waiting +5 per day"
(capped), "small PR +15", "failing CI −40", "draft −80". The detail pane shows
the breakdown; all weights are configurable.

## Keeping track

- **New** (★): someone other than you acted since you last looked. Opening a
  PR (`o`) or Space marks it seen; `N` marks everything shown as seen.
- **Snooze** (`z`): hide for a day, 3 days, a week, or until there's new
  activity. `z` again wakes it.
- **Mute** (`M`): hide until unmuted.
- Snoozed and muted PRs stay hidden unless the filter mentions `snoozed` or `muted`.
- `v` marks rows and `V` marks all; opening, copying, seen, snooze, and mute
  then apply to every marked row.

This is stored locally in `$XDG_STATE_HOME/ghpr/state.json`, never on GitHub.

## Notifications

While the UI is open, ghpr shows a desktop notification (`notify-send` on
Linux, Notification Center on macOS) when a refresh brings news: by default,
a PR became your turn (a review request, a reply to your review, a mention,
CI breaking on your PR, ...) or one of your PRs left the merge queue, merged
or removed. The default view (`for-me`) does the notifying, and keeps
refreshing in the background while you look at other tabs.

Each thing is announced once: a turn that flickers, or a PR that drops out
of a search and comes back, doesn't notify again, but a new request or reply
does. Snoozed and muted PRs never notify, nothing is announced for what was
already there when ghpr started, and more than three at once become one
summary. Notifications also flash in the status bar.

With several ghpr windows open for the same profile, only one notifies (it
holds `notify.lock` in the profile's state directory). If it closes, another
takes over within half a minute, starting afresh: what changes during the
handover isn't announced. News arrives with the next refresh of the
notifying view, so within `refresh_interval` (5 minutes by default).

```toml
[notify]
enabled = true                  # false turns them off
view = "for-me"                 # the view that notifies (default: default_view)
events = ["my_turn", "queue"]   # also: team, ready_to_merge, conflict, merged, opened
```

`team`: a review was requested from one of your teams. `ready_to_merge`,
`conflict`, `merged`: about your own PRs. `opened`: any PR new to the view.

## Keys

| Key | Action |
|---|---|
| `j`/`k`, arrows, `g`/`G`, Ctrl-d/u, PgUp/PgDn | move |
| `o` / `O` | open in browser / the Files tab |
| `y` / `Y` | copy URL / `owner/repo#123` |
| Enter, Tab | toggle the detail pane |
| `/`, `s`, `S`, `1`–`9` | filter, sort |
| `f` | facets |
| Space, `N`, `z`, `M`, `v`, `V` | seen, all seen, snooze, mute, mark, mark all |
| `[`, `]`, Alt-1..9 | switch views |
| `r` | refresh now |
| Esc | clear marks, then the filter |
| `?` | help |
| `q` | quit |

## Scripting

```sh
ghpr list                                   # table of the default view
ghpr list mine --json                       # everything, as JSON
ghpr list -q 'turn:me' --format '{repo}#{number} {title} ({waiting})'
ghpr count -q 'turn:me'                     # a number, for status bars
```

`count` reuses the cache while it's younger than `refresh_interval`, so it's
cheap to run often. `--offline` never contacts GitHub; `--no-cache` always does.

Template fields: `repo`, `number`, `title`, `author`, `url`, `draft`, `ci`,
`review`, `additions`, `deletions`, `files`, `base`, `head`, `labels`,
`created`, `updated`, `turn`, `merge`, `waiting`, `last`, `mine`, `threads`,
`mentioned`, `score`. `{{` and `}}` are literal braces.

## Shell completion

Completes views, filters (keys and values from your cached PRs), and sort keys:

```sh
echo 'source <(COMPLETE=bash ghpr)' >> ~/.bashrc                        # bash
echo 'source <(COMPLETE=zsh ghpr)' >> ~/.zshrc                          # zsh
echo 'COMPLETE=fish ghpr | source' > ~/.config/fish/completions/ghpr.fish  # fish
```

## Configuration

`ghpr config path` shows where the file lives (usually
`~/.config/ghpr/config.toml`); `ghpr config check` validates it and shows the
profile in effect; `ghpr config init` runs the [guided setup](#setup).
Everything is optional:

```toml
default_view = "for-me"
refresh_interval = "5m"
bots = ["some-ci-user"]      # GitHub App bots are detected automatically
columns = ["turn", "repo", "number", "title", "author", "size", "ci", "review", "waiting", "last"]
# also available: merge, updated, age, score, threads, files, labels

[team_reviews]
teams = ["acme/platform"]    # team review requests to include in for-me
count_as_mine = false        # false: shown as turn "team", below "me"

[team_reviews.repos]         # per repo; the most specific pattern wins
"acme/platform-*" = true     # counts as your turn
"acme/monorepo" = false      # shown as "team"
"acme/legacy" = "ignore"     # team-only requests aren't shown

[score]                      # weights; unset ones keep their defaults
turn_me = 100
turn_team = 60
turn_merge = 40
mentioned = 30
waiting_per_day = 5
cap_waiting_days = 10
small_pr_bonus = 15
small_pr_lines = 100
ci_failing = -40
draft = -80
conflict = -30
label.urgent = 50
repo."acme/core" = 20

[notify]                     # desktop notifications, see Notifications
events = ["my_turn", "queue"]

[views.backend]              # a custom view (becomes a tab)
query = "is:open org:acme label:backend"   # GitHub search syntax
filter = "-draft"                          # optional initial filter
sort = "turn,waiting"                      # optional initial sort
```

On narrow terminals the lowest-priority columns are dropped first; the title
always stays.

## Profiles

Profiles keep separate accounts apart, say work and personal: each has its own
token, settings, cache, and seen/snoozed/muted state, so switching between them
never overwrites the other's. [Setup](#setup) writes them for you; this is what
it writes.

```toml
[profiles.work]
dirs = ["~/work"]              # used when run under these directories
email = "me@work.example"      # or in a repo whose git user.email matches
gh_user = "me-work"            # token from `gh auth token --user me-work`
# token_env = "WORK_TOKEN"     # or from an environment variable
default_view = "mine"          # any setting can be overridden per profile
[profiles.work.team_reviews]
teams = ["acme/robotics"]
[profiles.work.views.robots]
query = "is:open org:acme"

[profiles.personal]
gh_user = "me"
default = true                 # when no other profile applies
```

The first match wins: `--profile NAME` or `GHPR_PROFILE`; then
`git config ghpr.profile` (handy with git's `includeIf`); then `dirs` (the
longest match); then `email`; then the profile with `default = true`. Tables in
a profile merge into the top-level ones (so a profile can add a view); other
values replace them.

Log in to each account with `gh auth login` once; gh keeps them side by side.
`ghpr paths` shows which profile applies where you are, and why.

## Logs

Each run logs to `$XDG_STATE_HOME/ghpr/logs/` (usually
`~/.local/state/ghpr/logs/`), one file per day, a week kept: startup and profile,
every request with its duration, status, retries, and remaining rate limit,
fetch summaries, cache and state writes, UI actions, errors, and panics. Tokens
are never logged. `GHPR_LOG=debug` (or `trace`, or any `tracing` filter like
`ghpr::github=debug`) logs more.

`ghpr paths` prints where the config, cache, state, and logs are.

## Development

```sh
cargo test      # unit tests, including snapshot tests of the UI (insta)
cargo clippy --all-targets
```

`docs/PLAN.md` describes the design and how it evolved.
