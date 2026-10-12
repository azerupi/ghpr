# ghpr — plan

A terminal tool for finding the pull requests that need your attention and deciding which to handle first.

- `ghpr` opens an interactive table (ratatui).
- `ghpr list ... --json|--table` prints the same data without the TUI, for scripts and shell aliases.

---

## 1. Core idea: "whose turn is it?"

Listing PRs is easy (`gh pr list` already does it). The useful part is working out, for each PR, **who needs to act next**, and putting the ones waiting on *you* at the top.

For each PR we compute a `Turn`:

| Turn | Meaning (from the viewer's perspective) |
|---|---|
| `Me` | I've been asked to review and haven't yet, **or** I reviewed and the author has pushed or replied since, **or** I'm the author and a reviewer commented or requested changes after my last activity, **or** I was @mentioned after my last activity |
| `Author` | A reviewer acted last (changes requested or comments left) and the author hasn't responded |
| `Reviewers` | The author acted last and is waiting on someone other than me |
| `Team` | Review requested from one of my configured teams (not me directly) and I haven't reviewed it. Counts as `Me` when team reviews are enabled for that repo (see §9) |
| `Merge` | Approved, CI green, no conflicts: it only needs merging |
| `Blocked` | Draft, CI failing, merge conflicts, or labelled `blocked`/`wip` |

The input to this is the **last meaningful activity**: the latest event that is a commit push, review, review comment, issue comment, ready-for-review, or re-request of review, with bots and the viewer's own no-op events left out. We record who did it (`Author` / `Reviewer` / `Me` / `Bot` / `Other`) and when.

GitHub quirk that shapes the queries: once you submit a review, GitHub **removes you from `requested_reviewers`**. A "requested from me" search on its own therefore loses every PR you've already reviewed and now need to re-review. The "for me" query has to be a union of:

- `user-review-requested:@me` (plain `review-requested:@me` also matches requests to any of your teams)
- `reviewed-by:@me` (then keep only those whose Turn is `Me`)
- `team-review-requested:<org/team>` for each configured team (on/off per repo, see §9)
- `author:@me` (my own PRs waiting on me)
- `mentions:@me`

---

## 2. Data source

**GitHub GraphQL API** through `reqwest`, with hand-written query strings deserialized into `serde` structs. A single `search(type: ISSUE, query: "...")` call returns PRs with nested reviews, timeline, and CI status. REST would need N+1 calls.

**Auth**, tried in this order:
1. `GH_TOKEN` / `GITHUB_TOKEN` env var
2. `gh auth token` (reuses the gh CLI login, so no setup)
3. A clear error explaining how to log in

Only github.com is supported; there's no GitHub Enterprise support.

**Fetch strategy (two phases, to keep rate-limit cost down):**
1. *Search phase*: run each server-side query in parallel with light fields only (id, number, title, repo, author, isDraft, updatedAt, createdAt, additions/deletions/changedFiles, labels, reviewDecision, mergeable, headRefOid, `statusCheckRollup.state`). Page at 50–100 per page; GitHub search stops at 1000 results.
2. *Detail phase*: dedupe by node id, then batch `nodes(ids: [...])` in chunks of about 25 and fetch `timelineItems(last: 30, itemTypes: [...])`, `reviews(last: 20)`, `reviewRequests`, `reviewThreads(first: 50) { isResolved }`, and `latestReviews`.
3. Read `rateLimit { cost remaining resetAt }` on every response and show it in the status bar.

**Caching:** save the last result set to `$XDG_CACHE_HOME/ghpr/<view>.json`. On startup, render the cache immediately (dimmed "stale" marker), refresh in the background, and replace it in place. A later optimization is to skip the detail phase for PRs whose `updatedAt` hasn't changed.

---

## 3. Data model (derived, per PR)

```rust
struct Pr {
    // identity
    id: String, repo: RepoName, number: u64, title: String, url: Url,
    author: Actor, is_draft: bool, labels: Vec<String>,
    base_ref: String, head_ref: String,
    created_at: Timestamp, updated_at: Timestamp,

    // size
    additions: u32, deletions: u32, changed_files: u32,

    // review state
    review_decision: Option<ReviewDecision>,     // APPROVED / CHANGES_REQUESTED / REVIEW_REQUIRED
    requested_reviewers: Vec<Reviewer>,          // users + teams
    requested_via_team: Option<TeamName>,        // I'm only requested through this team
    my_review: Option<MyReviewState>,            // None / Commented / Approved / ChangesRequested, + at commit oid
    approvals: u8, changes_requested: u8,
    unresolved_threads: u16,
    commits_since_my_review: u16,                // "new changes since you looked"

    // health
    ci: CiState,                                 // Pass / Fail / Pending / None
    mergeable: Mergeable,                        // Clean / Conflicting / Unknown
    behind_base: Option<bool>,

    // activity
    last_activity: Activity { who: ActorRole, kind: ActivityKind, at: Timestamp },
    turn: Turn,
    waiting_since: Timestamp,                    // when the ball landed in the current court
    mentions_me: bool,

    // local state (not from GitHub)
    seen_at: Option<Timestamp>,                  // for the "new since I last looked" marker
    snoozed_until: Option<Timestamp>,
    muted: bool,
    note: Option<String>,

    // computed
    score: f32,
}
```

`waiting_since` matters more than `updated_at`. `updated_at` changes on every bot comment or label edit, while "waiting on me for 4 days" is what actually tells you the priority.

---

## 4. Filtering

### 4.1 Query language (client-side, `/` to open)

Uses GitHub search syntax where possible so it feels familiar, and adds derived fields:

```
repo:org/api  -draft  turn:me  ci:fail  size:<300  waiting:>2d  label:urgent  author:alice
```

| Key | Values |
|---|---|
| `repo:` / `org:` | glob, e.g. `repo:org/*-service` |
| `author:` / `reviewer:` / `team:` | login, `@me` |
| `turn:` | `me`, `team`, `author`, `reviewers`, `merge`, `queued`, `blocked` |
| `last:` | who acted last: `author`, `reviewer`, `me`, `bot` |
| `draft` / `-draft` | |
| `ci:` | `pass`, `fail`, `pending`, `none` |
| `review:` | `approved`, `changes`, `required`, `none` |
| `mine:` | `unreviewed`, `stale` (new commits since my review), `approved`, `changes` |
| `conflict` / `-conflict` | |
| `label:` / `-label:` | |
| `age:` `updated:` `waiting:` | `>7d`, `<2h`, `2026-09-01..2026-09-20` |
| `size:` | total lines changed, `<100`, `>1000`; or `size:xs\|s\|m\|l\|xl` |
| `files:` | changed-file count |
| `threads:` | unresolved thread count, `>0` |
| `new` | changed since I last looked (local `seen_at`) |
| `snoozed` / `muted` | normally hidden; opt in to see them |
| `base:` | target branch, e.g. `base:main`, `base:release/*` |
| bare words | fuzzy match on title, branch, or `#number` |

- Terms are ANDed together. `-` negates a term, and `a,b` means OR within one key (`ci:fail,pending`). No parentheses in v1: a flat AND of per-key ORs covers almost every real case, and the parser stays small enough to trust.
- All filtering is local (instant, and the cache stays per view). *Later:* push terms GitHub can evaluate (repo, author, label, draft, updated) into the search query to fetch less.
- A parse error is shown inline under the input and the previous filter stays in effect.
- **Completion:** the best match for the word at the cursor shows as dim text (→ or Tab accepts it). With several matches, Tab fills in their shared start and opens a menu (Tab/↑/↓ to move, Enter to pick, Esc to close) listing descriptions and how many PRs each choice would match given the rest of the filter; zero-match choices are dimmed and last. Values come from the loaded data (repos, authors, labels, ...), most frequent first. A word that's still being completed (`tu`, `turn:`, a lone `-`) isn't applied or reported as an error until it's finished. The sort prompt completes sort keys the same way. Shell completion for `-q` comes with milestone 7.

### 4.2 Quick toggles (one key, no typing)

`d` hide drafts · `b` hide bots' PRs · `m` only turn:me · `c` hide CI failing · `n` only new · `z` show snoozed

The active filter always appears as text in the header, so toggles and typed filters are the same thing underneath and a combination can be saved as a view.

### 4.3 Facets panel (`f`)

A sidebar with counts per repo, author, label, turn, and CI state. `Space` includes a value and `x` excludes it. This answers "what's in here?" without typing.

---

## 5. Sorting

- Multi-key sort, for example `turn,waiting,-size`. Each key has a natural direction (score: highest first, waiting: longest first, updated: newest first, size: smallest first, ...) and `-` reverses it. `s` opens a sort prompt; `S` reverses the primary key; `1`–`9` sort by that column (press again to reverse).
- The default sort is by **priority score**, a weighted sum you can change in the config:

```toml
[score]
turn_me          = 100
turn_team        = 60
turn_merge       = 40
mentioned        = 30
waiting_per_day  = 5      # capped
cap_waiting_days = 10
small_pr_bonus   = 15     # quick wins first
small_pr_lines   = 100
ci_failing       = -40    # author needs to fix it first
draft            = -80
conflict         = -30
label.urgent     = 50
label.hotfix     = 50
repo."org/core"  = 20
```

- The detail pane breaks the score down term by term, so the ordering is never a mystery. `{score}` is available in `ghpr list --format`.

---

## 6. TUI layout

```
┌ ghpr · view: for-me · 23 PRs (7 your turn) · filter: -draft turn:me ─────────── ⟳ 2m ago · RL 4812/5000 ┐
│ T  Repo           #     Title                              Author   Size   CI  Rev   Waiting  Last     │
│ ●  org/api        1234  Fix token refresh race              alice    +42-8  ✓   ◐1/2  3d       author   │
│ ●  org/web        889   Migrate settings page to new form   bob      +610   ✗   ✓2    1d       author ★ │
│ ◌  org/api        1201  Add tracing to job runner           me       +120   ✓   ✎     5h       reviewer │
│ ...                                                                                                    │
├─ #1234 Fix token refresh race ─────────────────────────────────────────────────────────────────────────┤
│ alice → main · opened 6d · 2 commits since your review · 1 unresolved thread · labels: bug, auth       │
│ Last: alice pushed 3 commits (3d ago) · Reviewers: you (changes requested), carol (approved)           │
│ [o]pen  [y]ank url  [z]snooze  [M]ute  [space] seen  [r]efresh                                         │
└────────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

- Columns are configurable: pick which ones to show and their order. Narrow terminals drop low-priority columns first.
- Colours: rows where it's my turn are bold; drafts dimmed; `★` for new since last seen; waiting time goes yellow after 2d and red after 5d (thresholds configurable).
- Detail pane (toggle with `Enter` or `Tab`): description excerpt, reviewers with their state, CI check names (failing ones first), and the last 5 timeline events.
- Views are tabs (`[`/`]` or `Alt+1..9`), each a saved filter + sort + server query.
- Status bar: refresh age, rate limit, errors (a failed query shows as a warning while the other results still display).

### Keys

| Key | Action |
|---|---|
| `j/k`, `↑/↓`, `g/G`, `Ctrl-d/u` | navigate |
| `/` | filter query |
| `s` / `S` | sort picker / reverse |
| `f` | facets panel |
| `Enter` / `Tab` | detail pane |
| `o` | open in browser (`O`: open the Files tab) |
| `y` | copy URL (`Y`: copy `org/repo#123`) |
| `z` | snooze for 1d / 3d / 1w / until updated |
| `M` | mute (hide permanently until un-muted) |
| `space` | mark as seen |
| `r` | refresh now |
| `v` | toggle multi-select for bulk actions (open all, snooze all) |
| `?` | help / score breakdown |
| `q` | quit |

The tool never writes to GitHub. Reviewing, approving, and checking out branches happen in the browser or with `gh`, and `o` / `y` get you there quickly. Snooze, mute, and seen are stored only on your machine.

---

## 7. Built-in views (all overridable)

| View | Server query | Default local filter / sort |
|---|---|---|
| `for-me` | union from §1 | `-muted -snoozed`, sort by score |
| `mine` | `author:@me is:open` | sort `turn, -waiting` |
| `repo` | `repo:<current git remote> is:open` | `-draft`, sort `-waiting` |
| `team` | `team-review-requested:…` for configured teams | `-draft turn:team,me` |
| `ready-to-merge` | `author:@me review:approved` | `turn:merge` |
| `stale` | configured repos | `waiting:>7d` |

The `repo` view auto-detects the repository from `git remote get-url origin` when run inside a checkout, so `ghpr repo` works with no arguments.

---

## 8. CLI (clap)

```
ghpr                              # TUI, default view
ghpr <view>                       # TUI on that view
ghpr -q 'turn:me ci:pass'         # TUI with an initial filter
ghpr repo [OWNER/REPO]            # repo view; detects repo from cwd
ghpr list [VIEW] [-q FILTER] [--sort KEYS] [--json | --table | --format '{repo}#{number} {title}'] [--limit N]
ghpr count [VIEW] [-q FILTER]     # prints a number: status bars, tmux, prompt segments
ghpr views                        # list configured views
ghpr config path|edit|check
ghpr cache clear
ghpr completions <shell>          # clap_complete
```

Global options: `--repo/-R` (repeatable), `--no-cache`, `--offline` (cache only), `--config`.

`ghpr count -q turn:me` is cheap and lets the tool show up in your shell prompt or status bar without opening the TUI.

---

## 9. Config (`$XDG_CONFIG_HOME/ghpr/config.toml`)

```toml
default_view = "for-me"
refresh_interval = "5m"
bots = ["some-ci-user"]   # extra accounts treated as bots; GitHub App bots (`__typename: Bot`) are detected automatically.
                          # Note: GraphQL returns bot logins without the `[bot]` suffix (e.g. `dependabot`).
columns = ["turn", "repo", "number", "title", "author", "size", "ci", "review", "waiting", "last"]

[team_reviews]
teams = ["org/platform", "org/robotics"]   # teams to query; empty = none
count_as_mine = false                       # default: team requests show as turn:team, below turn:me

[team_reviews.repos]                        # per-repo overrides (globs; the most specific match wins)
"org/platform-*" = true                     # my team owns these, so a team request means me
"org/monorepo"   = false                    # too noisy: team requests show as turn:team
"org/legacy"     = "ignore"                 # don't fetch or show team-only requests at all

[thresholds]
waiting_warn = "2d"
waiting_crit = "5d"
size = { xs = 10, s = 100, m = 400, l = 1000 }

[views.backend]
query  = "is:open is:pr org:org label:backend"
filter = "-draft ci:pass,pending"
sort   = "score"

[score]
# see §5
```

Local state (seen / snoozed / muted / notes) lives in `$XDG_STATE_HOME/ghpr/state.json`, keyed by PR node id and pruned when a PR closes.

---

## 10. Architecture

```
src/
  main.rs            // clap parse → dispatch
  cli.rs             // clap derive structs
  config.rs          // load/merge/validate TOML, defaults
  auth.rs            // token discovery
  github/
    client.rs        // reqwest + GraphQL POST, retry/backoff, rate-limit tracking
    queries.rs       // query strings (search phase, detail phase)
    types.rs         // raw serde response structs
  model/
    pr.rs            // Pr (the derived model above)
    derive.rs        // raw → Pr: last_activity, turn, waiting_since, my_review, ...
    score.rs
  filter/
    lexer.rs, parser.rs, ast.rs, eval.rs, pushdown.rs
  sort.rs
  store/
    cache.rs         // result cache
    state.rs         // seen / snooze / mute
  tui/
    app.rs           // App state + update(msg) (Elm-style)
    event.rs         // crossterm events + tick + fetch results → Msg
    ui/ table.rs, detail.rs, facets.rs, filter_input.rs, help.rs, status.rs
    keymap.rs
  output.rs          // table / json / format for `list`
```

- **Runtime:** `tokio`. The TUI loop reads from a single `mpsc<Msg>` channel fed by crossterm's `EventStream`, a refresh ticker, and fetch tasks. Fetching never blocks rendering.
- **Elm-style `update(&mut App, Msg) -> Vec<Cmd>`** keeps state changes pure and testable. `Cmd`s (fetch, open URL, copy to clipboard, write local state) run outside `update`.
- **`derive.rs`, `filter`, `score`, and `sort` are pure functions over plain data.** Most of the logic lives here and nearly all tests target it.

### Crates

`clap` (+ `clap_complete`), `ratatui`, `crossterm` (`event-stream`), `tokio`, `reqwest` (`rustls-tls`, `json`), `serde`/`serde_json`, `toml`, `jiff` (timestamps and friendly durations), `directories`, `color-eyre`, `nucleo-matcher` (fuzzy title match), `tui-input` (filter line editing), `open`, `arboard` (clipboard), `globset` (repo globs), `futures`. Dev: `insta` (snapshots), `wiremock` (HTTP fixtures).

---

## 11. Testing

- **Derivation:** JSON fixtures of real GraphQL responses (anonymised) → assert `turn`, `last_activity`, `waiting_since`, and `commits_since_my_review`. This is the core logic, so it gets the most cases: reviewer removed after review, bot-only activity, force-push after approval, team requests, author replies with a comment only, draft converted to ready.
- **Filter:** parser round-trip tests, eval table tests, error messages; optionally property tests that parse → print → parse gives the same result.
- **Sort/score:** table tests.
- **TUI:** `ratatui::backend::TestBackend` + `insta` snapshots for the main layouts; `update()` tests that feed key `Msg`s and assert state.
- **Client:** `wiremock` for pagination, rate-limit headers, and partial GraphQL errors.

---

## 12. Milestones

1. **Skeleton:** clap commands, config loading, auth via `gh auth token`, a single search query, `ghpr list --table`. *Done when:* it prints the open PRs for one repo.
2. **Derivation:** detail-phase fetch, `last_activity`, `turn`, `waiting_since`, my review state, the `for-me` union query. *Done when:* `ghpr list for-me` correctly separates PRs that are my turn from those that aren't, across the fixture cases.
3. **Basic TUI:** table, navigation, open in browser, copy URL, detail pane, background refresh, cache-first startup.
4. **Filter & sort:** query language, quick toggles, multi-key sort, score with breakdown.
5. **Views & local state:** tabs, saved views, seen/new marker, snooze, mute. *As built:* tabs for every resolvable view (`[`/`]`, Alt-1..9), each with its own filter/sort/selection; only the visible tab refreshes in the background. "New" means someone other than you acted since you last looked (opening with `o` or Space marks seen; `N` marks all); on the very first run existing PRs start out seen. `z` snoozes for 1 day / 3 days / 1 week / until new activity (`z` again wakes it), `M` mutes; both are hidden unless the filter mentions `snoozed`/`muted` (`Z` toggles `snoozed`). `ghpr list` honours the same hiding, read-only. Requests retry connection drops and 502/503/504 twice with backoff.
6. **Facets & team reviews:** facets panel, team-review config with per-repo overrides, bulk select (open all, snooze all). *As built:* `f` opens a side panel (turn, repo, author, label, CI, review) with counts of shown PRs; Space includes and `x` excludes a value, merging values of one key into `key:a,b`; excluded values stay listed. `[team_reviews.repos]` maps patterns to `true` / `false` / `"ignore"`, most specific pattern wins; ignored repos drop team-only PRs. `v` marks rows, `V` marks all (or clears); open, copy, seen, snooze, and mute act on the marked rows, and Esc clears marks before the filter.
7. **Polish:** `count` command, completions, column config, narrow-terminal handling, README. *As built:* `ghpr count` reuses the cache while it's younger than `refresh_interval`. Shell completion uses clap's dynamic `COMPLETE=<shell> ghpr` mechanism rather than a `completions` subcommand, so `-q` completes filter keys and values from the cached PRs, `--sort` completes sort keys, and views come from the config. `columns = [...]` picks the table columns for both the UI and `ghpr list`; the number keys sort by the Nth configured column; on narrow terminals columns are dropped lowest-priority first and the title always stays.

8. **Guided setup:** first-run setup, `ghpr config init`, and a re-offer when setup gains questions. *As built:* a ratatui wizard (accounts from `gh auth status --json hosts`, profile names, default profile and `dirs`/`email`, teams, default view, bots, review) with a preview per step: where each profile applies (via `profile::apply` on the draft), teams listed one org at a time so an SSO-protected org only costs its own teams, open requests per team, a live 10-per-search sample of each view, and bot suggestions from the samples. It edits the file with `toml_edit`, so only the keys setup owns change, validates the result with `config::parse_with` for every profile, backs up the old file, and writes atomically. No-profile state moves to the profile of gh's active account, and renamed profiles take their state along; samples become each profile's cache. `setup_version` in the file versus `setup::SETUP_VERSION` decides the re-offer; "don't ask again" is recorded in `<state>/setup-dismissed`.

9. **Merge state and merge queue:** *As built:* the search phase also fetches `mergeStateStatus`, `isMergeQueueEnabled`, and `mergeQueueEntry`. `merge` now also needs the merge state not to be `BEHIND`, `BLOCKED`, or `DIRTY`; an approved PR that's `BEHIND` is the author's turn. Repos with a merge queue skip both rules (the queue tests the merged result, and how GitHub reports PRs waiting to be queued wasn't observable), so they keep the old approval/CI/conflict rule. `UNSTABLE` doesn't relax the failing-CI rule, since repos without required checks report it for PRs whose every check fails. A new turn `queued` covers PRs in the queue; one the queue reports `UNMERGEABLE` is the author's again. A `merge` column, filter key, and template field show `queue N`, `behind`, `blocked`, and so on; the detail pane explains them.

10. **Notifications (in the UI):** *As built:* `notify::Notifier` runs on every refresh of one view (`[notify] view`, else the default view), which keeps refreshing in the background even when its tab is hidden. It's pure: it remembers what it announced by (PR id, event, `waiting_since` or enqueue time), so flickering turns and PRs that fall off a search and return don't repeat, and a new request or reply does. The first live refresh only records. The view's own filter decides what counts (snoozed and muted PRs never notify), not whatever is typed on screen. PRs that leave the view while queued, or your own when `merged` is wanted, are looked up one at a time (`node(id) { state }`, since an id that no longer resolves fails the request) to tell merged from closed. Events: `my_turn` and `queue` by default; `team`, `ready_to_merge`, `conflict`, `merged`, `opened` on request. Delivery is `notify-send` or `osascript` with output discarded; after one failure it stays off for the session. More than three notices at once become a summary. With several instances open for a profile, only the one holding an exclusive lock on `<state>/profiles/<name>/notify.lock` (`File::try_lock`, released on exit or crash) notifies and refreshes the notify view in the background; the others retry every 30s, and a new holder starts with a fresh notifier, so a handover can miss a change but never repeats one. A headless `ghpr watch` reusing the notifier is the next step.

Milestones 1 and 2 already give a useful `list` command before any TUI work starts.

---

## 13. Later ideas

- **`ghpr watch`:** the notifier without the UI, for a systemd user service; it could compare against the cache to survive restarts and poll faster while a PR is queued.
- **Review load:** facet showing how many open reviews each teammate has, to help pick a reviewer.
- **Stacked PRs:** detect when a PR's base branch is another open PR's head, and show them as a tree so the stack gets reviewed bottom-up.
- **CODEOWNERS awareness:** flag PRs that touch paths you own even when you weren't requested.
- **Diff preview** in the detail pane (`gh pr diff` piped through `delta`, when installed).
- **Linked issues / Jira keys** pulled from the title or branch and shown as a column.
