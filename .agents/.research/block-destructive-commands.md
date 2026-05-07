# Blocking Destructive Commands in Bash Tool

This document describes how to implement command protection in a Bash tool to prevent destructive operations that could cause data loss. The implementation is language-agnostic, with notes for Rust implementation where relevant.

## Architecture Overview

The protection system has two main components:

1. **Detection Module** - Pure functions that analyze command strings and return whether they should be blocked
2. **Integration Point** - Calls detection before command execution and throws an error if blocked

### Flow

```
User Command → validateCommand() → detectDestructiveCommand() → 
  If blocked: throw Error with formatted message
  If safe: proceed with execution
```

### Check Order (Important)

Checks must run in this order because earlier checks can contain later patterns:

1. **Inline scripts** (`bash -c`, `python -c`, etc.) - checked first because they can contain any destructive command
2. **Heredocs** (`<<EOF ... EOF`) - checked second for same reason
3. **Git commands** - direct destructive git operations
4. **rm -rf** - direct filesystem operations

This ordering ensures that `bash -c "git reset --hard"` is caught by the inline script detector, not missed because the git check comes first.

## Detection Module Implementation

### Types

```typescript
export interface BlockedCommandResult {
  blocked: true;
  reason: string;
  command: string;
  tip: string;
}

export interface SafeCommandResult {
  blocked: false;
}

export type CommandSafetyResult = BlockedCommandResult | SafeCommandResult;
```

### Main Detection Function

```typescript
export function detectDestructiveCommand(command: string): CommandSafetyResult {
  const trimmed = command.trim();

  // Check categories in order - most specific first
  
  // 1. Inline scripts (bash -c, python -c, etc.)
  const inlineScriptResult = detectDangerousInlineScripts(trimmed);
  if (inlineScriptResult.blocked) return inlineScriptResult;

  // 2. Heredocs and here-strings
  const heredocResult = detectDangerousHeredocs(trimmed);
  if (heredocResult.blocked) return heredocResult;

  // 3. Git destructive commands
  const gitResult = detectDestructiveGitCommands(trimmed);
  if (gitResult.blocked) return gitResult;

  // 4. Dangerous rm -rf
  const rmResult = detectDangerousRmRf(trimmed);
  if (rmResult.blocked) return rmResult;

  return { blocked: false };
}
```

## Blocked Commands

### 1. Git Operations

#### `git reset --hard` and `git reset --merge`

**Reason**: Destroys all uncommitted changes without recovery.

```typescript
if (
  lowerCommand.includes("git reset --hard") ||
  lowerCommand.includes("git reset --merge")
) {
  return {
    blocked: true,
    reason: "git reset --hard or --merge destroys uncommitted changes",
    command,
    tip: "Consider using 'git stash' first to save your changes, or use 'git reset --soft' to preserve changes.",
  };
}
```

#### `git checkout -- <file>`

**Reason**: Discards uncommitted file changes.

```typescript
if (lowerCommand.match(/git\s+checkout\s+--\s+\S+/)) {
  return {
    blocked: true,
    reason: "git checkout -- <file> discards uncommitted file changes",
    command,
    tip: "Use 'git restore --staged <file>' to unstage changes, or 'git stash' to save changes temporarily.",
  };
}
```

#### `git restore <file>` (without `--staged`)

**Reason**: Discards uncommitted changes. The `--staged` flag is safe because it only unstages.

**Edge case**: `git restore -b <branch>` creates a new branch and should NOT be blocked.

```typescript
if (
  lowerCommand.startsWith("git restore") &&
  !lowerCommand.includes(" --staged")
) {
  const afterRestore = lowerCommand.substring("git restore".length).trim();
  // Only block if it's restoring files (not branches with -b flag)
  if (afterRestore && !afterRestore.startsWith("-b")) {
    return {
      blocked: true,
      reason: "git restore <file> (without --staged) discards uncommitted changes",
      command,
      tip: "Use 'git restore --staged <file>' to only unstage, or 'git stash' to save changes.",
    };
  }
}
```

#### `git clean -f` and `git clean --force`

**Reason**: Permanently deletes untracked files.

```typescript
if (
  lowerCommand.includes("git clean -f") ||
  lowerCommand.includes("git clean --force")
) {
  return {
    blocked: true,
    reason: "git clean -f permanently deletes untracked files",
    command,
    tip: "Use 'git clean -n' to preview what would be deleted, or 'git clean -f -d' to only delete untracked directories.",
  };
}
```

#### `git push --force` (but allow `--force-with-lease`)

**Reason**: Overwrites remote commit history, can lose others' work.

**Important**: `--force-with-lease` is allowed because it checks that the remote hasn't been updated by someone else first.

```typescript
if (
  (lowerCommand.includes("git push --force") &&
    !lowerCommand.includes("--force-with-lease")) ||
  lowerCommand.match(/git\s+push\s+-f\b/)
) {
  return {
    blocked: true,
    reason: "git push --force overwrites remote commit history",
    command,
    tip: "Use 'git push --force-with-lease' for safer force pushes, or prefer creating a new branch instead.",
  };
}
```

#### `git branch -D` (uppercase D = force delete)

**Reason**: Force deletes branches without checking if they're merged.

**Implementation note**: Must match `git` case-insensitively (for `Git`, `GIT`, etc.) but preserve case for the flag. Lowercase `-d` is safe (checks merge status), uppercase `-D` is blocked.

**Important**: Must also verify the command is an actual git command, not text inside quoted strings (e.g., commit messages).

```typescript
const branchMatch = lowerCommand.match(/git\s+branch\s+-([a-z])/);
if (branchMatch) {
  const flagInOriginal = command.match(/git\s+branch\s+-([A-Za-z])/i);
  if (flagInOriginal && flagInOriginal[1] === flagInOriginal[1].toUpperCase()) {
    // Verify this is an actual git branch command, not text inside quotes
    const isActualCommand = isActualGitCommand(command, "branch");
    if (isActualCommand) {
      return {
        blocked: true,
        reason: "git branch -D force-deletes branches without checking if they're merged",
        command,
        tip: "Use 'git branch -d' (lowercase) to safely delete branches that are merged.",
      };
    }
  }
}
```

**Why this approach**: Using `/i` flag on `/git\s+branch\s+-[A-Z]/i` would make `[A-Z]` match both uppercase and lowercase, incorrectly blocking `git branch -d`. The solution is to:
1. Use `lowerCommand` to match `git` case-insensitively
2. Use the original `command` to check if the flag character is uppercase
3. Use `isActualGitCommand()` to verify it's not text inside quoted strings

**Rust note**: In Rust, use `fancy_regex::Regex::new(r"(?i)git\s+branch\s+-([A-Za-z])")` to capture the flag, then check if `capture[1].is_uppercase()`.

#### `git stash drop` and `git stash clear`

**Reason**: Permanently deletes stashed changes.

```typescript
if (
  lowerCommand.includes("git stash drop") ||
  lowerCommand.includes("git stash clear")
) {
  return {
    blocked: true,
    reason: "git stash drop/clear permanently deletes stashed changes",
    command,
    tip: "Use 'git stash list' to see stashes, or 'git stash pop' to apply and remove a stash.",
  };
}
```

### 2. Filesystem Operations (`rm -rf`)

**Policy**: Block `rm -rf` everywhere EXCEPT temporary directories.

**Allowed paths**:
- `/tmp/*`
- `/var/tmp/*`
- `$TMPDIR/*`
- `${TMPDIR}/*`

```typescript
function detectDangerousRmRf(command: string): CommandSafetyResult {
  const rmMatch = command.match(/rm\s+-rf\s+/i);
  if (!rmMatch || rmMatch.index === undefined) {
    return { blocked: false };
  }

  const matchIndex = rmMatch.index + rmMatch[0].length;
  const afterRmRf = command.substring(matchIndex).trim();

  if (!afterRmRf) {
    return { blocked: false }; // Will fail naturally
  }

  const tempDirs = ["/tmp", "/var/tmp", process.env["TMPDIR"] || "/tmp"];

  // Check if path starts with a temp directory
  const isTempDirectoryOnly = tempDirs.some((tempDir) => {
    if (afterRmRf === tempDir || afterRmRf.startsWith(`${tempDir}/`)) {
      return true;
    }
    return false;
  });

  // Check for $TMPDIR variable patterns
  const tmpDirVar = "$" + "TMPDIR";
  const tmpDirVarBraces = "$" + "{TMPDIR}";
  if (
    afterRmRf === tmpDirVar ||
    afterRmRf.startsWith(`${tmpDirVar}/`) ||
    afterRmRf === tmpDirVarBraces ||
    afterRmRf.startsWith(`${tmpDirVarBraces}/`)
  ) {
    return { blocked: false };
  }

  if (isTempDirectoryOnly) {
    return { blocked: false };
  }

  return {
    blocked: true,
    reason: "rm -rf outside of temporary directories can cause permanent data loss",
    command,
    tip: "Only rm -rf is allowed for /tmp/*, /var/tmp/*, or $TMPDIR/* to clean temporary files.",
  };
}
```

### 3. Inline Scripts

Scripts passed via `-c` or `-e` flags can contain destructive commands that need to be scanned.

**Languages checked**:
- `bash -c`
- `sh -c`
- `python -c` / `python3 -c`
- `node -e`
- `npx -c`
- `ruby -e`
- `perl -e`

```typescript
function detectDangerousInlineScripts(command: string): CommandSafetyResult {
  const languagePatterns = [
    { pattern: /\bbash\s+-c\s+\S+/i, language: "bash" },
    { pattern: /\bsh\s+-c\s+\S+/i, language: "sh" },
    { pattern: /\bpython\d?\s+-c\s+\S+/i, language: "Python" },
    { pattern: /\bnode\s+-e\s+\S+/i, language: "Node.js" },
    { pattern: /\bnpx\s+-c\s+\S+/i, language: "npx" },
    { pattern: /\bruby\s+-e\s+\S+/i, language: "Ruby" },
    { pattern: /\bperl\s+-e\s+\S+/i, language: "Perl" },
  ];

  for (const { pattern, language } of languagePatterns) {
    if (pattern.test(command)) {
      // Scan for destructive patterns within the script
      const destructivePatterns = [
        /git\s+reset\s+--hard/i,
        /git\s+reset\s+--merge/i,
        /git\s+clean\s+-f/i,
        /git\s+checkout\s+--\s+\S+/i,
        /git\s+push\s+(-f|--force)/i,
        /git\s+branch\s+-[A-Z]/i,
        /git\s+stash\s+(drop|clear)/i,
        /rm\s+-rf\s+\/home/i,
        /rm\s+-rf\s+\/usr/i,
        /rm\s+-rf\s+~/i,
      ];

      for (const destructivePattern of destructivePatterns) {
        if (destructivePattern.test(command)) {
          return {
            blocked: true,
            reason: `Inline ${language} script contains destructive operation`,
            command,
            tip: "Review the script content for destructive commands.",
          };
        }
      }
    }
  }

  return { blocked: false };
}
```

### 4. Heredocs and Here-strings

Heredocs (`<<EOF ... EOF`) and here-strings (`<<<`) can contain multi-line scripts with destructive commands.

#### Heredoc Pattern Breakdown

The regex `/<<-?\s*['"]?(\w+)['"]?\s*([\s\S]*?)\n\1\b/gi`:

- `<<-?` - `<<` followed by optional `-` (tab-stripped heredoc)
- `\s*['"]?` - optional whitespace and quote
- `(\w+)` - capture group 1: the delimiter word (e.g., `EOF`)
- `['"]?\s*` - optional closing quote and whitespace
- `([\s\S]*?)` - capture group 2: content (non-greedy, matches any char including newlines)
- `\n\1\b` - newline followed by the same delimiter (backreference) as a word boundary

**Rust note**: Use `(?s:.*?)` instead of `[\s\S]*?` for matching across newlines. The `fancy-regex` crate supports backreferences for the delimiter matching.

```rust
// Rust example using fancy-regex for backreferences
let heredoc_pattern = fancy_regex::Regex::new(
  r"<<-?\s*['\"]?(\w+)['\"]?\s*((?s:.*?))\n\1\b"
).unwrap();
```

#### Here-string Pattern

The regex `/<<<\s*(['"])([^"']+)\1/gi`:

- `<<<` - here-string operator
- `\s*` - optional whitespace
- `(['"])` - capture group 1: opening quote
- `([^"']+)` - capture group 2: content (not containing the quote)
- `\1` - matching closing quote (backreference)

```typescript
function detectDangerousHeredocs(command: string): CommandSafetyResult {
  // Match heredoc patterns: <<EOF ... EOF
  const heredocPattern = /<<-?\s*['"]?(\w+)['"]?\s*([\s\S]*?)\n\1\b/gi;
  let match = heredocPattern.exec(command);
  
  while (match !== null) {
    const heredocContent = match[2];
    const heredocResult = detectDangerousScriptContent(heredocContent);
    if (heredocResult.blocked) {
      return {
        blocked: true,
        reason: "Heredoc contains destructive operation",
        command,
        tip: heredocResult.tip || "Review the heredoc content for destructive commands.",
      };
    }
    match = heredocPattern.exec(command);
  }

  // Match here-string patterns (<<<)
  const hereStringPattern = /<<<\s*(['"])([^"']+)\1/gi;
  match = hereStringPattern.exec(command);
  
  while (match !== null) {
    const stringContent = match[2];
    const stringResult = detectDangerousScriptContent(stringContent);
    if (stringResult.blocked) {
      return {
        blocked: true,
        reason: "Here-string contains destructive operation",
        command,
        tip: stringResult.tip || "Review the here-string content for destructive commands.",
      };
    }
    match = hereStringPattern.exec(command);
  }

  return { blocked: false };
}
```

### Script Content Scanner

Used by both heredoc and inline script detection:

```typescript
function detectDangerousScriptContent(content: string): {
  blocked: boolean;
  tip?: string;
} {
  const lowerContent = content.toLowerCase();

  // Destructive git patterns
  const dangerousGitPatterns = [
    /\bgit\s+reset\s+(--hard|--merge|--keep)\b/i,
    /\bgit\s+clean\s+-f\b/i,
    /\bgit\s+checkout\s+--\s+\S+/i,
    /\bgit\s+restore\s+(?!--staged)\s+\S+/i,
    /\bgit\s+push\s+(-f|--force)\b/i,
    /\bgit\s+branch\s+-[D]\b/i,
    /\bgit\s+stash\s+(drop|clear)\b/i,
  ];

  for (const pattern of dangerousGitPatterns) {
    if (pattern.test(lowerContent)) {
      return {
        blocked: true,
        tip: "The script contains a destructive git command. Review the script content.",
      };
    }
  }

  // Dangerous rm -rf patterns
  const dangerousRmPatterns = [
    /\brm\s+-rf\s+\/[^\s*]*[a-z]/i,  // rm -rf /something (allow /tmp/* patterns)
    /\brm\s+-rf\s+\/home/i,
    /\brm\s+-rf\s+\/usr/i,
    /\brm\s+-rf\s+\/etc/i,
    /\brm\s+-rf\s+\/var\s*$/i,       // Block /var alone but allow /var/tmp
    /\brm\s+-rf\s+~(?!\/)/i,         // rm -rf ~ but allow ~/tmp
  ];

  for (const pattern of dangerousRmPatterns) {
    if (pattern.test(content)) {
      return {
        blocked: true,
        tip: "The script contains a dangerous rm -rf command.",
      };
    }
  }

  // Variable expansion dangers
  const dangerousPatterns = [
    /\brm\s+-rf\s+\$\w+/i,  // rm -rf $VAR (variable could expand to anything)
  ];

  for (const pattern of dangerousPatterns) {
    if (pattern.test(content)) {
      return {
        blocked: true,
        tip: "The script contains a potentially dangerous rm command with variable expansion.",
      };
    }
  }

  return { blocked: false };
}
```

## Integration in Bash Tool

In the bash tool's validation function:

```typescript
function validateCommand(
  command: string,
  allowedDirs: string[],
  cwd: string,
): void {
  // ... other validations ...

  const destructiveCheck = detectDestructiveCommand(command);
  if (destructiveCheck.blocked) {
    throw new Error(formatBlockedCommandMessage(destructiveCheck));
  }
}
```

### Error Message Formatting

```typescript
export function formatBlockedCommandMessage(
  result: BlockedCommandResult,
): string {
  return `BLOCKED

Reason: ${result.reason}

Command: ${result.command}

Tip: ${result.tip}`;
}
```

## Example Block Messages

### Git Reset Hard

```
BLOCKED

Reason: git reset --hard or --merge destroys uncommitted changes

Command: git reset --hard HEAD~1

Tip: Consider using 'git stash' first to save your changes, or use 'git reset --soft' to preserve changes.
```

### Force Push

```
BLOCKED

Reason: git push --force overwrites remote commit history

Command: git push --force origin main

Tip: Use 'git push --force-with-lease' for safer force pushes, or prefer creating a new branch instead.
```

### RM RF

```
BLOCKED

Reason: rm -rf outside of temporary directories can cause permanent data loss

Command: rm -rf ~/projects

Tip: Only rm -rf is allowed for /tmp/*, /var/tmp/*, or $TMPDIR/* to clean temporary files.
```

### Inline Script

```
BLOCKED

Reason: Inline bash script contains destructive operation

Command: bash -c "git clean -f && git reset --hard"

Tip: Review the script content for destructive commands.
```

## Testing Considerations

When implementing tests, cover:

1. **Direct commands**: `git reset --hard`, `rm -rf /home/user`
2. **Variations**: `git reset --hard HEAD`, `git reset --hard HEAD~3`
3. **Case insensitivity**: `GIT RESET --HARD`, `Git Reset --Hard`
4. **Whitespace variations**: `git  reset  --hard`, `git	reset	--hard`
5. **Allowed commands**: `git push --force-with-lease`, `rm -rf /tmp/*`
6. **Inline scripts**: `bash -c "git reset --hard"`
7. **Heredocs**: Multi-line scripts with destructive commands
8. **Edge cases**: Commands that look similar but are safe
9. **git restore -b**: Should NOT be blocked (creates branch, not restoring files)
10. **git branch -d vs -D**: Lowercase allowed, uppercase blocked
11. **Temp directory variations**: `/tmp/foo`, `$TMPDIR/foo`, `${TMPDIR}/foo`

## Rust Implementation Notes

### Regex Crate Choice

Use `fancy-regex` instead of standard `regex` crate because:
- Backreferences needed for heredoc delimiter matching (`\1`)
- Lookahead needed for some patterns (`(?!--staged)`)

```toml
[dependencies]
fancy-regex = "0.13"
```

### Environment Variables

```rust
// Get TMPDIR equivalent
let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
let temp_dirs = vec!["/tmp", "/var/tmp", &tmpdir];
```

### Case Insensitivity

```rust
// Use (?i) flag for case-insensitive matching
let pattern = fancy_regex::Regex::new(r"(?i)git\s+reset\s+--hard").unwrap();

// Or use .to_lowercase() on input and match lowercase patterns
let lower_command = command.to_lowercase();
if lower_command.contains("git reset --hard") { ... }
```

### Word Boundaries

```rust
// \b works in Rust regex for word boundaries
let pattern = fancy_regex::Regex::new(r"(?i)\bgit\s+reset\b").unwrap();
```

### Matching Across Newlines

```rust
// Use (?s:...) for dot-matches-newline mode
let heredoc = fancy_regex::Regex::new(
  r"<<-?\s*['\"]?(\w+)['\"]?\s*((?s:.*?))\n\1\b"
).unwrap();
```

## Design Principles

1. **Fail closed**: Block by default, allow explicitly
2. **Clear messages**: Every block has a reason and a tip
3. **Layered defense**: Check inline scripts and heredocs, not just direct commands
4. **Practical**: Allow safe alternatives like `--force-with-lease` and temp directory cleanup
5. **No false negatives**: Better to block a safe command than allow a destructive one
6. **Avoid false positives**: Don't block commands that only contain destructive text as data

## Avoiding False Positives

### Heredocs Used as Data

Heredocs are often used for data (commit messages, config files) rather than executable scripts. Only block heredocs that are explicitly executed:

**Blocked** (executed by shell):
```bash
bash <<EOF
git reset --hard
EOF

cat <<EOF | bash
git clean -f
EOF
```

**Allowed** (used as data):
```bash
git commit -m "$(cat <<'EOF'
fix: handle git branch -D correctly
EOF
)"

cat > config.yml <<EOF
name: test
EOF
```

### Commands Inside Quoted Strings

Destructive command text inside quoted strings (e.g., commit messages) should not be blocked. Use the `isActualGitCommand()` helper:

```typescript
function isActualGitCommand(command: string, subcommand: string): boolean {
  const pattern = new RegExp(`\\bgit\\s+${subcommand}\\b`, "gi");
  const matches: { index: number }[] = [];
  let match: RegExpExecArray | null = pattern.exec(command);

  while (match !== null) {
    matches.push({ index: match.index });
    match = pattern.exec(command);
  }

  if (matches.length === 0) return false;

  for (const { index } of matches) {
    if (!isInsideQuotes(command, index)) {
      const beforeMatch = command.slice(0, index).trim();
      if (
        beforeMatch === "" ||
        beforeMatch.endsWith("&&") ||
        beforeMatch.endsWith("||") ||
        beforeMatch.endsWith(";") ||
        beforeMatch.endsWith("|") ||
        beforeMatch.endsWith("\n")
      ) {
        return true;
      }
    }
  }

  return false;
}

function isInsideQuotes(command: string, position: number): boolean {
  let inSingleQuote = false;
  let inDoubleQuote = false;
  let escaped = false;

  for (let i = 0; i < position; i++) {
    const char = command[i];

    if (escaped) {
      escaped = false;
      continue;
    }

    if (char === "\\") {
      escaped = true;
      continue;
    }

    if (char === '"' && !inSingleQuote) {
      inDoubleQuote = !inDoubleQuote;
    } else if (char === "'" && !inDoubleQuote) {
      inSingleQuote = !inSingleQuote;
    }
  }

  return inSingleQuote || inDoubleQuote;
}
```

**Blocked** (actual command):
```bash
git branch -D feature
```

**Allowed** (text inside commit message):
```bash
git commit -m "fix: handle git branch -D correctly"
```

## Summary Table

| Pattern | Blocked | Allowed Alternative |
|---------|---------|---------------------|
| `git reset --hard` | Yes | `git reset --soft`, `git stash` |
| `git reset --merge` | Yes | `git stash` |
| `git checkout -- <file>` | Yes | `git restore --staged <file>` |
| `git restore <file>` | Yes | `git restore --staged <file>` |
| `git restore -b <branch>` | No | N/A (creates branch) |
| `git clean -f` | Yes | `git clean -n` (preview) |
| `git push --force` | Yes | `git push --force-with-lease` |
| `git branch -D` | Yes | `git branch -d` |
| `git stash drop` | Yes | `git stash pop` |
| `git stash clear` | Yes | `git stash pop` |
| `rm -rf /tmp/*` | No | N/A (allowed) |
| `rm -rf /var/tmp/*` | No | N/A (allowed) |
| `rm -rf $TMPDIR/*` | No | N/A (allowed) |
| `rm -rf <other>` | Yes | N/A |
| Inline script with above | Yes | N/A |
| Heredoc executed by shell | Yes | N/A |
| Heredoc used as data | No | N/A (commit messages, config files) |
| Command in quoted string | No | N/A (text, not executed) |