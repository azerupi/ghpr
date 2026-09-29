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
accounts, see [Profiles](#profiles).

## Whose turn is it?

The turn comes from the PR's latest activity by a person (bots are ignored):

| Turn | Meaning |
|---|---|
| `me` | You're requested and haven't reviewed; or you reviewed and the author responded; or someone @mentioned you since you last acted. On your own PRs: a reviewer responded, or CI fails, or there are conflicts. |
| `team` | Requested from one of your configured teams, not you directly. |
| `merge` | Approved, green, and mergeable. |
| `author` | A reviewer acted last; waiting on the author. |
| `reviewers` | The author acted last; waiting on someone other than you. |
| `blocked` | Draft, failing CI, merge conflicts, or a `wip` / `blocked` / `do-not-merge` label. |

GitHub removes you from the requested reviewers once you review, so the
`for-me` view also searches PRs you've reviewed and mentions of you, keeping
those only when it's your turn again.

## Views

| View | Shows |
|---|---|
| `for-me` (default) | Review requests for you (and your teams), your own PRs, and PRs you reviewed or were mentioned in when it's your turn. Sorted by score. |
| `mine` | Your open PRs. |
| `repo` | Open PRs in the current checkout's GitHub repo, or the ones given with `-R`. |

Add your own in the config. Each view is a tab: `[` / `]` or Alt-1..9 switch.

## Filtering

Press `/` and type; the table updates as you go. **Tab** completes keys and
values (with how many PRs each would match); Enter keeps the filter, Esc undoes it.

```
turn:me -draft waiting:>2d repo:acme/* label:"needs review" ci:fail,pending
```

- Terms are ANDed, `-` negates, `a,b` means either.
- Keys: `turn`, `repo`, `org`, `author`, `reviewer`, `team`, `label`, `ci`,
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
`created`, `updated`, `turn`, `waiting`, `last`, `mine`, `threads`,
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
profile in effect. Everything is
optional:

```toml
default_view = "for-me"
refresh_interval = "5m"
bots = ["some-ci-user"]      # GitHub App bots are detected automatically
columns = ["turn", "repo", "number", "title", "author", "size", "ci", "review", "waiting", "last"]
# also available: updated, age, score, threads, files, labels

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
never overwrites the other's.

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
