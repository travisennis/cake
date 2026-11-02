# Repository Rulesets

`default-branch-safety.json` defines the only branch ruleset: it blocks deletion and force-pushes on the default branch. It requires no pull request, status checks, or linear history.

Apply it after changing the file:

```sh
gh api --method POST repos/travisennis/cake/rulesets \
  --input .github/rulesets/default-branch-safety.json
```

`POST` creates a ruleset. To update an existing one, list the rulesets to get its id and use `PUT`:

```sh
gh api repos/travisennis/cake/rulesets
gh api --method PUT repos/travisennis/cake/rulesets/<id> \
  --input .github/rulesets/default-branch-safety.json
```
