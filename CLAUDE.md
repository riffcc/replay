# Agent Instructions

This project uses the native **Tasks** tool for issue tracking whenever possible. The Tasks tool uses **bd** (beads) under the hood in this repo, so prefer native task tool calls first and use direct `bd` CLI commands as a fallback when the native tool cannot express the needed operation. Run `bd onboard` to get started if you need the CLI fallback.

## Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work atomically
bd close <id>         # Complete work
bd dolt push          # Push beads data to remote
```

## Task Tracking Preference

- Prefer the native **Tasks** tool and native tool calls whenever possible
- Use direct `bd` CLI commands as a fallback
- Do not use markdown TODO lists for project tracking
- If there is a mismatch, treat Tasks as the preferred interface and beads as the backing store

## Tooling Preference

- Prefer native structured developer tools over ad hoc shell commands when possible
- Prefer **Read** for inspecting code and files instead of shelling out to `sed`, `cat`, or one-off Python readers
- **Read can be used on files anywhere on the machine**, not just inside the current repo, so prefer it for SmartRead-style inspection of sibling projects such as `~/projects/llm-code-sdk`
- Use `search`, `grep`, `glob`, and `list_directory` for discovery before falling back to shell exploration
- Reserve shell commands primarily for builds, tests, git, repo-specific CLIs, and cases where the native tools cannot express the needed operation cleanly

## Non-Interactive Shell Commands

**ALWAYS use non-interactive flags** with file operations to avoid hanging on confirmation prompts.

Shell commands like `cp`, `mv`, and `rm` may be aliased to include `-i` (interactive) mode on some systems, causing the agent to hang indefinitely waiting for y/n input.

**Use these forms instead:**
```bash
# Force overwrite without prompting
cp -f source dest           # NOT: cp source dest
mv -f source dest           # NOT: mv source dest
rm -f file                  # NOT: rm file

# For recursive operations
rm -rf directory            # NOT: rm -r directory
cp -rf source dest          # NOT: cp -r source dest
```

**Other commands that may prompt:**
- `scp` - use `-o BatchMode=yes` for non-interactive
- `ssh` - use `-o BatchMode=yes` to fail instead of prompting
- `apt-get` - use `-y` flag
- `brew` - use `HOMEBREW_NO_AUTO_UPDATE=1` env var

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:ca08a54f -->
## Beads Issue Tracker

This project uses **bd (beads)** as the backing store for task tracking. Prefer the native **Tasks** tool first; run `bd prime` to see full workflow context and commands when you need the CLI fallback.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Prefer the native **Tasks** tool for task tracking whenever possible
- Use direct `bd` commands as a fallback when the native tool cannot perform the required action
- Do NOT use markdown TODO lists for task tracking
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

## Session Completion

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
   bd dolt push
   git push
   git status  # MUST show "up to date with origin"
   ```
5. **Clean up** - Clear stashes, prune remote branches
6. **Verify** - All changes committed AND pushed
7. **Hand off** - Provide context for next session

**CRITICAL RULES:**
- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing - that leaves work stranded locally
- NEVER say "ready to push when you are" - YOU must push
- If push fails, resolve and retry until it succeeds
<!-- END BEADS INTEGRATION -->
