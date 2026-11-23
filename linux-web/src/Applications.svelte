<script lang="ts">
  import { onMount } from 'svelte'
  import { invokeTool, startJob, type Discovery, type Job, type Status } from './lib/api'

  let { status: _status, discovery }: { status: Status; discovery: Discovery } = $props()

  interface Application {
    id: string
    name: string
    description: string
    kind: string
    engine: string
    backend: string
    enabled: boolean
    state: string
    containerCount: number
    runtimeMatches?: boolean | null
    restartRequired?: boolean
    members?: Array<{name: string; state: string}>
    lifecycle: boolean
    logs: boolean
  }

  interface PlanChange {
    resource: string
    backend: string
    operation: string
    before?: string
    after?: string
  }

  interface Plan {
    baseRevision: string
    desiredRevision: string
    fingerprint: string
    changes: PlanChange[]
  }

  interface Review {
    plan: Plan
    blockedResources: string[]
  }

  let applications = $state<Application[]>([])
  let loadingApps = $state(true)
  let appFilter = $state('all')
  let appError = $state('')

  let activeLogsId = $state<string | null>(null)
  let logsContent = $state<string | null>(null)
  let logsLoading = $state(false)
  let logsError = $state('')

  let currentYaml = $state('')
  let currentRevision = $state<string | null>(null)
  let reviewedDraft = $state<{ candidate: string; result: Review } | null>(null)
  const review = $derived(reviewedDraft?.candidate === currentYaml ? reviewedDraft.result : null)
  let loadingWorkloads = $state(true)
  const plan = $derived(review?.plan ?? null)
  let planning = $state(false)
  let planError = $state('')

  let applying = $state(false)
  let applyJob = $state<Job | null>(null)
  let applyError = $state('')
  let applyMessage = $state('')

  let history = $state<string[]>([])
  let actionPending = $state<string | null>(null)
  let pending = $state<{ plan: Plan; completed: number; mode?: string } | null>(null)
  interface ApplyStatus { stage: string; updatedAt?: string; resources?: Record<string, {state: string; message?: string}> }
  let applyStatus = $state<ApplyStatus | null>(null)
  let recoveryChecked = $state(false)
  let recoveryError = $state('')
  const writable = $derived(discovery.modules.some(module => module.id === 'workloads' && module.writable))


  async function loadPending() {
    recoveryChecked = false
    try {
      const [operation, status] = await Promise.all([
        invokeTool<{ pending: typeof pending }>('workloads_pending'),
        invokeTool<ApplyStatus>('managed_apply_status'),
      ])
      pending = operation.pending
      applyStatus = status
      recoveryChecked = true
      recoveryError = ''
    } catch (failure) {
      recoveryError = failure instanceof Error ? failure.message : 'Could not check workload recovery.'
    }
  }

  const filteredApps = $derived(
    applications.filter(app => {
      if (appFilter === 'all') return true
      if (appFilter === 'compose') return app.backend === 'compose'
      if (appFilter === 'quadlet') return app.backend === 'quadlet'
      if (appFilter === 'incus') return app.backend === 'incus'
      if (appFilter === 'libvirt') return app.backend === 'libvirt'
      return true
    })
  )

  async function loadApplications() {
    loadingApps = true
    appError = ''
    try {
      const result = await invokeTool<{ applications: Application[] }>('applications_list', {})
      applications = result.applications
    } catch (e) {
      appError = e instanceof Error ? e.message : 'Could not load applications'
    } finally {
      loadingApps = false
    }
  }

  async function loadWorkloads() {
    loadingWorkloads = true
    try {
      const result = await invokeTool<{ yaml: string; revision: string | null }>('workloads_get', {})
      currentYaml = result.yaml
      currentRevision = result.revision
    } catch (e) {
      planError = e instanceof Error ? e.message : 'Could not load workload configuration'
    } finally {
      loadingWorkloads = false
    }
  }

  async function loadHistory() {
    try {
      const result = await invokeTool<{ history: string[] }>('workloads_history', {})
      history = result.history
    } catch {
      // non-blocking
    }
  }

  async function runAction(id: string, action: string) {
    if (!writable || !applications.some(app => app.id === id && app.lifecycle) || !recoveryChecked || pending || actionPending || applying) return
    if (action !== 'start' && !window.confirm(`${action === 'stop' ? 'Stop' : 'Restart'} ${id}?`)) return
    actionPending = `${id}:${action}`
    appError = ''
    try {
      let job = await startJob(`managed_service_${action}`, action === 'start' ? { service: id } : { service: id, confirm: true })
      while (job.state === 'running') {
        await new Promise(r => setTimeout(r, 600))
        job = await invokeTool<Job>('jobs_get', { id: job.id })
      }
      if (job.state === 'failed') {
        appError = `Action ${action} on ${id} failed`
      } else {
        await loadApplications()
      }
    } catch (e) {
      appError = e instanceof Error ? e.message : `Action ${action} failed`
    } finally {
      actionPending = null
    }
  }

  async function showLogs(id: string) {
    activeLogsId = id
    logsLoading = true
    logsError = ''
    logsContent = null
    try {
      const result = await invokeTool<{ text: string; truncated: boolean; source: string }>('managed_service_logs', { service: id, tail: 200 })
      logsContent = (result.truncated ? '[Earlier log output truncated]\n' : '') + result.text
    } catch (e) {
      logsError = e instanceof Error ? e.message : 'Could not fetch logs'
    } finally {
      logsLoading = false
    }
  }

  function closeLogs() {
    activeLogsId = null
    logsContent = null
    logsError = ''
  }

  async function reviewPlan() {
    if (planning || applying || loadingWorkloads) return
    planning = true
    planError = ''
    reviewedDraft = null
    const candidate = currentYaml
    try {
      const result = await invokeTool<Review>('workloads_plan', {
        candidate,
        expected: currentRevision ?? undefined,
      })
      reviewedDraft = { candidate, result }
    } catch (e) {
      planError = e instanceof Error ? e.message : 'Planning failed'
    } finally {
      planning = false
    }
  }

  async function applyCurrentPlan(resume = false) {
    if (!writable || !recoveryChecked || applying || planning || actionPending) return
    const reviewed = review
    const interrupted = pending
    if (resume ? !interrupted : !reviewed || !!interrupted || !!reviewed?.blockedResources.length) return
    if (resume && !window.confirm('Inspect and retry safe publication? Interrupted removals are not replayed automatically.')) return
    applying = true
    applyError = ''
    applyMessage = ''
    applyJob = null
    try {
      let job = resume && interrupted
        ? await startJob('managed_apply_retry', { confirm: true })
        : await startJob('workloads_apply', {
          candidate: currentYaml,
          expected: reviewed?.plan.baseRevision,
          fingerprint: reviewed?.plan.fingerprint,
        })
      applyJob = job
      while (job.state === 'running') {
        await new Promise(r => setTimeout(r, 1000))
        job = await invokeTool<Job>('jobs_get', { id: job.id })
        applyJob = job
      }
      if (job.state === 'succeeded') {
        const result = job.result as {stage?: string} | null
        if (result?.stage === 'aborted') applyMessage = 'The interrupted operation had not saved its YAML. Its prepared intent was discarded; configuration and native resources were preserved.'
        reviewedDraft = null
        await Promise.all([loadApplications(), loadWorkloads(), loadHistory()])
      } else {
        applyError = job.error ?? 'Application of plan failed. Check job output.'
      }
    } catch (e) {
      applyError = e instanceof Error ? e.message : 'Could not apply plan'
    } finally {
      await loadPending()
      applying = false
    }
  }

  async function revertToRevision(rev: string) {
    if (planning || applying || loadingWorkloads) return
    if (!window.confirm(`Revert workloads to revision ${rev.slice(0, 8)}?`)) return
    planning = true
    planError = ''
    reviewedDraft = null
    try {
      const result = await invokeTool<Review & { candidate: string }>('workloads_revert', {
        revision: rev,
        expected: currentRevision ?? undefined,
      })
      currentYaml = result.candidate
      reviewedDraft = { candidate: result.candidate, result }
      window.scrollTo({ top: document.body.scrollHeight, behavior: 'smooth' })
    } catch (e) {
      planError = e instanceof Error ? e.message : 'Revert preview failed'
    } finally {
      planning = false
    }
  }

  onMount(() => {
    void Promise.all([loadApplications(), loadWorkloads(), loadHistory(), loadPending()])
  })
</script>

<div class="applications-container">
  {#if recoveryError}
    <section class="panel">
      <p role="alert">{recoveryError}</p>
      <button type="button" onclick={loadPending}>Retry recovery check</button>
    </section>
  {:else if pending}
    <section class="panel" aria-labelledby="workload-recovery">
      <h2 id="workload-recovery">Interrupted workload apply</h2>
      <p>{pending.completed} of {pending.plan.changes.length} changes recorded.</p>
      <p>Recorded apply mode: {pending.mode === 'next_start' ? 'Next start — publish definitions without starting workloads' : 'Now'}.</p>
      <p>Plan fingerprint: <code>{pending.plan.fingerprint}</code></p>
      {#if applyStatus}
        <p>Durable stage: {applyStatus.stage}{applyStatus.updatedAt ? ' · ' + applyStatus.updatedAt : ''}</p>
        <ul>{#each Object.entries(applyStatus.resources ?? {}) as [resource, result]}
          <li>{resource}: {result.state}{result.message ? ' — ' + result.message : ''}</li>
        {/each}</ul>
      {/if}
      {#if writable}
        <button type="button" disabled={applying || !recoveryChecked || actionPending !== null} onclick={() => applyCurrentPlan(true)}>
          Retry safe publication
        </button>
      {/if}
    </section>
  {/if}
  <!-- Applications Inventory -->
  <section class="panel" aria-labelledby="apps-heading">
    <div class="panel-header">
      <h2 id="apps-heading">Applications</h2>
      <div class="filter-chips">
        {#each ['all', 'compose', 'quadlet', 'incus', 'libvirt'] as filterOption}
          <button
            type="button"
            class="chip"
            class:active={appFilter === filterOption}
            onclick={() => (appFilter = filterOption)}
          >
            {filterOption.charAt(0).toUpperCase() + filterOption.slice(1)}
          </button>
        {/each}
      </div>
    </div>

    {#if appError}
      <p role="alert" class="error-msg">{appError}</p>
    {/if}

    {#if loadingApps}
      <p role="status">Loading applications…</p>
    {:else if filteredApps.length === 0}
      <p class="empty-state">No applications found matching the selected filter.</p>
    {:else}
      <div class="apps-grid">
        {#each filteredApps as app (app.id)}
          <article class="app-card">
            <div class="app-card-header">
              <h3>{app.name}</h3>
              <span class="status-badge" class:running={app.state === 'running'}>
                {app.state}
              </span>
            </div>
            <p class="app-desc">{app.description || app.id}</p>
            <div class="app-meta">
              <span>Backend: <strong>{app.backend}</strong></span>
              {#if app.engine}
                <span>Engine: <strong>{app.engine}</strong></span>
              {/if}
              <span>Containers: <strong>{app.containerCount}</strong></span>
            </div>
            {#if app.restartRequired}
              <p role="status">Running containers differ from the saved configuration. Restart to apply the current definitions.</p>
            {/if}
            {#if app.members?.length}
              <ul>{#each app.members as member (member.name)}<li>{member.name}: {member.state}</li>{/each}</ul>
            {/if}
            <div class="app-actions">
              {#if writable && app.lifecycle}
                {#each (app.restartRequired ? ['stop'] : !['running', 'stopped'].includes(app.state) ? ['start', 'stop'] : [app.state === 'running' ? 'stop' : 'start']) as action}
                  <button type="button"
                    disabled={actionPending !== null || applying || !recoveryChecked || !!pending}
                    onclick={() => runAction(app.id, action)}>
                    {action === 'start' ? 'Start' : 'Stop'}
                  </button>
                {/each}
              <button
                type="button"
                disabled={actionPending !== null || applying || !recoveryChecked || !!pending}
                onclick={() => runAction(app.id, 'restart')}
              >
                Restart
              </button>
              {/if}
              {#if app.logs}<button type="button" onclick={() => showLogs(app.id)}>Logs</button>{/if}
            </div>
          </article>
        {/each}
      </div>
    {/if}
  </section>

  <!-- Logs Modal/Viewer -->
  {#if activeLogsId}
    <section class="panel logs-viewer" aria-labelledby="logs-heading">
      <div class="panel-header">
        <h2 id="logs-heading">Logs: {activeLogsId}</h2>
        <button type="button" onclick={closeLogs}>Close</button>
      </div>
      {#if logsLoading}
        <p role="status">Loading logs…</p>
      {:else if logsError}
        <p role="alert" class="error-msg">{logsError}</p>
      {:else if logsContent !== null}
        <pre class="logs-pre">{logsContent || '(No recent log output)'}</pre>
      {/if}
    </section>
  {/if}

  <!-- Declarative YAML Editor & Review -->
  <section class="panel" aria-labelledby="yaml-heading">
    <div class="panel-header">
      <h2 id="yaml-heading">Declarative Configuration</h2>
      {#if currentRevision}
        <span class="rev-badge">Rev: {currentRevision.slice(0, 8)}</span>
      {/if}
    </div>
    <p class="hint">
      Edit workloads, storage directories, and network shares in authoritative YAML.
    </p>

    <form onsubmit={(e) => { e.preventDefault(); void reviewPlan(); }}>
      <label for="workload-yaml" class="sr-only">Workload YAML</label>
      <textarea
        id="workload-yaml"
        class="yaml-textarea"
        bind:value={currentYaml}
        disabled={loadingWorkloads || planning || applying}
        rows="16"
        placeholder="version: 1&#10;services:&#10;  ...&#10;storage:&#10;  ...&#10;shares:&#10;  ..."
        spellcheck="false"
      ></textarea>

      <div class="editor-buttons">
        <button type="submit" disabled={loadingWorkloads || planning || applying}>
          {planning ? 'Planning changes…' : 'Review Plan'}
        </button>
      </div>
    </form>

    {#if planError}
      <p role="alert" class="error-msg">{planError}</p>
    {/if}

    {#if plan}
      <div class="plan-review">
        <h3>Plan Review</h3>
        <p>Fingerprint: <code>{plan.fingerprint}</code></p>
        {#if plan.changes.length === 0}
          <p>No native resource changes are required.</p>
        {:else}
          <table class="plan-table">
            <thead>
              <tr>
                <th>Resource</th>
                <th>Backend</th>
                <th>Operation</th>
              </tr>
            </thead>
            <tbody>
              {#each plan.changes as change}
                <tr>
                  <td><code>{change.resource}</code></td>
                  <td>{change.backend}</td>
                  <td><span class="op-badge op-{change.operation}">{change.operation}</span></td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
        {#if review?.blockedResources.length}
          <p role="status">This build or host cannot apply changes to: {review.blockedResources.join(', ')}.</p>
        {:else if writable && plan.baseRevision !== plan.desiredRevision}
          <button
            type="button"
            class="apply-btn"
            disabled={applying || !recoveryChecked || !!pending || actionPending !== null}
            onclick={() => applyCurrentPlan()}
          >
            {applying ? 'Applying…' : 'Apply Plan'}
          </button>
        {/if}
      </div>
    {/if}

    {#if applyError}
      <p role="alert" class="error-msg">{applyError}</p>
    {/if}

    {#if applyMessage}<p role="status">{applyMessage}</p>{/if}
    {#if applyJob}
      <div class="job-status">
        <p>Apply Job State: <strong>{applyJob.state}</strong></p>
      </div>
    {/if}
  </section>

  <!-- Revision History -->
  {#if history.length > 0}
    <section class="panel" aria-labelledby="history-heading">
      <h2 id="history-heading">Revision History</h2>
      <ul class="history-list">
        {#each history as rev}
          <li class="history-item">
            <code>{rev.slice(0, 16)}…</code>
            <button type="button" class="revert-btn" disabled={loadingWorkloads || planning || applying} onclick={() => revertToRevision(rev)}>
              Review Revert
            </button>
          </li>
        {/each}
      </ul>
    </section>
  {/if}
</div>

<style>
  .applications-container {
    display: grid;
    gap: 1.5rem;
  }
  .panel-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.75rem;
    margin-bottom: 1rem;
  }
  .filter-chips {
    display: flex;
    gap: 0.5rem;
  }
  .chip {
    padding: 0.3rem 0.75rem;
    min-height: 2rem;
    font-size: 0.85rem;
    border-radius: 999px;
    background: transparent;
  }
  .chip.active {
    background: var(--selected);
    border-color: var(--accent);
  }
  .apps-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(18rem, 1fr));
    gap: 1rem;
  }
  .app-card {
    padding: 1rem;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg);
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }
  .app-card-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  .app-card-header h3 {
    margin: 0;
    font-size: 1.1rem;
  }
  .status-badge {
    padding: 0.2rem 0.5rem;
    border-radius: 4px;
    font-size: 0.75rem;
    background: #3b2828;
    color: #ff9b9b;
  }
  .status-badge.running {
    background: #1c3b2b;
    color: #79f7a7;
  }
  .app-desc {
    color: var(--muted);
    font-size: 0.9rem;
    margin: 0;
  }
  .app-meta {
    display: flex;
    gap: 0.75rem;
    font-size: 0.8rem;
    color: var(--muted);
  }
  .app-actions {
    display: flex;
    gap: 0.5rem;
    margin-top: auto;
    padding-top: 0.5rem;
  }
  .app-actions button {
    flex: 1;
    min-height: 2.2rem;
    padding: 0.4rem;
    font-size: 0.85rem;
  }
  .logs-viewer {
    background: #080d17;
  }
  .logs-pre {
    background: #04070d;
    padding: 1rem;
    border-radius: 6px;
    max-height: 24rem;
    overflow: auto;
    font-size: 0.85rem;
    white-space: pre-wrap;
    word-break: break-all;
  }
  .yaml-textarea {
    width: 100%;
    font-family: monospace;
    font-size: 0.9rem;
    padding: 0.75rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    color: var(--text);
    resize: vertical;
  }
  .editor-buttons {
    margin-top: 0.75rem;
    display: flex;
    gap: 0.75rem;
  }
  .plan-review {
    margin-top: 1.5rem;
    padding: 1rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
  }
  .plan-table {
    width: 100%;
    border-collapse: collapse;
    margin: 1rem 0;
    font-size: 0.9rem;
  }
  .plan-table th, .plan-table td {
    padding: 0.5rem;
    text-align: left;
    border-bottom: 1px solid var(--border);
  }
  .op-badge {
    padding: 0.15rem 0.4rem;
    border-radius: 3px;
    font-size: 0.75rem;
    text-transform: uppercase;
  }
  .op-create { background: #1c3b2b; color: #79f7a7; }
  .op-update { background: #3d3419; color: #f7d579; }
  .op-remove { background: #3b2828; color: #ff9b9b; }
  .apply-btn {
    background: var(--accent);
    color: #fff;
    font-weight: 600;
  }
  .rev-badge {
    font-size: 0.8rem;
    padding: 0.2rem 0.5rem;
    border-radius: 4px;
    background: var(--selected);
  }
  .history-list {
    list-style: none;
    padding: 0;
    margin: 0;
    display: grid;
    gap: 0.5rem;
  }
  .history-item {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.5rem;
    border: 1px solid var(--border);
    border-radius: 4px;
  }
  .revert-btn {
    min-height: 2rem;
    padding: 0.3rem 0.6rem;
    font-size: 0.8rem;
  }
  .error-msg {
    color: #ff6b6b;
  }
  .hint {
    color: var(--muted);
    font-size: 0.9rem;
  }
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    padding: 0;
    margin: -1px;
    overflow: hidden;
    clip: rect(0, 0, 0, 0);
    border: 0;
  }
</style>
