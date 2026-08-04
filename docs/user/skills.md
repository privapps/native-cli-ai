# Skills

Skills are discoverable instruction packs that extend nca's agent behavior. Each skill is a `SKILL.md` file that teaches the agent how to handle specific tasks, frameworks, or workflows.

## How Skills Work

Skills are **not code plugins** — they are structured instruction documents that the agent loads into its context when relevant. When a skill is invoked, the agent reads the skill's `SKILL.md` and follows its instructions.

## Skill Discovery

nca combines compatible skill sources in a stable precedence order. Configure
additional workspace-relative or absolute directories with:

```toml
[harness]
skill_directories = ["skills", ".nca/skills", ".claude/skills", ".agents/skills"]
```

The built-in roots are:

| Source | Location | Purpose |
|--------|----------|---------|
| Product-home skills | `$NCA_HOME/skills/`, `$XDG_DATA_HOME/ncacli/skills/`, or `~/.local/share/ncacli/skills/` | Skills installed for the nca product home |
| User nca skills | `~/.nca/skills/` | nca-compatible global skills |
| User Claude skills | `~/.claude/skills/` | Claude-compatible global skills |
| User agent skills | `~/.agents/skills/` | Agent-compatible global skills |
| Configured roots | `harness.skill_directories` | Explicit workspace or absolute directories |
| Workspace agent skills | `<workspace>/.agents/skills/` | Always-discovered workspace-compatible skills |
| Workspace manifest | `<workspace>/AGENTS.md` | Root instructions plus `##` skill projections |

The product also ships selected manual-only skills inside the `nca` binary.
These embedded skills are the final discovery fallback, so a clean installation
does not depend on the ignored repository `skills/` directory or a manually
copied product directory. Repository and user skills with the same command
continue to take precedence. The shipped `financial-research` skill is
available through explicit `/financial-research` selection, but is excluded
from the model's implicit skill catalog and enables its specialized financial
tools only after that explicit selection.

`AGENTS.md` is parsed before filesystem skills, so its command wins over a
duplicate filesystem command. Filesystem roots keep their configured order;
the workspace `.agents/skills` fallback is still checked when a custom list
replaces the defaults. `nca skills` and `nca skills --json` expose the same
catalog used by model manifests, explicit commands, completion, child-session
requests, and the TUI picker.

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

### Dollar Reference

In an interactive prompt, type `$` to complete a discovered skill reference:

```text
$research compare these designs
Use both $research and $graphify to analyze this repository.
```

`$skill` adds bounded `SKILL.md` guidance to that turn; it does not execute a
skill or grant permissions. References can appear inline, are resolved only
for exact discovered names, and can be repeated without duplicating the
context. Unknown `$tokens`, environment-style forms such as `$HOME`, escaped
references (`\$skill`), and path-like forms remain ordinary text. The skill
body is treated as untrusted task guidance and cannot override system
 instructions, tool permissions, or safety policy.

Each selected body is limited to 32,000 Unicode characters, and newly injected
bodies in one turn share a 96,000-character aggregate limit. Oversized content
shows a truncation marker; later bodies omitted by the aggregate limit show an
explicit omission marker. Use `/skill` or the existing `invoke_skill` path for
the established execution/loading behavior and its normal authorization.

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

The full instruction block and the skill projection have different purposes:
the former guides every ordinary turn, while the latter is loaded only when a
skill is explicitly selected or invoked. Parent and nested `AGENTS.md` files
are not searched implicitly.

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
