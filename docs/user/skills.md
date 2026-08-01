# Skills

Skills are discoverable instruction packs that extend nca's agent behavior. Each skill is a `SKILL.md` file that teaches the agent how to handle specific tasks, frameworks, or workflows.

## How Skills Work

Skills are **not code plugins** — they are structured instruction documents that the agent loads into its context when relevant. When a skill is invoked, the agent reads the skill's `SKILL.md` and follows its instructions.

## Skill Discovery

nca looks for skills in configured directories:

```toml
[harness]
skill_directories = ["skills", ".nca/skills", ".claude/skills", ".agents/skills"]
```

Default search paths (relative to workspace):
1. `.nca/skills/` — nca-specific skills
2. `.claude/skills/` — compatible with Claude Code skills
3. `.agents/skills/` — compatible agent skill packs

The global `~/.agents/skills/` directory is also searched, alongside the
existing nca and Claude-compatible global directories.

### Skill Structure

Each skill is a directory containing a `SKILL.md` file:

```
.nca/skills/
├── rust-patterns/
│   └── SKILL.md
├── api-design/
│   └── SKILL.md
└── testing/
    └── SKILL.md
```

## Managing Skills

### List Skills

```bash
nca skills                    # List all discovered skills
nca skills list               # Same
nca skills list --json        # JSON output
```

Or in interactive mode:

```
/skills
```

### Install Skills

```bash
# Install from a remote source
nca skills add https://github.com/user/skill-repo

# Install specific skills from a source
nca skills add https://github.com/user/skill-repo -s rust-patterns -s testing

# Install globally (available in all workspaces)
nca skills add https://github.com/user/skill-repo --global
```

### Remove Skills

```bash
nca skills remove rust-patterns
nca skills remove rust-patterns --global
```

### Update Skills

```bash
nca skills update                # Update all skills
nca skills update rust-patterns  # Update a specific skill
```

## Using Skills

### Automatic Discovery

The agent's system prompt includes a list of available skills. The agent can choose to invoke relevant skills based on the task.

### Explicit Invocation

Ask the agent to use a skill:

```
Use the rust-patterns skill to review this code.
```

Or the agent can invoke skills programmatically via the `invoke_skill` tool.

When delegating work to a child session with `spawn_subagent`, the parent can also pass a `skills` list so the sub-agent knows which skill names to load first if they match the task.

### Slash Command

```
/skills                    # Open the searchable TUI picker
/skills rust               # Open it with an initial search query
```

In the full-screen TUI, search matches commands, display names, and
descriptions. Use Up/Down (or `j`/`k`) to select a row. Enter inserts
`/<skill> ` into the composer without executing it; add a task and submit the
draft normally. Escape or `q` closes the picker without changing the draft.
Rows include the source directory and mark manual-only skills.

## Writing Skills

### `SKILL.md` Format

Create a `SKILL.md` file in a skill directory:

```markdown
# Skill Name

Brief description of what this skill does.

## When to Use

Describe when this skill should be activated.

## Instructions

Step-by-step instructions for the agent to follow.

### Step 1: Analysis

Analyze the codebase for...

### Step 2: Implementation

Apply the following patterns...

## Examples

### Before
\`\`\`rust
// problematic code
\`\`\`

### After
\`\`\`rust
// improved code
\`\`\`
```

### Best Practices

1. **Be specific** — give clear, actionable instructions the agent can follow
2. **Include examples** — show before/after code when applicable
3. **Define scope** — explain when the skill should and shouldn't be used
4. **Keep it focused** — one skill per concern (don't combine testing and deployment)
5. **Use structured steps** — numbered steps help the agent track progress

## Skill Directories

### Workspace Skills

```
my-project/.nca/skills/my-skill/SKILL.md
```

Available only in this workspace. Good for project-specific conventions.

### Global Skills

```
~/.nca/skills/my-skill/SKILL.md
```

Available in all workspaces. Good for personal coding standards and reusable patterns.

### Claude-Compatible Skills

```
my-project/.claude/skills/my-skill/SKILL.md
```

nca discovers skills from `.claude/skills/` by default, making it compatible with Claude Code skill conventions.

## System Prompt Integration

When skills are available, nca adds a skills section to the system prompt listing all discovered skills by name. The agent can then use the `invoke_skill` tool to load any skill's full instructions on demand.

`AGENTS.md` is also a project instruction source. nca reads the non-empty file
at the configured workspace root on every model turn and layers its complete
text before `.ncarc`, local instructions, and skill summaries. The workspace
root is the scope boundary: parent and descendant `AGENTS.md` files are not
implicitly searched, and an absent root file contributes no instructions.
Root-level `##` sections are additionally exposed as `AGENTS.md`-sourced
skills; that catalog view does not replace or duplicate the full instruction
block. A child session rebuilds the same applicable block for its own workspace
root, while explicitly requested child skills remain resolvable even when they
are manual-only for model discovery.

## Compatible metadata and manual-only skills

`SKILL.md` remains the required contract. Compatible packs may optionally add
`agents/openai.yaml` next to it:

```yaml
interface:
  display_name: Review Changes
  short_description: Inspect a diff
policy:
  allow_implicit_invocation: false
```

Presentation fields only change labels and descriptions. Missing or malformed
optional metadata falls back to `SKILL.md`. Setting
`allow_implicit_invocation: false`, or adding
`disable-model-invocation: true` to `SKILL.md` frontmatter, hides a skill from
model-facing discovery while keeping explicit `/skill` commands, picker
selection, and child-session skill requests available.
