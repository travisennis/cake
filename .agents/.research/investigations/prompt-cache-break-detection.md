# Prompt Cache Break Detection System - Implementation Guide

## Overview

The prompt cache break detection system is a two-phase telemetry mechanism that detects and explains why Anthropic's prompt cache might have been invalidated. It tracks changes to prompt configuration between API calls and correlates them with cache metrics from API responses to identify the root cause of cache misses.

## Architecture

The system uses a **two-phase approach**:

1. **Phase 1 (Pre-call)**: `recordPromptState()` captures the current prompt/tool configuration and detects what changed from the previous call
2. **Phase 2 (Post-call)**: `checkResponseForCacheBreak()` analyzes the API response cache metrics to determine if an actual cache break occurred

## Data Structures

### PreviousState

```typescript
type PreviousState = {
  // Content hashes (cache_control stripped)
  systemHash: number
  toolsHash: number
  
  // Hash of cache_control values only - catches scope/TTL flips
  // that stripCacheControl erases from systemHash
  cacheControlHash: number
  
  // Tool identification
  toolNames: string[]
  
  // Per-tool schema hashes for identifying specific changes
  perToolHashes: Record<string, number>
  
  // Metrics
  systemCharCount: number
  model: string
  fastMode: boolean
  globalCacheStrategy: string
  betas: string[]  // Sorted beta header list
  autoModeActive: boolean
  isUsingOverage: boolean
  cachedMCEnabled: boolean
  effortValue: string
  extraBodyHash: number
  
  // Call tracking
  callCount: number
  pendingChanges: PendingChanges | null
  prevCacheReadTokens: number | null
  cacheDeletionsPending: boolean
  
  // For diff generation
  buildDiffableContent: () => string
}
```

### PendingChanges

```typescript
type PendingChanges = {
  // Change flags
  systemPromptChanged: boolean
  toolSchemasChanged: boolean
  modelChanged: boolean
  fastModeChanged: boolean
  cacheControlChanged: boolean
  globalCacheStrategyChanged: boolean
  betasChanged: boolean
  autoModeChanged: boolean
  overageChanged: boolean
  cachedMCChanged: boolean
  effortChanged: boolean
  extraBodyChanged: boolean
  
  // Tool deltas
  addedToolCount: number
  removedToolCount: number
  addedTools: string[]
  removedTools: string[]
  changedToolSchemas: string[]
  
  // Other deltas
  systemCharDelta: number
  previousModel: string
  newModel: string
  prevGlobalCacheStrategy: string
  newGlobalCacheStrategy: string
  addedBetas: string[]
  removedBetas: string[]
  prevEffortValue: string
  newEffortValue: string
  
  // For diff generation
  buildPrevDiffableContent: () => string
}
```

### PromptStateSnapshot (Input to Phase 1)

```typescript
type PromptStateSnapshot = {
  system: TextBlockParam[]  // Array of system prompt blocks
  toolSchemas: ToolUnion[]  // Array of tool definitions
  querySource: QuerySource  // e.g., 'repl_main_thread', 'agent:custom', etc.
  model: string
  agentId?: string
  fastMode?: boolean
  globalCacheStrategy?: string
  betas?: readonly string[]
  autoModeActive?: boolean
  isUsingOverage?: boolean
  cachedMCEnabled?: boolean
  effortValue?: string | number
  extraBodyParams?: unknown
}
```

## State Storage

```typescript
// Global state map with LRU eviction
const previousStateBySource = new Map<string, PreviousState>()
const MAX_TRACKED_SOURCES = 10

// Source prefixes that should be tracked
const TRACKED_SOURCE_PREFIXES = [
  'repl_main_thread',
  'sdk',
  'agent:custom',
  'agent:default',
  'agent:builtin',
]
```

## Phase 1: Record Prompt State (Pre-Call)

### Entry Point

```typescript
export function recordPromptState(snapshot: PromptStateSnapshot): void {
  try {
    // 1. Resolve tracking key
    const key = getTrackingKey(snapshot.querySource, snapshot.agentId)
    if (!key) return  // Untracked source
    
    // 2. Extract and process data
    const processed = processSnapshot(snapshot)
    
    // 3. Get or create state
    const prev = previousStateBySource.get(key)
    
    if (!prev) {
      initializeState(key, processed)
      return
    }
    
    // 4. Update existing state
    updateState(prev, processed)
    
  } catch (e: unknown) {
    // Never throw - this is telemetry
    logError(e)
  }
}
```

### Step 1: Resolve Tracking Key

```typescript
function getTrackingKey(
  querySource: QuerySource,
  agentId?: string,
): string | null {
  // Compact shares cache with main thread
  if (querySource === 'compact') return 'repl_main_thread'
  
  // Check if source should be tracked
  for (const prefix of TRACKED_SOURCE_PREFIXES) {
    if (querySource.startsWith(prefix)) {
      // Use agentId for isolation if available, otherwise use querySource
      return agentId || querySource
    }
  }
  
  // Untracked source (e.g., speculation, session_memory)
  return null
}
```

### Step 2: Process Snapshot

```typescript
interface ProcessedData {
  systemHash: number
  toolsHash: number
  cacheControlHash: number
  toolNames: string[]
  perToolHashes: Record<string, number>
  systemCharCount: number
  sortedBetas: string[]
  effortStr: string
  extraBodyHash: number
  lazyDiffableContent: () => string
  model: string
  fastMode: boolean
  globalCacheStrategy: string
  autoModeActive: boolean
  isUsingOverage: boolean
  cachedMCEnabled: boolean
}

function processSnapshot(snapshot: PromptStateSnapshot): ProcessedData {
  const { 
    system, 
    toolSchemas, 
    model, 
    fastMode, 
    globalCacheStrategy = '',
    betas = [], 
    autoModeActive = false,
    isUsingOverage = false,
    cachedMCEnabled = false,
    effortValue, 
    extraBodyParams 
  } = snapshot
  
  // Strip cache_control from content before hashing
  const strippedSystem = stripCacheControl(system)
  const strippedTools = stripCacheControl(toolSchemas)
  
  // Compute content hashes
  const systemHash = computeHash(strippedSystem)
  const toolsHash = computeHash(strippedTools)
  
  // Compute cache_control hash separately
  const cacheControlHash = computeHash(
    system.map(b => ('cache_control' in b ? b.cache_control : null))
  )
  
  // Extract tool names
  const toolNames = toolSchemas.map(t => ('name' in t ? t.name : 'unknown'))
  
  // Compute per-tool hashes (eagerly - will be used for comparison)
  const perToolHashes = computePerToolHashes(strippedTools, toolNames)
  
  // Count system characters
  const systemCharCount = getSystemCharCount(system)
  
  // Process betas
  const sortedBetas = [...betas].sort()
  
  // Normalize effort value
  const effortStr = effortValue === undefined ? '' : String(effortValue)
  
  // Hash extra body params
  const extraBodyHash = extraBodyParams === undefined ? 0 : computeHash(extraBodyParams)
  
  // Lazy diffable content generator
  const lazyDiffableContent = () => buildDiffableContent(system, toolSchemas, model)
  
  return {
    systemHash,
    toolsHash,
    cacheControlHash,
    toolNames,
    perToolHashes,
    systemCharCount,
    sortedBetas,
    effortStr,
    extraBodyHash,
    lazyDiffableContent,
    model,
    fastMode: fastMode ?? false,
    globalCacheStrategy,
    autoModeActive,
    isUsingOverage,
    cachedMCEnabled,
  }
}
```

### Helper: Strip Cache Control

```typescript
function stripCacheControl<T extends Record<string, unknown>>(
  items: ReadonlyArray<T>,
): unknown[] {
  return items.map(item => {
    if (!('cache_control' in item)) return item
    const { cache_control: _, ...rest } = item
    return rest
  })
}
```

### Helper: Compute Hash

Use a fast non-cryptographic hash. DJB2 is simple and effective:

```typescript
function djb2Hash(str: string): number {
  let hash = 5381
  for (let i = 0; i < str.length; i++) {
    hash = ((hash << 5) + hash) + str.charCodeAt(i) // hash * 33 + c
  }
  return hash >>> 0 // Convert to unsigned 32-bit
}

function computeHash(data: unknown): number {
  const str = JSON.stringify(data, Object.keys(data || {}).sort())
  
  // Prefer runtime-native hash if available
  if (typeof Bun !== 'undefined') {
    const hash = Bun.hash(str)
    return typeof hash === 'bigint' ? Number(hash & BigInt(0xffffffff)) : hash
  }
  
  return djb2Hash(str)
}
```

### Helper: Compute Per-Tool Hashes

```typescript
function computePerToolHashes(
  strippedTools: ReadonlyArray<unknown>,
  names: string[],
): Record<string, number> {
  const hashes: Record<string, number> = {}
  for (let i = 0; i < strippedTools.length; i++) {
    const key = names[i] ?? `__idx_${i}`
    hashes[key] = computeHash(strippedTools[i])
  }
  return hashes
}
```

### Helper: Build Diffable Content

```typescript
function buildDiffableContent(
  system: TextBlockParam[],
  tools: ToolUnion[],
  model: string,
): string {
  const systemText = system.map(b => b.text).join('\n\n')
  
  const toolDetails = tools
    .map(t => {
      if (!('name' in t)) return 'unknown'
      const desc = 'description' in t ? t.description : ''
      const schema = 'input_schema' in t ? JSON.stringify(t.input_schema, null, 2) : ''
      return `${t.name}\n  description: ${desc}\n  input_schema: ${schema}`
    })
    .sort()
    .join('\n\n')
  
  return `Model: ${model}\n\n=== System Prompt ===\n\n${systemText}\n\n=== Tools (${tools.length}) ===\n\n${toolDetails}\n`
}
```

### Step 3: Initialize New State

```typescript
function initializeState(key: string, data: ProcessedData): void {
  // Evict oldest if at capacity (LRU)
  while (previousStateBySource.size >= MAX_TRACKED_SOURCES) {
    const oldest = previousStateBySource.keys().next().value
    if (oldest !== undefined) {
      previousStateBySource.delete(oldest)
    }
  }
  
  previousStateBySource.set(key, {
    systemHash: data.systemHash,
    toolsHash: data.toolsHash,
    cacheControlHash: data.cacheControlHash,
    toolNames: data.toolNames,
    perToolHashes: data.perToolHashes,
    systemCharCount: data.systemCharCount,
    model: data.model,
    fastMode: data.fastMode,
    globalCacheStrategy: data.globalCacheStrategy,
    betas: data.sortedBetas,
    autoModeActive: data.autoModeActive,
    isUsingOverage: data.isUsingOverage,
    cachedMCEnabled: data.cachedMCEnabled,
    effortValue: data.effortStr,
    extraBodyHash: data.extraBodyHash,
    callCount: 1,
    pendingChanges: null,
    prevCacheReadTokens: null,
    cacheDeletionsPending: false,
    buildDiffableContent: data.lazyDiffableContent,
  })
}
```

### Step 4: Update Existing State

```typescript
interface ChangeFlags {
  systemPromptChanged: boolean
  toolSchemasChanged: boolean
  modelChanged: boolean
  fastModeChanged: boolean
  cacheControlChanged: boolean
  globalCacheStrategyChanged: boolean
  betasChanged: boolean
  autoModeChanged: boolean
  overageChanged: boolean
  cachedMCChanged: boolean
  effortChanged: boolean
  extraBodyChanged: boolean
}

function updateState(prev: PreviousState, data: ProcessedData): void {
  prev.callCount++
  
  // Detect changes
  const flags: ChangeFlags = {
    systemPromptChanged: data.systemHash !== prev.systemHash,
    toolSchemasChanged: data.toolsHash !== prev.toolsHash,
    modelChanged: data.model !== prev.model,
    fastModeChanged: data.fastMode !== prev.fastMode,
    cacheControlChanged: data.cacheControlHash !== prev.cacheControlHash,
    globalCacheStrategyChanged: data.globalCacheStrategy !== prev.globalCacheStrategy,
    betasChanged: 
      data.sortedBetas.length !== prev.betas.length ||
      data.sortedBetas.some((b, i) => b !== prev.betas[i]),
    autoModeChanged: data.autoModeActive !== prev.autoModeActive,
    overageChanged: data.isUsingOverage !== prev.isUsingOverage,
    cachedMCChanged: data.cachedMCEnabled !== prev.cachedMCEnabled,
    effortChanged: data.effortStr !== prev.effortValue,
    extraBodyChanged: data.extraBodyHash !== prev.extraBodyHash,
  }
  
  // If any changes detected, compute detailed deltas
  const hasChanges = Object.values(flags).some(Boolean)
  
  if (hasChanges) {
    prev.pendingChanges = computePendingChanges(prev, data, flags)
  } else {
    prev.pendingChanges = null
  }
  
  // Update stored state for next comparison
  prev.systemHash = data.systemHash
  prev.toolsHash = data.toolsHash
  prev.cacheControlHash = data.cacheControlHash
  prev.toolNames = data.toolNames
  prev.systemCharCount = data.systemCharCount
  prev.model = data.model
  prev.fastMode = data.fastMode
  prev.globalCacheStrategy = data.globalCacheStrategy
  prev.betas = data.sortedBetas
  prev.autoModeActive = data.autoModeActive
  prev.isUsingOverage = data.isUsingOverage
  prev.cachedMCEnabled = data.cachedMCEnabled
  prev.effortValue = data.effortStr
  prev.extraBodyHash = data.extraBodyHash
  prev.buildDiffableContent = data.lazyDiffableContent
}
```

### Helper: Compute Pending Changes

```typescript
function computePendingChanges(
  prev: PreviousState,
  data: ProcessedData,
  flags: ChangeFlags,
): PendingChanges {
  // Tool set operations
  const prevToolSet = new Set(prev.toolNames)
  const newToolSet = new Set(data.toolNames)
  const addedTools = data.toolNames.filter(n => !prevToolSet.has(n))
  const removedTools = prev.toolNames.filter(n => !newToolSet.has(n))
  
  // Beta set operations
  const prevBetaSet = new Set(prev.betas)
  const newBetaSet = new Set(data.sortedBetas)
  const addedBetas = data.sortedBetas.filter(b => !prevBetaSet.has(b))
  const removedBetas = prev.betas.filter(b => !newBetaSet.has(b))
  
  // Identify which specific tools changed schemas
  const changedToolSchemas: string[] = []
  if (flags.toolSchemasChanged) {
    for (const name of data.toolNames) {
      if (!prevToolSet.has(name)) continue // Skip newly added tools
      if (data.perToolHashes[name] !== prev.perToolHashes[name]) {
        changedToolSchemas.push(name)
      }
    }
    // Update per-tool hashes for next comparison
    prev.perToolHashes = data.perToolHashes
  }
  
  return {
    systemPromptChanged: flags.systemPromptChanged,
    toolSchemasChanged: flags.toolSchemasChanged,
    modelChanged: flags.modelChanged,
    fastModeChanged: flags.fastModeChanged,
    cacheControlChanged: flags.cacheControlChanged,
    globalCacheStrategyChanged: flags.globalCacheStrategyChanged,
    betasChanged: flags.betasChanged,
    autoModeChanged: flags.autoModeChanged,
    overageChanged: flags.overageChanged,
    cachedMCChanged: flags.cachedMCChanged,
    effortChanged: flags.effortChanged,
    extraBodyChanged: flags.extraBodyChanged,
    addedToolCount: addedTools.length,
    removedToolCount: removedTools.length,
    addedTools,
    removedTools,
    changedToolSchemas,
    systemCharDelta: data.systemCharCount - prev.systemCharCount,
    previousModel: prev.model,
    newModel: data.model,
    prevGlobalCacheStrategy: prev.globalCacheStrategy,
    newGlobalCacheStrategy: data.globalCacheStrategy,
    addedBetas,
    removedBetas,
    prevEffortValue: prev.effortValue,
    newEffortValue: data.effortStr,
    buildPrevDiffableContent: prev.buildDiffableContent,
  }
}
```


## Phase 2: Check Response for Cache Break (Post-Call)

See the source file for full implementation.

## Special Case Notifications

These functions allow external systems to notify the detector of expected cache changes:

```typescript
export function notifyCacheDeletion(querySource: QuerySource, agentId?: string): void {
  const key = getTrackingKey(querySource, agentId)
  const state = key ? previousStateBySource.get(key) : undefined
  if (state) state.cacheDeletionsPending = true
}

export function notifyCompaction(querySource: QuerySource, agentId?: string): void {
  const key = getTrackingKey(querySource, agentId)
  const state = key ? previousStateBySource.get(key) : undefined
  if (state) state.prevCacheReadTokens = null
}

export function cleanupAgentTracking(agentId: string): void {
  previousStateBySource.delete(agentId)
}

export function resetPromptCacheBreakDetection(): void {
  previousStateBySource.clear()
}
```

## Integration Points

### Where to Call Phase 1 (Pre-Call)

Call `recordPromptState()` immediately before making an API request:

```typescript
async function makeApiRequest(params: ApiRequestParams): Promise<Response> {
  const snapshot: PromptStateSnapshot = {
    system: params.systemPrompt,
    toolSchemas: params.tools,
    querySource: params.source,
    model: params.model,
    agentId: params.agentId,
    fastMode: params.fastMode,
    globalCacheStrategy: params.cacheStrategy,
    betas: params.betaHeaders,
    autoModeActive: params.autoMode,
    isUsingOverage: params.overageEnabled,
    cachedMCEnabled: params.cachedMicrocompact,
    effortValue: params.effort,
    extraBodyParams: params.extraBody,
  }
  
  recordPromptState(snapshot)
  
  const response = await anthropic.messages.create({
    model: params.model,
    system: params.systemPrompt,
    tools: params.tools,
    messages: params.messages,
  })
  
  return response
}
```

### Where to Call Phase 2 (Post-Call)

Call `checkResponseForCacheBreak()` immediately after receiving an API response:

```typescript
const cacheReadTokens = response.usage.cache_read_input_tokens ?? 0
const cacheCreationTokens = response.usage.cache_creation_input_tokens ?? 0

await checkResponseForCacheBreak(
  params.source,
  cacheReadTokens,
  cacheCreationTokens,
  params.messages,
  params.agentId,
  response.id,
)
```

## Constants

```typescript
const MAX_TRACKED_SOURCES = 10
const MIN_CACHE_MISS_TOKENS = 2000
const CACHE_TTL_5MIN_MS = 5 * 60 * 1000      // 300,000ms
const CACHE_TTL_1HOUR_MS = 60 * 60 * 1000    // 3,600,000ms

function isExcludedModel(model: string): boolean {
  return model.includes("haiku")
}
```

## Error Handling

All functions must be fail-safe. Never let telemetry errors affect the main application:

```typescript
function recordPromptState(snapshot: PromptStateSnapshot): void {
  try {
    // ... implementation
  } catch (e: unknown) {
    logError(e)
  }
}
```

## Performance Considerations

1. **Lazy hashing**: Only compute per-tool hashes when tools actually change
2. **Lazy diffable content**: buildDiffableContent is only called when needed (cache break detected)
3. **Map size limit**: Cap at 10 sources to prevent unbounded growth with many subagents
4. **Fast hash function**: Use DJB2 or runtime-native hash, not cryptographic hashes

