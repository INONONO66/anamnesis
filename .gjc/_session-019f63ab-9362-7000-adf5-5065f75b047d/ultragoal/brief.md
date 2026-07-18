Complete STEP 12 from docs/superpowers/plans/2026-07-14-codex-handoffs.md. Base every branch on latest main. Never use --admin, force push, direct main commits, required-check bypass, Co-Authored-By, R3 code, or committed docs/superpowers changes. Stop after three consecutive identical test failures; rerun recognized flakes at most three times. Reverify each issue before work; false/already-resolved claims get evidence comment and not-planned closure; design decisions get an options comment and are skipped. One branch and PR per issue, required checks before squash merge.

@goal: Issue 112 — FastEmbed cache startup
Reverify #112, use TDD, fix cache environment setup before Tokio runtime startup, run full gates, open a Closes #112 PR, wait for required checks, and squash merge.

@goal: Issue 113 — embedded stats parity
Reverify #113, use TDD, make embedded and daemon stats output contracts match, run full gates, open a Closes #113 PR, wait for required checks, and squash merge.

@goal: Issue 118 — inert QueryConfig controls
Reverify #118. If preserving versus removing public controls requires design judgment, post concrete options and skip without changing public API; otherwise use TDD and complete one green PR.

@goal: Issue 119 — contradiction tension assertion
Reverify #119, use TDD to replace the tautological assertion with behaviorally meaningful coverage, run full gates, open a Closes #119 PR, wait for required checks, and squash merge.

@goal: Issue 114 — npm binary override
Reverify #114, use TDD/script evidence to unify the local binary override, run full gates, open a Closes #114 PR, wait for required checks, and squash merge.

@goal: Issue 115 — migration policy v11
Reverify source and update only truthful migration policy documentation through v11, run full gates, open a Closes #115 PR, wait for required checks, and squash merge.

@goal: Issue 116 — hook top-k documentation
Reverify the runtime default, correct documentation, run full gates, open a Closes #116 PR, wait for required checks, and squash merge.

@goal: Issue 117 — MCP tool inventory
Reverify registered tools and align published inventories truthfully, run full gates, open a Closes #117 PR, wait for required checks, and squash merge.

@goal: Issue 120 — release version preflight
Reverify #120, add fail-closed tag/version preflight with shell/workflow tests, open a Closes #120 PR, wait for required checks, and squash merge.

@goal: Issue 121 — GitHub Actions pins
Reverify official/current first-party action releases, update and pin safely with workflow/script evidence, open a Closes #121 PR, wait for required checks, and squash merge.

@goal: Part 2 — daemon grace-exit instrumentation
From latest main, instrument grace timer, client detection, socket unlink, and teardown intervals; collect numeric evidence. Implement an evidence-based improvement or justified test comment, never a guessed timeout increase; run full gates, open PR, wait for required checks, squash merge, and report the measurements.
