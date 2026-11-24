<script lang="ts">
  import { onMount } from 'svelte'
  import { invokeTool } from './api'
  interface Workload { provider: string; scope: string; id: string; name: string; kind: string; state: string; image: string; addresses: string[] }
  interface Inventory { workloads: Workload[]; warnings: string[] }
  let { query = '' }: { query?: string } = $props()
  let inventory = $state<Inventory | null>(null)
  let loading = $state(false)
  let error = $state('')
  let controller: AbortController | undefined
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
  onMount(() => { void refresh(); return () => controller?.abort() })
</script>
<section class="panel" aria-labelledby="native-workloads-heading">
  <div class="native-heading">
    <h2 id="native-workloads-heading">External workloads</h2>
    <button type="button" disabled={loading} onclick={refresh}>{loading ? 'Refreshing…' : 'Refresh'}</button>
  </div>
  <p>Existing containers and VMs visible to this host account. Known and reserved Maestro-managed identities are excluded. External workloads are read-only; native permissions and configured connections limit discovery.</p>
  {#if error}<p role="alert">{error}</p>{/if}
  {#if inventory?.warnings.length}<ul aria-label="Inventory warnings">{#each inventory.warnings as warning}<li>{warning}</li>{/each}</ul>{/if}
  {#if loading}<p role="status">Reading native inventory…</p>
  {:else if inventory && filtered.length === 0}<p>No matching external workloads in the available inventories.</p>
  {:else if filtered.length}
    <ul class="native-list">{#each filtered as workload (`${workload.provider}/${workload.scope}/${workload.id}`)}
      <li><strong>{workload.name}</strong> <span>{workload.provider} · {workload.scope} · {workload.kind} · {workload.state}</span>
        {#if workload.image}<small>{workload.image}</small>{/if}
        {#if workload.addresses.length}<small>{workload.addresses.join(', ')}</small>{/if}
      </li>
    {/each}</ul>
  {/if}
</section>
<style>
  .native-heading { display: flex; align-items: center; justify-content: space-between; gap: .75rem; }
  .native-list { list-style: none; padding: 0; }
  .native-list li { padding: .75rem 0; border-top: 1px solid var(--border); display: flex; flex-direction: column; gap: .25rem; overflow-wrap: anywhere; }
</style>
