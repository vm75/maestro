<script lang="ts">
  import { onMount } from 'svelte'
  import { invokeTool, startJob, type Discovery, type Job, type Status } from './lib/api'

  let { status: _status, discovery: _discovery }: { status: Status; discovery: Discovery } = $props()

  type Scope = 'system' | 'user'
  interface Inventory {
    available: boolean
    journalAvailable: boolean
    reason?: string
    permissionReason?: string
    warnings: string[]
    services: Array<{
      name: string
      description: string
      load: string
      active: string
      sub: string
      enabled: string
      writable: boolean
    }>
  }

  let scope = $state<Scope>('system')
  let inventory = $state<Inventory | null>(null)
  let loading = $state(true)
  let error = $state('')
  let pending: AbortController | undefined
  let detailRequest: AbortController | undefined
  let detailUnit = $state('')
  let detail = $state<{
    name: string
    description: string
    load: string
    active: string
    sub: string
    enabled: string
    mainPID?: number
    result?: string
    execMainCode?: string
    execMainStatus?: string
    activeSince?: string
    inactiveSince?: string
    fragmentPath?: string
    canStart: boolean
    canStop: boolean
    canReload: boolean
  } | null>(null)
  let detailError = $state('')
  let journalRequest: AbortController | undefined
  let journalUnit = $state('')
  let journal = $state<{ text: string; truncated: boolean } | null>(null)
  let journalError = $state('')

  const actions = ['start', 'stop', 'restart', 'reload', 'enable', 'disable']
  let action = $state('start')
  let actionUnit = $state('')
  let actionError = $state('')
  let actionPending = $state(false)
  let job = $state<Job | null>(null)
  let jobRequest: AbortController | undefined
  const writeUnits = $derived(inventory?.services.filter(unit => unit.writable) ?? [])

  async function runAction(event: SubmitEvent) {
    event.preventDefault()
    if (!actionUnit || actionPending || !window.confirm(`Run ${action} for ${actionUnit} in the ${scope} manager?`)) return
    jobRequest?.abort()
    const controller = new AbortController()
    jobRequest = controller
    actionPending = true
    actionError = ''
    job = null
    try {
      let current = await startJob(`services_${action}`, {
        scope,
        unit: actionUnit,
        ...(!['start', 'enable'].includes(action) ? { confirm: true } : {}),
      })
      if (controller.signal.aborted) return
      job = current
      while (current.state === 'running') {
        await new Promise(resolve => setTimeout(resolve, 1000))
        if (controller.signal.aborted) return
        current = await invokeTool<Job>('jobs_get', { id: current.id }, controller.signal)
        if (controller.signal.aborted) return
        job = current
      }
      void refresh()
    } catch (failure) {
      if (!controller.signal.aborted) {
        actionError = failure instanceof Error ? failure.message : 'Could not track the operation.'
      }
    } finally {
      if (!controller.signal.aborted) actionPending = false
    }
  }

  function closeDetails() {
    detailRequest?.abort()
    detailUnit = ''
    detail = null
    detailError = ''
  }

  async function openDetails(unit: string) {
    closeDetails()
    detailUnit = unit
    const controller = new AbortController()
    detailRequest = controller
    try {
      const result = await invokeTool<NonNullable<typeof detail>>(
        'services_details', { scope, unit }, controller.signal,
      )
      if (!controller.signal.aborted) detail = result
    } catch (failure) {
      if (!controller.signal.aborted) {
        detailError = failure instanceof Error ? failure.message : 'Could not read service details.'
      }
    }
  }

  function closeJournal() {
    journalRequest?.abort()
    journalUnit = ''
    journal = null
    journalError = ''
  }

  async function openJournal(unit: string) {
    closeJournal()
    journalUnit = unit
    const controller = new AbortController()
    journalRequest = controller
    try {
      const result = await invokeTool<{ text: string; truncated: boolean }>(
        'services_logs', { scope, unit }, controller.signal,
      )
      if (!controller.signal.aborted) journal = result
    } catch (failure) {
      if (!controller.signal.aborted) {
        journalError = failure instanceof Error ? failure.message : 'Could not load the journal.'
      }
    }
  }

  async function refresh() {
    pending?.abort()
    const controller = new AbortController()
    pending = controller
    loading = true
    inventory = null
    error = ''
    try {
      const result = await invokeTool<Inventory>('services_list', { scope }, controller.signal)
      if (!controller.signal.aborted) inventory = result
    } catch (failure) {
      if (!controller.signal.aborted) {
        error = failure instanceof Error ? failure.message : 'Could not load unit inventory.'
      }
    } finally {
      if (!controller.signal.aborted) loading = false
    }
  }

  onMount(() => {
    void refresh()
    return () => {
      pending?.abort()
      detailRequest?.abort()
      journalRequest?.abort()
      jobRequest?.abort()
    }
  })
</script>

<section class="panel" aria-label="Systemd unit inventory">
  <div class="filters">
    <label for="manager-scope">Manager</label>
    <select id="manager-scope" value={scope} disabled={actionPending} onchange={(event) => {
      closeDetails()
      closeJournal()
      actionUnit = ''
      scope = event.currentTarget.value === 'user' ? 'user' : 'system'
      void refresh()
    }}>
      <option value="system">System</option>
      <option value="user">User</option>
    </select>
    <button type="button" onclick={refresh} disabled={loading}>Refresh</button>
  </div>
  {#if loading}<p role="status">Loading services…</p>{/if}
  {#if error}<p role="alert">{error}</p>{/if}
  {#if inventory}
    {#if !inventory.available}
      <p>{inventory.reason ?? 'Systemd is unavailable.'}</p>
    {:else}
      {#if inventory.permissionReason}
        <p role="status">{inventory.permissionReason} You can continue reading system services and managing authorized user services without the optional helper.</p>
      {/if}
      {#each inventory.warnings as warning}
        <p role="status">{warning}</p>
      {/each}
      {#if inventory.services.length}
        <div class="table-scroll">
          <table>
            <thead><tr><th scope="col">Unit</th><th scope="col">Load</th><th scope="col">State</th><th scope="col">Enabled</th><th scope="col">Details</th>{#if inventory.journalAvailable}<th scope="col">Journal</th>{/if}</tr></thead>
            <tbody>
              {#each inventory.services as unit (unit.name)}
                <tr>
                  <th scope="row">{unit.name}{#if unit.description}<small>{unit.description}</small>{/if}</th>
                  <td>{unit.load}</td><td>{unit.active} / {unit.sub}</td><td>{unit.enabled}</td>
                  <td><button type="button" aria-label={'Inspect ' + unit.name} onclick={() => openDetails(unit.name)}>Inspect</button></td>
                  {#if inventory.journalAvailable}
                    <td><button type="button" aria-label={'Read journal for ' + unit.name} onclick={() => openJournal(unit.name)}>View</button></td>
                  {/if}
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {:else if !inventory.warnings.length}
        <p>No service units found.</p>
      {/if}
    {/if}
  {/if}
</section>

{#if writeUnits.length || job || actionError}
  <section class="panel" aria-labelledby="service-actions">
    <h2 id="service-actions">Service actions</h2>
    {#if writeUnits.length}
      <form class="filters" onsubmit={runAction}>
        <label for="action-unit">Unit</label>
        <select id="action-unit" bind:value={actionUnit} required disabled={actionPending}>
          <option value="">Select a service</option>
          {#each writeUnits as unit (unit.name)}<option value={unit.name}>{unit.name}</option>{/each}
        </select>
        <label for="unit-action">Action</label>
        <select id="unit-action" bind:value={action} disabled={actionPending}>
          {#each actions as choice}
            <option value={choice}>{choice === 'enable' || choice === 'disable' ? choice + ' at boot' : choice}</option>
          {/each}
        </select>
        <button type="submit" disabled={actionPending || !actionUnit}>Run</button>
      </form>
    {/if}
    {#if actionError}<p role="alert">{actionError}</p>{/if}
    {#if job}
      <p role="status">{job.state}</p>
      {#if job.error}<p role="alert">{job.error}</p>{/if}
      {#if job.output}<pre>{job.output}</pre>{/if}
    {/if}
  </section>
{/if}

{#if detailUnit}
  <section class="panel" aria-labelledby="service-detail-title">
    <div class="filters">
      <h2 id="service-detail-title">Service details: {detailUnit}</h2>
      <button type="button" onclick={closeDetails}>Close</button>
    </div>
    {#if detailError}
      <p role="alert">{detailError}</p>
    {:else if detail}
      <p>{detail.description || detail.name}</p>
      <dl>
        <dt>Canonical unit</dt><dd>{detail.name}</dd>
        <dt>Load state</dt><dd>{detail.load}</dd>
        <dt>Runtime</dt><dd>{detail.active} / {detail.sub}</dd>
        <dt>Enabled at boot</dt><dd>{detail.enabled || 'Unknown'}</dd>
        {#if detail.mainPID !== undefined}<dt>Main PID</dt><dd>{detail.mainPID}</dd>{/if}
        {#if detail.result}<dt>Result</dt><dd>{detail.result}{detail.execMainStatus ? ' · exit ' + detail.execMainStatus : ''}</dd>{/if}
        {#if detail.activeSince}<dt>Active since</dt><dd>{detail.activeSince}</dd>{/if}
        {#if detail.inactiveSince}<dt>Inactive since</dt><dd>{detail.inactiveSince}</dd>{/if}
        {#if detail.fragmentPath}<dt>Unit file</dt><dd>{detail.fragmentPath}</dd>{/if}
        <dt>Available runtime actions</dt><dd>{detail.canStart ? 'start ' : ''}{detail.canStop ? 'stop ' : ''}{detail.canReload ? 'reload' : ''}</dd>
      </dl>
    {:else}
      <p role="status">Loading service details…</p>
    {/if}
  </section>
{/if}

{#if journalUnit}
  <section class="panel" aria-labelledby="journal-title">
    <div class="filters">
      <h2 id="journal-title">Journal for {journalUnit}</h2>
      <button type="button" onclick={closeJournal}>Close</button>
    </div>
    {#if journalError}
      <p role="alert">{journalError}</p>
    {:else if journal}
      {#if journal.truncated}<p>Showing the most recent output. Earlier entries were truncated.</p>{/if}
      <pre>{journal.text || 'No recent entries.'}</pre>
    {:else}
      <p role="status">Loading journal…</p>
    {/if}
  </section>
{/if}

<style>
  .filters { display: flex; flex-wrap: wrap; gap: .75rem; align-items: center; margin-bottom: 1rem; }
  select { font: inherit; padding: .6rem; border: 1px solid var(--border); border-radius: 6px; background: var(--panel); color: var(--text); }
  .table-scroll { overflow-x: auto; }
  table { width: 100%; border-collapse: collapse; }
  th, td { padding: .65rem; text-align: left; border-bottom: 1px solid var(--border); }
  pre { max-height: 28rem; overflow: auto; white-space: pre-wrap; overflow-wrap: anywhere; }
  small { display: block; font-weight: normal; color: var(--muted); }
</style>
