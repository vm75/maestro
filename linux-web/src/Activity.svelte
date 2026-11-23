<script lang="ts">
  import { onMount } from 'svelte'
  import { invokeTool, type Job, type Discovery, type Status } from './lib/api'

  let { status: _status, discovery: _discovery }: { status: Status; discovery: Discovery } = $props()
  let jobs = $state<Job[]>([])
  let chosen = $state<Job | null>(null)
  let loading = $state(false)
  let error = $state('')
  let actionError = $state('')

  async function refresh() {
    if (loading) return
    loading = true
    error = ''
    try {
      jobs = await invokeTool<Job[]>('jobs_list', {})
      if (chosen) {
        chosen = jobs.find(job => job.id === chosen?.id) ?? chosen
      }
    } catch (failure) {
      error = failure instanceof Error ? failure.message : 'Could not read activity.'
    } finally {
      loading = false
    }
  }

  async function inspect(id: string) {
    actionError = ''
    try {
      chosen = await invokeTool<Job>('jobs_get', { id })
    } catch (failure) {
      actionError = failure instanceof Error ? failure.message : 'Could not read job.'
    }
  }

  async function cancel(id: string) {
    actionError = ''
    if (!window.confirm('Request cancellation? Already committed host changes will not be undone.')) return
    try {
      await invokeTool('jobs_cancel', { id })
      await refresh()
      await inspect(id)
    } catch (failure) {
      actionError = failure instanceof Error ? failure.message : 'Cancellation is unavailable.'
    }
  }

  onMount(() => { void refresh() })
</script>

<section class="panel" aria-labelledby="activity-heading">
  <div class="controls">
    <h2 id="activity-heading">Management jobs</h2>
    <button type="button" disabled={loading} onclick={refresh}>Refresh activity</button>
  </div>
  <p>Recent serialized management operations are shown below. A cancelled request does not undo native changes that were already committed.</p>
  {#if error}<p role="alert">{error}</p>{/if}
  {#if loading}<p role="status">Reading activity…</p>{/if}
  {#if jobs.length}
    <div class="table-scroll">
      <table>
        <thead><tr><th scope="col">Operation</th><th scope="col">State</th><th scope="col">Started</th><th scope="col">Details</th></tr></thead>
        <tbody>
          {#each jobs as job (job.id)}
            <tr>
              <th scope="row">{job.tool}</th>
              <td>{job.state}</td>
              <td>{job.createdAt}</td>
              <td><button type="button" onclick={() => inspect(job.id)}>Inspect</button></td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {:else if !loading}
    <p>No recent management jobs.</p>
  {/if}
</section>

{#if chosen}
  <section class="panel" aria-labelledby="job-details">
    <div class="controls">
      <h2 id="job-details">Job details</h2>
      <button type="button" onclick={() => chosen = null}>Close</button>
      <button type="button" onclick={() => inspect(chosen!.id)}>Refresh job</button>
      {#if chosen.state === 'running'}
        <button type="button" onclick={() => cancel(chosen!.id)}>Request cancellation</button>
      {/if}
    </div>
    <dl>
      <dt>ID</dt><dd><code>{chosen.id}</code></dd>
      <dt>Operation</dt><dd>{chosen.tool}</dd>
      <dt>State</dt><dd>{chosen.state}</dd>
      <dt>Started</dt><dd>{chosen.createdAt}</dd>
      {#if chosen.finishedAt}<dt>Finished</dt><dd>{chosen.finishedAt}</dd>{/if}
    </dl>
    {#if actionError}<p role="alert">{actionError}</p>{/if}
    {#if chosen.error}<p role="alert">{chosen.error}</p>{/if}
    {#if chosen.truncated}<p role="status">Earlier job output was truncated.</p>{/if}
    {#if chosen.output}<pre>{chosen.output}</pre>{/if}
    {#if chosen.result}<pre>{JSON.stringify(chosen.result, null, 2)}</pre>{/if}
  </section>
{/if}

<style>
  .controls { display: flex; gap: .75rem; align-items: center; flex-wrap: wrap; }
  .controls h2 { margin-right: auto; }
  .table-scroll { overflow-x: auto; }
  table { border-collapse: collapse; width: 100%; }
  th, td { padding: .65rem; text-align: left; border-bottom: 1px solid var(--border); }
  pre { overflow: auto; max-height: 28rem; white-space: pre-wrap; overflow-wrap: anywhere; }
  dd { margin: .2rem 0 .75rem; overflow-wrap: anywhere; }
</style>
