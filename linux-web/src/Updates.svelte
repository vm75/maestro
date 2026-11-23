<script lang="ts">
  import { onMount } from 'svelte'
  import { invokeTool, startJob, type Discovery, type Job, type Status } from './lib/api'

  let { status: _status, discovery: _discovery }: { status: Status; discovery: Discovery } = $props()

  interface PackageStatus {
    manager: { title: string; available: boolean; canRefresh: boolean; canInstall: boolean }
    distribution: string
    packages: Array<{ name: string; architecture?: string; installed?: string; candidate?: string; kind: string }>
    valid: boolean
    notice: string
    reboot: { required: boolean | null; reason: string }
    readAt: string
  }

  let result = $state<PackageStatus | null>(null)
  let pending: AbortController | undefined
  let loading = $state(true)
  let error = $state('')

  let mutation = $state(false)
  let job = $state<Job | null>(null)
  let jobRequest: AbortController | undefined

  async function runUpdate(install: boolean) {
    if (mutation || (install && !window.confirm('Install system updates now? Packages may restart services.'))) return
    const controller = new AbortController()
    jobRequest = controller
    mutation = true
    error = ''
    job = null
    try {
      let current = await startJob(install ? 'updates_install' : 'updates_refresh', install ? { confirm: true } : {})
      if (controller.signal.aborted) return
      job = current
      while (current.state === 'running') {
        await new Promise(resolve => setTimeout(resolve, 1000))
        if (controller.signal.aborted) return
        current = await invokeTool<Job>('jobs_get', { id: current.id }, controller.signal)
        if (controller.signal.aborted) return
        job = current
      }
      if (current.state === 'succeeded') void readCached()
    } catch (failure) {
      if (!controller.signal.aborted) error = failure instanceof Error ? failure.message : 'Could not track the update.'
    } finally {
      if (!controller.signal.aborted) mutation = false
    }
  }

  async function readCached() {
    pending?.abort()
    const request = new AbortController()
    pending = request
    loading = true
    error = ''
    try {
      const status = await invokeTool<PackageStatus>('updates_status', {}, request.signal)
      if (!request.signal.aborted) result = status
    } catch (failure) {
      if (!request.signal.aborted) {
        error = failure instanceof Error ? failure.message : 'Could not read cached updates.'
      }
    } finally {
      if (!request.signal.aborted) loading = false
    }
  }

  onMount(() => {
    void readCached()
    return () => {
      pending?.abort()
      jobRequest?.abort()
    }
  })
</script>

<section class="panel" aria-labelledby="cached-updates">
  <div class="heading">
    <h2 id="cached-updates">Cached package updates</h2>
    <div class="controls">
      <button type="button" onclick={readCached} disabled={loading || mutation}>Check cache</button>
      {#if result?.manager.canRefresh}
        <button type="button" onclick={() => runUpdate(false)} disabled={loading || mutation}>Refresh metadata</button>
      {/if}
      {#if result?.manager.canInstall}
        <button type="button" onclick={() => runUpdate(true)} disabled={loading || mutation}>Install updates</button>
      {/if}
    </div>
  </div>
  {#if loading}<p role="status">Reading cached updates…</p>{/if}
  {#if error}<p role="alert">{error}</p>{/if}
  {#if job}
    <p role="status">Update job: {job.state}</p>
    {#if job.error}<p role="alert">{job.error}</p>{/if}
    {#if job.output}<pre>{job.output}</pre>{/if}
  {/if}
  {#if result}
    {#if result.distribution}<p>{result.distribution} · {result.manager.title}</p>{/if}
    <p>{result.notice}</p>
    <p>{result.reboot.reason}</p>
    {#if result.valid && result.packages.length}
      <div class="table-scroll">
        <table>
          <thead><tr><th scope="col">Package</th><th scope="col">Installed</th><th scope="col">Candidate</th><th scope="col">Status</th></tr></thead>
          <tbody>
            {#each result.packages as row}
              <tr>
                <th scope="row">{row.name}{#if row.architecture}<small>{row.architecture}</small>{/if}</th>
                <td>{row.installed ?? '—'}</td><td>{row.candidate ?? '—'}</td><td>{row.kind}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {:else if result.valid}
      <p>No pending updates in the cache.</p>
    {/if}
  {/if}
</section>

<style>
  .heading { display: flex; flex-wrap: wrap; align-items: baseline; justify-content: space-between; gap: .5rem; margin-bottom: 1rem; }
  .controls { display: flex; flex-wrap: wrap; gap: .5rem; }
  pre { max-height: 20rem; overflow: auto; white-space: pre-wrap; }
  .heading h2 { margin: 0; }
  .table-scroll { overflow-x: auto; }
  table { width: 100%; border-collapse: collapse; }
  th, td { padding: .65rem; text-align: left; border-bottom: 1px solid var(--border); }
  small { display: block; font-weight: normal; color: var(--muted); }
</style>
