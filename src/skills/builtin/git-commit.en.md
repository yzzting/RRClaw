---
name: git-commit
description: Git commit conventions. Generates well-formed commit messages, checks the staging area, and performs atomic commits. Use when the user asks to commit code.
tags: [dev, git]
---

# Git Commit Conventions

## Commit Workflow
1. Run `git status` via shell to see the current state
2. Run `git diff --cached` via shell to inspect staged changes
3. Analyze the changes and generate a commit message that follows the conventions below
4. Execute the commit via shell

## Commit Message Format
```
<type>: <short description (English, max 72 characters)>
```

Allowed types:
- `feat` — new feature
- `fix` — bug fix
- `docs` — documentation changes
- `test` — test-related changes
- `refactor` — refactoring (no external behavior change)
- `chore` — build / dependency / configuration changes

## Principles
- Each commit does exactly one thing (atomic commits)
- Describe **why**, not just **what**
- If the staging area contains multiple unrelated changes, split them into separate commits
- Do not use `git add .` or `git add -A`; specify files explicitly

## Notes
- Never force push without explicit user confirmation
- Do not amend already-published commits
- Before committing, confirm that no `.env` files or files containing secrets have been staged
