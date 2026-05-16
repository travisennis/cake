# Task Artifacts

This directory stores task artifacts for this repository. For the full workflow, read `.agents/TASKS.md`.

Use `.agents/.tasks/index.md` as the generated queue. Create non-completed task files in `active/` and move completed task files to `completed/`, preserving stable three-digit ids such as `001.md`. Keep task status, priority, effort, labels, ExecPlan, and dependencies in front matter so the generated indexes stay useful.

After changing task files or task front matter, regenerate indexes with:

```bash
just task-index
```

To check that generated indexes are current without rewriting them, run:

```bash
just task-index-check
```

Do not edit generated indexes by hand. Update task files and regenerate.