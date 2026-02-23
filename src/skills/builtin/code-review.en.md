---
name: code-review
description: Code review workflow. Checks code quality, security, and potential bugs. Use when the user asks to review or audit code.
tags: [dev, review]
---

# Code Review

## Review Process
1. Use `file_read` to read the target file(s)
2. Evaluate each of the following dimensions:
   - **Readability**: Are names clear? Is the structure reasonable? Are comments sufficient?
   - **Correctness**: Logic errors, boundary conditions, completeness of error handling
   - **Security**: Injection risks, unauthorized access, sensitive information leakage
   - **Performance**: Unnecessary memory allocations, O(n²) loops, blocking operations
3. For Rust projects, run `cargo clippy -- -W clippy::all` via shell
4. Output a structured report

## Report Format
For each identified issue, output:
- **File:Line** — location
- **Severity** — 🔴 Critical / 🟡 Warning / 🔵 Suggestion
- **Issue** — brief description
- **Recommendation** — fix approach or code example

End with a summary: total issue count, distribution by severity, and an overall rating (Excellent / Good / Needs Improvement).
