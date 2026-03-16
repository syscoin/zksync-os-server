# PR Breaking Guidance Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Update PR automation so explicit author-provided breaking titles are preserved while the PR body warns when AI believes the change is not actually breaking, and document the full PR authoring workflow.

**Architecture:** Add a small unit-tested classification module for PR breaking-title/body behavior, reuse it from the GitHub script validator, and tighten the AI workflow prompt so title/body generation follows the new rules. Add a developer-facing guide under the docs book and link it from the guides index.

**Tech Stack:** GitHub Actions YAML, Node.js built-in `node:test`, JavaScript modules, mdBook docs.

---

### Task 1: Add a failing test for breaking-change classification

**Files:**
- Create: `.github/scripts/check-breaking-pr.test.js`
- Modify: `.github/scripts/check-breaking-pr.js`

**Step 1: Write the failing test**

Add tests that cover:
- author title contains `!:` and body has no warning yet
- AI classifies change as non-breaking
- expected result preserves the breaking title and asks for a warning block in the body

Also add a test that verifies:
- title contains `!:` and AI classifies change as breaking
- expected result requires metadata validation instead of warning

**Step 2: Run test to verify it fails**

Run: `node --test .github/scripts/check-breaking-pr.test.js`
Expected: FAIL because the classification helper does not exist yet.

**Step 3: Write minimal implementation**

Extract minimal helper logic from `.github/scripts/check-breaking-pr.js` so tests can call it directly.

**Step 4: Run test to verify it passes**

Run: `node --test .github/scripts/check-breaking-pr.test.js`
Expected: PASS

**Step 5: Commit**

```bash
git add .github/scripts/check-breaking-pr.js .github/scripts/check-breaking-pr.test.js
git commit -m "ci: classify explicit breaking PR metadata"
```

### Task 2: Update PR automation prompt and metadata validation behavior

**Files:**
- Modify: `.github/workflows/ai-update-pr.yaml`
- Modify: `.github/scripts/check-breaking-pr.js`
- Modify: `.github/workflows/check-pr-metadata.yml`

**Step 1: Write the failing test**

Extend `.github/scripts/check-breaking-pr.test.js` with a case that expects explicit-breaking-but-AI-non-breaking PRs to emit warning metadata instead of hard validation errors.

**Step 2: Run test to verify it fails**

Run: `node --test .github/scripts/check-breaking-pr.test.js`
Expected: FAIL because the validator still only supports required-section checks.

**Step 3: Write minimal implementation**

Update:
- the workflow prompt so AI never removes an explicit author-provided `!`
- the body rules so AI inserts a warning callout when it thinks an explicit breaking marker is unnecessary
- the validator output model so breaking metadata checks only fail when the PR is truly treated as breaking, not when the warning path applies

**Step 4: Run test to verify it passes**

Run: `node --test .github/scripts/check-breaking-pr.test.js`
Expected: PASS

**Step 5: Commit**

```bash
git add .github/workflows/ai-update-pr.yaml .github/workflows/check-pr-metadata.yml .github/scripts/check-breaking-pr.js .github/scripts/check-breaking-pr.test.js
git commit -m "ci: preserve explicit breaking titles with warnings"
```

### Task 3: Document PR authoring and breaking-change behavior

**Files:**
- Create: `docs/src/guides/pr_metadata_automation.md`
- Modify: `docs/src/guides/index.md`
- Modify: `docs/src/SUMMARY.md`

**Step 1: Write the failing test**

There is no automated docs test in this repo for mdBook navigation; use structural verification instead by adding the guide and navigation entries, then verifying they exist in the rendered book index files.

**Step 2: Run test to verify it fails**

Run: `rg -n "PR metadata automation|breaking change" docs/src/guides/index.md docs/src/SUMMARY.md docs/src/guides/pr_metadata_automation.md`
Expected: FAIL because the new guide file and references do not exist yet.

**Step 3: Write minimal implementation**

Create a guide that explains:
- how to open a PR when you want AI to handle everything
- how to keep your own title/body
- how breaking changes are inferred
- what happens when the author explicitly marks a PR as breaking
- how reviewers/authors should handle the warning callout

**Step 4: Run test to verify it passes**

Run: `rg -n "PR metadata automation|breaking change" docs/src/guides/index.md docs/src/SUMMARY.md docs/src/guides/pr_metadata_automation.md`
Expected: PASS

**Step 5: Commit**

```bash
git add docs/src/guides/pr_metadata_automation.md docs/src/guides/index.md docs/src/SUMMARY.md
git commit -m "docs: add PR metadata automation guide"
```
