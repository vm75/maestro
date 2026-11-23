<script lang="ts">
  import { onMount } from 'svelte'
  import { invokeTool } from './lib/api'

  interface ProcessList {
    at: string | null
    matched: number
    limited: boolean
    processes: Array<{ pid: number; name: string; state: string; cpu: number | null; memory: number }>
  }

  let filter = $state('')
  let sort = $state('cpu')
  let query = $state({ filter: '', sort: 'cpu' })
  let snapshot = $state<ProcessList | null>(null)
  let error = $state('')
  let pending: AbortController | undefined
  let timer: ReturnType<typeof setTimeout>

  async function refresh() {
    pending?.abort()
    clearTimeout(timer)
    const request = new AbortController()
    pending = request
    try {
      const result = await invokeTool<ProcessList>('monitor_processes', { ...query, limit: 25 }, request.signal)
      if (!request.signal.aborted) {
        snapshot = result
        error = ''
      }
    } catch (failure) {
      if (!request.signal.aborted) {
        error = failure instanceof Error ? failure.message : 'Could not load process activity.'
      }
    } finally {
      if (!request.signal.aborted) timer = setTimeout(refresh, 3000)
    }
  }

  function search(event: SubmitEvent) {
    event.preventDefault()
    query = { filter, sort }
    void refresh()
  }

  onMount(() => {
    void refresh()
    return () => {
      pending?.abort()
      clearTimeout(timer)
    }
  })
</script>

<section class="panel" aria-labelledby="processes-title">
  <h2 id="processes-title">Process activity</h2>
  <form onsubmit={search}>
    <label for="process-filter">Name or PID</label><input id="process-filter" bind:value={filter} maxlength="128">
    <label for="process-sort">Sort by</label>
    <select id="process-sort" bind:value={sort}>
      <option value="cpu">CPU</option><option value="memory">Memory</option><option value="pid">PID</option><option value="name">Name</option>
    </select>
    <button type="submit">Apply</button>
  </form>
  {#if error}<p role="status">{error}</p>{/if}
  {#if snapshot?.at}
    <p>Showing {snapshot.processes.length} of {snapshot.matched}{snapshot.limited ? ' sampled matches' : ' matches'}</p>
    <div class="table-scroll">
      <table>
        <thead><tr><th scope="col">PID</th><th scope="col">Name</th><th scope="col">State</th><th scope="col">CPU</th><th scope="col">Memory</th></tr></thead>
        <tbody>
          {#each snapshot.processes as process (process.pid)}
            <tr><td>{process.pid}</td><th scope="row">{process.name}</th><td>{process.state}</td>
              <td>{process.cpu === null ? '—' : process.cpu.toFixed(1) + '%'}</td>
              <td>{(process.memory / 1024 ** 2).toFixed(1)} MiB</td></tr>
          {/each}
        </tbody>
      </table>
    </div>
  {:else if !error}
    <p role="status">Waiting for a process sample…</p>
  {/if}
</section>

<style>
  form { display: flex; flex-wrap: wrap; align-items: center; gap: .6rem; margin-bottom: 1rem; }
  select { font: inherit; padding: .6rem; border: 1px solid var(--border); border-radius: 6px; background: var(--panel); color: var(--text); }
  .table-scroll { overflow-x: auto; }
  table { width: 100%; border-collapse: collapse; }
  th, td { padding: .55rem; text-align: left; border-bottom: 1px solid var(--border); }
</style>
