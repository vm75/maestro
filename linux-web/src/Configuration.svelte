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
  .controls h2 { margin-right: auto; }
  dt { font-weight: 600; }
  dd { margin: .25rem 0 .8rem; overflow-wrap: anywhere; }
  small { display: block; font-weight: normal; color: var(--muted); }
  .table-scroll { overflow-x: auto; }
  table { width: 100%; border-collapse: collapse; }
  th, td { padding: .6rem; text-align: left; border-bottom: 1px solid var(--border); }
  code { overflow-wrap: anywhere; }
</style>
