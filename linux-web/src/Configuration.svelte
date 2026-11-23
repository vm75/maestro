<script lang="ts">
  import { onMount } from 'svelte'
  import { invokeTool, type Discovery, type Status } from './lib/api'

  let { status: _status, discovery }: { status: Status; discovery: Discovery } = $props()

  interface Input {
    id: string
    kind: string
    strategy: string
    path?: string
    required: boolean
    sensitive: boolean
    inspection: string
    logicalBytes?: number
  }
  interface Requirement { resource: string; kind: string; description: string }
  interface Issue { severity: string; resource: string; code: string; message: string; remedy: string }
  interface Inventory {
    stage: string
    revision: string
    checkedAt: string
    restorable: boolean
    inputs: Input[]
    requirements: Requirement[]
    issues: Issue[]
    exclusions: Array<{kind: string; reason: string}>
  }

  let revision = $state<string | null>(null)
  let history = $state<string[]>([])
  let inventory = $state<Inventory | null>(null)
  let editorKind = $state<'storage' | 'share'>('storage')
  let editId = $state('')
  let editOperation = $state<'save' | 'remove'>('save')
  let storagePath = $state('')
  let storageKind = $state<'directory' | 'file'>('directory')
  let storageFormat = $state<'raw' | 'qcow2'>('raw')
  let shareProtocol = $state<'samba' | 'nfs'>('samba')
  let shareStorage = $state('')
  let shareEnabled = $state(false)
  let shareReadOnly = $state(true)
  let editorSaving = $state(false)
  let editorPlanning = $state(false)
  let editorError = $state('')
  let editorJob = $state('')
  let staged = $state<{key: string; result: {
    plan: { fingerprint: string; changes: Array<{resource: string; operation: string; backend: string}> }
    blockedResources: string[]
  }} | null>(null)

  const editDefinition = $derived(
    editorKind === 'storage'
      ? JSON.stringify({kind: storageKind, path: storagePath,
          ...(storageKind === 'file' ? { format: storageFormat } : {})})
      : JSON.stringify({protocol: shareProtocol, storage: shareStorage,
          enabled: shareEnabled, read_only: shareReadOnly})
  )
  const editKey = $derived(JSON.stringify({
    kind: editorKind, id: editId, operation: editOperation,
    definition: editOperation === 'save' ? editDefinition : undefined,
    revision: revision ?? 'missing'
  }))
  const reviewed = $derived(staged?.key === editKey ? staged.result : null)
  let loading = $state(false)
  let error = $state('')
  let inventoryError = $state('')
  const writable = $derived(discovery.modules.some(module => module.id === 'workloads' && module.writable))

  async function inspect() {
    if (loading) return
    loading = true
    error = ''
    try {
      const [current, revisions] = await Promise.all([
        invokeTool<{yaml: string; revision: string | null}>('workloads_get', {}),
        invokeTool<{history: string[]}>('workloads_history', {}),
      ])
      revision = current.revision
      history = revisions.history
    } catch (failure) {
      error = failure instanceof Error ? failure.message : 'Could not read configuration.'
    } finally {
      loading = false
    }
  }

  async function inspectBackup() {
    inventoryError = ''
    inventory = null
    try {
      inventory = await invokeTool<Inventory>('managed_backup_inventory', {})
    } catch (failure) {
      inventoryError = failure instanceof Error ? failure.message : 'Could not read backup input inventory.'
    }
  }

  function typedArgs() {
    return {
      kind: editorKind,
      id: editId.trim(),
      operation: editOperation,
      ...(editOperation === 'save' ? {definition: editDefinition} : {}),
      revision: revision ?? 'missing',
    }
  }

  async function planTyped(event: SubmitEvent) {
    event.preventDefault()
    if (editorPlanning || editorSaving || !writable) return
    editorPlanning = true
    editorError = ''
    editorJob = ''
    staged = null
    try {
      const result = await invokeTool<{
        plan: {fingerprint: string; changes: Array<{resource: string; operation: string; backend: string}>}
        blockedResources: string[]
      }>('managed_resource_plan', typedArgs())
      staged = {key: editKey, result}
    } catch (failure) {
      editorError = failure instanceof Error ? failure.message : 'Could not preview resource change.'
    } finally {
      editorPlanning = false
    }
  }

  async function saveTyped() {
    if (!reviewed || editorSaving || !writable || reviewed.blockedResources.length) return
    if (!window.confirm('Apply the reviewed resource edit now? Native workloads may be interrupted.')) return
    editorSaving = true
    editorError = ''
    editorJob = ''
    try {
      const started = await invokeTool<{job: {id: string; state: string}}>(
        'managed_resource_save', {
          ...typedArgs(), mode: 'now', fingerprint: reviewed.plan.fingerprint, confirm: true
        }
      )
      let job = started.job
      editorJob = 'Apply job: ' + job.id + ' (' + job.state + ')'
      while (job.state === 'running') {
        await new Promise(resolve => setTimeout(resolve, 1000))
        job = await invokeTool<{id: string; state: string; error?: string}>('jobs_get', {id: job.id})
        editorJob = 'Apply job: ' + job.id + ' (' + job.state + ')'
        if (job.state === 'failed') editorError = job.error ?? 'Resource apply failed.'
      }
      if (job.state === 'succeeded') {
        staged = null
        await inspect()
      }
    } catch (failure) {
      editorError = failure instanceof Error ? failure.message : 'Could not apply reviewed edit.'
    } finally {
      editorSaving = false
    }
  }

  onMount(() => {
    void inspect()
  })
</script>

<section class="panel" aria-labelledby="config-title">
  <div class="controls">
    <h2 id="config-title">Authoritative workload configuration</h2>
    <button type="button" onclick={inspect} disabled={loading}>Refresh</button>
  </div>
  <p>The authoritative workload configuration tracks managed applications, storage, bindings and shares. Existing Maestro server settings remain separate pending the Phase 2 single-authority migration.</p>
  {#if error}<p role="alert">{error}</p>{/if}
  {#if loading}<p role="status">Reading configuration…</p>{/if}
  <dl>
    <dt>Current workload revision</dt><dd>{revision ?? 'No authored YAML revision yet'}</dd>
    <dt>Stored historical revisions</dt><dd>{history.length}</dd>
    <dt>Mutation policy</dt><dd>{writable ? 'Workload writes permitted by configuration (provider safety checks still apply)' : 'Workload mutations disabled by administrator policy'}</dd>
  </dl>
  <p><a href="#applications">Open Applications to edit and review workload changes</a>. The edit workflow requires a reviewed plan and a current revision fingerprint; the presence of a form does not authorize native mutations.</p>
</section>

<section class="panel" aria-labelledby="typed-title">
  <h2 id="typed-title">Typed resource editor</h2>
  <p>These forms edit the same authoritative YAML as Applications, not a separate configuration store. Every mutation requires an exact current revision, reviewed plan, and provider-safe native publication. Resources needing unavailable provider operations cannot be applied.</p>
  {#if !writable}
    <p role="status">Typed resource editing is disabled by the workload write policy.</p>
  {/if}
  <form onsubmit={planTyped}>
    <div class="editor-grid">
      <label>Resource type
        <select bind:value={editorKind} disabled={editorSaving}>
          <option value="storage">Storage</option>
          <option value="share">File share</option>
        </select>
      </label>
      <label>Resource ID
        <input bind:value={editId} maxlength="63" pattern="[a-z][a-z0-9_-]*"
          placeholder="media" required disabled={editorSaving} />
      </label>
      <label>Operation
        <select bind:value={editOperation} disabled={editorSaving}>
          <option value="save">Save definition</option>
          <option value="remove">Remove definition (data preserved)</option>
        </select>
      </label>
      {#if editOperation === 'save' && editorKind === 'storage'}
        <label>Storage type
          <select bind:value={storageKind} disabled={editorSaving}>
            <option value="directory">Directory</option>
            <option value="file">Disk file</option>
          </select>
        </label>
        <label>Storage path (relative to managed storage root)
          <input bind:value={storagePath} placeholder="media" required disabled={editorSaving} />
        </label>
        {#if storageKind === 'file'}
          <label>Disk format
            <select bind:value={storageFormat} disabled={editorSaving}>
              <option value="raw">raw</option>
              <option value="qcow2">qcow2</option>
            </select>
          </label>
        {/if}
      {:else if editOperation === 'save' && editorKind === 'share'}
        <label>Share protocol
          <select bind:value={shareProtocol} disabled={editorSaving}>
            <option value="samba">Samba</option>
            <option value="nfs">NFS</option>
          </select>
        </label>
        <label>Existing managed storage ID
          <input bind:value={shareStorage} placeholder="media" required disabled={editorSaving} />
        </label>
        <label><input type="checkbox" bind:checked={shareEnabled} disabled={editorSaving} /> Published</label>
        <label><input type="checkbox" bind:checked={shareReadOnly} disabled={editorSaving} /> Read only</label>
      {/if}
    </div>
    <button type="submit" disabled={!writable || editorPlanning || editorSaving || !revision}>
      {editorPlanning ? 'Reviewing…' : 'Review typed change'}
    </button>
  </form>
  {#if editorError}<p role="alert">{editorError}</p>{/if}
  {#if editorJob}<p role="status">{editorJob}</p>{/if}
  {#if reviewed}
    <h3>Reviewed plan</h3>
    <p>Fingerprint: <code>{reviewed.plan.fingerprint}</code></p>
    <ul>
      {#each reviewed.plan.changes as change}
        <li>{change.resource} — {change.operation} ({change.backend})</li>
      {/each}
    </ul>
    {#if reviewed.blockedResources.length}
      <p role="status">Native changes blocked for: {reviewed.blockedResources.join(', ')}. Resolve ownership and provider readiness first.</p>
    {:else}
      <button type="button" disabled={editorSaving || !writable} onclick={saveTyped}>
        {editorSaving ? 'Applying…' : 'Apply reviewed edit'}
      </button>
    {/if}
  {/if}
</section>

<section class="panel" aria-labelledby="backup-title">
  <div class="controls">
    <h2 id="backup-title">Backup input readiness</h2>
    <button type="button" onclick={inspectBackup}>Inventory inputs</button>
  </div>
  <p>This is a metadata-only inventory. It does not copy files, export guests, produce a verified backup, or make any capture restorable.</p>
  {#if inventoryError}<p role="alert">{inventoryError}</p>{/if}
  {#if inventory}
    <p>Inventory revision: <code>{inventory.revision.slice(0, 16)}</code> · checked {inventory.checkedAt}</p>
    <p>Inputs: {inventory.inputs.length} · requirements: {inventory.requirements.length} · unresolved issues: {inventory.issues.length}</p>
    {#each inventory.issues as issue}
      <p role="status"><strong>{issue.resource}</strong>: {issue.message} {issue.remedy}</p>
    {/each}
    <div class="table-scroll">
      <table>
        <thead><tr><th scope="col">Input</th><th scope="col">Strategy</th><th scope="col">Inspection</th><th scope="col">Size</th></tr></thead>
        <tbody>
          {#each inventory.inputs as input (input.id)}
            <tr>
              <th scope="row">{input.id}<small>{input.kind}</small></th>
              <td>{input.strategy}</td>
              <td>{input.inspection}</td>
              <td>{input.logicalBytes === undefined ? '—' : input.logicalBytes + ' B'}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
    <details>
      <summary>Capture prerequisites and exclusions</summary>
      {#each inventory.requirements as requirement}
        <p><strong>{requirement.resource}</strong> ({requirement.kind}): {requirement.description}</p>
      {/each}
      {#each inventory.exclusions as exclusion}
        <p><strong>Excluded — {exclusion.kind}:</strong> {exclusion.reason}</p>
      {/each}
    </details>
  {/if}
</section>

<style>
  .controls { display: flex; flex-wrap: wrap; align-items: center; gap: .75rem; }
  .editor-grid { display: grid; gap: .75rem; grid-template-columns: repeat(auto-fit, minmax(15rem, 1fr)); margin: 1rem 0; }
  .editor-grid label { display: flex; flex-direction: column; gap: .35rem; }
  .editor-grid input:not([type='checkbox']), .editor-grid select { font: inherit; padding: .55rem; background: var(--panel); border: 1px solid var(--border); color: var(--text); border-radius: 6px; }
  .editor-grid label:has([type='checkbox']) { flex-direction: row; align-items: center; }
  .controls h2 { margin-right: auto; }
  dt { font-weight: 600; }
  dd { margin: .25rem 0 .8rem; overflow-wrap: anywhere; }
  small { display: block; font-weight: normal; color: var(--muted); }
  .table-scroll { overflow-x: auto; }
  table { width: 100%; border-collapse: collapse; }
  th, td { padding: .6rem; text-align: left; border-bottom: 1px solid var(--border); }
  code { overflow-wrap: anywhere; }
</style>
