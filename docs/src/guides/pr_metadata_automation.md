# PR metadata automation

This guide explains how pull request titles and descriptions are handled in this repository, including the rules for breaking changes.

```admonish tip
If you want to leave PR metadata entirely to automation, open the PR with a minimal placeholder title and body. The AI workflow will rewrite the title and description on PR open and on every new commit push.
```

## Default workflow

The repository has an AI workflow that updates PR metadata to make reviews easier:

* it rewrites the PR description every time
* it keeps titles in Conventional Commits format
* it refreshes metadata when new commits are pushed to the PR branch
* it adds breaking-change guidance when needed

```admonish info
The AI workflow runs on PR open and on `pull_request.synchronize`, so PR metadata is refreshed automatically as the branch evolves.
```

## How to open PRs

### Let AI do everything

Open the PR without spending time on a polished title or description.

Good examples:

* title: `wip`
* title: `draft`
* description: empty
* description: one rough sentence with context

The workflow will infer a Conventional Commit title and generate a structured Markdown summary for you.

```admonish note
This is the fastest path if you mostly want the PR metadata to be generated from commits and diffs.
```

### Keep your own title

If you open the PR with a title that already matches Conventional Commits, the workflow keeps it.

Examples:

* `fix(state_keeper): handle empty miniblock batch`
* `ci: refresh PR metadata automation`

The description is still regenerated so reviewers get a consistent summary.

```admonish tip
Use this mode when you care about the exact squash-merge commit message and want to control it directly.
```

### Keep useful context in the description

The workflow always rewrites the body, but it is instructed to preserve useful author-provided content when it still applies.

Useful content includes:

* rollout notes
* links to related PRs or docs
* migration context
* reviewer-specific caveats

```admonish important
Do not rely on temporary placeholder text surviving unchanged. If something matters for deployment or review, write it clearly so the generated body can preserve it.
```

## Breaking changes

A PR should be considered breaking only when both of these are true:

* deployment or rollout requires extra action such as a new required env var, config rename, schema migration, wire-format change, RPC change, or contract upgrade
* the change is not backward compatible with currently deployed instances

Non-breaking examples:

* CI-only updates
* docs changes
* local development setup changes
* test-only changes
* internal refactors with no external compatibility impact

```admonish warning
Adding `!` to a Conventional Commit title is not just formatting. It signals release and rollout impact, so use it only when the deployed system or its consumers must react to the change.
```

## How automation handles breaking changes

### Case 1: the change is breaking and the title is missing `!`

If the AI concludes the change is breaking and the current title is not marked as breaking, it updates the title to a breaking Conventional Commit and adds:

* `## Breaking Changes`
* `## Rollout Instructions`

### Case 2: you explicitly mark the PR as breaking

If you open the PR with a title that already includes `!`, the workflow keeps that title.

This is true even if the AI thinks the change may not actually be breaking.

```admonish info
Author intent wins for the title once you explicitly provide a valid breaking Conventional Commit.
```

### Case 3: you mark the PR as breaking, but AI does not agree

In this case, the workflow keeps the breaking title and adds a warning callout in the PR body instead of forcing breaking rollout sections.

The warning tells reviewers that:

* the PR is currently marked as breaking
* the automated review did not identify a deployment or compatibility break
* the `!` marker should be confirmed manually

```admonish warning
If you see this warning, treat it as a review prompt, not an automatic override. Either keep the breaking title because the automation missed something, or remove `!` if the change is not actually breaking.
```

## Reviewer guidance

When reviewing PR metadata:

* trust AI for routine summaries
* confirm the title is still the right squash-merge message
* pay extra attention to any breaking-change warning callout
* make sure real breaking PRs include concrete migration and rollout guidance

```admonish tip
If the generated summary is correct but incomplete, add the missing operational context directly in the PR and push another commit if needed. The next metadata refresh should keep useful reviewer-facing details.
```
