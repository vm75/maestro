<script lang="ts">
  import { onMount } from 'svelte'
  import { invokeTool, startJob, type Job } from './api'
  interface Workload { provider: string; scope: string; id: string; name: string; kind: string; state: string; image: string; addresses: string[]; actions: string[]; logs: boolean }
  interface Inventory { workloads: Workload[]; warnings: string[] }
  let { query = '', allowWrites = false }: { query?: string; allowWrites?: boolean } = $props()
  let inventory = $state<Inventory | null>(null)
  let loading = $state(false)
  let error = $state('')
  let controller: AbortController | undefined
  let logController: AbortController | undefined
  let activeJob = $state<string | null>(null)
  let submitting = $state(false)
  let feedback = $state('')
  let logs = $state<{name: string; text: string; truncated: boolean} | null>(null)
  let logsLoading = $state(false)
  const filtered = $derived(inventory?.workloads.filter(w => `${w.name} ${w.provider} ${w.scope} ${w.kind}`.toLowerCase().includes(query.toLowerCase())) ?? [])
  async function refresh() {
    controller?.abort()
    const current = new AbortController()
    controller = current
    loading = true; error = ''
    try {
      const result = await invokeTool<Inventory>('native_workloads_list', {}, current.signal)
      if (!current.signal.aborted) inventory = result
    } catch (failure) {
      if (!current.signal.aborted) { inventory = null; error = failure instanceof Error ? failure.message : 'Native inventory unavailable' }
    } finally { if (!current.signal.aborted) loading = false }
  }
  async function act(workload: Workload, action: string) {
    if (!allowWrites || activeJob || submitting || !workload.actions.includes(action)) return
    if (!window.confirm(`${action} external workload ${workload.name} (${workload.provider}, ${workload.scope})? This changes its runtime state but not its configuration.`)) return
    submitting = true; error = ''; feedback = ''
    try {
      const job = await startJob('native_workload_action', { provider: workload.provider, scope: workload.scope, id: workload.id, action, confirm: true })
      activeJob = job.id; feedback = 'External operation started. See Activity for progress.'
    } catch (failure) { error = failure instanceof Error ? failure.message : 'Could not start operation' }
    finally { submitting = false }
  }
  async function readLogs(workload: Workload) {
    logController?.abort(); const current = new AbortController(); logController = current
    logsLoading = true; logs = null; error = ''
    try {
      const result = await invokeTool<{text: string; truncated: boolean}>('native_workload_logs', {provider: workload.provider, scope: workload.scope, id: workload.id, tail: 200}, current.signal)
      if (!current.signal.aborted) logs = {name: workload.name, ...result}
    } catch (failure) { if (!current.signal.aborted) error = failure instanceof Error ? failure.message : 'Could not read logs' }
    finally { if (!current.signal.aborted) logsLoading = false }
  }
  onMount(() => {
    const finished = (event: Event) => {
      const job = (event as CustomEvent<Job>).detail
      if (job.id !== activeJob) return
      activeJob = null
      feedback = job.state === 'succeeded' ? 'Operation submitted; refreshed native state below.' : job.error || `Operation ${job.state}.`
      void refresh()
    }
    window.addEventListener('maestro:job-finished', finished)
    void refresh()
    return () => { controller?.abort(); logController?.abort(); window.removeEventListener('maestro:job-finished', finished) }
  })
</script>
<section class="panel" aria-labelledby="native-workloads-heading">
  <div class="native-heading">
    <h2 id="native-workloads-heading">External workloads</h2>
    <button type="button" disabled={loading} onclick={refresh}>{loading ? 'Refreshing…' : 'Refresh'}</button>
  </div>
  <p>Existing containers and VMs visible to this host account. Known and reserved Maestro-managed identities are excluded. Lifecycle controls are available only for eligible native identities; other workloads are read-only. Native permissions and configured connections limit discovery.</p>
  {#if error}<p role="alert">{error}</p>{/if}
  {#if feedback}<p role="status">{feedback}</p>{/if}
  {#if inventory?.warnings.length}<ul aria-label="Inventory warnings">{#each inventory.warnings as warning}<li>{warning}</li>{/each}</ul>{/if}
  {#if loading}<p role="status">Reading native inventory…</p>
  {:else if inventory && filtered.length === 0}<p>No matching external workloads in the available inventories.</p>
  {:else if filtered.length}
    <ul class="native-list">{#each filtered as workload (`${workload.provider}/${workload.scope}/${workload.id}`)}
      <li><strong>{workload.name}</strong> <span>{workload.provider} · {workload.scope} · {workload.kind} · {workload.state}</span>
        {#if workload.image}<small>{workload.image}</small>{/if}
        {#if workload.addresses.length}<small>{workload.addresses.join(', ')}</small>{/if}
        <div class="native-actions">{#if allowWrites}{#each workload.actions as action}<button type="button" disabled={submitting || activeJob !== null || loading} onclick={() => act(workload,action)}>{action}</button>{/each}{/if}{#if workload.logs}<button type="button" disabled={logsLoading} onclick={() => readLogs(workload)}>Logs</button>{/if}</div>
      </li>
    {/each}</ul>
  {/if}
  {#if logs}<div><h3>Logs: {logs.name}</h3><button type="button" onclick={() => {logs = null}}>Close</button>{#if logs.truncated}<p>Earlier output truncated.</p>{/if}<pre>{logs.text || '(No recent stdout output)'}</pre></div>{/if}
</section>
<style>
  .native-heading { display: flex; align-items: center; justify-content: space-between; gap: .75rem; }
  .native-actions { display: flex; gap: .5rem; flex-wrap: wrap; }
  pre { max-height: 24rem; overflow: auto; white-space: pre-wrap; }
  .native-list { list-style: none; padding: 0; }
  .native-list li { padding: .75rem 0; border-top: 1px solid var(--border); display: flex; flex-direction: column; gap: .25rem; overflow-wrap: anywhere; }
</style>
