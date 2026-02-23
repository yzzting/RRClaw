---
name: find-skills
description: Search and add AI Agent skills. Use when the user wants to add a new skill, browse the skill marketplace, or find a skill for a specific capability.
tags: [skill, registry, search, add]
---

# find-skills

Use this skill when the user wants to search for or add new AI Agent skills.

## Use Cases

1. **User wants to add a new skill**: e.g. "help me add a skill for xxx"
2. **User wants to search for skills**: e.g. "is there a skill that can xxx"
3. **User wants to view the skill list**: e.g. "list all available skills"

## Steps

### 1. Search for skills

Use `npx skills find <keyword>` to search the skill marketplace.

Examples:
```bash
npx skills find "react"
npx skills find "database"
```

### 2. Add a skill

Use `npx skills add <repository-URL> --skill <skill-name>` to add a skill.

Common skill repositories:
- `https://github.com/vercel-labs/skills` — Vercel official skill library
- `https://github.com/bolt-sdk/awesome-bolt-skills` — Bolt skill collection

Examples:
```bash
# Add the find-skills skill from vercel-labs
npx skills add https://github.com/vercel-labs/skills --skill find-skills

# Add another skill
npx skills add https://github.com/vercel-labs/skills --skill <skill-name>
```

### 3. List installed skills

```bash
npx skills list
```

### 4. Remove a skill

```bash
npx skills remove <skill-name>
```

## Notes

- Ensure Node.js and npm are installed before adding skills
- Skills are installed into the `.skills` directory under the user's home directory
- Some skills may require environment variables or API keys to be configured
