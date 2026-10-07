Keep automated task listings scoped and avoid loading backlog unless needed (SKILL)

Completion note:

COMPLETED 2026-08-25: Updated the bundled `clt-task-management` skill to prefer status-scoped listings, avoid loading backlog unless explicitly requested or operationally necessary, and use scoped examples. Checks: skill frontmatter and scaffold review; `git diff --check -- skills/clt-task-management/SKILL.md`; `cargo test agent_codex_prompt_embeds_only_missing_required_skills -- --test-threads=1` (1 passed). The Skill Creator validator could not run because PyYAML is not installed.
